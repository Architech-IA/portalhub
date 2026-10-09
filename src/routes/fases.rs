//! Motor de fases — MASD-0024. Conduce un proyecto desde que es un lead hasta la entrega, sobre la
//! MISMA Solución: 6 fases de preventa (las etapas del lead) y 6 de ejecución. Cada fase trae sus
//! actividades (que se vuelven tareas del Backlog), sus entregables y una puerta con criterios que
//! una persona marca antes de pasar a la siguiente.
//!
//! - La plantilla vive en `plantillas/proyecto_completo.json` y se COPIA a `ProyectoFase.definicion`
//!   al iniciar: cambiar la plantilla después no altera los proyectos en curso.
//! - El estado del lead y la fase se mantienen sincronizados en ambos sentidos
//!   (`tras_actualizar_lead` lo llama `leads.rs`; avanzar/retroceder actualizan el lead).
//! - La puerta de Negociación pide el resultado: GANADO convierte la preventa en proyecto sobre la
//!   misma Solución (sin copiar nada) y PERDIDO cierra el lead.
//! - Mover fases es cosa de administradores; saltarse criterios (`forzar`) solo de un SUPERADMIN.

use std::sync::LazyLock;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, new_id, s, s_no_vacio, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proyectos/{id}/fases", get(obtener))
        .route("/api/proyectos/{id}/fases/iniciar", post(iniciar))
        .route("/api/proyectos/{id}/fases/criterio", post(criterio))
        .route("/api/proyectos/{id}/fases/avanzar", post(avanzar))
        .route("/api/proyectos/{id}/fases/retroceder", post(retroceder))
        .route("/api/proyectos/{id}/prd/versiones", get(prd_versiones))
        .route("/api/proyectos/{id}/prd/versiones/{version}", get(prd_version))
}

const PLANTILLA_JSON: &str = include_str!("../../plantillas/proyecto_completo.json");
const FASE_ARRANQUE: &str = "arranque";
/// Lugares donde vive lo que produce una fase (cada tipo se resuelve en `resolver_recursos`).
const TIPOS_RECURSO: [&str; 12] = [
    "LEAD_HUB", "PROPUESTAS", "HUB_PRD", "HUB_DISENO", "HUB_ARQUITECTURA", "HUB_PLAN", "HUB_CODIGO", "HUB_DESPLIEGUE", "BACKLOG", "SALA_CONTROL", "REUNIONES", "ADJUNTOS",
];

static BASE: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(PLANTILLA_JSON).expect("plantilla de fases inválida"));

// ── Plantilla (funciones puras) ─────────────────────────────────────────────────────────────
pub(super) fn lista(def: &Value) -> &[Value] {
    def["fases"].as_array().map(|a| a.as_slice()).unwrap_or(&[])
}

pub(super) fn indice(def: &Value, clave: &str) -> Option<usize> {
    lista(def).iter().position(|f| f["clave"] == clave)
}

fn clave_de(def: &Value, idx: usize) -> String {
    lista(def).get(idx).and_then(|f| f["clave"].as_str()).unwrap_or_default().to_string()
}

fn es_preventa(def: &Value, idx: usize) -> bool {
    lista(def).get(idx).map(|f| f["bloque"] == "PREVENTA").unwrap_or(false)
}

/// Comprueba que una plantilla es coherente antes de usarla (se corre en las pruebas y al iniciar).
fn validar_plantilla(def: &Value) -> Result<(), String> {
    let fases = lista(def);
    if fases.is_empty() {
        return Err("la plantilla no tiene fases".into());
    }
    let mut claves: Vec<&str> = Vec::new();
    let mut estados_lead: Vec<&str> = Vec::new();
    let mut resultados = 0;
    let mut en_ejecucion = false;
    for (i, f) in fases.iter().enumerate() {
        let clave = f["clave"].as_str().filter(|c| !c.is_empty()).ok_or("fase sin clave")?;
        if claves.contains(&clave) {
            return Err(format!("clave de fase repetida: {clave}"));
        }
        claves.push(clave);
        if f["numero"].as_u64() != Some(i as u64 + 1) {
            return Err(format!("la fase {clave} no tiene el número {}", i + 1));
        }
        match f["bloque"].as_str() {
            Some("PREVENTA") if !en_ejecucion => {}
            Some("PREVENTA") => return Err(format!("{clave}: preventa después de ejecución")),
            Some("EJECUCION") => en_ejecucion = true,
            _ => return Err(format!("{clave}: bloque inválido")),
        }
        match (f["bloque"].as_str(), f["leadStatus"].as_str()) {
            (Some("PREVENTA"), Some(ls)) if !estados_lead.contains(&ls) => estados_lead.push(ls),
            (Some("PREVENTA"), _) => return Err(format!("{clave}: estado de lead faltante o repetido")),
            (_, Some(_)) => return Err(format!("{clave}: solo las fases de preventa se ligan a un estado de lead")),
            _ => {}
        }
        if f["puerta"]["criterios"].as_array().map(|c| c.is_empty()).unwrap_or(true) {
            return Err(format!("{clave}: la puerta no tiene criterios"));
        }
        if f["puerta"]["tipo"] == "RESULTADO" {
            resultados += 1;
        }
        if f["puerta"]["aprobador"].as_str().map(str::is_empty).unwrap_or(true) {
            return Err(format!("{clave}: la puerta no tiene aprobador"));
        }
        for r in f["recursos"].as_array().ok_or_else(|| format!("{clave}: sin recursos"))? {
            if !TIPOS_RECURSO.contains(&r["tipo"].as_str().unwrap_or_default()) {
                return Err(format!("{clave}: tipo de recurso desconocido {}", r["tipo"]));
            }
            if r["etiqueta"].as_str().map(str::is_empty).unwrap_or(true) {
                return Err(format!("{clave}: recurso sin etiqueta"));
            }
        }
        let acts = f["actividades"].as_array().ok_or_else(|| format!("{clave}: sin actividades"))?;
        let mut ac: Vec<&str> = Vec::new();
        for a in acts {
            let k = a["clave"].as_str().filter(|c| !c.is_empty()).ok_or_else(|| format!("{clave}: actividad sin clave"))?;
            if ac.contains(&k) {
                return Err(format!("{clave}: actividad repetida {k}"));
            }
            ac.push(k);
            if !matches!(a["tipo"].as_str(), Some("AGENTE") | Some("HUMANA")) {
                return Err(format!("{clave}/{k}: tipo de actividad inválido"));
            }
            for campo in ["titulo", "area", "backlogType", "prioridad"] {
                if a[campo].as_str().map(str::is_empty).unwrap_or(true) {
                    return Err(format!("{clave}/{k}: falta {campo}"));
                }
            }
        }
    }
    if resultados != 1 {
        return Err("debe haber exactamente una puerta de tipo RESULTADO".into());
    }
    let r = fases.iter().position(|f| f["puerta"]["tipo"] == "RESULTADO").unwrap_or(0);
    if fases.get(r + 1).map(|f| f["bloque"] != "EJECUCION").unwrap_or(true) {
        return Err("la puerta de resultado debe estar justo antes de la ejecución".into());
    }
    Ok(())
}

/// A dónde lleva un estado del lead.
#[derive(Debug, PartialEq)]
enum Destino {
    Fase(String),
    Ganado,
    Perdido,
    Ninguno,
}

fn destino_de_lead(def: &Value, status: &str, outcome: Option<&str>) -> Destino {
    if status == "RESULT" {
        return match outcome {
            Some("WON") => Destino::Ganado,
            Some("LOST") => Destino::Perdido,
            _ => Destino::Ninguno,
        };
    }
    lista(def)
        .iter()
        .find(|f| f["leadStatus"] == status)
        .and_then(|f| f["clave"].as_str())
        .map(|c| Destino::Fase(c.to_string()))
        .unwrap_or(Destino::Ninguno)
}

