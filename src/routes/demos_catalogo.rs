//! Catálogo de demos, MVPs y casos: registro interno (solo ADMIN escribe) y vitrina pública de solo lectura.
//! La salud (`vivo`) la comprueba una tarea cada 15 minutos; la vitrina muestra solo lo marcado como público.

use axum::{
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::time::Duration;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, s, s_no_vacio, s_o_nulo, truthy, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/demos-catalogo", get(listar).post(crear))
        .route("/api/demos-catalogo/{id}", put(actualizar).delete(eliminar))
        .route("/api/demos-catalogo/{id}/verificar", post(verificar_una))
        .route("/api/publico/demos", get(publico))
}

const SERVICIOS: [&str; 4] = ["agentes", "automatizacion", "datos", "otro"];
const TIPOS: [&str; 3] = ["DEMO", "MVP", "CASO_REAL"];

struct Campos {
    nombre: String,
    slug: String,
    resumen: String,
    sector: String,
    servicio: String,
    tipo: String,
    url: Option<String>,
    icono: String,
    publico: bool,
    orden: i64,
    solucion: Option<String>,
}

fn slug_de(t: &str) -> String {
    let mut o = String::new();
    for c in t.to_lowercase().chars() {
        let c = match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            o.push(c);
        } else if !o.is_empty() && !o.ends_with('-') {
            o.push('-');
        }
    }
    o.trim_end_matches('-').chars().take(60).collect()
}

/// Solo https hacia un nombre de dominio (nada de IP, localhost ni direcciones internas).
fn url_ok(u: &str) -> bool {
    let Some(r) = u.strip_prefix("https://") else { return false };
    let host = r.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("");
    u.len() <= 300 && host.contains('.') && host.parse::<std::net::IpAddr>().is_err() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') && !host.ends_with(".local") && !host.ends_with(".internal")
}

fn campos(b: &Value) -> Result<Campos, ApiError> {
    let nombre = s_no_vacio(b, "nombre").map(|x| x.trim().to_string()).ok_or_else(|| ApiError::bad_request("Falta el nombre"))?;
    if nombre.chars().count() > 120 {
        return Err(ApiError::bad_request("El nombre es demasiado largo (máximo 120)"));
    }
    let resumen = s(b, "resumen").unwrap_or_default().trim().to_string();
    if resumen.chars().count() > 240 {
        return Err(ApiError::bad_request("La frase de valor es demasiado larga (máximo 240)"));
    }
    let sector = s(b, "sector").unwrap_or_default().trim().to_string();
    if sector.chars().count() > 60 {
        return Err(ApiError::bad_request("El sector es demasiado largo"));
    }
    let servicio = s(b, "servicio").unwrap_or_else(|| "otro".into());
    if !SERVICIOS.contains(&servicio.as_str()) {
        return Err(ApiError::bad_request("Servicio no válido"));
    }
    let tipo = s(b, "tipo").unwrap_or_else(|| "DEMO".into());
    if !TIPOS.contains(&tipo.as_str()) {
        return Err(ApiError::bad_request("Tipo no válido"));
    }
    let url = s_o_nulo(b, "url").map(|u| u.trim().to_string());
    if let Some(u) = &url {
        if !url_ok(u) {
            return Err(ApiError::bad_request("La dirección debe empezar con https:// y ser un dominio público"));
        }
    }
    let icono = s_o_nulo(b, "icono").unwrap_or_else(|| "sparkles".into());
    if icono.len() > 30 || !icono.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(ApiError::bad_request("Icono no válido"));
    }
    let slug = match s_no_vacio(b, "slug") {
        Some(x) => slug_de(&x),
        None => slug_de(&nombre),
    };
    if slug.is_empty() {
        return Err(ApiError::bad_request("No se pudo formar un identificador con ese nombre"));
    }
    Ok(Campos {
        nombre,
        slug,
        resumen,
        sector,
        servicio,
        tipo,
        url,
        icono,
        publico: truthy(b, "publico"),
        orden: b.get("orden").and_then(|v| v.as_i64()).unwrap_or(0).clamp(0, 9999),
        solucion: s_o_nulo(b, "solucionId"),
    })
}

async fn listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(d) || jsonb_build_object('solucionNombre', (SELECT nombre FROM "Solucion" so WHERE so.id = d."solucionId")) ORDER BY d.orden, d.nombre), '[]'::jsonb) FROM "DemoCatalogo" d"#,
            &[],
        )
        .await?,
    ))
}

