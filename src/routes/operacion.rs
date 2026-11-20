//! Incidentes y solicitudes de cambio de un proyecto en operación (MASD-0026-0007).
//!
//! Un incidente no se cierra con un parche suelto: para cerrarlo debe quedar enlazado al requisito (REQ-###) que lo recoge y a la
//! versión que lo corrige. Una solicitud de cambio es un ciclo corto (spec, prueba, construcción, versión) y puede ser facturable.
//! «Proponer como requisito» deja una propuesta de cambio sobre el PRD (el caso NO edita la spec por su cuenta).

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
        .route("/api/proyectos/{id}/operacion", get(listar).post(crear))
        .route("/api/proyectos/{id}/operacion/{cid}", put(actualizar))
        .route("/api/proyectos/{id}/operacion/{cid}/proponer-requisito", post(proponer_requisito))
}

const TIPOS: [&str; 2] = ["INCIDENTE", "SOLICITUD_CAMBIO"];
const SEVERIDADES: [&str; 4] = ["CRITICA", "ALTA", "MEDIA", "BAJA"];
const ESTADOS: [&str; 5] = ["ABIERTO", "EN_ANALISIS", "EN_CORRECCION", "CERRADO", "RECHAZADO"];

/// Siguiente código del tipo (INC-001, SC-001...) a partir de los existentes.
pub(crate) fn siguiente_codigo(tipo: &str, existentes: &[String]) -> String {
    let prefijo = if tipo == "INCIDENTE" { "INC" } else { "SC" };
    let max = existentes.iter().filter_map(|c| c.strip_prefix(&format!("{prefijo}-"))).filter_map(|n| n.parse::<u32>().ok()).max().unwrap_or(0);
    format!("{prefijo}-{:03}", max + 1)
}

fn req_valido(req: &str) -> bool {
    req.len() >= 5 && req.starts_with("REQ-") && req[4..].chars().all(|c| c.is_ascii_digit())
}

/// Reglas para cerrar o rechazar un caso.
pub(crate) fn validar_cierre(tipo: &str, estado: &str, req: Option<&str>, version: Option<&str>, nota: Option<&str>) -> Result<(), &'static str> {
    let lleno = |x: Option<&str>| x.map(|s| !s.trim().is_empty()).unwrap_or(false);
    match (tipo, estado) {
        ("INCIDENTE", "CERRADO") => {
            if !req.map(req_valido).unwrap_or(false) {
                return Err("Un incidente no se cierra con un parche suelto: enlázalo al requisito (REQ-###) que lo recoge.");
            }
            if !lleno(version) {
                return Err("Indica la versión que corrige el incidente antes de cerrarlo.");
            }
            Ok(())
        }
        ("SOLICITUD_CAMBIO", "CERRADO") if !lleno(version) => Err("Indica la versión en que se entregó el cambio antes de cerrarlo."),
        (_, "RECHAZADO") if !lleno(nota) => Err("Anota el motivo del rechazo."),
        _ => Ok(()),
    }
}

fn exigir(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

async fn listar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir(&sesion)?;
    let casos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c."createdAt" DESC), '[]'::jsonb) FROM "CasoOperacion" c WHERE c."solucionId" = $1"#, &[B::T(id)]).await?;
    let lista = casos.as_array().cloned().unwrap_or_default();
    let cuenta = |tipo: &str, abiertos: bool| lista.iter().filter(|c| c["tipo"] == tipo && (c["estado"] != "CERRADO" && c["estado"] != "RECHAZADO") == abiertos).count();
    Ok(Json(json!({
        "casos": lista,
        "resumen": {
            "incidentes": { "abiertos": cuenta("INCIDENTE", true), "cerrados": cuenta("INCIDENTE", false) },
            "solicitudes": { "abiertas": cuenta("SOLICITUD_CAMBIO", true), "cerradas": cuenta("SOLICITUD_CAMBIO", false) },
        },
    })))
}

