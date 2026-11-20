//! Identidad y responsabilidad de los agentes (MASD-0028-0001).
//!
//! Cada agente tiene una ficha (`AgenteFicha`): alma (misión, tono, valores, límites), alcance, superior en el organigrama,
//! herramientas permitidas y nivel de autonomía (general y por acción). Además declara con quién se comunica
//! (`AgenteComunicacion`). Lo escribe solo un administrador; cada cambio sube la versión y queda en el historial.
//! El organigrama junta las áreas (con subáreas) y los agentes con su superior.

use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    routes::solucion_hub::nombre_de,
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agentes/organigrama", get(organigrama))
        .route("/api/agentes/alta", post(alta))
        .route("/api/agentes/{slug}/ficha", get(ver_ficha).put(guardar_ficha))
        .route("/api/agentes/{slug}/comunicacion", put(guardar_comunicacion))
}

pub const NIVELES: [&str; 3] = ["ASISTIDA", "SUPERVISADA", "AUTONOMA"];
pub const CANALES: [&str; 3] = ["TELEGRAM", "CORREO", "PORTAL"];
const TIPOS_DESTINO: [&str; 5] = ["AGENTE", "PERSONA", "ROL", "AREA", "EXTERNO"];
const DIRECCIONES: [&str; 3] = ["ENTRADA", "SALIDA", "AMBAS"];
const ROLES_PERSONA: [&str; 3] = ["EQUIPO", "CLIENTE", "EXTERNO"];

fn exigir_lectura(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

fn slug_valido(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !s.starts_with('-') && !s.ends_with('-')
}

/// ¿Poner `nuevo` como superior de `slug` crearía un ciclo? `superiores` da el superior actual de cada agente.
pub(crate) fn crea_ciclo(slug: &str, nuevo: &str, superiores: &dyn Fn(&str) -> Option<String>) -> bool {
    let mut actual = nuevo.to_string();
    for _ in 0..50 {
        if actual == slug {
            return true;
        }
        match superiores(&actual) {
            Some(s) => actual = s,
            None => return false,
        }
    }
    true // cadena demasiado larga: se trata como ciclo
}

/// Valida y normaliza la lista de comunicación. Devuelve error legible si algo no cuadra.
pub(crate) fn validar_comunicacion(items: &Value) -> Result<Vec<Value>, String> {
    let lista = items.as_array().ok_or("La comunicación debe ser una lista")?;
    let mut vistos = std::collections::HashSet::new();
    let mut out = vec![];
    for it in lista {
        let tipo = it["tipoDestino"].as_str().unwrap_or("").to_uppercase();
        let destino = it["destino"].as_str().unwrap_or("").trim().to_string();
        let direccion = it["direccion"].as_str().unwrap_or("AMBAS").to_uppercase();
        if !TIPOS_DESTINO.contains(&tipo.as_str()) {
            return Err(format!("Tipo de destino no válido: «{tipo}»"));
        }
        if destino.is_empty() {
            return Err("Cada regla de comunicación necesita un destino".into());
        }
        if tipo == "ROL" && !ROLES_PERSONA.contains(&destino.to_uppercase().as_str()) {
            return Err(format!("Rol de destino no válido: «{destino}» (usa EQUIPO, CLIENTE o EXTERNO)"));
        }
        if !DIRECCIONES.contains(&direccion.as_str()) {
            return Err(format!("Dirección no válida: «{direccion}»"));
        }
        let canales: Vec<String> = it["canales"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(|s| s.to_uppercase())).collect()).unwrap_or_default();
        if let Some(mal) = canales.iter().find(|c| !CANALES.contains(&c.as_str())) {
            return Err(format!("Canal no válido: «{mal}»"));
        }
        let destino = if tipo == "ROL" { destino.to_uppercase() } else { destino };
        if !vistos.insert((tipo.clone(), destino.clone(), direccion.clone())) {
            return Err(format!("Regla repetida: {tipo} {destino} ({direccion})"));
        }
        out.push(json!({
            "tipoDestino": tipo, "destino": destino, "direccion": direccion, "canales": canales,
            "requiereAprobacion": it["requiereAprobacion"].as_bool().unwrap_or(false),
            "nota": it["nota"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        }));
    }
    Ok(out)
}

fn lista_texto(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

/// Valida la parte `alma` y devuelve la versión normalizada.
pub(crate) fn validar_alma(alma: &Value) -> Result<Value, String> {
    let mision = alma["mision"].as_str().unwrap_or("").trim().to_string();
    if mision.len() < 10 {
        return Err("El alma necesita una misión clara (al menos una frase)".into());
    }
    Ok(json!({
        "mision": mision,
        "tono": alma["tono"].as_str().unwrap_or("").trim(),
        "valores": lista_texto(&alma["valores"]),
        "limites": lista_texto(&alma["limites"]),
        "ejemplos": lista_texto(&alma["ejemplos"]),
    }))
}

fn validar_niveles_por_accion(v: &Value, herramientas: &[String]) -> Result<Value, String> {
    let Some(obj) = v.as_object() else { return Ok(json!({})) };
    for (accion, nivel) in obj {
        if !herramientas.contains(accion) && accion != "responder" {
            return Err(format!("La acción «{accion}» no está entre las herramientas del agente"));
        }
        if !nivel.as_str().map(|n| NIVELES.contains(&n)).unwrap_or(false) {
            return Err(format!("Nivel no válido para «{accion}»"));
        }
    }
    Ok(v.clone())
}

// ── Rutas ────────────────────────────────────────────────────────────────────────────────────────

/// Áreas con su padre y agentes con su superior, área y nivel: el árbol lo arma la pantalla.
async fn organigrama(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion)?;
    let areas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'nombre', name, 'slug', slug, 'padre', "parentAreaId", 'color', color, 'agentePorDefecto', "defaultAgentId") ORDER BY "sortOrder", name), '[]'::jsonb) FROM "Area""#, &[]).await?;
    let agentes = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
              'slug', a.slug, 'nombre', a.name, 'rol', a.role, 'areaId', a."areaId", 'estado', a.status, 'modelo', a."llmModel",
              'superior', f."superiorSlug", 'autonomia', f.autonomia, 'herramientas', COALESCE(to_jsonb(f.herramientas), '[]'::jsonb),
              'conFicha', f.slug IS NOT NULL, 'conFuncion', COALESCE(cardinality(f.herramientas), 0) > 0,
              'reglasComunicacion', (SELECT count(*) FROM "AgenteComunicacion" c WHERE c."agenteSlug" = a.slug)
            ) ORDER BY a.name), '[]'::jsonb)
           FROM "Agent" a LEFT JOIN "AgenteFicha" f ON f.slug = a.slug"#,
        &[],
    )
    .await?;
    Ok(Json(json!({ "areas": areas, "agentes": agentes })))
}

