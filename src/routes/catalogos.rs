//! Clientes, prospectos y nichos de mercado — MASD PHUB-0001-0009.
//! Paridad con `src/app/api/{clientes,prospectos,niches}/**` (7 archivos de ruta de Next).
//! Son las pestañas Clientes y Mercado de /leads (y los prospectos del embudo).

use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, new_id, parse_float, presente, s, s_no_vacio, s_o_nulo, truthy, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/clientes", get(clientes_listar).post(clientes_crear))
        .route("/api/clientes/{id}", get(cliente_obtener).put(cliente_actualizar).delete(cliente_eliminar))
        .route("/api/prospectos", get(prospectos_listar).post(prospecto_crear))
        .route("/api/prospectos/{id}", put(prospecto_actualizar).delete(prospecto_eliminar))
        .route("/api/niches", get(nichos_listar).post(nicho_crear))
        .route("/api/niches/{id}", put(nicho_actualizar).delete(nicho_eliminar))
        .route("/api/niches/connections", post(conexion_crear).delete(conexion_eliminar))
}

/// `valor || default` de JavaScript para un campo numérico del cuerpo.
fn num_o(body: &Value, k: &str, def: f64) -> f64 {
    if truthy(body, k) {
        body.get(k).and_then(|v| v.as_f64()).unwrap_or(def)
    } else {
        def
    }
}

// ═══════════════════════════════ CLIENTES ═══════════════════════════════
async fn clientes_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c."createdAt" DESC), '[]'::jsonb) FROM "Cliente" c"#,
            &[],
        )
        .await?,
    ))
}

async fn clientes_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let (Some(nombre), Some(industria), Some(contacto), Some(email), Some(pais)) = (
        s_no_vacio(&body, "nombre"),
        s(&body, "industria"),
        s(&body, "contacto"),
        s(&body, "email"),
        s(&body, "pais"),
    ) else {
        return Err(ApiError::bad_request("Faltan nombre, industria, contacto, email o pais"));
    };
    let cliente = fetch_json(
        &st.pool,
        r#"WITH ins AS (
             INSERT INTO "Cliente" (id, nombre, industria, contacto, email, pais, estado, "valorTotal", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, 'Activo'), $8, NOW(), NOW()) RETURNING *)
           SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::T(nombre.clone()),
            B::T(industria),
            B::T(contacto),
            B::T(email),
            B::T(pais),
            B::OT(s(&body, "estado")),
            B::F(parse_float(body.get("valorTotal"))),
        ],
    )
    .await?;
    let cid = cliente["id"].as_str().unwrap_or_default();
    log_activity(&st.pool, "CREATED", &format!("creó el cliente {nombre}"), "cliente", cid, Some(&sesion.id), None).await;
    Ok(Json(cliente))
}

async fn cliente_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(c) || jsonb_build_object('leads', COALESCE((
             SELECT jsonb_agg(to_jsonb(l) || jsonb_build_object('user',
                      (SELECT jsonb_build_object('name', u.name) FROM "User" u WHERE u.id = l."userId"))
                    ORDER BY l."createdAt" DESC)
             FROM "Lead" l WHERE l."clienteId" = c.id), '[]'::jsonb))
           FROM "Cliente" c WHERE c.id = $1"#,
        &[B::T(id)],
    )
    .await?
    .map(Json)
    .ok_or_else(|| ApiError::not_found("No encontrado"))
}