async fn crear(State(st): State<AppState>, se: Session, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let c = campos(&b)?;
    let id = new_id();
    let n = exec(
        &st.pool,
        r#"INSERT INTO "DemoCatalogo" (id, slug, nombre, resumen, sector, servicio, tipo, url, icono, publico, orden, "solucionId") VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::int,$12) ON CONFLICT (slug) DO NOTHING"#,
        &[B::T(id.clone()), B::T(c.slug), B::T(c.nombre), B::T(c.resumen), B::T(c.sector), B::T(c.servicio), B::T(c.tipo), B::OT(c.url), B::T(c.icono), B::Bo(c.publico), B::I(c.orden), B::OT(c.solucion)],
    )
    .await
    .map_err(|_| ApiError::bad_request("No se pudo guardar: revisa que la Solución enlazada exista"))?;
    if n == 0 {
        return Err(ApiError::bad_request("Ya hay una ficha con ese identificador"));
    }
    Ok(Json(json!({ "id": id })))
}

async fn actualizar(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let c = campos(&b)?;
    let n = exec(
        &st.pool,
        r#"UPDATE "DemoCatalogo" SET slug=$2, nombre=$3, resumen=$4, sector=$5, servicio=$6, tipo=$7, url=$8, icono=$9, publico=$10, orden=$11::int, "solucionId"=$12, "updatedAt"=NOW(),
             vivo = CASE WHEN url IS DISTINCT FROM $8 THEN NULL ELSE vivo END WHERE id=$1"#,
        &[B::T(id), B::T(c.slug), B::T(c.nombre), B::T(c.resumen), B::T(c.sector), B::T(c.servicio), B::T(c.tipo), B::OT(c.url), B::T(c.icono), B::Bo(c.publico), B::I(c.orden), B::OT(c.solucion)],
    )
    .await
    .map_err(|_| ApiError::bad_request("No se pudo guardar: el identificador ya existe o la Solución enlazada no existe"))?;
    if n == 0 {
        return Err(ApiError::not_found("Ficha no encontrada"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn eliminar(State(st): State<AppState>, se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    exec(&st.pool, r#"DELETE FROM "DemoCatalogo" WHERE id=$1"#, &[B::T(id)]).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn comprobar(st: &AppState, url: &str) -> bool {
    match st.http.get(url).timeout(Duration::from_secs(10)).send().await {
        Ok(r) => r.status().as_u16() < 500,
        Err(_) => false,
    }
}

async fn verificar_una(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let f = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('url', url) FROM "DemoCatalogo" WHERE id=$1"#, &[B::T(id.clone())]).await?;
    let url = f.and_then(|v| v["url"].as_str().map(String::from)).ok_or_else(|| ApiError::bad_request("La ficha no tiene dirección que comprobar"))?;
    let vivo = comprobar(&st, &url).await;
    exec(&st.pool, r#"UPDATE "DemoCatalogo" SET vivo=$2, "verificadoAt"=NOW() WHERE id=$1"#, &[B::T(id), B::Bo(vivo)]).await?;
    Ok(Json(json!({ "vivo": vivo })))
}

/// Vitrina: solo lo marcado como público y solo los campos de presentación.
async fn publico(State(st): State<AppState>) -> ApiResult<Response> {
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('slug', slug, 'nombre', nombre, 'resumen', resumen, 'sector', sector, 'servicio', servicio, 'tipo', tipo, 'url', url, 'icono', icono, 'vivo', vivo) ORDER BY orden, nombre), '[]'::jsonb) FROM "DemoCatalogo" WHERE publico"#,
        &[],
    )
    .await?;
    Ok(([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"), (header::CACHE_CONTROL, "public, max-age=60")], Json(v)).into_response())
}

/// Comprueba cada 15 minutos que las direcciones del catálogo respondan.
pub fn iniciar(st: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(45)).await;
        loop {
            let filas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'url', url)), '[]'::jsonb) FROM "DemoCatalogo" WHERE url IS NOT NULL"#, &[]).await.unwrap_or_else(|_| json!([]));
            for f in filas.as_array().cloned().unwrap_or_default() {
                let (Some(id), Some(url)) = (f["id"].as_str(), f["url"].as_str()) else { continue };
                let vivo = comprobar(&st, url).await;
                let _ = exec(&st.pool, r#"UPDATE "DemoCatalogo" SET vivo=$2, "verificadoAt"=NOW() WHERE id=$1"#, &[B::T(id.into()), B::Bo(vivo)]).await;
            }
            tokio::time::sleep(Duration::from_secs(900)).await;
        }
    });
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn slug_y_url() {
        assert_eq!(slug_de("Asistente de cotización"), "asistente-de-cotizacion");
        assert_eq!(slug_de("  CRM  de predios! "), "crm-de-predios");
        assert!(url_ok("https://smartlex.architechia.co/"));
        assert!(!url_ok("http://smartlex.architechia.co"));
        assert!(!url_ok("https://127.0.0.1:3000"));
        assert!(!url_ok("https://localhost"));
        assert!(!url_ok("https://10.0.0.5/x"));
    }
}
