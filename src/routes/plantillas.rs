//! Editor de las plantillas de fases — MASD-0024. Los valores de fábrica viven en `plantillas/*.json`;
//! lo que un administrador cambia se guarda como AJUSTES por fase (`PlantillaFasesAjuste`) y se
//! superpone a esa base. La plantilla efectiva se mantiene en memoria (`efectiva`) y cada cambio se
//! propaga a los proyectos ya iniciados: su copia congelada (`ProyectoFase.definicion`) recibe el
//! objetivo, los entregables y la puerta nuevos, y las marcas de criterios se reubican por texto.
//!
//! Se pueden editar por fase: objetivo, aprobador de la puerta, criterios, entregables (texto),
//! entregables requeridos (con su verificación) y roles de sesión. Las fases, su orden, sus
//! actividades y sus recursos no se tocan desde aquí.

use std::{
    collections::HashMap,
    sync::{LazyLock, RwLock},
};

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Map, Value};

use super::fases::{base_de, lista, validar_plantilla};
use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, new_id, B},
};

pub const COMERCIAL: &str = "PROYECTO_COMPLETO";
pub const INTERNA: &str = "INICIATIVA_INTERNA";

const APROBADORES: [&str; 7] = ["COMERCIAL", "LIDER_PREVENTA", "LIDER_TECNICO", "DIRECCION", "LIDER_PROYECTO", "CLIENTE", "DUENO"];
const TIPOS_VERIFICACION: [&str; 11] = ["nota_lead", "reunion_lead", "documento", "arquitectura", "propuesta", "tareas", "despliegue", "lead_registrado", "ficha_cliente", "ficha_contacto", "manual"];
const ROLES_FICHA: [&str; 3] = ["DECISOR", "REPRESENTANTE_LEGAL", "FIRMANTE"];
const TIPOS_SESION: [&str; 4] = ["KICKOFF", "PLANIFICACION", "REVISION", "LIBRE"];
const ARTEFACTOS: [&str; 13] = ["prd", "diseno", "plan-ejec", "plan", "arquitectura", "diagramas", "cronograma", "riesgos", "cumplimiento", "cambios", "revision", "entrega", "codigo"];
const TIPOS_TAREA: [&str; 7] = ["TASK", "FEATURE", "IMPROVEMENT", "DOCUMENTACION", "TEST_QA", "INFRAESTRUCTURA", "DESARROLLO"];
const PRIORIDADES: [&str; 4] = ["LOW", "MEDIUM", "HIGH", "CRITICAL"];
const ESTADOS_PROPUESTA: [&str; 5] = ["DRAFT", "SENT", "UNDER_REVIEW", "ACCEPTED", "REJECTED"];
const DOCUMENTOS: [&str; 3] = ["prd", "diseno", "plan"];
/// Formatos que puede esperar un entregable que marca una persona. Los automáticos llevan el de su verificación.
const FORMATOS_MANUALES: [&str; 6] = ["pdf", "pdf_firmado", "zip", "texto", "hoja", "enlace"];

/// Formato de un entregable que se verifica solo.
pub fn formato_automatico(tipo: &str) -> &'static str {
    match tipo {
        "nota_lead" => "nota",
        "reunion_lead" | "lead_registrado" => "registro",
        "documento" => "portal",
        "arquitectura" => "diagrama",
        "propuesta" => "propuesta",
        "tareas" => "backlog",
        "despliegue" => "url",
        "ficha_cliente" | "ficha_contacto" => "ficha",
        _ => "enlace",
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/fases/plantillas", get(listar))
        .route("/api/fases/plantillas/{codigo}", axum::routing::put(guardar))
        .route("/api/fases/plantillas/{codigo}/restaurar", post(restaurar))
        .route("/api/fases/plantillas/{codigo}/historial", get(historial))
}

// ── Plantilla efectiva en memoria ────────────────────────────────────────────────────────────
static EFECTIVAS: LazyLock<RwLock<HashMap<String, &'static Value>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

fn normalizar(codigo: &str) -> &'static str {
    if codigo == INTERNA { INTERNA } else { COMERCIAL }
}

/// La plantilla vigente de un código: la de fábrica con los ajustes superpuestos.
pub fn efectiva(codigo: &str) -> &'static Value {
    let k = normalizar(codigo);
    EFECTIVAS.read().ok().and_then(|m| m.get(k).copied()).unwrap_or_else(|| base_de(k))
}

/// Deja la plantilla efectiva en memoria (la referencia se conserva: las ediciones son poco frecuentes).
fn publicar(codigo: &str, def: Value) {
    let r: &'static Value = Box::leak(Box::new(def));
    if let Ok(mut m) = EFECTIVAS.write() {
        m.insert(normalizar(codigo).to_string(), r);
    }
}

/// Lee los ajustes guardados y arma las plantillas efectivas. Se llama al arrancar y tras cada cambio.
pub async fn cargar_efectivas(st: &AppState) {
    for codigo in [COMERCIAL, INTERNA] {
        let fila = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('fases', fases) FROM "PlantillaFasesAjuste" WHERE codigo = $1"#, &[B::T(codigo.into())]).await.ok().flatten();
        let Some(fila) = fila else { continue };
        let merged = aplicar_ajustes(base_de(codigo), &fila["fases"]);
        match validar_plantilla(&merged) {
            Ok(()) => publicar(codigo, merged),
            Err(e) => tracing::error!("ajustes de la plantilla {codigo} inválidos, se usa la de fábrica: {e}"),
        }
    }
}

/// Lleva a los proyectos ya iniciados lo que cambió en la plantilla vigente (por ejemplo, una versión nueva de fábrica). Es idempotente: si no hay
/// diferencias no toca nada.
pub async fn propagar_fabrica(st: &AppState) {
    for codigo in [COMERCIAL, INTERNA] {
        match propagar(st, codigo, efectiva(codigo), false).await {
            Ok((n, _)) if n > 0 => tracing::info!("[plantillas] {n} proyecto(s) de {codigo} puestos al día con la plantilla vigente"),
            Ok(_) => {}
            Err(_) => tracing::error!("[plantillas] no se pudo poner al día los proyectos de {codigo}"),
        }
    }
}

