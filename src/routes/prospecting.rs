//! Tabla del Prospector — /api/prospecting/table (MASD PHUB-0001-0009).
//! Paridad con `src/app/api/prospecting/table/route.ts`. Las demás rutas de /api/prospecting
//! (search, autocomplete, convert, stats) siguen en Next: llaman a servicios externos.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, numero, s, s_no_vacio, B},
};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/prospecting/table",
        get(listar).post(guardar).patch(marcar).delete(borrar),
    )
}

fn con_usuario(s: &Session) -> ApiResult<()> {
    if s.is_service || s.id.is_empty() {
        Err(ApiError::new(StatusCode::UNAUTHORIZED, "No autenticado"))
    } else {
        Ok(())
    }
}

async fn listar(
    State(st): State<AppState>,
    sesion: Session,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    con_usuario(&sesion)?;
    let ciudad = q.get("city").filter(|x| !x.is_empty()).cloned();
    let categoria = q.get("category").filter(|x| !x.is_empty()).cloned();
    // `converted` presente (con cualquier valor) filtra; solo "true" es verdadero.
    let convertido = q.get("converted").map(|c| c == "true");

    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY r."createdAt" DESC), '[]'::jsonb)
           FROM "ProspectorResult" r
           WHERE ($1::text IS NULL OR position($1 in r.city) > 0)
             AND ($2::text IS NULL OR position($2 in r.category) > 0)
             AND ($3::bool IS NULL OR r."convertedToLead" = $3::bool)"#,
        &[
            B::OT(ciudad),
            B::OT(categoria),
            B::OBo(convertido),
        ],
    )
    .await?;
    Ok(Json(v))
}

async fn guardar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    con_usuario(&sesion)?;
    let nombre_usuario = if !sesion.name.is_empty() {
        sesion.name.clone()
    } else if !sesion.email.is_empty() {
        sesion.email.clone()
    } else {
        "unknown".into()
    };
    let lugares = body.get("places").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    if lugares.is_empty() {
        return Err(ApiError::bad_request("Sin datos"));
    }
    let ciudad = s(&body, "city");
    let categoria = s(&body, "category");

    let (mut guardados, mut duplicados) = (0u32, 0u32);
    for p in &lugares {
        let o_nulo = |k: &str| s(p, k).filter(|x| !x.is_empty());
        let tipos = p
            .get("types")
            .and_then(|t| t.as_array())
            .filter(|t| !t.is_empty())
            .map(|t| Value::Array(t.clone()).to_string());
        let r = exec(
            &st.pool,
            r#"INSERT INTO "ProspectorResult" (id, "placeId", name, address, phone, website, rating, "totalRatings", types,
                                              lat, lng, city, category, "savedById", "savedByName", "createdAt", "updatedAt")
               VALUES ($1, $2, $3, $4, $5, $6, $7::float8, $8, $9, $10::float8, $11::float8, $12, $13, $14, $15, NOW(), NOW())
               ON CONFLICT ("placeId") DO UPDATE
                 SET city = EXCLUDED.city, category = EXCLUDED.category, "savedById" = EXCLUDED."savedById",
                     "savedByName" = EXCLUDED."savedByName", "updatedAt" = NOW()"#,
            &[
                B::T(new_id()),
                B::OT(s(p, "placeId")),
                B::OT(s(p, "name")),
                B::OT(o_nulo("address")),
                B::OT(o_nulo("phone")),
                B::OT(o_nulo("website")),
                B::OF(p.get("rating").and_then(|v| v.as_f64())),
                B::I(numero(p.get("totalRatings")) as i64),
                B::OT(tipos),
                B::OF(p.get("lat").and_then(|v| v.as_f64())),
                B::OF(p.get("lng").and_then(|v| v.as_f64())),
                B::OT(ciudad.clone()),
                B::OT(categoria.clone()),
                B::T(sesion.id.clone()),
                B::T(nombre_usuario.clone()),
            ],
        )
        .await;
        if r.is_ok() { guardados += 1 } else { duplicados += 1 }
    }
    Ok(Json(json!({ "saved": guardados, "duplicates": duplicados })))
}

async fn marcar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    con_usuario(&sesion)?;
    let id = s_no_vacio(&body, "id").ok_or_else(|| ApiError::internal("Error al actualizar"))?;
    let convertido = body.get("convertedToLead").and_then(|c| c.as_bool());
    let Some(c) = convertido else { return Err(ApiError::internal("Error al actualizar")) };
    fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "ProspectorResult" SET "convertedToLead" = $2, "updatedAt" = NOW() WHERE id = $1 RETURNING *)
           SELECT to_jsonb(up) FROM up"#,
        &[B::T(id), B::Bo(c)],
    )
    .await?
    .map(Json)
    .ok_or_else(|| ApiError::internal("Error al actualizar"))
}

async fn borrar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    con_usuario(&sesion)?;
    let id = s_no_vacio(&body, "id").ok_or_else(|| ApiError::internal("Error al eliminar"))?;
    match exec(&st.pool, r#"DELETE FROM "ProspectorResult" WHERE id = $1"#, &[B::T(id)]).await {
        Ok(n) if n > 0 => Ok(Json(json!({ "ok": true }))),
        _ => Err(ApiError::internal("Error al eliminar")),
    }
}