async fn cliente_actualizar(
    State(st): State<AppState>,
    sesion: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al actualizar");
    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(id.clone())];
    for k in ["nombre", "industria", "contacto", "email", "pais", "estado"] {
        if let Some(v) = s(&body, k) {
            binds.push(B::T(v));
            sets.push(format!("{k} = ${}", binds.len()));
        }
    }
    // Se SOBREESCRIBE siempre (0 si no viene), igual que en Next.
    binds.push(B::F(parse_float(body.get("valorTotal"))));
    sets.push(format!(r#""valorTotal" = ${}"#, binds.len()));

    let sql = format!(r#"WITH up AS (UPDATE "Cliente" SET {} WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#, sets.join(", "));
    let cliente = match fetch_json_opt(&st.pool, &sql, &binds).await {
        Ok(Some(c)) => c,
        _ => return Err(fallo()),
    };
    log_activity(
        &st.pool,
        "UPDATED",
        &format!("actualizó el cliente {}", cliente["nombre"].as_str().unwrap_or("")),
        "cliente",
        &id,
        Some(&sesion.id),
        None,
    )
    .await;
    Ok(Json(cliente))
}

async fn cliente_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al eliminar");
    let nombre = fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Cliente" WHERE id = $1"#, &[B::T(id.clone())])
        .await
        .map_err(|_| fallo())?;
    match exec(&st.pool, r#"DELETE FROM "Cliente" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(n) if n > 0 => {}
        _ => return Err(fallo()),
    }
    log_activity(
        &st.pool,
        "UPDATED",
        &format!("eliminó el cliente {}", nombre.unwrap_or_default()),
        "cliente",
        &id,
        Some(&sesion.id),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ PROSPECTOS ═══════════════════════════════
const PROSPECTO_CON_USER: &str = r#"to_jsonb(p) || jsonb_build_object('user',
  (SELECT jsonb_build_object('id', u.id, 'name', u.name) FROM "User" u WHERE u.id = p."userId"))"#;

async fn prospectos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg({PROSPECTO_CON_USER} ORDER BY p."createdAt" DESC), '[]'::jsonb) FROM "Prospecto" p"#
    );
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn prospecto_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let (Some(empresa), Some(industria), Some(user_id)) =
        (s_no_vacio(&body, "empresa"), s(&body, "industria"), s_no_vacio(&body, "userId"))
    else {
        return Err(ApiError::bad_request("Faltan empresa, industria o userId"));
    };
    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "Prospecto" (id, empresa, industria, nicho, contacto, email, telefono, pais, fuente, estado,
                                      prioridad, notas, "userId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW(), NOW()) RETURNING *)
           SELECT {PROSPECTO_CON_USER} FROM ins p"#
    );
    let p = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::T(empresa.clone()),
            B::T(industria),
            B::OT(s_o_nulo(&body, "nicho")),
            B::OT(s_o_nulo(&body, "contacto")),
            B::OT(s_o_nulo(&body, "email")),
            B::OT(s_o_nulo(&body, "telefono")),
            B::OT(s_o_nulo(&body, "pais")),
            B::T(s_no_vacio(&body, "fuente").unwrap_or_else(|| "LinkedIn".into())),
            B::T(s_no_vacio(&body, "estado").unwrap_or_else(|| "Identificado".into())),
            B::T(s_no_vacio(&body, "prioridad").unwrap_or_else(|| "Media".into())),
            B::OT(s_o_nulo(&body, "notas")),
            B::T(user_id.clone()),
        ],
    )
    .await?;
    let pid = p["id"].as_str().unwrap_or_default();
    log_activity(&st.pool, "CREATED", &format!("creó el prospecto {empresa}"), "prospecto", pid, Some(&user_id), None).await;
    Ok(Json(p))
}

async fn prospecto_actualizar(
    State(st): State<AppState>,
    sesion: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al actualizar");
    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(id.clone())];
    // Campos que guardan `valor || null`
    for k in ["nicho", "contacto", "email", "telefono", "pais", "notas"] {
        if presente(&body, k) {
            binds.push(B::OT(s_o_nulo(&body, k)));
            sets.push(format!("{k} = ${}", binds.len()));
        }
    }
    // Campos que guardan el valor tal cual
    for (k, col) in [("empresa", "empresa"), ("industria", "industria"), ("fuente", "fuente"), ("estado", "estado"), ("prioridad", "prioridad"), ("userId", r#""userId""#)] {
        if presente(&body, k) {
            binds.push(B::T(s(&body, k).ok_or_else(fallo)?));
            sets.push(format!("{col} = ${}", binds.len()));
        }
    }
    let sql = format!(
        r#"WITH up AS (UPDATE "Prospecto" SET {} WHERE id = $1 RETURNING *) SELECT {PROSPECTO_CON_USER} FROM up p"#,
        sets.join(", ")
    );
    let p = match fetch_json_opt(&st.pool, &sql, &binds).await {
        Ok(Some(p)) => p,
        _ => return Err(fallo()),
    };
    let actor = s_no_vacio(&body, "userId").unwrap_or_else(|| sesion.id.clone());
    let empresa = p["empresa"].as_str().unwrap_or("");
    if presente(&body, "estado") {
        let estado = body["estado"].as_str().unwrap_or("");
        log_activity(&st.pool, "STATUS_CHANGED", &format!("cambió el estado del prospecto {empresa} a {estado}"), "prospecto", &id, Some(&actor), None).await;
    } else {
        log_activity(&st.pool, "UPDATED", &format!("actualizó el prospecto {empresa}"), "prospecto", &id, Some(&actor), None).await;
    }
    Ok(Json(p))
}

async fn prospecto_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let empresa = fetch_text_opt(&st.pool, r#"SELECT empresa FROM "Prospecto" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    match exec(&st.pool, r#"DELETE FROM "Prospecto" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(n) if n > 0 => {}
        _ => return Err(ApiError::internal("Error al eliminar")),
    }
    log_activity(&st.pool, "UPDATED", &format!("eliminó el prospecto {}", empresa.unwrap_or_default()), "prospecto", &id, Some(&sesion.id), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ NICHOS DE MERCADO ═══════════════════════════════
const NICHO_CON_USER: &str = r#"to_jsonb(n) || jsonb_build_object('user',
  (SELECT jsonb_build_object('id', u.id, 'name', u.name) FROM "User" u WHERE u.id = n."userId"))"#;