// ── Funciones puras ──────────────────────────────────────────────────────────────────────────
/// Superpone los ajustes (por clave de fase) a la plantilla de fábrica.
pub fn aplicar_ajustes(base: &Value, ajustes: &Value) -> Value {
    let mut out = base.clone();
    if let Some(fases) = out["fases"].as_array_mut() {
        for f in fases.iter_mut() {
            let clave = f["clave"].as_str().unwrap_or_default().to_string();
            let Some(a) = ajustes.get(&clave).filter(|a| a.is_object()) else { continue };
            for k in ["objetivo", "entregables", "requeridos", "sesiones", "actividades"] {
                if let Some(v) = a.get(k) {
                    f[k] = v.clone();
                }
            }
            if let Some(v) = a.get("aprobador") {
                f["puerta"]["aprobador"] = v.clone();
            }
            if let Some(v) = a.get("criterios") {
                f["puerta"]["criterios"] = v.clone();
            }
        }
    }
    // Una actividad no puede apuntar a un entregable que ya no existe en su fase: el vínculo se suelta solo
    if let Some(fases) = out["fases"].as_array_mut() {
        for f in fases.iter_mut() {
            let claves: Vec<String> = f["requeridos"].as_array().map(|a| a.iter().filter_map(|r| r["clave"].as_str().map(String::from)).collect()).unwrap_or_default();
            if let Some(acts) = f["actividades"].as_array_mut() {
                for a in acts.iter_mut() {
                    let huerfano = a["entregable"].as_str().map(|e| !claves.iter().any(|c| c == e)).unwrap_or(false);
                    if huerfano {
                        if let Some(o) = a.as_object_mut() {
                            o.remove("entregable");
                        }
                    }
                }
            }
        }
    }
    out
}

/// Lo editable de una fase, tal como lo ve y lo devuelve el editor.
fn editable(f: &Value) -> Value {
    json!({
        "clave": f["clave"], "numero": f["numero"], "bloque": f["bloque"], "nombre": f["nombre"], "leadStatus": f["leadStatus"], "tipoPuerta": f["puerta"]["tipo"],
        "objetivo": f["objetivo"], "aprobador": f["puerta"]["aprobador"], "criterios": f["puerta"]["criterios"],
        "entregables": f["entregables"], "requeridos": f["requeridos"].as_array().cloned().unwrap_or_default(), "sesiones": f["sesiones"].as_array().cloned().unwrap_or_default(),
        "actividades": f["actividades"].as_array().cloned().unwrap_or_default(),
    })
}

fn texto(v: &Value, min: usize, max: usize, que: &str) -> Result<String, String> {
    let t = v.as_str().map(str::trim).unwrap_or_default();
    let n = t.chars().count();
    if n < min || n > max {
        return Err(format!("{que}: debe tener entre {min} y {max} caracteres"));
    }
    Ok(t.to_string())
}

fn lista_textos(v: &Value, min: usize, max: usize, largo: usize, que: &str) -> Result<Vec<Value>, String> {
    let a = v.as_array().ok_or_else(|| format!("{que}: debe ser una lista"))?;
    if a.len() < min || a.len() > max {
        return Err(format!("{que}: debe tener entre {min} y {max} elementos"));
    }
    a.iter().map(|x| texto(x, 3, largo, que).map(Value::String)).collect()
}

fn clave_valida(v: &Value, que: &str) -> Result<String, String> {
    let c = v.as_str().unwrap_or_default();
    if c.len() < 2 || c.len() > 40 || !c.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
        return Err(format!("{que}: la clave debe tener 2 a 40 caracteres en minúscula, números o guion bajo"));
    }
    Ok(c.to_string())
}

fn entero(v: &Value, min: i64, max: i64, que: &str) -> Result<i64, String> {
    let n = v.as_i64().ok_or_else(|| format!("{que}: debe ser un número"))?;
    if n < min || n > max {
        return Err(format!("{que}: debe estar entre {min} y {max}"));
    }
    Ok(n)
}