async fn ver_ficha(State(st): State<AppState>, sesion: Session, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion)?;
    let agente = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('slug', a.slug, 'nombre', a.name, 'rol', a.role, 'areaId', a."areaId", 'area', ar.name, 'estado', a.status, 'modelo', a."llmModel", 'personalidad', a.personality)
           FROM "Agent" a LEFT JOIN "Area" ar ON ar.id = a."areaId" WHERE a.slug = $1"#,
        &[B::T(slug.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Agente no encontrado"))?;
    let ficha = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(f) FROM "AgenteFicha" f WHERE f.slug = $1"#, &[B::T(slug.clone())]).await?;
    let comunicacion = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c."tipoDestino", c.destino), '[]'::jsonb) FROM "AgenteComunicacion" c WHERE c."agenteSlug" = $1"#, &[B::T(slug.clone())]).await?;
    let historial = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(h) ORDER BY h."creadoEn" DESC), '[]'::jsonb) FROM (SELECT * FROM "AgenteFichaHistorial" WHERE slug = $1 ORDER BY "creadoEn" DESC LIMIT 15) h"#, &[B::T(slug)]).await?;
    Ok(Json(json!({ "agente": agente, "ficha": ficha, "comunicacion": comunicacion, "historial": historial })))
}

async fn comprobar_superior(st: &AppState, slug: &str, superior: &str) -> Result<(), ApiError> {
    if superior == slug {
        return Err(ApiError::bad_request("Un agente no puede ser su propio superior"));
    }
    let existe = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Agent" WHERE slug = $1"#, &[B::T(superior.to_string())]).await?.is_some();
    if !existe {
        return Err(ApiError::bad_request(format!("El superior «{superior}» no existe")));
    }
    // Cadena de superiores en memoria para detectar el ciclo.
    let filas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_object_agg(slug, "superiorSlug"), '{}'::jsonb) FROM "AgenteFicha" WHERE "superiorSlug" IS NOT NULL"#, &[]).await?;
    let mapa = filas.as_object().cloned().unwrap_or_default();
    let buscar = |s: &str| mapa.get(s).and_then(|v| v.as_str()).map(String::from);
    if crea_ciclo(slug, superior, &buscar) {
        return Err(ApiError::bad_request("Ese superior crearía un ciclo en el organigrama"));
    }
    Ok(())
}