async fn nichos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"SELECT jsonb_build_object(
             'niches', COALESCE((SELECT jsonb_agg({NICHO_CON_USER} ORDER BY n."createdAt" DESC) FROM "NicheMarket" n), '[]'::jsonb),
             'connections', COALESCE((SELECT jsonb_agg(to_jsonb(c) ORDER BY c.ctid) FROM "NicheConnection" c), '[]'::jsonb))"#
    );
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn nicho_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let (Some(name), Some(industry), Some(user_id)) =
        (s_no_vacio(&body, "name"), s(&body, "industry"), s_no_vacio(&body, "userId"))
    else {
        return Err(ApiError::bad_request("Faltan name, industry o userId"));
    };
    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "NicheMarket" (id, name, color, size, x, y, description, industry, potential, competitors, trend,
                                        "userId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW(), NOW()) RETURNING *)
           SELECT {NICHO_CON_USER} FROM ins n"#
    );
    let n = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::T(name),
            B::T(s_no_vacio(&body, "color").unwrap_or_else(|| "#f97316".into())),
            B::F(num_o(&body, "size", 30.0)),
            B::F(num_o(&body, "x", 0.0)),
            B::F(num_o(&body, "y", 0.0)),
            B::OT(s_o_nulo(&body, "description")),
            B::T(industry),
            B::F(num_o(&body, "potential", 0.0)),
            B::I(num_o(&body, "competitors", 0.0) as i64),
            B::T(s_no_vacio(&body, "trend").unwrap_or_else(|| "stable".into())),
            B::T(user_id),
        ],
    )
    .await?;
    Ok(Json(n))
}

async fn nicho_actualizar(
    State(st): State<AppState>,
    _s: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al actualizar");
    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(id)];
    for (k, col) in [("name", "name"), ("color", "color"), ("industry", "industry"), ("trend", "trend"), ("userId", r#""userId""#)] {
        if presente(&body, k) {
            binds.push(B::T(s(&body, k).ok_or_else(fallo)?));
            sets.push(format!("{col} = ${}", binds.len()));
        }
    }
    if presente(&body, "description") {
        binds.push(B::OT(s_o_nulo(&body, "description")));
        sets.push(format!("description = ${}", binds.len()));
    }
    for k in ["size", "x", "y", "potential"] {
        if presente(&body, k) {
            binds.push(B::F(body[k].as_f64().ok_or_else(fallo)?));
            sets.push(format!("{k} = ${}", binds.len()));
        }
    }
    if presente(&body, "competitors") {
        binds.push(B::I(body["competitors"].as_i64().ok_or_else(fallo)?));
        sets.push(format!("competitors = ${}", binds.len()));
    }
    let sql = format!(
        r#"WITH up AS (UPDATE "NicheMarket" SET {} WHERE id = $1 RETURNING *) SELECT {NICHO_CON_USER} FROM up n"#,
        sets.join(", ")
    );
    match fetch_json_opt(&st.pool, &sql, &binds).await {
        Ok(Some(n)) => Ok(Json(n)),
        _ => Err(fallo()),
    }
}

async fn nicho_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al eliminar");
    let mut tx = st.pool.begin().await.map_err(|_| fallo())?;
    let pasos = async {
        sqlx::query(r#"DELETE FROM "NicheConnection" WHERE "fromId" = $1 OR "toId" = $1"#).bind(&id).execute(&mut *tx).await?;
        let r = sqlx::query(r#"DELETE FROM "NicheMarket" WHERE id = $1"#).bind(&id).execute(&mut *tx).await?;
        Ok::<u64, sqlx::Error>(r.rows_affected())
    }
    .await;
    match pasos {
        Ok(n) if n > 0 => tx.commit().await.map_err(|_| fallo())?,
        _ => return Err(fallo()),
    }
    Ok(Json(json!({ "ok": true })))
}

async fn conexion_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let (Some(from), Some(to)) = (s_no_vacio(&body, "fromId"), s_no_vacio(&body, "toId")) else {
        return Err(ApiError::bad_request("Faltan fromId o toId"));
    };
    let existe = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(1) FROM "NicheConnection"
           WHERE ("fromId" = $1 AND "toId" = $2) OR ("fromId" = $2 AND "toId" = $1) LIMIT 1"#,
        &[B::T(from.clone()), B::T(to.clone())],
    )
    .await?;
    if existe.is_some() {
        return Err(ApiError::new(axum::http::StatusCode::CONFLICT, "Ya existe una conexión entre estos nichos"));
    }
    let c = fetch_json(
        &st.pool,
        r#"WITH ins AS (
             INSERT INTO "NicheConnection" (id, "fromId", "toId", label, strength, "createdAt")
             VALUES ($1, $2, $3, $4, $5, NOW()) RETURNING *)
           SELECT to_jsonb(ins) FROM ins"#,
        &[B::T(new_id()), B::T(from), B::T(to), B::OT(s_o_nulo(&body, "label")), B::F(num_o(&body, "strength", 1.0))],
    )
    .await?;
    Ok(Json(c))
}

async fn conexion_eliminar(
    State(st): State<AppState>,
    _s: Session,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let id = q.get("id").filter(|x| !x.is_empty()).ok_or_else(|| ApiError::bad_request("id requerido"))?;
    match exec(&st.pool, r#"DELETE FROM "NicheConnection" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(n) if n > 0 => Ok(Json(json!({ "ok": true }))),
        _ => Err(ApiError::internal("Error al eliminar")),
    }
}