fn limpiar_verificacion(v: &Value, con_lead: bool, comercial: bool, donde: &str) -> Result<Value, String> {
    let tipo = v["tipo"].as_str().unwrap_or_default();
    if !TIPOS_VERIFICACION.contains(&tipo) {
        return Err(format!("{donde}: tipo de verificación desconocido"));
    }
    match tipo {
        "nota_lead" => Ok(json!({ "tipo": tipo })),
        "reunion_lead" => {
            if !con_lead || !comercial {
                return Err(format!("{donde}: «{tipo}» solo aplica a las fases de preventa del proyecto comercial"));
            }
            Ok(json!({ "tipo": tipo }))
        }
        "lead_registrado" => {
            if !comercial {
                return Err(format!("{donde}: el lead solo existe en el proyecto comercial"));
            }
            Ok(json!({ "tipo": tipo }))
        }
        "ficha_cliente" => {
            if !comercial {
                return Err(format!("{donde}: la ficha del cliente solo existe en el proyecto comercial"));
            }
            if v["nivel"].as_str() == Some("basica") { Ok(json!({ "tipo": tipo, "nivel": "basica" })) } else { Ok(json!({ "tipo": tipo })) }
        }
        "ficha_contacto" => {
            let rol = v["rol"].as_str().unwrap_or_default();
            if !comercial || !ROLES_FICHA.contains(&rol) {
                return Err(format!("{donde}: elige a quién identificar en la ficha (decisor, representante legal o firmante)"));
            }
            Ok(json!({ "tipo": tipo, "rol": rol }))
        }
        "documento" => {
            let clave = v["clave"].as_str().unwrap_or_default();
            if !DOCUMENTOS.contains(&clave) {
                return Err(format!("{donde}: el documento debe ser prd, diseno o plan"));
            }
            if v["aprobado"] == true && v.get("min_requisitos").map(|x| !x.is_null()).unwrap_or(false) {
                return Err(format!("{donde}: elige «aprobado» o «mínimo de requisitos», no ambos"));
            }
            let mut o = json!({ "tipo": tipo, "clave": clave });
            if v["aprobado"] == true {
                o["aprobado"] = json!(true);
            }
            if let Some(m) = v.get("min_requisitos").filter(|x| !x.is_null()) {
                o["min_requisitos"] = json!(entero(m, 1, 50, &format!("{donde}: requisitos mínimos"))?);
            }
            Ok(o)
        }
        "arquitectura" => Ok(json!({ "tipo": tipo, "min": entero(v.get("min").unwrap_or(&json!(1)), 1, 50, &format!("{donde}: componentes mínimos"))? })),
        "propuesta" => {
            if !comercial {
                return Err(format!("{donde}: la propuesta solo existe en el proyecto comercial"));
            }
            let estados: Vec<&str> = v["estados"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
            if estados.is_empty() || !estados.iter().all(|e| ESTADOS_PROPUESTA.contains(e)) {
                return Err(format!("{donde}: elige al menos un estado válido de la propuesta"));
            }
            Ok(json!({ "tipo": tipo, "estados": estados }))
        }
        "tareas" => {
            let min = entero(v.get("min").unwrap_or(&json!(1)), 1, 500, &format!("{donde}: tareas mínimas"))?;
            let hechas = entero(v.get("hechas_min").unwrap_or(&json!(0)), 0, 500, &format!("{donde}: tareas hechas mínimas"))?;
            Ok(json!({ "tipo": tipo, "min": min, "hechas_min": hechas, "sin_fallidas": v["sin_fallidas"] == true }))
        }
        _ => Ok(json!({ "tipo": tipo })),
    }
}

/// Valida y limpia los ajustes de UNA fase. Devuelve solo los campos editables, ya normalizados.
fn limpiar_fase(f: &Value, a: &Value, comercial: bool, aprobadores: &[String]) -> Result<Value, String> {
    let clave = f["clave"].as_str().unwrap_or_default();
    let con_lead = f["leadStatus"].is_string();
    let mut o = Map::new();
    let obj = a.as_object().ok_or_else(|| format!("{clave}: ajustes inválidos"))?;
    for k in obj.keys() {
        if !["objetivo", "aprobador", "criterios", "entregables", "requeridos", "sesiones", "actividades"].contains(&k.as_str()) {
            return Err(format!("{clave}: no se puede editar «{k}»"));
        }
    }
    if let Some(v) = obj.get("objetivo") {
        o.insert("objetivo".into(), Value::String(texto(v, 5, 400, &format!("{clave}: objetivo"))?));
    }
    if let Some(v) = obj.get("aprobador") {
        let ap = v.as_str().unwrap_or_default();
        if !aprobadores.iter().any(|x| x == ap) {
            return Err(format!("{clave}: aprobador desconocido"));
        }
        o.insert("aprobador".into(), json!(ap));
    }
    if let Some(v) = obj.get("criterios") {
        o.insert("criterios".into(), Value::Array(lista_textos(v, 1, 12, 200, &format!("{clave}: criterios"))?));
    }
    if let Some(v) = obj.get("entregables") {
        o.insert("entregables".into(), Value::Array(lista_textos(v, 0, 12, 200, &format!("{clave}: entregables"))?));
    }
    if let Some(v) = obj.get("requeridos") {
        let a = v.as_array().ok_or_else(|| format!("{clave}: entregables requeridos debe ser una lista"))?;
        if a.is_empty() || a.len() > 8 {
            return Err(format!("{clave}: cada fase debe exigir entre 1 y 8 entregables"));
        }
        let mut claves: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (i, r) in a.iter().enumerate() {
            let donde = format!("{clave}: entregable {}", i + 1);
            let c = clave_valida(&r["clave"], &donde)?;
            if claves.contains(&c) {
                return Err(format!("{donde}: clave repetida «{c}»"));
            }
            claves.push(c.clone());
            let ayuda = r["ayuda"].as_str().map(str::trim).unwrap_or_default();
            if ayuda.chars().count() > 300 {
                return Err(format!("{donde}: la ayuda admite hasta 300 caracteres"));
            }
            let verifica = limpiar_verificacion(&r["verifica"], con_lead, comercial, &donde)?;
            let tipo = verifica["tipo"].as_str().unwrap_or("manual").to_string();
            let formato = if tipo == "manual" {
                let f = r["formato"].as_str().unwrap_or("enlace");
                if !FORMATOS_MANUALES.contains(&f) {
                    return Err(format!("{donde}: el formato esperado debe ser uno de {}", FORMATOS_MANUALES.join(", ")));
                }
                f
            } else {
                formato_automatico(&tipo)
            };
            out.push(json!({ "clave": c, "titulo": texto(&r["titulo"], 3, 200, &format!("{donde}: título"))?, "ayuda": ayuda, "formato": formato, "verifica": verifica }));
        }
        o.insert("requeridos".into(), Value::Array(out));
    }
    if let Some(v) = obj.get("sesiones") {
        let a = v.as_array().ok_or_else(|| format!("{clave}: roles de sesión debe ser una lista"))?;
        if a.is_empty() || a.len() > 4 {
            return Err(format!("{clave}: cada fase debe tener entre 1 y 4 roles de sesión"));
        }
        let mut claves: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (i, r) in a.iter().enumerate() {
            let donde = format!("{clave}: rol {}", i + 1);
            let c = clave_valida(&r["clave"], &donde)?;
            if claves.contains(&c) {
                return Err(format!("{donde}: clave repetida «{c}»"));
            }
            claves.push(c.clone());
            let tipo = r["tipo"].as_str().unwrap_or_default();
            if !TIPOS_SESION.contains(&tipo) {
                return Err(format!("{donde}: tipo de sesión desconocido"));
            }
            let arts: Vec<&str> = r["artefactos"].as_array().map(|x| x.iter().filter_map(|y| y.as_str()).collect()).unwrap_or_default();
            if !arts.iter().all(|x| ARTEFACTOS.contains(x)) {
                return Err(format!("{donde}: artefacto desconocido"));
            }
            out.push(json!({
                "clave": c, "titulo": texto(&r["titulo"], 3, 80, &format!("{donde}: título"))?, "tipo": tipo, "objetivo": texto(&r["objetivo"], 5, 300, &format!("{donde}: objetivo"))?,
                "artefactos": arts, "resultado": texto(&r["resultado"], 5, 300, &format!("{donde}: resultado esperado"))?,
                "cierre": lista_textos(&r["cierre"], 1, 8, 200, &format!("{donde}: lista de cierre"))?,
            }));
        }
        o.insert("sesiones".into(), Value::Array(out));
    }
    if let Some(v) = obj.get("actividades") {
        let a = v.as_array().ok_or_else(|| format!("{clave}: actividades debe ser una lista"))?;
        if a.is_empty() || a.len() > 6 {
            return Err(format!("{clave}: cada fase debe tener entre 1 y 6 actividades"));
        }
        let mut claves: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (i, r) in a.iter().enumerate() {
            let donde = format!("{clave}: actividad {}", i + 1);
            let c = clave_valida(&r["clave"], &donde)?;
            if claves.contains(&c) {
                return Err(format!("{donde}: clave repetida «{c}»"));
            }
            claves.push(c.clone());
            let tipo = r["tipo"].as_str().unwrap_or_default();
            if tipo != "AGENTE" && tipo != "HUMANA" {
                return Err(format!("{donde}: debe ser de un agente o de una persona"));
            }
            let (bt, pr) = (r["backlogType"].as_str().unwrap_or_default(), r["prioridad"].as_str().unwrap_or_default());
            if !TIPOS_TAREA.contains(&bt) {
                return Err(format!("{donde}: tipo de tarea desconocido"));
            }
            if !PRIORIDADES.contains(&pr) {
                return Err(format!("{donde}: prioridad desconocida"));
            }
            let mut item = json!({
                "clave": c, "titulo": texto(&r["titulo"], 3, 200, &format!("{donde}: título"))?, "tipo": tipo, "area": texto(&r["area"], 2, 60, &format!("{donde}: área"))?,
                "backlogType": bt, "prioridad": pr, "descripcion": texto(&r["descripcion"], 5, 1000, &format!("{donde}: descripción"))?,
            });
            // Entregable que esta actividad ayuda a cumplir (opcional): debe existir entre los de la fase
            if let Some(e) = r["entregable"].as_str().map(str::trim).filter(|e| !e.is_empty()) {
                let de_la_fase: Vec<String> = match obj.get("requeridos") {
                    Some(rq) => rq.as_array().map(|x| x.iter().filter_map(|y| y["clave"].as_str().map(String::from)).collect()).unwrap_or_default(),
                    None => f["requeridos"].as_array().map(|x| x.iter().filter_map(|y| y["clave"].as_str().map(String::from)).collect()).unwrap_or_default(),
                };
                if !de_la_fase.iter().any(|c| c == e) {
                    return Err(format!("{donde}: el entregable «{e}» no existe en esta fase"));
                }
                item["entregable"] = json!(e);
            }
            out.push(item);
        }
        o.insert("actividades".into(), Value::Array(out));
    }
    Ok(Value::Object(o))
}

fn aprobadores_validos() -> Vec<String> {
    let mut v: Vec<String> = APROBADORES.iter().map(|s| s.to_string()).collect();
    for c in [COMERCIAL, INTERNA] {
        for f in lista(base_de(c)) {
            if let Some(a) = f["puerta"]["aprobador"].as_str() {
                if !v.iter().any(|x| x == a) {
                    v.push(a.to_string());
                }
            }
        }
    }
    v
}

/// Valida los ajustes recibidos sobre la plantilla de fábrica y devuelve los ajustes limpios y la plantilla resultante.
pub fn validar_ajustes(codigo: &str, ajustes: &Value) -> Result<(Value, Value), String> {
    let base = base_de(normalizar(codigo));
    let obj = ajustes.as_object().ok_or("los ajustes deben ser un objeto por fase")?;
    let aprobadores = aprobadores_validos();
    let mut limpio = Map::new();
    for (clave, a) in obj {
        let f = lista(base).iter().find(|f| f["clave"] == clave.as_str()).ok_or_else(|| format!("la fase «{clave}» no existe en la plantilla"))?;
        limpio.insert(clave.clone(), limpiar_fase(f, a, normalizar(codigo) == COMERCIAL, &aprobadores)?);
    }
    let limpio = Value::Object(limpio);
    let merged = aplicar_ajustes(base, &limpio);
    validar_plantilla(&merged)?;
    Ok((limpio, merged))
}

fn marcas_reubicadas(viejos: &[Value], nuevos: &[Value], marcas: &[Value]) -> Vec<Value> {
    nuevos
        .iter()
        .map(|t| viejos.iter().position(|v| v == t).and_then(|i| marcas.get(i).cloned()).unwrap_or_else(|| json!({ "ok": false, "por": null, "en": null })))
        .collect()
}

/// Copia a una definición congelada lo editable de la plantilla nueva y reubica las marcas de criterios. Devuelve si hubo cambios.
pub fn propagar_a_definicion(def: &mut Value, criterios: &mut Value, nueva: &Value) -> bool {
    let mut cambio = false;
    let n_fases = lista(nueva).len();
    for fn_ in lista(nueva).iter().take(n_fases) {
        let clave = fn_["clave"].as_str().unwrap_or_default().to_string();
        let Some(fv) = def["fases"].as_array_mut().and_then(|a| a.iter_mut().find(|f| f["clave"] == clave.as_str())) else { continue };
        let viejos: Vec<Value> = fv["puerta"]["criterios"].as_array().cloned().unwrap_or_default();
        let nuevos: Vec<Value> = fn_["puerta"]["criterios"].as_array().cloned().unwrap_or_default();
        for k in ["objetivo", "entregables"] {
            if fv[k] != fn_[k] {
                fv[k] = fn_[k].clone();
                cambio = true;
            }
        }
        if fv["puerta"]["aprobador"] != fn_["puerta"]["aprobador"] {
            fv["puerta"]["aprobador"] = fn_["puerta"]["aprobador"].clone();
            cambio = true;
        }
        if fv["actividades"] != fn_["actividades"] {
            fv["actividades"] = fn_["actividades"].clone();
            cambio = true;
        }
        if viejos != nuevos {
            fv["puerta"]["criterios"] = Value::Array(nuevos.clone());
            if let Some(m) = criterios.get(&clave).and_then(|m| m.as_array()).cloned() {
                criterios[&clave] = Value::Array(marcas_reubicadas(&viejos, &nuevos, &m));
            }
            cambio = true;
        }
    }
    cambio
}

// ── Persistencia ─────────────────────────────────────────────────────────────────────────────
async fn ajustes_guardados(st: &AppState, codigo: &str) -> Result<Value, ApiError> {
    Ok(fetch_json_opt(&st.pool, r#"SELECT fases FROM "PlantillaFasesAjuste" WHERE codigo = $1"#, &[B::T(codigo.into())]).await?.unwrap_or_else(|| json!({})))
}

/// Actualiza las tareas del Backlog de las actividades que cambiaron, SOLO si siguen en cola y su texto es exactamente el que generó la plantilla
/// anterior (nadie las empezó ni las editó). Devuelve cuántas tareas se actualizaron.
async fn sincronizar_tareas(st: &AppState, sol: &str, anterior: &Value, nuevo: &Value) -> usize {
    let mut total = 0;
    for fn_ in lista(nuevo) {
        let fase = fn_["clave"].as_str().unwrap_or_default();
        let Some(fv) = lista(anterior).iter().find(|f| f["clave"] == fase) else { continue };
        for act_n in fn_["actividades"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let k = act_n["clave"].as_str().unwrap_or_default();
            let Some(act_v) = fv["actividades"].as_array().and_then(|a| a.iter().find(|x| x["clave"] == k)) else { continue };
            if act_v == act_n {
                continue;
            }
            let Some(item) = fetch_text_opt(&st.pool, r#"SELECT "backlogItemId" FROM "FaseActividad" WHERE "solucionId" = $1 AND fase = $2 AND clave = $3"#, &[B::T(sol.into()), B::T(fase.into()), B::T(k.into())]).await.ok().flatten() else { continue };
            let ((t_v, d_v), (t_n, d_n)) = (super::fases::texto_tarea(fv, act_v), super::fases::texto_tarea(fn_, act_n));
            let area = fetch_text_opt(&st.pool, r#"SELECT id FROM "Area" WHERE name = $1"#, &[B::T(act_n["area"].as_str().unwrap_or_default().into())]).await.ok().flatten();
            let n = exec(
                &st.pool,
                r#"UPDATE "BacklogItem" SET title = $2, description = $3, type = $4, priority = $5, "areaId" = COALESCE($6, "areaId"), "updatedAt" = NOW() WHERE id = $1 AND status = 'BACKLOG' AND title = $7 AND description = $8"#,
                &[B::T(item), B::T(t_n), B::T(d_n), B::T(act_n["backlogType"].as_str().unwrap_or("TASK").into()), B::T(act_n["prioridad"].as_str().unwrap_or("MEDIUM").into()), B::OT(area), B::T(t_v), B::T(d_v)],
            )
            .await
            .unwrap_or(0);
            total += n as usize;
        }
    }
    total
}

async fn propagar(st: &AppState, codigo: &str, nueva: &Value, actualizar_tareas: bool) -> Result<(usize, usize), ApiError> {
    let filas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', "solucionId", 'def', definicion, 'criterios', criterios)), '[]'::jsonb) FROM "ProyectoFase" WHERE plantilla = $1"#,
        &[B::T(codigo.into())],
    )
    .await?;
    let (mut n, mut tareas) = (0, 0);
    for fila in filas.as_array().cloned().unwrap_or_default() {
        let (mut def, mut crit) = (fila["def"].clone(), fila["criterios"].clone());
        let anterior = def.clone();
        if propagar_a_definicion(&mut def, &mut crit, nueva) {
            if actualizar_tareas {
                tareas += sincronizar_tareas(st, fila["id"].as_str().unwrap_or_default(), &anterior, &def).await;
            }
            exec(&st.pool, r#"UPDATE "ProyectoFase" SET definicion = $2, criterios = $3, "updatedAt" = NOW() WHERE "solucionId" = $1"#, &[B::T(fila["id"].as_str().unwrap_or_default().into()), B::J(def), B::J(crit)]).await?;
            n += 1;
        }
    }
    Ok((n, tareas))
}

async fn vista_de(st: &AppState, codigo: &str) -> Result<Value, ApiError> {
    let meta = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('version', version, 'actualizadoPor', "actualizadoPor", 'actualizadoEn', "updatedAt", 'fases', fases) FROM "PlantillaFasesAjuste" WHERE codigo = $1"#,
        &[B::T(codigo.into())],
    )
    .await?;
    let ef = efectiva(codigo);
    let modificada = meta.as_ref().map(|m| m["fases"].as_object().map(|o| !o.is_empty()).unwrap_or(false)).unwrap_or(false);
    Ok(json!({
        "codigo": codigo, "nombre": ef["nombre"], "modificada": modificada,
        "version": meta.as_ref().map(|m| m["version"].clone()).unwrap_or(json!(0)),
        "actualizadoPor": meta.as_ref().map(|m| m["actualizadoPor"].clone()).unwrap_or(Value::Null),
        "actualizadoEn": meta.as_ref().map(|m| m["actualizadoEn"].clone()).unwrap_or(Value::Null),
        "efectiva": lista(ef).iter().map(editable).collect::<Vec<_>>(),
        "base": lista(base_de(codigo)).iter().map(editable).collect::<Vec<_>>(),
    }))
}

fn catalogo() -> Value {
    json!({
        "aprobadores": aprobadores_validos(), "tiposVerificacion": TIPOS_VERIFICACION, "tiposSesion": TIPOS_SESION, "artefactos": ARTEFACTOS,
        "estadosPropuesta": ESTADOS_PROPUESTA, "documentos": DOCUMENTOS, "formatosManuales": FORMATOS_MANUALES, "rolesFicha": ROLES_FICHA,
        "tiposTarea": TIPOS_TAREA, "prioridades": PRIORIDADES,
    })
}

async fn listar(State(st): State<AppState>, se: Session) -> ApiResult<Json<Value>> {
    let mut plantillas = Vec::new();
    for c in [COMERCIAL, INTERNA] {
        plantillas.push(vista_de(&st, c).await?);
    }
    let mut cat = catalogo();
    cat["areas"] = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(name ORDER BY name), '[]'::jsonb) FROM "Area""#, &[]).await?;
    Ok(Json(json!({ "puedeEditar": se.is_admin() || se.is_service, "catalogo": cat, "plantillas": plantillas })))
}

fn exigir_admin(se: &Session) -> Result<String, ApiError> {
    if se.is_admin() || se.is_service {
        Ok(if se.name.is_empty() { se.email.clone() } else { se.name.clone() })
    } else {
        Err(ApiError::forbidden("Solo un administrador puede editar las plantillas de fases"))
    }
}

fn codigo_valido(c: &str) -> Result<&'static str, ApiError> {
    if c == COMERCIAL { Ok(COMERCIAL) } else if c == INTERNA { Ok(INTERNA) } else { Err(ApiError::not_found("Plantilla no encontrada")) }
}

/// Guarda ajustes ya validados: queda el historial, se actualiza la plantilla en memoria y se propaga a los proyectos iniciados.
async fn aplicar(st: &AppState, codigo: &str, ajustes: Value, merged: Value, nota: Option<String>, quien: &str, actualizar_tareas: bool) -> Result<(usize, usize), ApiError> {
    let version = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(version) FROM "PlantillaFasesAjuste" WHERE codigo = $1"#, &[B::T(codigo.into())]).await?.and_then(|v| v.as_i64()).unwrap_or(0) + 1;
    exec(
        &st.pool,
        r#"INSERT INTO "PlantillaFasesAjuste" (codigo, fases, version, "actualizadoPor", "updatedAt") VALUES ($1, $2, $3, $4, NOW())
           ON CONFLICT (codigo) DO UPDATE SET fases = EXCLUDED.fases, version = EXCLUDED.version, "actualizadoPor" = EXCLUDED."actualizadoPor", "updatedAt" = NOW()"#,
        &[B::T(codigo.into()), B::J(ajustes.clone()), B::I(version), B::T(quien.into())],
    )
    .await?;
    exec(
        &st.pool,
        r#"INSERT INTO "PlantillaFasesHistorial" (id, codigo, fases, version, nota, "usuarioNombre") VALUES ($1, $2, $3, $4, $5, $6)"#,
        &[B::T(new_id()), B::T(codigo.into()), B::J(ajustes), B::I(version), B::OT(nota), B::T(quien.into())],
    )
    .await?;
    publicar(codigo, merged.clone());
    propagar(st, codigo, &merged, actualizar_tareas).await
}

async fn guardar(State(st): State<AppState>, se: Session, Path(codigo): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let quien = exigir_admin(&se)?;
    let codigo = codigo_valido(&codigo)?;
    // Lo recibido reemplaza fase por fase a lo ya guardado.
    let mut ajustes = ajustes_guardados(&st, codigo).await?;
    if !ajustes.is_object() {
        ajustes = json!({});
    }
    for (k, v) in body["fases"].as_object().ok_or_else(|| ApiError::bad_request("Faltan las fases a guardar"))? {
        ajustes[k] = v.clone();
    }
    let (limpio, merged) = validar_ajustes(codigo, &ajustes).map_err(ApiError::bad_request)?;
    let nota = body["nota"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(300).collect::<String>());
    // Las áreas de las actividades deben existir (de ahí sale el área de la tarea del Backlog)
    let existentes = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(name), '[]'::jsonb) FROM "Area""#, &[]).await?;
    let existentes: Vec<&str> = existentes.as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
    for f in limpio.as_object().map(|o| o.values()).into_iter().flatten() {
        for a in f["actividades"].as_array().map(|x| x.as_slice()).unwrap_or(&[]) {
            if !existentes.contains(&a["area"].as_str().unwrap_or_default()) {
                return Err(ApiError::bad_request(format!("El área «{}» no existe", a["area"].as_str().unwrap_or_default())));
            }
        }
    }
    let (proyectos, tareas) = aplicar(&st, codigo, limpio, merged, nota, &quien, body["actualizarTareas"] == true).await?;
    Ok(Json(json!({ "ok": true, "proyectosActualizados": proyectos, "tareasActualizadas": tareas, "plantilla": vista_de(&st, codigo).await? })))
}

/// Vuelve a los valores de fábrica (`base: true`), a los de una fase (`fase`) o a una versión del historial (`historialId`).
async fn restaurar(State(st): State<AppState>, se: Session, Path(codigo): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let quien = exigir_admin(&se)?;
    let codigo = codigo_valido(&codigo)?;
    let (ajustes, nota): (Value, String) = if let Some(h) = body["historialId"].as_str() {
        let f = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('fases', fases, 'version', version) FROM "PlantillaFasesHistorial" WHERE id = $1 AND codigo = $2"#, &[B::T(h.into()), B::T(codigo.into())])
            .await?
            .ok_or_else(|| ApiError::not_found("Esa versión no existe"))?;
        (f["fases"].clone(), format!("Restaurada la versión {}", f["version"]))
    } else if let Some(fase) = body["fase"].as_str() {
        let mut a = ajustes_guardados(&st, codigo).await?;
        if let Some(o) = a.as_object_mut() {
            o.remove(fase);
        }
        (a, format!("Fase «{fase}» vuelta a los valores de fábrica"))
    } else if body["base"] == true {
        (json!({}), "Plantilla vuelta a los valores de fábrica".into())
    } else {
        return Err(ApiError::bad_request("Indica qué restaurar: base, fase o historialId"));
    };
    let (limpio, merged) = validar_ajustes(codigo, &ajustes).map_err(ApiError::bad_request)?;
    // Al volver a la base, las fases sin ajuste se reescriben con los valores de fábrica en los proyectos iniciados.
    let (proyectos, _) = aplicar(&st, codigo, limpio, merged, Some(nota), &quien, false).await?;
    Ok(Json(json!({ "ok": true, "proyectosActualizados": proyectos, "tareasActualizadas": 0, "plantilla": vista_de(&st, codigo).await? })))
}

async fn historial(State(st): State<AppState>, _se: Session, Path(codigo): Path<String>) -> ApiResult<Json<Value>> {
    let codigo = codigo_valido(&codigo)?;
    let h = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', h.id, 'version', h.version, 'nota', h.nota, 'usuario', h."usuarioNombre", 'en', h."createdAt", 'fasesEditadas', (SELECT COUNT(*) FROM jsonb_object_keys(h.fases))) ORDER BY h."createdAt" DESC), '[]'::jsonb)
             FROM (SELECT * FROM "PlantillaFasesHistorial" WHERE codigo = $1 ORDER BY "createdAt" DESC LIMIT 30) h"#,
        &[B::T(codigo.into())],
    )
    .await?;
    Ok(Json(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todos_los_ajustes(codigo: &str) -> Value {
        let mut m = Map::new();
        for f in lista(base_de(codigo)) {
            let mut e = editable(f);
            let o = e.as_object_mut().unwrap();
            for k in ["clave", "numero", "bloque", "nombre", "leadStatus", "tipoPuerta"] {
                o.remove(k);
            }
            m.insert(f["clave"].as_str().unwrap().to_string(), e);
        }
        Value::Object(m)
    }

    #[test]
    fn los_valores_de_fabrica_pasan_la_validacion_y_no_cambian_nada() {
        for c in [COMERCIAL, INTERNA] {
            let (limpio, merged) = validar_ajustes(c, &todos_los_ajustes(c)).unwrap_or_else(|e| panic!("{c}: {e}"));
            assert!(limpio.as_object().unwrap().len() == lista(base_de(c)).len());
            for (a, b) in lista(&merged).iter().zip(lista(base_de(c))) {
                assert_eq!(a["objetivo"], b["objetivo"]);
                assert_eq!(a["puerta"]["criterios"], b["puerta"]["criterios"]);
                // la limpieza completa los valores por defecto de la verificación: se compara lo esencial
                let esencial = |v: &Value| v["requeridos"].as_array().unwrap().iter().map(|r| (r["clave"].clone(), r["titulo"].clone(), r["verifica"]["tipo"].clone())).collect::<Vec<_>>();
                assert_eq!(esencial(a), esencial(b));
                for r in a["requeridos"].as_array().unwrap() {
                    assert!(r["formato"].as_str().is_some(), "{}: sin formato", r["clave"]);
                }
            }
        }
    }

    #[test]
    fn rechaza_ajustes_malos() {
        let mal = |a: Value| validar_ajustes(COMERCIAL, &a).is_err();
        assert!(mal(json!({ "no_existe": { "objetivo": "algo largo" } })));
        assert!(mal(json!({ "diagnostico": { "criterios": [] } })));
        assert!(mal(json!({ "diagnostico": { "aprobador": "NADIE" } })));
        assert!(mal(json!({ "diagnostico": { "puerta": {} } })));
        assert!(mal(json!({ "diagnostico": { "requeridos": [] } })));
        assert!(mal(json!({ "diagnostico": { "requeridos": [{ "clave": "x", "titulo": "Algo", "verifica": { "tipo": "manual" } }] } })), "clave muy corta");
        assert!(mal(json!({ "diagnostico": { "requeridos": [{ "clave": "algo", "titulo": "Algo", "verifica": { "tipo": "inventado" } }] } })));
        assert!(mal(json!({ "arranque": { "requeridos": [{ "clave": "nota", "titulo": "Reunión del lead", "verifica": { "tipo": "reunion_lead" } }] } })), "reunion_lead solo en preventa");
        assert!(mal(json!({ "contacto": { "requeridos": [{ "clave": "algo", "titulo": "Algo", "formato": "docx", "verifica": { "tipo": "manual" } }] } })), "formato manual desconocido");
        assert!(validar_ajustes(INTERNA, &json!({ "idea": { "requeridos": [{ "clave": "prop", "titulo": "Propuesta enviada", "verifica": { "tipo": "propuesta", "estados": ["SENT"] } }] } })).is_err());
    }

    #[test]
    fn acepta_un_cambio_valido_y_lo_superpone() {
        let a = json!({ "contacto": { "objetivo": "Un objetivo nuevo para esta fase", "criterios": ["Primero", "Segundo criterio"],
            "requeridos": [{ "clave": "mi_entregable", "titulo": "Mi entregable", "ayuda": "", "verifica": { "tipo": "documento", "clave": "prd", "min_requisitos": 2 } }] } });
        let (_, merged) = validar_ajustes(COMERCIAL, &a).unwrap();
        let f = lista(&merged).iter().find(|f| f["clave"] == "contacto").unwrap();
        assert_eq!(f["objetivo"], "Un objetivo nuevo para esta fase");
        assert_eq!(f["puerta"]["criterios"].as_array().unwrap().len(), 2);
        assert_eq!(f["requeridos"][0]["verifica"]["min_requisitos"], 2);
        // las demás fases quedan como estaban
        assert_eq!(lista(&merged)[0]["objetivo"], lista(base_de(COMERCIAL))[0]["objetivo"]);
    }

    #[test]
    fn las_actividades_se_editan_con_reglas() {
        let act = |extra: Value| {
            let mut a = json!({ "clave": "mi_actividad", "titulo": "Hacer algo útil", "tipo": "AGENTE", "area": "Sales & Presales", "backlogType": "TASK", "prioridad": "HIGH", "descripcion": "Qué hay que hacer y cómo." });
            for (k, v) in extra.as_object().cloned().unwrap_or_default() {
                a[k] = v;
            }
            a
        };
        let con = |acts: Vec<Value>| json!({ "diagnostico": { "actividades": acts } });
        let (_, merged) = validar_ajustes(COMERCIAL, &con(vec![act(json!({}))])).unwrap();
        let f = lista(&merged).iter().find(|f| f["clave"] == "diagnostico").unwrap();
        assert_eq!(f["actividades"].as_array().unwrap().len(), 1);
        assert_eq!(f["actividades"][0]["titulo"], "Hacer algo útil");
        assert!(validar_ajustes(COMERCIAL, &con(vec![])).is_err(), "al menos una");
        assert!(validar_ajustes(COMERCIAL, &con(vec![act(json!({ "tipo": "ROBOT" }))])).is_err());
        assert!(validar_ajustes(COMERCIAL, &con(vec![act(json!({ "prioridad": "URGENTE" }))])).is_err());
        assert!(validar_ajustes(COMERCIAL, &con(vec![act(json!({ "backlogType": "COSA" }))])).is_err());
        assert!(validar_ajustes(COMERCIAL, &con(vec![act(json!({ "descripcion": "x" }))])).is_err());
        assert!(validar_ajustes(COMERCIAL, &con(vec![act(json!({})), act(json!({}))])).is_err(), "clave repetida");
        let siete: Vec<Value> = (0..7).map(|i| act(json!({ "clave": format!("act_{i}x") }))).collect();
        assert!(validar_ajustes(COMERCIAL, &con(siete)).is_err(), "máximo 6");
    }

    #[test]
    fn el_vinculo_actividad_entregable_se_valida_y_se_suelta_si_el_entregable_desaparece() {
        let f = lista(base_de(COMERCIAL)).iter().find(|f| f["clave"] == "identificacion").unwrap();
        // los vínculos de fábrica apuntan a entregables que existen
        for c in [COMERCIAL, INTERNA] {
            for f in lista(base_de(c)) {
                let claves: Vec<&str> = f["requeridos"].as_array().unwrap().iter().filter_map(|r| r["clave"].as_str()).collect();
                for a in f["actividades"].as_array().unwrap() {
                    if let Some(e) = a["entregable"].as_str() {
                        assert!(claves.contains(&e), "{c}/{}/{}: «{e}»", f["clave"], a["clave"]);
                    }
                }
            }
        }
        assert_eq!(f["actividades"][0]["entregable"], "ficha_cliente");
        let mut acts = f["actividades"].as_array().unwrap().clone();
        acts[0]["entregable"] = json!("no_existe");
        assert!(validar_ajustes(COMERCIAL, &json!({ "identificacion": { "actividades": acts.clone() } })).is_err(), "entregable inexistente");
        acts[0]["entregable"] = json!("decision");
        let (_, m) = validar_ajustes(COMERCIAL, &json!({ "identificacion": { "actividades": acts } })).unwrap();
        assert_eq!(lista(&m)[0]["actividades"][0]["entregable"], "decision");
        // si el entregable se quita de la fase, el vínculo de la actividad de fábrica se suelta solo (no bloquea guardar)
        let solo_lead = f["requeridos"].as_array().unwrap().iter().filter(|r| r["clave"] == "lead_registrado").cloned().collect::<Vec<_>>();
        let (_, m2) = validar_ajustes(COMERCIAL, &json!({ "identificacion": { "requeridos": solo_lead } })).unwrap();
        assert!(lista(&m2)[0]["actividades"][0].get("entregable").is_none());
    }

    #[test]
    fn el_texto_de_la_tarea_sale_de_un_solo_sitio() {
        let f = lista(base_de(COMERCIAL)).iter().find(|f| f["clave"] == "identificacion").unwrap();
        let act = &f["actividades"][0];
        let (t, d) = super::super::fases::texto_tarea(f, act);
        assert!(t.starts_with("[Fase 1 · Identificación]") && t.ends_with(act["titulo"].as_str().unwrap()));
        assert!(d.contains("Actividad para un agente"));
        let (_, d2) = super::super::fases::texto_tarea(f, &f["actividades"][1]);
        assert!(d2.contains("Actividad humana"));
    }

    #[test]
    fn las_marcas_de_criterios_se_reubican_por_texto() {
        let viejos = vec![json!("A"), json!("B"), json!("C")];
        let marcas = vec![json!({ "ok": true, "por": "x", "en": "t" }), json!({ "ok": false, "por": null, "en": null }), json!({ "ok": true, "por": "y", "en": "t" })];
        let nuevos = vec![json!("C"), json!("Nuevo"), json!("A")];
        let r = marcas_reubicadas(&viejos, &nuevos, &marcas);
        assert_eq!(r[0]["ok"], true);
        assert_eq!(r[0]["por"], "y");
        assert_eq!(r[1]["ok"], false);
        assert_eq!(r[2]["por"], "x");
    }

    #[test]
    fn propagar_actualiza_la_copia_congelada_de_un_proyecto() {
        let mut def = base_de(COMERCIAL).clone();
        let mut crit = json!({ "contacto": [{ "ok": true, "por": "x", "en": "t" }, { "ok": false, "por": null, "en": null }] });
        let nuevo_c = json!(["Segundo nuevo", "Otro"]);
        let a = json!({ "contacto": { "objetivo": "Otro objetivo de la fase", "aprobador": "DIRECCION", "criterios": nuevo_c } });
        let (_, merged) = validar_ajustes(COMERCIAL, &a).unwrap();
        assert!(propagar_a_definicion(&mut def, &mut crit, &merged));
        let f = lista(&def).iter().find(|f| f["clave"] == "contacto").unwrap();
        assert_eq!(f["objetivo"], "Otro objetivo de la fase");
        assert_eq!(f["puerta"]["aprobador"], "DIRECCION");
        assert_eq!(crit["contacto"].as_array().unwrap().len(), 2);
        assert_eq!(crit["contacto"][0]["ok"], false);
        // sin cambios no hay nada que guardar
        assert!(!propagar_a_definicion(&mut def, &mut crit, &merged));
        // las actividades editadas también viajan a la copia congelada
        let b = json!({ "contacto": { "actividades": [{ "clave": "nueva_act", "titulo": "Actividad nueva", "tipo": "HUMANA", "area": "Sales & Presales", "backlogType": "TASK", "prioridad": "MEDIUM", "descripcion": "Descripción nueva" }] } });
        let (_, merged2) = validar_ajustes(COMERCIAL, &b).unwrap();
        assert!(propagar_a_definicion(&mut def, &mut crit, &merged2));
        let f = lista(&def).iter().find(|f| f["clave"] == "contacto").unwrap();
        assert_eq!(f["actividades"][0]["clave"], "nueva_act");
    }
}