async fn guardar_ficha(State(st): State<AppState>, sesion: Session, Path(slug): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let existe = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Agent" WHERE slug = $1"#, &[B::T(slug.clone())]).await?.is_some();
    if !existe {
        return Err(ApiError::not_found("Agente no encontrado"));
    }
    let actual = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(f) FROM "AgenteFicha" f WHERE f.slug = $1"#, &[B::T(slug.clone())]).await?;
    let previo = actual.clone().unwrap_or_else(|| json!({}));

    let alma = if body.get("alma").is_some() { validar_alma(&body["alma"]).map_err(ApiError::bad_request)? } else { previo["alma"].clone() };
    let alcance = body["alcance"].as_str().map(|s| s.trim().to_string()).unwrap_or_else(|| previo["alcance"].as_str().unwrap_or("").to_string());
    let herramientas: Vec<String> = if body.get("herramientas").is_some() { lista_texto(&body["herramientas"]) } else { lista_texto(&previo["herramientas"]) };
    let autonomia = body["autonomia"].as_str().map(str::to_uppercase).unwrap_or_else(|| previo["autonomia"].as_str().unwrap_or("ASISTIDA").to_string());
    if !NIVELES.contains(&autonomia.as_str()) {
        return Err(ApiError::bad_request("Nivel de autonomía no válido (ASISTIDA, SUPERVISADA o AUTONOMA)"));
    }
    let por_accion = validar_niveles_por_accion(if body.get("autonomiaPorAccion").is_some() { &body["autonomiaPorAccion"] } else { &previo["autonomiaPorAccion"] }, &herramientas).map_err(ApiError::bad_request)?;
    let superior: Option<String> = if body.get("superiorSlug").is_some() { body["superiorSlug"].as_str().map(str::to_string).filter(|s| !s.is_empty()) } else { previo["superiorSlug"].as_str().map(str::to_string) };
    if let Some(s) = &superior {
        if previo["superiorSlug"].as_str() != Some(s.as_str()) {
            comprobar_superior(&st, &slug, s).await?;
        }
    }
    let version = previo["version"].as_i64().unwrap_or(0) + 1;
    let quien = nombre_de(&sesion);
    exec(
        &st.pool,
        r#"INSERT INTO "AgenteFicha" (slug, alma, alcance, "superiorSlug", herramientas, autonomia, "autonomiaPorAccion", version, "actualizadoPor", "updatedAt")
           VALUES ($1, $2, $3, $4, $5::text[], $6, $7, $8, $9, now())
           ON CONFLICT (slug) DO UPDATE SET alma = EXCLUDED.alma, alcance = EXCLUDED.alcance, "superiorSlug" = EXCLUDED."superiorSlug",
             herramientas = EXCLUDED.herramientas, autonomia = EXCLUDED.autonomia, "autonomiaPorAccion" = EXCLUDED."autonomiaPorAccion",
             version = EXCLUDED.version, "actualizadoPor" = EXCLUDED."actualizadoPor", "updatedAt" = now()"#,
        &[B::T(slug.clone()), B::J(alma.clone()), B::T(alcance.clone()), B::OT(superior.clone()), B::T(formato_array(&herramientas)), B::T(autonomia.clone()), B::J(por_accion.clone()), B::I(version), B::T(quien.clone())],
    )
    .await?;
    let cambio = json!({ "antes": previo, "despues": { "alma": alma, "alcance": alcance, "superiorSlug": superior, "herramientas": herramientas, "autonomia": autonomia, "autonomiaPorAccion": por_accion } });
    exec(&st.pool, r#"INSERT INTO "AgenteFichaHistorial" (id, slug, version, cambio, por) VALUES ($1, $2, $3, $4, $5)"#, &[B::T(new_id()), B::T(slug.clone()), B::I(version), B::J(cambio), B::T(quien)]).await?;
    Ok(Json(json!({ "ok": true, "version": version })))
}

/// Literal de arreglo de Postgres ({a,b}) con comillas: los valores vienen ya validados como texto libre.
pub(crate) fn formato_array(v: &[String]) -> String {
    format!("{{{}}}", v.iter().map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))).collect::<Vec<_>>().join(","))
}