fn criterios_vacios(def: &Value) -> Value {
    let mut m = serde_json::Map::new();
    for f in lista(def) {
        let n = f["puerta"]["criterios"].as_array().map(|c| c.len()).unwrap_or(0);
        let v: Vec<Value> = (0..n).map(|_| json!({ "ok": false, "por": null, "en": null })).collect();
        m.insert(f["clave"].as_str().unwrap_or_default().to_string(), Value::Array(v));
    }
    Value::Object(m)
}

/// Criterios de una fase con su estado guardado: `[{texto, ok, por, en}]`.
pub(super) fn criterios_fase(def: &Value, guardados: &Value, idx: usize) -> Vec<Value> {
    let Some(f) = lista(def).get(idx) else { return vec![] };
    let g = guardados.get(f["clave"].as_str().unwrap_or_default()).and_then(|v| v.as_array());
    f["puerta"]["criterios"]
        .as_array()
        .map(|a| {
            a.iter()
                .enumerate()
                .map(|(j, t)| {
                    let x = g.and_then(|g| g.get(j));
                    json!({
                        "texto": t,
                        "ok": x.map(|x| x["ok"] == true).unwrap_or(false),
                        "por": x.and_then(|x| x.get("por")).cloned().unwrap_or(Value::Null),
                        "en": x.and_then(|x| x.get("en")).cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pendientes(def: &Value, guardados: &Value, idx: usize) -> Vec<String> {
    criterios_fase(def, guardados, idx)
        .iter()
        .filter(|c| c["ok"] != true)
        .map(|c| c["texto"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// La vista que consume la pantalla: una entrada por fase con su estado, criterios y actividades.
fn vista_fases(def: &Value, fase_actual: &str, estado: &str, criterios: &Value, acts: &[Value], ctx: &Value) -> Vec<Value> {
    let cur = indice(def, fase_actual).unwrap_or(0);
    lista(def)
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let clave = f["clave"].as_str().unwrap_or_default();
            let est = if estado == "COMPLETADO" || i < cur {
                "HECHA"
            } else if i == cur {
                if estado == "CERRADO_PERDIDO" { "CERRADA" } else { "ACTUAL" }
            } else {
                "PENDIENTE"
            };
            let crit = criterios_fase(def, criterios, i);
            let cumplida = crit.iter().all(|c| c["ok"] == true);
            let items: Vec<Value> = f["actividades"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|t| {
                            let k = t["clave"].as_str().unwrap_or_default();
                            let fila = acts.iter().find(|x| x["fase"] == clave && x["clave"] == k);
                            json!({
                                "clave": k, "titulo": t["titulo"], "tipo": t["tipo"], "area": t["area"],
                                "creada": fila.is_some(),
                                "backlogItemId": fila.map(|x| x["backlogItemId"].clone()).unwrap_or(Value::Null),
                                "taskCode": fila.map(|x| x["taskCode"].clone()).unwrap_or(Value::Null),
                                "status": fila.map(|x| x["status"].clone()).unwrap_or(Value::Null),
                                "assigneeName": fila.map(|x| x["assigneeName"].clone()).unwrap_or(Value::Null),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let hechas = items.iter().filter(|x| x["status"] == "DONE").count();
            json!({
                "clave": clave, "numero": f["numero"], "bloque": f["bloque"], "nombre": f["nombre"], "objetivo": f["objetivo"],
                "estado": est, "entregables": f["entregables"],
                "puerta": { "aprobador": f["puerta"]["aprobador"], "tipo": f["puerta"]["tipo"], "criterios": crit, "cumplida": cumplida },
                "actividades": { "total": items.len(), "hechas": hechas, "items": items },
                "recursos": resolver_recursos(f, ctx),
            })
        })
        .collect()
}

// ── Dónde está lo de cada fase ──────────────────────────────────────────────────────────────
fn traduccion_propuesta(s: &str) -> &str {
    match s {
        "DRAFT" => "Borrador", "SENT" => "Enviada", "UNDER_REVIEW" => "En revisión", "ACCEPTED" => "Aceptada", "REJECTED" => "Rechazada", otro => otro,
    }
}

/// Resume un documento del hub (PRD, diseño, plan) guardado como JSON en la Solución: si tiene contenido, su estado y los requisitos con tarea.
fn estado_documento(texto: Option<&str>) -> Value {
    let v: Value = texto.and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    let tiene = v.as_object().map(|o| o.iter().any(|(k, x)| {
        k != "estadoDocumento" && match x {
            Value::String(s) => !s.trim().is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(m) => !m.is_empty(),
            _ => false,
        }
    })).unwrap_or(false);
    let reqs = v["requisitos"].as_array().cloned().unwrap_or_default();
    let con = reqs.iter().filter(|r| r["backlogItemId"].as_str().map(|s| !s.is_empty()).unwrap_or(false)).count();
    json!({ "tiene": tiene, "estado": v["estadoDocumento"].as_str().unwrap_or("BORRADOR"), "requisitos": reqs.len(), "conBacklog": con })
}

fn texto_documento(d: &Value, con_requisitos: bool) -> (String, &'static str) {
    if d["tiene"] != true {
        return ("Sin contenido todavía".into(), "vacio");
    }
    let base = match d["estado"].as_str() { Some("APROBADO") => ("Aprobado", "ok"), Some("EN_REVISION") => ("En revisión", "medio"), _ => ("Borrador", "medio") };
    let reqs = d["requisitos"].as_i64().unwrap_or(0);
    if con_requisitos && reqs > 0 {
        (format!("{} · {}/{} requisitos con tarea", base.0, d["conBacklog"], reqs), base.1)
    } else {
        (base.0.to_string(), base.1)
    }
}

/// Convierte los recursos de una fase en "dónde está": etiqueta, estado actual, y a dónde ir (enlace o panel del proyecto).
fn resolver_recursos(fase: &Value, ctx: &Value) -> Vec<Value> {
    let sol = ctx["solucionId"].as_str().unwrap_or_default();
    let lead = ctx["leadId"].as_str();
    // Los proyectos iniciados antes de que existieran los recursos usan los de la plantilla vigente.
    let recursos = fase["recursos"].as_array().cloned().or_else(|| {
        lista(&BASE).iter().find(|f| f["clave"] == fase["clave"]).and_then(|f| f["recursos"].as_array().cloned())
    }).unwrap_or_default();
    let hub = |seccion: &str| format!("/solutions/pilots/{sol}?seccion={seccion}");
    recursos
        .iter()
        .map(|r| {
            let tipo = r["tipo"].as_str().unwrap_or_default();
            let (estado, nivel, href, panel): (String, &str, Option<String>, Option<&str>) = match tipo {
                "LEAD_HUB" => match lead {
                    None => ("Sin lead".into(), "vacio", None, None),
                    Some(l) => {
                        let h = &ctx["leadHub"][fase["leadStatus"].as_str().unwrap_or_default()];
                        let (chars, arch) = (h["chars"].as_i64().unwrap_or(0), h["archivos"].as_i64().unwrap_or(0));
                        let archivos = if arch > 0 { format!(" y {arch} archivo{}", if arch == 1 { "" } else { "s" }) } else { String::new() };
                        if chars > 0 { (format!("Nota con contenido{archivos}"), "ok", Some(format!("/leads/{l}/hub")), None) } else if arch > 0 { (format!("Sin nota, con {arch} archivo(s)"), "medio", Some(format!("/leads/{l}/hub")), None) } else { ("Sin notas todavía".into(), "vacio", Some(format!("/leads/{l}/hub")), None) }
                    }
                },
                "PROPUESTAS" => match ctx["propuestas"].as_array().filter(|a| !a.is_empty()) {
                    None => ("Sin propuestas".into(), "vacio", None, None),
                    Some(a) => {
                        let ultima = &a[0];
                        let st = ultima["status"].as_str().unwrap_or_default();
                        let nivel = if matches!(st, "SENT" | "ACCEPTED") { "ok" } else { "medio" };
                        (format!("{} propuesta{} · la última: {}", a.len(), if a.len() == 1 { "" } else { "s" }, traduccion_propuesta(st)), nivel, ultima["id"].as_str().map(|i| format!("/proposals/{i}")), None)
                    }
                },
                "HUB_PRD" => { let (t, n) = texto_documento(&ctx["prd"], true); (t, n, Some(hub("prd")), None) }
                "HUB_DISENO" => { let (t, n) = texto_documento(&ctx["diseno"], false); (t, n, Some(hub("diseno")), None) }
                "HUB_ARQUITECTURA" => {
                    let n = ctx["arquitecturaNodos"].as_i64().unwrap_or(0);
                    if n > 0 { (format!("{n} componente{} en el diagrama", if n == 1 { "" } else { "s" }), "ok", Some(hub("arquitectura")), None) } else { ("Sin diagrama todavía".into(), "vacio", Some(hub("arquitectura")), None) }
                }
                "HUB_PLAN" => { let (t, n) = texto_documento(&ctx["plan"], false); (t, n, Some(hub("plan-ejec")), None) }
                "HUB_CODIGO" => match ctx["repositorio"].as_str().filter(|r| !r.is_empty()) {
                    Some(r) => (format!("Repositorio: {r}"), "ok", Some(hub("codigo")), None),
                    None => ("Sin repositorio".into(), "vacio", Some(hub("codigo")), None),
                },
                "HUB_DESPLIEGUE" => {
                    let url = ctx["deployUrl"].as_str().filter(|u| !u.is_empty());
                    let stt = ctx["deployStatus"].as_str().filter(|u| !u.is_empty());
                    let href = Some(format!("/solutions/pilots/{sol}"));
                    match (url, stt) {
                        (Some(u), _) => (format!("Desplegado en {u}"), "ok", href, None),
                        (None, Some(s)) => (format!("Estado: {s}"), "medio", href, None),
                        _ => ("Sin despliegue".into(), "vacio", href, None),
                    }
                }
                "BACKLOG" => {
                    let t = &ctx["tareas"];
                    let (total, hechas) = (t["total"].as_i64().unwrap_or(0), t["hechas"].as_i64().unwrap_or(0));
                    if total == 0 { ("Sin tareas".into(), "vacio", Some("/backlog/solution".into()), None) } else { (format!("{hechas}/{total} tareas del proyecto hechas"), if hechas == total { "ok" } else { "medio" }, Some("/backlog/solution".into()), None) }
                }
                "SALA_CONTROL" => {
                    let t = &ctx["tareas"];
                    let (en_curso, fallidas) = (t["enCurso"].as_i64().unwrap_or(0), t["fallidas"].as_i64().unwrap_or(0));
                    if fallidas > 0 { (format!("{fallidas} tarea{} fallida{}", if fallidas == 1 { "" } else { "s" }, if fallidas == 1 { "" } else { "s" }), "aviso", None, Some("ejecucion")) }
                    else if en_curso > 0 { (format!("{en_curso} en curso"), "ok", None, Some("ejecucion")) }
                    else { ("Nada corriendo".into(), "vacio", None, Some("ejecucion")) }
                }
                "REUNIONES" => ("Las reuniones aún no se ligan a un lead: búscala por título".into(), "aviso", Some("/meetings".into()), None),
                "ADJUNTOS" => {
                    let n = ctx["adjuntos"].as_i64().unwrap_or(0);
                    if n > 0 { (format!("{n} adjunto{}", if n == 1 { "" } else { "s" }), "ok", None, Some("adjuntos")) } else { ("Sin adjuntos".into(), "vacio", None, Some("adjuntos")) }
                }
                _ => ("".into(), "vacio", None, None),
            };
            json!({ "tipo": tipo, "etiqueta": r["etiqueta"], "descripcion": r["descripcion"], "estado": estado, "nivel": nivel, "href": href, "panel": panel })
        })
        .collect()
}

/// Reúne lo que hace falta para decir dónde está cada cosa: notas del Lead Hub, propuestas, documentos del hub, repositorio, despliegue, tareas y adjuntos.
async fn contexto_recursos(st: &AppState, sol: &str, lead_id: Option<&str>) -> Result<Value, ApiError> {
    let s = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('repositorio', repositorio, 'deployUrl', "deployUrl", 'deployStatus', "deployStatus", 'prd', prd, 'diseno', "disenoTecnico", 'plan', "planEjecucion", 'arquitectura', arquitectura,
                  'adjuntos', (SELECT COUNT(*) FROM "ProyectoAdjunto" a WHERE a."solucionId" = $1),
                  'tareas', (SELECT jsonb_build_object('total', COUNT(*), 'hechas', COUNT(*) FILTER (WHERE b.status = 'DONE'), 'enCurso', COUNT(*) FILTER (WHERE b.status = 'IN_PROGRESS'),
                                    'fallidas', COUNT(*) FILTER (WHERE b.status = 'FAILED')) FROM "BacklogItem" b WHERE b."solucionId" = $1))
             FROM "Solucion" WHERE id = $1"#,
        &[B::T(sol.into())],
    )
    .await?
    .unwrap_or(Value::Null);
    let mut ctx = json!({
        "solucionId": sol, "leadId": lead_id, "repositorio": s["repositorio"], "deployUrl": s["deployUrl"], "deployStatus": s["deployStatus"],
        "adjuntos": s["adjuntos"], "tareas": s["tareas"],
        "arquitecturaNodos": s["arquitectura"].as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()).and_then(|a| a["nodes"].as_array().map(|n| n.len())).unwrap_or(0),
        "prd": estado_documento(s["prd"].as_str()), "diseno": estado_documento(s["diseno"].as_str()), "plan": estado_documento(s["plan"].as_str()),
        "leadHub": {}, "propuestas": [],
    });
    if let Some(l) = lead_id {
        let filas = fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('phase', h.phase, 'content', h.content, 'archivos', (SELECT COUNT(*) FROM "LeadHubFile" f WHERE f."hubId" = h.id))), '[]'::jsonb)
                 FROM "LeadHub" h WHERE h."leadId" = $1"#,
            &[B::T(l.into())],
        )
        .await?;
        for f in filas.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let fase = f["phase"].as_str().unwrap_or_default();
            let chars = crate::routes::aichat::texto_fase(f["content"].as_str().unwrap_or("")).trim().chars().count();
            ctx["leadHub"][fase] = json!({ "chars": chars, "archivos": f["archivos"] });
        }
        ctx["propuestas"] = fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'title', title, 'status', status::text, 'amount', amount) ORDER BY "updatedAt" DESC), '[]'::jsonb) FROM "Proposal" WHERE "leadId" = $1"#,
            &[B::T(l.into())],
        )
        .await?;
    }
    Ok(ctx)
}

// ── Quién actúa ─────────────────────────────────────────────────────────────────────────────
struct Actor {
    id: Option<String>,
    nombre: String,
    super_admin: bool,
}

fn actor(se: &Session) -> Actor {
    if se.is_service || se.id.is_empty() {
        Actor { id: None, nombre: "Sistema".into(), super_admin: false }
    } else {
        let nombre = if se.name.is_empty() { se.email.clone() } else { se.name.clone() };
        Actor { id: Some(se.id.clone()), nombre, super_admin: se.role == "SUPERADMIN" }
    }
}

fn exigir_puede(se: &Session) -> Result<Actor, ApiError> {
    if se.is_admin() || se.is_service {
        Ok(actor(se))
    } else {
        Err(ApiError::forbidden("Solo un administrador puede mover las fases del proyecto"))
    }
}

// ── Acceso a datos ──────────────────────────────────────────────────────────────────────────
async fn cargar(st: &AppState, id: &str) -> Result<Option<Value>, ApiError> {
    Ok(fetch_json_opt(&st.pool, r#"SELECT to_jsonb(p) FROM "ProyectoFase" p WHERE p."solucionId" = $1"#, &[B::T(id.to_string())]).await?)
}

async fn cargar_iniciado(st: &AppState, id: &str) -> Result<Value, ApiError> {
    cargar(st, id).await?.ok_or_else(|| ApiError::bad_request("Este proyecto todavía no tiene el motor de fases iniciado"))
}

fn exigir_en_curso(est: &Value) -> Result<(), ApiError> {
    if est["estado"] == "EN_CURSO" {
        Ok(())
    } else {
        Err(ApiError::bad_request("El proyecto ya está cerrado: no se pueden mover sus fases"))
    }
}

async fn ahora(st: &AppState) -> String {
    fetch_text_opt(&st.pool, r#"SELECT to_char(NOW() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')"#, &[])
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn historial(st: &AppState, sol: &str, fase: &str, accion: &str, a: &Actor, nota: Option<&str>) {
    let r = exec(
        &st.pool,
        r#"INSERT INTO "ProyectoFaseHistorial" (id, "solucionId", fase, accion, "usuarioId", "usuarioNombre", nota, "createdAt")
           VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())"#,
        &[
            B::T(new_id()),
            B::T(sol.into()),
            B::T(fase.into()),
            B::T(accion.into()),
            B::OT(a.id.clone()),
            B::T(a.nombre.clone()),
            B::OT(nota.map(String::from)),
        ],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("historial de fases: {e}");
    }
}

async fn notificar_admins(st: &AppState, sol: &str, titulo: &str, mensaje: &str) {
    let r = exec(
        &st.pool,
        r#"INSERT INTO "Notification" (id, "userId", type, title, message, link, read, "createdAt")
           SELECT gen_random_uuid()::text, u.id, 'info', $1, $2, $3, false, NOW() FROM "User" u WHERE u.role::text IN ('ADMIN', 'SUPERADMIN')"#,
        &[B::T(titulo.into()), B::T(mensaje.into()), B::T(format!("/oficina?view=proyectos&p={sol}"))],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("notificación de fases: {e}");
    }
}

/// Crea en el Backlog las tareas de la fase (una vez: la tabla `FaseActividad` lo garantiza).
async fn crear_actividades(st: &AppState, sol: &str, def: &Value, idx: usize) {
    let Some(f) = lista(def).get(idx) else { return };
    let fase = f["clave"].as_str().unwrap_or_default();
    let prefijo = format!("[Fase {} · {}]", f["numero"], f["nombre"].as_str().unwrap_or_default());
    for act in f["actividades"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let clave = act["clave"].as_str().unwrap_or_default();
        let item_id = new_id();
        let reservada = exec(
            &st.pool,
            r#"INSERT INTO "FaseActividad" (id, "solucionId", fase, clave, "backlogItemId", "createdAt") VALUES ($1, $2, $3, $4, $5, NOW())
               ON CONFLICT ("solucionId", fase, clave) DO NOTHING"#,
            &[B::T(new_id()), B::T(sol.into()), B::T(fase.into()), B::T(clave.into()), B::T(item_id.clone())],
        )
        .await;
        match reservada {
            Ok(0) => continue,
            Ok(_) => {}
            Err(e) => {
                tracing::error!("actividad de fase: {e}");
                continue;
            }
        }
        let area = fetch_text_opt(&st.pool, r#"SELECT id FROM "Area" WHERE name = $1"#, &[B::T(act["area"].as_str().unwrap_or_default().into())])
            .await
            .ok()
            .flatten();
        let quien = if act["tipo"] == "AGENTE" {
            "Actividad para un agente: se ejecuta desde Oficina, en el Motor."
        } else {
            "Actividad humana: la realiza una persona del equipo."
        };
        let descripcion = format!("{}\n\n{quien}", act["descripcion"].as_str().unwrap_or_default());
        let r = exec(
            &st.pool,
            r#"INSERT INTO "BacklogItem" (id, title, description, type, priority, status, "solucionId", "areaId", "createdAt", "updatedAt")
               VALUES ($1, $2, $3, $4, $5, 'BACKLOG', $6, $7, NOW(), NOW())"#,
            &[
                B::T(item_id),
                B::T(format!("{prefijo} {}", act["titulo"].as_str().unwrap_or_default())),
                B::T(descripcion),
                B::T(act["backlogType"].as_str().unwrap_or("TASK").into()),
                B::T(act["prioridad"].as_str().unwrap_or("MEDIUM").into()),
                B::T(sol.into()),
                B::OT(area),
            ],
        )
        .await;
        if let Err(e) = r {
            tracing::error!("tarea de fase: {e}");
            let _ = exec(&st.pool, r#"DELETE FROM "FaseActividad" WHERE "solucionId" = $1 AND fase = $2 AND clave = $3"#, &[B::T(sol.into()), B::T(fase.into()), B::T(clave.into())]).await;
        }
    }
}

/// Pone el estado del lead que corresponde a una fase de preventa (nunca toca un lead con resultado).
async fn poner_estado_lead(st: &AppState, lead: &str, status: &str, a: &Actor) {
    let n = exec(
        &st.pool,
        r#"UPDATE "Lead" SET status = $2::text::"LeadStatus", "updatedAt" = NOW() WHERE id = $1 AND status::text <> $2::text AND status::text <> 'RESULT'"#,
        &[B::T(lead.into()), B::T(status.into())],
    )
    .await;
    if matches!(n, Ok(x) if x > 0) {
        if let Some(uid) = &a.id {
            log_activity(&st.pool, "STATUS_CHANGED", &format!("cambió el estado del lead a {status} (motor de fases)"), "lead", lead, Some(uid), Some(lead)).await;
        }
    }
}

async fn lead_de(st: &AppState, sol: &str) -> Result<Option<String>, ApiError> {
    Ok(fetch_text_opt(&st.pool, r#"SELECT "leadId" FROM "Solucion" WHERE id = $1"#, &[B::T(sol.into())]).await?)
}

/// Mueve el proyecto a otra fase: estado, historial, tareas y, en preventa, el estado del lead.
async fn mover(st: &AppState, sol: &str, def: &Value, destino: usize, accion: &str, a: &Actor, nota: Option<&str>) -> Result<(), ApiError> {
    let clave = clave_de(def, destino);
    exec(&st.pool, r#"UPDATE "ProyectoFase" SET "faseActual" = $2, "updatedAt" = NOW() WHERE "solucionId" = $1"#, &[B::T(sol.into()), B::T(clave.clone())]).await?;
    historial(st, sol, &clave, accion, a, nota).await;
    crear_actividades(st, sol, def, destino).await;
    if es_preventa(def, destino) {
        if let (Some(lead), Some(ls)) = (lead_de(st, sol).await?, lista(def)[destino]["leadStatus"].as_str()) {
            poner_estado_lead(st, &lead, ls, a).await;
        }
    }
    Ok(())
}

// ── Conversión de la venta ──────────────────────────────────────────────────────────────────
/// La preventa se vuelve proyecto sobre la MISMA Solución. `tocar_lead`: la puerta fija el
/// resultado en el lead; si la venta se marcó desde el lead, el lead ya está como lo dejó la persona.
async fn confirmar_venta(st: &AppState, sol: &str, def: &Value, a: &Actor, nota: Option<&str>, tocar_lead: bool) -> Result<(), ApiError> {
    let lead = lead_de(st, sol).await?.ok_or_else(|| ApiError::bad_request("La solución no está ligada a un lead: no hay venta que confirmar"))?;

    // 1. El PRD de preventa queda guardado como una versión antes de que el proyecto lo reescriba.
    let prd = fetch_text_opt(&st.pool, r#"SELECT prd FROM "Solucion" WHERE id = $1"#, &[B::T(sol.into())]).await?;
    if let Some(p) = prd.filter(|p| !p.trim().is_empty()) {
        exec(
            &st.pool,
            r#"INSERT INTO "SolucionPrdVersion" (id, "solucionId", version, contenido, motivo, "usuarioId", "createdAt")
               SELECT $1, $2, COALESCE(MAX(version), 0) + 1, $3, 'PRD de preventa, guardado al confirmar la venta', $4, NOW()
                 FROM "SolucionPrdVersion" WHERE "solucionId" = $2"#,
            &[B::T(new_id()), B::T(sol.into()), B::T(p), B::OT(a.id.clone())],
        )
        .await?;
    }

    // 2. Lead ganado y Solución convertida en proyecto (mismo registro, mismo repositorio).
    let empresa = fetch_text_opt(&st.pool, r#"SELECT "companyName" FROM "Lead" WHERE id = $1"#, &[B::T(lead.clone())]).await?.unwrap_or_default();
    if tocar_lead {
        exec(
            &st.pool,
            r#"UPDATE "Lead" SET status = 'RESULT', outcome = 'WON', "lostReason" = NULL, "solucionAsociada" = 'Project', "updatedAt" = NOW() WHERE id = $1"#,
            &[B::T(lead.clone())],
        )
        .await?;
        exec(
            &st.pool,
            r#"UPDATE "Solucion" SET nombre = $2, tipo = 'PROJECT', "updatedAt" = NOW() WHERE id = $1"#,
            &[B::T(sol.into()), B::T(format!("{empresa} — Project"))],
        )
        .await?;
    }

    // 3. Los sprints que ya existían se marcan como hechos en preventa (para ver cuánto costó vender).
    exec(
        &st.pool,
        r#"UPDATE "Sprint" SET metadata = COALESCE(metadata, '{}'::jsonb) || '{"origen":"PREVENTA"}'::jsonb
            WHERE ("solucionId" = $1 OR "epicId" IN (SELECT id FROM "Epic" WHERE "solucionId" = $1))
              AND COALESCE(metadata->>'origen', '') = ''"#,
        &[B::T(sol.into())],
    )
    .await?;

    // 4. A la fase de arranque, con sus tareas.
    let destino = indice(def, FASE_ARRANQUE).ok_or_else(|| ApiError::internal("La plantilla no tiene fase de arranque"))?;
    mover(st, sol, def, destino, "VENTA_CONFIRMADA", a, nota).await?;
    if let Some(uid) = &a.id {
        log_activity(&st.pool, "STATUS_CHANGED", &format!("confirmó la venta de {empresa}: el proyecto pasa a ejecución"), "lead", &lead, Some(uid), Some(&lead)).await;
    }
    notificar_admins(st, sol, "Venta confirmada", &format!("{empresa}: la preventa pasó a proyecto y arrancó la fase de Arranque.")).await;
    Ok(())
}

async fn cerrar_perdida(st: &AppState, sol: &str, a: &Actor, motivo: &str, tocar_lead: bool) -> Result<(), ApiError> {
    let lead = lead_de(st, sol).await?;
    if let (true, Some(l)) = (tocar_lead, &lead) {
        exec(&st.pool, r#"UPDATE "Lead" SET status = 'RESULT', outcome = 'LOST', "lostReason" = $2, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(l.clone()), B::T(motivo.into())]).await?;
    }
    exec(&st.pool, r#"UPDATE "ProyectoFase" SET estado = 'CERRADO_PERDIDO', "updatedAt" = NOW() WHERE "solucionId" = $1"#, &[B::T(sol.into())]).await?;
    let fase = fetch_text_opt(&st.pool, r#"SELECT "faseActual" FROM "ProyectoFase" WHERE "solucionId" = $1"#, &[B::T(sol.into())]).await?.unwrap_or_default();
    historial(st, sol, &fase, "VENTA_PERDIDA", a, Some(motivo)).await;
    if let (Some(uid), Some(l)) = (&a.id, &lead) {
        log_activity(&st.pool, "STATUS_CHANGED", "cerró el lead como perdido", "lead", l, Some(uid), Some(l)).await;
    }
    notificar_admins(st, sol, "Oportunidad perdida", motivo).await;
    Ok(())
}

/// Lo llama `leads.rs` después de guardar un lead: si el proyecto tiene motor de fases, la fase
/// sigue al estado del lead. Nunca falla hacia afuera.
pub async fn tras_actualizar_lead(st: &AppState, lead_id: &str, a_id: &str, a_nombre: &str) {
    if let Err(e) = sincronizar_desde_lead(st, lead_id, a_id, a_nombre).await {
        tracing::error!("sincronizar fases desde el lead: {}", e.1);
    }
}

async fn sincronizar_desde_lead(st: &AppState, lead_id: &str, a_id: &str, a_nombre: &str) -> Result<(), ApiError> {
    let sol = proyecto_para_lead(st, lead_id, a_id, a_nombre).await?;
    let Some(est) = cargar(st, &sol).await? else { return Ok(()) };
    if est["estado"] != "EN_CURSO" {
        return Ok(());
    }
    let def = &est["definicion"];
    let actual = est["faseActual"].as_str().unwrap_or_default();
    let Some(cur) = indice(def, actual) else { return Ok(()) };
    if !es_preventa(def, cur) {
        return Ok(());
    }
    let Some(lead) = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('status', status::text, 'outcome', outcome) FROM "Lead" WHERE id = $1"#, &[B::T(lead_id.into())]).await? else { return Ok(()) };
    let a = Actor { id: (!a_id.is_empty()).then(|| a_id.to_string()), nombre: if a_nombre.is_empty() { "Sistema".into() } else { a_nombre.into() }, super_admin: false };
    match destino_de_lead(def, lead["status"].as_str().unwrap_or_default(), lead["outcome"].as_str()) {
        Destino::Fase(clave) if clave != actual => {
            if let Some(i) = indice(def, &clave) {
                mover(st, &sol, def, i, "SINCRONIZADO", &a, Some("Cambio hecho desde el lead")).await?;
            }
        }
        Destino::Ganado => confirmar_venta(st, &sol, def, &a, Some("Marcado como ganado desde el lead"), false).await?,
        Destino::Perdido => {
            let motivo = fetch_text_opt(&st.pool, r#"SELECT "lostReason" FROM "Lead" WHERE id = $1"#, &[B::T(lead_id.into())]).await?.unwrap_or_default();
            cerrar_perdida(st, &sol, &a, &motivo, false).await?;
        }
        _ => {}
    }
    Ok(())
}

// ── Rutas ───────────────────────────────────────────────────────────────────────────────────
async fn obtener(State(st): State<AppState>, se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sol = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', id, 'nombre', nombre, 'tipo', tipo, 'leadId', "leadId") FROM "Solucion" WHERE id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Proyecto no encontrado"))?;
    let lead = match sol["leadId"].as_str() {
        Some(l) => {
            fetch_json_opt(
                &st.pool,
                r#"SELECT jsonb_build_object('id', id, 'companyName', "companyName", 'status', status::text, 'outcome', outcome, 'lostReason', "lostReason") FROM "Lead" WHERE id = $1"#,
                &[B::T(l.into())],
            )
            .await?
        }
        None => None,
    }
    .unwrap_or(Value::Null);
    let puede = se.is_admin() || se.is_service;
    let ctx = contexto_recursos(&st, &id, sol["leadId"].as_str()).await?;

    let Some(est) = cargar(&st, &id).await? else {
        let sugerida = match lead["status"].as_str() {
            Some(ls) => match destino_de_lead(&BASE, ls, lead["outcome"].as_str()) {
                Destino::Fase(c) => c,
                Destino::Ganado | Destino::Ninguno | Destino::Perdido => FASE_ARRANQUE.to_string(),
            },
            None => FASE_ARRANQUE.to_string(),
        };
        // Vista previa: las 12 fases tal como serían, todas pendientes, para poder verlas antes de iniciar.
        let mut previa = vista_fases(&BASE, &sugerida, "EN_CURSO", &criterios_vacios(&BASE), &[], &ctx);
        for f in previa.iter_mut() {
            f["estado"] = json!("PENDIENTE");
        }
        return Ok(Json(json!({
            "iniciado": false, "solucion": sol, "lead": lead, "puedeAprobar": puede, "faseSugerida": sugerida, "fases": previa,
            "plantilla": { "codigo": BASE["codigo"], "nombre": BASE["nombre"], "descripcion": BASE["descripcion"], "fases": lista(&BASE).len() },
        })));
    };

    let def = &est["definicion"];
    let acts = match fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('fase', fa.fase, 'clave', fa.clave, 'backlogItemId', fa."backlogItemId", 'taskCode', bi."taskCode",
                  'status', bi.status, 'assigneeName', bi."assigneeName")), '[]'::jsonb)
             FROM "FaseActividad" fa LEFT JOIN "BacklogItem" bi ON bi.id = fa."backlogItemId" WHERE fa."solucionId" = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    {
        Value::Array(a) => a,
        _ => vec![],
    };
    let hist = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(h) ORDER BY h."createdAt" DESC), '[]'::jsonb)
             FROM (SELECT fase, accion, "usuarioNombre", nota, "createdAt" FROM "ProyectoFaseHistorial" WHERE "solucionId" = $1 ORDER BY "createdAt" DESC LIMIT 40) h"#,
        &[B::T(id.clone())],
    )
    .await?;
    let actual = est["faseActual"].as_str().unwrap_or_default();
    let estado = est["estado"].as_str().unwrap_or("EN_CURSO");
    Ok(Json(json!({
        "iniciado": true, "solucion": sol, "lead": lead, "puedeAprobar": puede, "esSuperadmin": se.role == "SUPERADMIN",
        "plantilla": { "codigo": def["codigo"], "nombre": def["nombre"], "version": def["version"] },
        "estado": estado, "faseActual": actual,
        "fases": vista_fases(def, actual, estado, &est["criterios"], &acts, &ctx),
        "historial": hist,
        "ventaConfirmada": indice(def, actual).map(|i| !es_preventa(def, i)).unwrap_or(false),
    })))
}

/// Inicia el motor de una Solución en la fase que corresponde al estado de su lead. Devuelve la fase.
async fn iniciar_para(st: &AppState, id: &str, a: &Actor, elegida: Option<&str>) -> Result<String, ApiError> {
    validar_plantilla(&BASE).map_err(|e| ApiError::internal(format!("Plantilla inválida: {e}")))?;
    let def: &Value = &BASE;
    let lead_id = lead_de(st, id).await?;
    if fetch_text_opt(&st.pool, r#"SELECT id FROM "Solucion" WHERE id = $1"#, &[B::T(id.to_string())]).await?.is_none() {
        return Err(ApiError::not_found("Proyecto no encontrado"));
    }
    // Sin lead no hay preventa: el proyecto arranca en la fase de arranque.
    let mut estado_lead: Option<(String, Option<String>)> = None;
    let (mut fase, mut estado) = match &lead_id {
        None => (FASE_ARRANQUE.to_string(), "EN_CURSO"),
        Some(l) => {
            let lead = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('status', status::text, 'outcome', outcome) FROM "Lead" WHERE id = $1"#, &[B::T(l.clone())])
                .await?
                .unwrap_or(Value::Null);
            estado_lead = Some((lead["status"].as_str().unwrap_or("NEW").to_string(), lead["outcome"].as_str().map(String::from)));
            match destino_de_lead(def, lead["status"].as_str().unwrap_or("NEW"), lead["outcome"].as_str()) {
                Destino::Fase(c) => (c, "EN_CURSO"),
                Destino::Ganado => (FASE_ARRANQUE.to_string(), "EN_CURSO"),
                Destino::Perdido => ("negociacion".to_string(), "CERRADO_PERDIDO"),
                Destino::Ninguno => ("negociacion".to_string(), "EN_CURSO"),
            }
        }
    };
    // Fase elegida por una persona (por ejemplo, un proyecto que ya venía avanzado): debe ser coherente con el lead.
    if let Some(clave) = elegida.filter(|c| !c.is_empty()) {
        let i = indice(def, clave).ok_or_else(|| ApiError::bad_request("Esa fase no existe"))?;
        if estado != "EN_CURSO" {
            return Err(ApiError::bad_request("El lead está perdido: el proyecto no se puede iniciar en otra fase"));
        }
        let ganado = matches!(&estado_lead, Some((s, Some(o))) if s == "RESULT" && o == "WON");
        let cerrado = matches!(&estado_lead, Some((s, _)) if s == "RESULT");
        if es_preventa(def, i) && cerrado {
            return Err(ApiError::bad_request("El lead ya tiene resultado: no se puede volver a la preventa"));
        }
        if !es_preventa(def, i) && estado_lead.is_some() && !ganado {
            return Err(ApiError::bad_request("La venta aún no está confirmada: solo se puede iniciar en una fase de preventa"));
        }
        fase = clave.to_string();
        estado = "EN_CURSO";
    }
    let n = exec(
        &st.pool,
        r#"INSERT INTO "ProyectoFase" ("solucionId", plantilla, definicion, "faseActual", estado, criterios, "createdAt", "updatedAt")
           VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW()) ON CONFLICT ("solucionId") DO NOTHING"#,
        &[B::T(id.to_string()), B::T(def["codigo"].as_str().unwrap_or_default().into()), B::J(def.clone()), B::T(fase.clone()), B::T(estado.into()), B::J(criterios_vacios(def))],
    )
    .await?;
    if n == 0 {
        return Err(ApiError::bad_request("Este proyecto ya tiene el motor de fases iniciado"));
    }
    historial(st, id, &fase, "INICIO", a, Some(if elegida.is_some() { "Motor de fases iniciado en una fase elegida" } else { "Motor de fases iniciado" })).await;
    if let Some(i) = indice(def, &fase) {
        if estado == "EN_CURSO" {
            crear_actividades(st, id, def, i).await;
            if es_preventa(def, i) {
                if let (Some(l), Some(ls)) = (&lead_id, def["fases"][i]["leadStatus"].as_str()) {
                    poner_estado_lead(st, l, ls, a).await;
                }
            }
        }
    }
    Ok(fase)
}

async fn iniciar(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let a = exigir_puede(&se)?;
    let fase = iniciar_para(&st, &id, &a, s_no_vacio(&body, "fase").as_deref()).await?;
    Ok(Json(json!({ "ok": true, "faseActual": fase })))
}

/// Toda oportunidad tiene su Solución: si el lead no la tiene, se crea (el nombre se ajusta cuando se elige la solución asociada).
pub async fn asegurar_solucion(st: &AppState, lead_id: &str) -> Result<String, ApiError> {
    exec(
        &st.pool,
        r#"INSERT INTO "Solucion" (id, nombre, descripcion, tipo, "valorEstimado", "leadId", "updatedAt")
           SELECT $1, l."companyName" || ' — ' || COALESCE(NULLIF(l."solucionAsociada", ''), 'Por definir'), l.scope,
                  CASE l."solucionAsociada" WHEN 'Demo' THEN 'DEMO' WHEN 'Partnership' THEN 'PARTNERSHIP' WHEN 'Products' THEN 'PRODUCT' WHEN 'Intern' THEN 'INTERN' ELSE 'PROJECT' END,
                  l."estimatedValue", l.id, NOW()
             FROM "Lead" l WHERE l.id = $2
           ON CONFLICT ("leadId") DO NOTHING"#,
        &[B::T(new_id()), B::T(lead_id.to_string())],
    )
    .await?;
    fetch_text_opt(&st.pool, r#"SELECT id FROM "Solucion" WHERE "leadId" = $1"#, &[B::T(lead_id.to_string())])
        .await?
        .ok_or_else(|| ApiError::not_found("Lead no encontrado"))
}

/// Deja al lead con su Solución y su motor de fases iniciado (no hace nada si ya los tiene). Devuelve el id de la Solución.
pub async fn proyecto_para_lead(st: &AppState, lead_id: &str, a_id: &str, a_nombre: &str) -> Result<String, ApiError> {
    let sol = asegurar_solucion(st, lead_id).await?;
    if cargar(st, &sol).await?.is_none() {
        let a = Actor { id: (!a_id.is_empty()).then(|| a_id.to_string()), nombre: if a_nombre.is_empty() { "Sistema".into() } else { a_nombre.into() }, super_admin: false };
        iniciar_para(st, &sol, &a, None).await?;
    }
    Ok(sol)
}

async fn criterio(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let a = exigir_puede(&se)?;
    let est = cargar_iniciado(&st, &id).await?;
    exigir_en_curso(&est)?;
    let def = &est["definicion"];
    let actual = est["faseActual"].as_str().unwrap_or_default();
    if s(&body, "fase").as_deref() != Some(actual) {
        return Err(ApiError::bad_request("Solo se marcan los criterios de la fase actual"));
    }
    let cur = indice(def, actual).ok_or_else(|| ApiError::internal("Fase actual desconocida"))?;
    let j = body["indice"].as_u64().ok_or_else(|| ApiError::bad_request("Falta el número del criterio"))? as usize;
    let ok = body["ok"].as_bool().ok_or_else(|| ApiError::bad_request("Falta indicar si el criterio se cumple"))?;
    let total = criterios_fase(def, &est["criterios"], cur).len();
    if j >= total {
        return Err(ApiError::bad_request("Ese criterio no existe"));
    }
    // Se reconstruye la lista de la fase con su estado actual y se cambia solo el criterio pedido.
    let mut lista_c: Vec<Value> = criterios_fase(def, &est["criterios"], cur)
        .into_iter()
        .map(|c| json!({ "ok": c["ok"], "por": c["por"], "en": c["en"] }))
        .collect();
    lista_c[j] = if ok { json!({ "ok": true, "por": a.nombre, "en": ahora(&st).await }) } else { json!({ "ok": false, "por": null, "en": null }) };
    let mut todos = est["criterios"].clone();
    if !todos.is_object() {
        todos = json!({});
    }
    todos[actual] = Value::Array(lista_c);
    exec(&st.pool, r#"UPDATE "ProyectoFase" SET criterios = $2, "updatedAt" = NOW() WHERE "solucionId" = $1"#, &[B::T(id), B::J(todos)]).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn avanzar(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let a = exigir_puede(&se)?;
    let est = cargar_iniciado(&st, &id).await?;
    exigir_en_curso(&est)?;
    let def = &est["definicion"];
    let actual = est["faseActual"].as_str().unwrap_or_default();
    let idx = indice(def, actual).ok_or_else(|| ApiError::internal("Fase actual desconocida"))?;
    let forzar = body["forzar"] == true;
    if forzar && !a.super_admin {
        return Err(ApiError::forbidden("Solo un superadministrador puede saltarse los criterios de la puerta"));
    }
    let nota = s_no_vacio(&body, "nota");
    let nota_h = |n: Option<&str>| -> Option<String> {
        match (forzar, n) {
            (true, Some(n)) => Some(format!("FORZADO · {n}")),
            (true, None) => Some("FORZADO".into()),
            (false, n) => n.map(String::from),
        }
    };
    let faltan = pendientes(def, &est["criterios"], idx);
    let revisar = |faltan: &Vec<String>| -> Result<(), ApiError> {
        if forzar || faltan.is_empty() {
            Ok(())
        } else {
            Err(ApiError::bad_request("Faltan criterios por cumplir en esta puerta").con_extra(json!({ "faltan": faltan })))
        }
    };

    if lista(def)[idx]["puerta"]["tipo"] == "RESULTADO" {
        match s(&body, "resultado").as_deref() {
            Some("GANADO") => {
                revisar(&faltan)?;
                confirmar_venta(&st, &id, def, &a, nota_h(nota.as_deref()).as_deref(), true).await?;
            }
            Some("PERDIDO") => {
                let motivo = s_no_vacio(&body, "motivo").ok_or_else(|| ApiError::bad_request("El motivo de pérdida es obligatorio"))?;
                cerrar_perdida(&st, &id, &a, &motivo, true).await?;
            }
            _ => return Err(ApiError::bad_request("En esta puerta hay que indicar el resultado: GANADO o PERDIDO")),
        }
    } else {
        revisar(&faltan)?;
        if idx + 1 >= lista(def).len() {
            exec(&st.pool, r#"UPDATE "ProyectoFase" SET estado = 'COMPLETADO', "updatedAt" = NOW() WHERE "solucionId" = $1"#, &[B::T(id.clone())]).await?;
            historial(&st, &id, actual, "COMPLETADO", &a, nota_h(nota.as_deref()).as_deref()).await;
            notificar_admins(&st, &id, "Proyecto completado", "Se cerró la última fase del proyecto.").await;
        } else {
            mover(&st, &id, def, idx + 1, "AVANCE", &a, nota_h(nota.as_deref()).as_deref()).await?;
        }
    }
    let nuevo = fetch_text_opt(&st.pool, r#"SELECT "faseActual" FROM "ProyectoFase" WHERE "solucionId" = $1"#, &[B::T(id)]).await?;
    Ok(Json(json!({ "ok": true, "faseActual": nuevo })))
}

async fn retroceder(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let a = exigir_puede(&se)?;
    let est = cargar_iniciado(&st, &id).await?;
    exigir_en_curso(&est)?;
    let def = &est["definicion"];
    let actual = est["faseActual"].as_str().unwrap_or_default();
    let cur = indice(def, actual).ok_or_else(|| ApiError::internal("Fase actual desconocida"))?;
    let destino = s(&body, "fase").and_then(|c| indice(def, &c)).ok_or_else(|| ApiError::bad_request("Falta la fase a la que se vuelve"))?;
    if destino >= cur {
        return Err(ApiError::bad_request("Solo se puede volver a una fase anterior"));
    }
    if es_preventa(def, destino) && !es_preventa(def, cur) {
        return Err(ApiError::bad_request("La venta ya está confirmada: no se vuelve a la preventa"));
    }
    let motivo = s_no_vacio(&body, "motivo").ok_or_else(|| ApiError::bad_request("El motivo del retroceso es obligatorio"))?;
    // Las fases que se repiten empiezan con sus criterios sin marcar.
    let mut crit = est["criterios"].clone();
    let vacios = criterios_vacios(def);
    for i in destino..=cur {
        let k = clave_de(def, i);
        crit[&k] = vacios[&k].clone();
    }
    exec(&st.pool, r#"UPDATE "ProyectoFase" SET criterios = $2, "updatedAt" = NOW() WHERE "solucionId" = $1"#, &[B::T(id.clone()), B::J(crit)]).await?;
    mover(&st, &id, def, destino, "RETROCESO", &a, Some(&motivo)).await?;
    Ok(Json(json!({ "ok": true, "faseActual": clave_de(def, destino) })))
}

async fn prd_versiones(State(st): State<AppState>, _se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY v.version DESC), '[]'::jsonb)
                 FROM (SELECT version, motivo, "usuarioId", length(contenido) AS chars, "createdAt" FROM "SolucionPrdVersion" WHERE "solucionId" = $1) v"#,
            &[B::T(id)],
        )
        .await?,
    ))
}

async fn prd_version(State(st): State<AppState>, _se: Session, Path((id, version)): Path<(String, i32)>) -> ApiResult<Json<Value>> {
    fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(v) FROM (SELECT version, motivo, contenido, "createdAt" FROM "SolucionPrdVersion" WHERE "solucionId" = $1 AND version = $2) v"#,
        &[B::T(id), B::I(version as i64)],
    )
    .await?
    .map(Json)
    .ok_or_else(|| ApiError::not_found("Esa versión del PRD no existe"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_plantilla_base_es_coherente() {
        validar_plantilla(&BASE).expect("plantilla base");
        assert_eq!(lista(&BASE).len(), 12);
        assert_eq!(lista(&BASE).iter().filter(|f| f["bloque"] == "PREVENTA").count(), 6);
        assert_eq!(indice(&BASE, FASE_ARRANQUE), Some(6));
    }

    #[test]
    fn la_validacion_rechaza_plantillas_rotas() {
        let mut p = BASE.clone();
        p["fases"][3]["clave"] = json!("contacto");
        assert!(validar_plantilla(&p).is_err());
        let mut p = BASE.clone();
        p["fases"][0]["puerta"]["criterios"] = json!([]);
        assert!(validar_plantilla(&p).is_err());
        let mut p = BASE.clone();
        p["fases"][5]["puerta"]["tipo"] = json!("NORMAL");
        assert!(validar_plantilla(&p).is_err());
        let mut p = BASE.clone();
        p["fases"][1]["actividades"][0]["tipo"] = json!("ROBOT");
        assert!(validar_plantilla(&p).is_err());
        let mut p = BASE.clone();
        p["fases"][2]["leadStatus"] = json!("CONTACTED");
        assert!(validar_plantilla(&p).is_err());
    }

    #[test]
    fn estado_del_lead_lleva_a_su_fase() {
        let casos = [
            ("NEW", "identificacion"),
            ("CONTACTED", "contacto"),
            ("DIAGNOSIS", "diagnostico"),
            ("DEMO_VALIDATION", "demo"),
            ("PROPOSAL_SENT", "propuesta"),
            ("NEGOTIATION", "negociacion"),
        ];
        for (ls, fase) in casos {
            assert_eq!(destino_de_lead(&BASE, ls, None), Destino::Fase(fase.into()), "{ls}");
        }
        assert_eq!(destino_de_lead(&BASE, "RESULT", Some("WON")), Destino::Ganado);
        assert_eq!(destino_de_lead(&BASE, "RESULT", Some("LOST")), Destino::Perdido);
        assert_eq!(destino_de_lead(&BASE, "RESULT", None), Destino::Ninguno);
        assert_eq!(destino_de_lead(&BASE, "INVENTADO", None), Destino::Ninguno);
    }

    #[test]
    fn criterios_marcados_y_pendientes() {
        let mut g = criterios_vacios(&BASE);
        assert_eq!(pendientes(&BASE, &g, 0).len(), 3);
        g["identificacion"][1] = json!({ "ok": true, "por": "Ana", "en": "2026-10-09T10:00:00.000Z" });
        let c = criterios_fase(&BASE, &g, 0);
        assert_eq!(c[1]["ok"], true);
        assert_eq!(c[1]["por"], "Ana");
        assert_eq!(pendientes(&BASE, &g, 0).len(), 2);
        // un estado guardado vacío o desparejo no rompe: todo queda pendiente
        assert_eq!(pendientes(&BASE, &json!({}), 0).len(), 3);
    }

    #[test]
    fn cada_fase_dice_donde_esta_lo_suyo() {
        let ctx = json!({
            "solucionId": "S1", "leadId": "L1", "repositorio": "acme/app", "deployUrl": null, "deployStatus": null, "adjuntos": 2,
            "tareas": { "total": 4, "hechas": 4, "enCurso": 0, "fallidas": 0 },
            "prd": estado_documento(Some(r#"{"estadoDocumento":"APROBADO","resumenEjecutivo":"x","requisitos":[{"backlogItemId":"a"},{"backlogItemId":""}]}"#)),
            "diseno": estado_documento(Some("{}")), "plan": estado_documento(None),
            "leadHub": { "NEW": { "chars": 120, "archivos": 1 } }, "arquitecturaNodos": 3,
            "propuestas": [{ "id": "P9", "status": "SENT" }],
        });
        let f = |c: &str| lista(&BASE)[indice(&BASE, c).unwrap()].clone();
        let r = resolver_recursos(&f("identificacion"), &ctx);
        assert_eq!(r[0]["href"], "/leads/L1/hub");
        assert_eq!(r[0]["nivel"], "ok");
        assert!(r[0]["estado"].as_str().unwrap().contains("1 archivo"));
        let contacto = resolver_recursos(&f("contacto"), &ctx);
        assert_eq!(contacto[0]["nivel"], "vacio", "una etapa sin nota se dice");
        let prop = resolver_recursos(&f("propuesta"), &ctx);
        assert_eq!(prop[0]["href"], "/proposals/P9");
        assert!(prop[0]["estado"].as_str().unwrap().contains("Enviada"));
        let arr = resolver_recursos(&f("arranque"), &ctx);
        assert_eq!(arr[0]["estado"], "Aprobado · 1/2 requisitos con tarea");
        assert_eq!(arr[0]["href"], "/solutions/pilots/S1?seccion=prd");
        assert_eq!(arr[2]["panel"], "adjuntos");
        let dis = resolver_recursos(&f("diseno_plan"), &ctx);
        assert_eq!(dis[0]["estado"], "3 componentes en el diagrama");
        assert_eq!(dis[0]["href"], "/solutions/pilots/S1?seccion=arquitectura");
        assert_eq!(dis[1]["nivel"], "vacio");
        assert_eq!(dis[2]["nivel"], "vacio");
        assert_eq!(dis[3]["nivel"], "ok");
        let demo = resolver_recursos(&f("demo"), &ctx);
        assert!(demo.iter().any(|r| r["tipo"] == "HUB_ARQUITECTURA") && demo.iter().any(|r| r["tipo"] == "HUB_DISENO"));
        assert!(f("demo")["actividades"].as_array().unwrap().iter().any(|a| a["clave"] == "esbozo_arquitectura"));
        assert!(f("construccion")["actividades"].as_array().unwrap().iter().any(|a| a["clave"] == "actualizar_diseno"));
        let sin_lead = resolver_recursos(&f("identificacion"), &json!({ "solucionId": "S1", "leadId": null }));
        assert_eq!(sin_lead[0]["estado"], "Sin lead");
        assert!(estado_documento(Some("{}"))["tiene"] == false && estado_documento(Some("no es json"))["tiene"] == false);
    }

    #[test]
    fn la_validacion_exige_recursos_conocidos() {
        let mut p = BASE.clone();
        p["fases"][0]["recursos"][0]["tipo"] = json!("NADA");
        assert!(validar_plantilla(&p).is_err());
        let mut p = BASE.clone();
        p["fases"][3]["recursos"] = json!(null);
        assert!(validar_plantilla(&p).is_err());
    }

    #[test]
    fn la_vista_marca_hechas_actual_y_pendientes() {
        let g = criterios_vacios(&BASE);
        let acts = vec![json!({ "fase": "diagnostico", "clave": "acta_requisitos", "backlogItemId": "x", "taskCode": null, "status": "DONE", "assigneeName": null })];
        let v = vista_fases(&BASE, "diagnostico", "EN_CURSO", &g, &acts, &json!({}));
        assert_eq!(v.len(), 12);
        assert_eq!(v[0]["estado"], "HECHA");
        assert_eq!(v[2]["estado"], "ACTUAL");
        assert_eq!(v[3]["estado"], "PENDIENTE");
        assert_eq!(v[2]["actividades"]["total"], 3);
        assert_eq!(v[2]["actividades"]["hechas"], 1);
        assert_eq!(vista_fases(&BASE, "negociacion", "CERRADO_PERDIDO", &g, &[], &json!({}))[5]["estado"], "CERRADA");
        assert!(vista_fases(&BASE, "entrega_cierre", "COMPLETADO", &g, &[], &json!({})).iter().all(|f| f["estado"] == "HECHA"));
    }
}