async fn crear(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let tipo = b["tipo"].as_str().unwrap_or("").to_uppercase();
    if !TIPOS.contains(&tipo.as_str()) {
        return Err(ApiError::bad_request("tipo: INCIDENTE o SOLICITUD_CAMBIO"));
    }
    let titulo = b["titulo"].as_str().map(str::trim).unwrap_or("");
    if titulo.is_empty() {
        return Err(ApiError::bad_request("El título es obligatorio"));
    }
    let severidad = b["severidad"].as_str().map(|s| s.to_uppercase()).filter(|s| SEVERIDADES.contains(&s.as_str())).unwrap_or_else(|| "MEDIA".into());
    if fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await?.is_none() {
        return Err(ApiError::not_found("No encontrado"));
    }
    let quien = nombre_de(&sesion);
    for intento in 0..3 {
        let codigos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(codigo), '[]'::jsonb) FROM "CasoOperacion" WHERE "solucionId" = $1 AND tipo = $2"#, &[B::T(id.clone()), B::T(tipo.clone())]).await?;
        let existentes: Vec<String> = codigos.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let codigo = siguiente_codigo(&tipo, &existentes);
        let cid = new_id();
        let r = exec(
            &st.pool,
            r#"INSERT INTO "CasoOperacion" (id, "solucionId", tipo, codigo, titulo, descripcion, severidad, facturable, "reportadoPor", "creadoPor") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)"#,
            &[B::T(cid.clone()), B::T(id.clone()), B::T(tipo.clone()), B::T(codigo.clone()), B::T(titulo.chars().take(200).collect()), B::T(b["descripcion"].as_str().unwrap_or("").trim().chars().take(5000).collect()), B::T(severidad.clone()), B::Bo(b["facturable"] == true), B::T(quien.clone())],
        )
        .await;
        match r {
            Ok(_) => {
                let _ = crate::routes::estado_vivo::actualizar(&st, &id, "caso de operación registrado").await;
                return Ok(Json(json!({ "ok": true, "id": cid, "codigo": codigo })));
            }
            Err(e) if intento < 2 && e.to_string().contains("unique") => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::bad_request("No se pudo asignar un código al caso. Vuelve a intentarlo."))
}

async fn actualizar(State(st): State<AppState>, sesion: Session, Path((id, cid)): Path<(String, String)>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let actual = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(c) FROM "CasoOperacion" c WHERE c.id = $1 AND c."solucionId" = $2"#, &[B::T(cid.clone()), B::T(id.clone())]).await?.ok_or_else(|| ApiError::not_found("Caso no encontrado"))?;
    let texto = |k: &str| b[k].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let estado = texto("estado").map(|s| s.to_uppercase());
    if let Some(e) = &estado {
        if !ESTADOS.contains(&e.as_str()) {
            return Err(ApiError::bad_request("estado: ABIERTO, EN_ANALISIS, EN_CORRECCION, CERRADO o RECHAZADO"));
        }
    }
    let severidad = texto("severidad").map(|s| s.to_uppercase());
    if let Some(s) = &severidad {
        if !SEVERIDADES.contains(&s.as_str()) {
            return Err(ApiError::bad_request("severidad: CRITICA, ALTA, MEDIA o BAJA"));
        }
    }
    let req = texto("reqCodigo").map(|s| s.to_uppercase());
    if let Some(r) = &req {
        if !req_valido(r) {
            return Err(ApiError::bad_request("El requisito debe tener la forma REQ-001"));
        }
    }
    let version = texto("versionCierre");
    let nota = texto("nota");
    // Reglas de cierre sobre el resultado final (lo nuevo, o lo que ya tenía).
    let final_estado = estado.clone().unwrap_or_else(|| actual["estado"].as_str().unwrap_or("").to_string());
    let final_req = req.clone().or_else(|| actual["reqCodigo"].as_str().map(String::from));
    let final_version = version.clone().or_else(|| actual["versionCierre"].as_str().map(String::from));
    let final_nota = nota.clone().or_else(|| actual["nota"].as_str().map(String::from));
    if estado.is_some() {
        validar_cierre(actual["tipo"].as_str().unwrap_or(""), &final_estado, final_req.as_deref(), final_version.as_deref(), final_nota.as_deref()).map_err(ApiError::bad_request)?;
    }
    exec(
        &st.pool,
        r#"UPDATE "CasoOperacion" SET estado = COALESCE($3, estado), "reqCodigo" = COALESCE($4, "reqCodigo"), "versionCierre" = COALESCE($5, "versionCierre"), nota = COALESCE($6, nota),
             facturable = COALESCE($7, facturable), severidad = COALESCE($8, severidad),
             "cerradoEn" = CASE WHEN COALESCE($3, estado) IN ('CERRADO', 'RECHAZADO') THEN COALESCE("cerradoEn", NOW()) ELSE NULL END, "updatedAt" = NOW()
           WHERE id = $1 AND "solucionId" = $2"#,
        &[B::T(cid), B::T(id.clone()), B::OT(estado), B::OT(req), B::OT(version), B::OT(nota), B::OBo(b["facturable"].as_bool()), B::OT(severidad)],
    )
    .await?;
    let _ = crate::routes::estado_vivo::actualizar(&st, &id, "caso de operación actualizado").await;
    Ok(Json(json!({ "ok": true })))
}