async fn aplicar_comunicacion(st: &AppState, slug: &str, reglas: &[Value]) -> Result<(), ApiError> {
    for r in reglas {
        if r["tipoDestino"] == "AGENTE" {
            let d = r["destino"].as_str().unwrap_or("");
            let ok = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Agent" WHERE slug = $1"#, &[B::T(d.to_string())]).await?.is_some();
            if !ok {
                return Err(ApiError::bad_request(format!("El agente destino «{d}» no existe")));
            }
        }
    }
    exec(&st.pool, r#"DELETE FROM "AgenteComunicacion" WHERE "agenteSlug" = $1"#, &[B::T(slug.to_string())]).await?;
    for r in reglas {
        let canales: Vec<String> = lista_texto(&r["canales"]);
        exec(
            &st.pool,
            r#"INSERT INTO "AgenteComunicacion" ("agenteSlug", "tipoDestino", destino, direccion, canales, "requiereAprobacion", nota) VALUES ($1, $2, $3, $4, $5::text[], $6, $7)"#,
            &[B::T(slug.to_string()), B::T(r["tipoDestino"].as_str().unwrap_or("").into()), B::T(r["destino"].as_str().unwrap_or("").into()), B::T(r["direccion"].as_str().unwrap_or("AMBAS").into()), B::T(formato_array(&canales)), B::Bo(r["requiereAprobacion"].as_bool().unwrap_or(false)), B::OT(r["nota"].as_str().map(String::from))],
        )
        .await?;
    }
    Ok(())
}

async fn guardar_comunicacion(State(st): State<AppState>, sesion: Session, Path(slug): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let existe = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Agent" WHERE slug = $1"#, &[B::T(slug.clone())]).await?.is_some();
    if !existe {
        return Err(ApiError::not_found("Agente no encontrado"));
    }
    let reglas = validar_comunicacion(&body["reglas"]).map_err(ApiError::bad_request)?;
    aplicar_comunicacion(&st, &slug, &reglas).await?;
    Ok(Json(json!({ "ok": true, "reglas": reglas.len() })))
}

/// Alta guiada: no se crea un agente sin área, superior, alma, alcance, autonomía y al menos una regla de comunicación.
async fn alta(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let slug = body["slug"].as_str().unwrap_or("").trim().to_string();
    if !slug_valido(&slug) {
        return Err(ApiError::bad_request("El identificador (slug) debe tener letras minúsculas, números y guiones"));
    }
    let nombre = body["nombre"].as_str().unwrap_or("").trim().to_string();
    let rol = body["rol"].as_str().unwrap_or("").trim().to_string();
    if nombre.is_empty() || rol.is_empty() {
        return Err(ApiError::bad_request("Falta el nombre o el rol del agente"));
    }
    let area_id = body["areaId"].as_str().unwrap_or("").to_string();
    let area_nombre = crate::util::fetch_text_opt(&st.pool, r#"SELECT name FROM "Area" WHERE id = $1"#, &[B::T(area_id.clone())]).await?.ok_or_else(|| ApiError::bad_request("El área no existe"))?;
    if fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Agent" WHERE slug = $1"#, &[B::T(slug.clone())]).await?.is_some() {
        return Err(ApiError::bad_request("Ya existe un agente con ese identificador"));
    }
    let alma = validar_alma(&body["alma"]).map_err(ApiError::bad_request)?;
    let alcance = body["alcance"].as_str().unwrap_or("").trim().to_string();
    if alcance.len() < 10 {
        return Err(ApiError::bad_request("Define el alcance: de qué es responsable este agente"));
    }
    let autonomia = body["autonomia"].as_str().unwrap_or("ASISTIDA").to_uppercase();
    if !NIVELES.contains(&autonomia.as_str()) {
        return Err(ApiError::bad_request("Nivel de autonomía no válido"));
    }
    let herramientas = lista_texto(&body["herramientas"]);
    let superior = body["superiorSlug"].as_str().unwrap_or("").to_string();
    if superior.is_empty() {
        return Err(ApiError::bad_request("Todo agente nuevo necesita un superior en el organigrama"));
    }
    comprobar_superior(&st, &slug, &superior).await?;
    let reglas = validar_comunicacion(&body["comunicacion"]).map_err(ApiError::bad_request)?;
    if reglas.is_empty() {
        return Err(ApiError::bad_request("Define con quién se comunica el agente (al menos una regla)"));
    }
    for r in &reglas {
        if r["tipoDestino"] == "AGENTE" && r["destino"].as_str() == Some(slug.as_str()) {
            return Err(ApiError::bad_request("Un agente no puede comunicarse consigo mismo"));
        }
    }
    let quien = nombre_de(&sesion);
    exec(
        &st.pool,
        r#"INSERT INTO "Agent" (id, slug, name, role, area, personality, "systemPrompt", "llmModel", "areaId", status, "updatedAt")
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ACTIVE', now())"#,
        &[B::T(new_id()), B::T(slug.clone()), B::T(nombre), B::T(rol), B::T(area_nombre), B::T(alma["tono"].as_str().unwrap_or("").into()), B::OT(body["systemPrompt"].as_str().map(String::from)), B::OT(body["modelo"].as_str().map(String::from)), B::T(area_id)],
    )
    .await?;
    exec(
        &st.pool,
        r#"INSERT INTO "AgenteFicha" (slug, alma, alcance, "superiorSlug", herramientas, autonomia, "actualizadoPor") VALUES ($1, $2, $3, $4, $5::text[], $6, $7)"#,
        &[B::T(slug.clone()), B::J(alma), B::T(alcance), B::T(superior), B::T(formato_array(&herramientas)), B::T(autonomia), B::T(quien.clone())],
    )
    .await?;
    aplicar_comunicacion(&st, &slug, &reglas).await?;
    exec(&st.pool, r#"INSERT INTO "AgenteFichaHistorial" (id, slug, version, cambio, por) VALUES ($1, $2, 1, $3, $4)"#, &[B::T(new_id()), B::T(slug.clone()), B::J(json!({ "alta": true })), B::T(quien)]).await?;
    Ok(Json(json!({ "ok": true, "slug": slug })))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn detecta_ciclos_en_el_organigrama() {
        let mapa: std::collections::HashMap<&str, &str> = [("b", "a"), ("c", "b")].into_iter().collect();
        let sup = |s: &str| mapa.get(s).map(|x| x.to_string());
        assert!(crea_ciclo("a", "c", &sup)); // a -> c -> b -> a
        assert!(!crea_ciclo("c", "a", &sup)); // c ya cuelga de a
        assert!(crea_ciclo("a", "a", &sup));
    }

    #[test]
    fn valida_la_comunicacion() {
        let ok = validar_comunicacion(&json!([{ "tipoDestino": "rol", "destino": "equipo", "canales": ["telegram"] }])).unwrap();
        assert_eq!(ok[0]["destino"], "EQUIPO");
        assert_eq!(ok[0]["direccion"], "AMBAS");
        assert!(validar_comunicacion(&json!([{ "tipoDestino": "ROL", "destino": "JEFE" }])).is_err());
        assert!(validar_comunicacion(&json!([{ "tipoDestino": "AGENTE", "destino": "x", "canales": ["SMS"] }])).is_err());
        assert!(validar_comunicacion(&json!([{ "tipoDestino": "AGENTE", "destino": "x" }, { "tipoDestino": "AGENTE", "destino": "x" }])).is_err());
        assert!(validar_comunicacion(&json!("no es lista")).is_err());
    }

    #[test]
    fn exige_una_mision_en_el_alma() {
        assert!(validar_alma(&json!({ "mision": "corta" })).is_err());
        let a = validar_alma(&json!({ "mision": "Llevar la agenda del equipo sin choques", "valores": ["claridad", " "], "tono": "cercano" })).unwrap();
        assert_eq!(a["valores"], json!(["claridad"]));
    }

    #[test]
    fn el_identificador_es_estable() {
        assert!(slug_valido("booking-agent"));
        assert!(!slug_valido("Booking Agent"));
        assert!(!slug_valido("-x"));
        assert!(!slug_valido(""));
    }

    #[test]
    fn arma_literales_de_arreglo() {
        assert_eq!(formato_array(&["a".into(), "b\"c".into()]), "{\"a\",\"b\\\"c\"}");
        assert_eq!(formato_array(&[]), "{}");
    }

    #[test]
    fn los_niveles_por_accion_solo_admiten_herramientas() {
        let h = vec!["crear".to_string()];
        assert!(validar_niveles_por_accion(&json!({ "crear": "AUTONOMA" }), &h).is_ok());
        assert!(validar_niveles_por_accion(&json!({ "borrar": "AUTONOMA" }), &h).is_err());
        assert!(validar_niveles_por_accion(&json!({ "crear": "LIBRE" }), &h).is_err());
        assert!(validar_niveles_por_accion(&json!({ "responder": "ASISTIDA" }), &h).is_ok());
    }
}