async fn proponer_requisito(State(st): State<AppState>, sesion: Session, Path((id, cid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let c = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(c) FROM "CasoOperacion" c WHERE c.id = $1 AND c."solucionId" = $2"#, &[B::T(cid.clone()), B::T(id.clone())]).await?.ok_or_else(|| ApiError::not_found("Caso no encontrado"))?;
    if !c["propuestaId"].is_null() {
        return Err(ApiError::new(axum::http::StatusCode::CONFLICT, "Este caso ya tiene una propuesta de requisito"));
    }
    let codigo = c["codigo"].as_str().unwrap_or("");
    let detalle = format!(
        "{} {codigo} ({}): {}\n\n{}\n\nRedacta el requisito con su criterio de aceptación, y deriva su prueba de aceptación. Cuando quede en el PRD, enlázalo en el caso (REQ-###).",
        if c["tipo"] == "INCIDENTE" { "Incidente" } else { "Solicitud de cambio" },
        c["severidad"].as_str().unwrap_or("MEDIA"),
        c["titulo"].as_str().unwrap_or(""),
        c["descripcion"].as_str().unwrap_or("")
    );
    let pid = new_id();
    exec(
        &st.pool,
        r#"INSERT INTO "ArtefactoPropuesta" (id, "solucionId", clave, titulo, detalle, ops, origen, "creadoPor", "createdAt") VALUES ($1, $2, 'prd', $3, $4, '[]'::jsonb, 'OPERACION', $5, NOW())"#,
        &[B::T(pid.clone()), B::T(id.clone()), B::T(format!("Nuevo requisito desde {codigo}")), B::T(detalle), B::T(nombre_de(&sesion))],
    )
    .await?;
    exec(&st.pool, r#"UPDATE "CasoOperacion" SET "propuestaId" = $2, estado = CASE WHEN estado = 'ABIERTO' THEN 'EN_ANALISIS' ELSE estado END, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(cid), B::T(pid.clone())]).await?;
    Ok(Json(json!({ "ok": true, "propuestaId": pid })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_codigo_sigue_la_secuencia_de_su_tipo() {
        assert_eq!(siguiente_codigo("INCIDENTE", &[]), "INC-001");
        assert_eq!(siguiente_codigo("INCIDENTE", &["INC-001".into(), "INC-007".into()]), "INC-008");
        assert_eq!(siguiente_codigo("SOLICITUD_CAMBIO", &["SC-002".into(), "INC-009".into()]), "SC-003");
    }

    #[test]
    fn un_incidente_no_se_cierra_sin_requisito_ni_version() {
        assert!(validar_cierre("INCIDENTE", "CERRADO", None, Some("1.1"), None).is_err());
        assert!(validar_cierre("INCIDENTE", "CERRADO", Some("REQ-014"), None, None).is_err());
        assert!(validar_cierre("INCIDENTE", "CERRADO", Some("REQ-014"), Some("1.1"), None).is_ok());
        assert!(validar_cierre("INCIDENTE", "CERRADO", Some("req 14"), Some("1.1"), None).is_err(), "el formato del requisito importa");
    }

    #[test]
    fn la_solicitud_pide_version_y_el_rechazo_pide_motivo() {
        assert!(validar_cierre("SOLICITUD_CAMBIO", "CERRADO", None, None, None).is_err());
        assert!(validar_cierre("SOLICITUD_CAMBIO", "CERRADO", None, Some("1.2"), None).is_ok());
        assert!(validar_cierre("SOLICITUD_CAMBIO", "RECHAZADO", None, None, None).is_err());
        assert!(validar_cierre("SOLICITUD_CAMBIO", "RECHAZADO", None, None, Some("Fuera del alcance")).is_ok());
        assert!(validar_cierre("INCIDENTE", "EN_ANALISIS", None, None, None).is_ok());
    }
}
