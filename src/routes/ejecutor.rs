//! Rutas del Motor en el servicio público: las que solo leen/escriben la base se resuelven acá; las que
//! tocan git, Docker, archivos o secretos del servidor se validan acá (sesión o clave interna) y se
//! reenvían al servicio privilegiado `portalhub-motor` (ver `motor/mod.rs`).

use std::{sync::LazyLock, time::Duration};

use axum::{
    body::{to_bytes, Body},
    extract::{Path, Query, Request, State},
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Opcional,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        // Ejecutor (workers de Python y botones del portal)
        .route("/api/executor/dispatch", post(a_motor))
        .route("/api/executor/dispatch-chain", post(a_motor))
        .route("/api/executor/complete", post(a_motor))
        .route("/api/executor/event", get(eventos_listar).post(evento_crear))
        .route("/api/executor/explain-complete", post(explicacion_completa))
        .route("/api/executor/plan-complete", post(plan_completo))
        // Diagnóstico de tareas fallidas
        .route("/api/backlog/task/{task_id}/explain", get(explicacion_estado).post(a_motor))
        .route("/api/backlog/task/{task_id}/plan", get(plan_estado).post(a_motor))
        .route("/api/backlog/task/{task_id}/apply-plan", post(a_motor))
        .route("/api/backlog/sprint/{sprint_id}/reactivate-blocked", post(a_motor))
        .route("/api/backlog/sprints/{id}/approve", post(a_motor))
        // Despliegue de proyectos
        .route("/api/proyectos/{id}/deploy", get(deploy_estado).post(a_motor))
        .route("/api/proyectos/{id}/db", get(db_estado).post(a_motor))
        .route("/api/proyectos/{id}/db/migrar", post(a_motor))
        .route("/api/proyectos/{id}/env", any(a_motor))
}

static CLIENTE: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(1800)).build().expect("cliente http hacia el motor")
});

fn salto(n: &HeaderName) -> bool {
    matches!(n.as_str(), "host" | "connection" | "content-length" | "transfer-encoding" | "keep-alive" | "upgrade" | "te" | "trailer" | "cookie" | "authorization" | "x-api-key" | "x-user-id" | "x-user-name")
}

/// Valida la sesión (o la clave interna) y reenvía la petición al motor privilegiado con la clave interna.
pub async fn a_motor(State(st): State<AppState>, o: Opcional, req: Request) -> Response {
    let Some(sesion) = o.0 else { return ApiError::unauthorized_msg("No autenticado").into_response() };
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, 40 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "cuerpo demasiado grande").into_response(),
    };
    let base = std::env::var("MOTOR_URL").unwrap_or_else(|_| "http://127.0.0.1:3101".to_string());
    let pq = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let mut rb = CLIENTE.request(parts.method.clone(), format!("{base}{pq}")).header("x-api-key", st.cfg.internal_api_key.clone().unwrap_or_default());
    for (k, v) in parts.headers.iter() {
        if !salto(k) {
            rb = rb.header(k, v);
        }
    }
    if !sesion.id.is_empty() {
        rb = rb.header("x-user-id", sesion.id.clone());
    }
    if !bytes.is_empty() {
        rb = rb.body(bytes);
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("el motor no respondió en {pq}: {e}");
            return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "El motor no respondió" }))).into_response();
        }
    };
    let estado = resp.status();
    let tipo = resp.headers().get(header::CONTENT_TYPE).cloned();
    let cuerpo = resp.bytes().await.unwrap_or_default();
    let mut out = Response::builder().status(estado);
    if let Some(t) = tipo {
        out = out.header(header::CONTENT_TYPE, t);
    }
    out.body(Body::from(cuerpo)).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

fn autenticado(o: &Opcional) -> Result<(), ApiError> {
    if o.0.is_some() {
        Ok(())
    } else {
        Err(ApiError::unauthorized_msg("No autenticado"))
    }
}

// ═══════════════════════════════ EVENTOS DE TRAZA ═══════════════════════════════
async fn evento_crear(State(st): State<AppState>, o: Opcional, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(Json(b)) = cuerpo else { return Err(ApiError::bad_request("Body inválido")) };
    let g = |k: &str| b[k].as_str().filter(|x| !x.is_empty()).map(String::from);
    let (Some(tarea), Some(tipo), Some(mensaje)) = (g("taskId"), g("kind"), g("message")) else {
        return Err(ApiError::bad_request("taskId, kind y message son requeridos"));
    };
    if !["info", "write", "check", "run", "fail"].contains(&tipo.as_str()) {
        return Err(ApiError::bad_request(format!("kind inválido: {tipo}")));
    }
    // Un fallo guardando el log no debe romper la ejecución que se está narrando.
    if let Err(e) = exec(
        &st.pool,
        r#"INSERT INTO "TaskExecutionEvent" (id, "taskId", "execId", kind, message, "createdAt") VALUES (gen_random_uuid()::text, $1, $2, $3, $4, NOW())"#,
        &[B::T(tarea), B::OT(g("execId")), B::T(tipo), B::T(mensaje)],
    )
    .await
    {
        tracing::error!("[TRACE_EVENT] No se pudo guardar el evento de traza (no bloqueante): {e}");
    }
    Ok(Json(json!({ "ok": true })))
}

async fn eventos_listar(State(st): State<AppState>, o: Opcional, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(tarea) = q.get("taskId").filter(|x| !x.is_empty()) else { return Err(ApiError::bad_request("taskId requerido")) };
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', x.id, 'kind', x.kind, 'message', x.message, 'createdAt', x."createdAt") ORDER BY x."createdAt" ASC), '[]'::jsonb)
               FROM (SELECT * FROM "TaskExecutionEvent" WHERE "taskId" = $1 ORDER BY "createdAt" ASC LIMIT 500) x"#,
            &[B::T(tarea.clone())],
        )
        .await?,
    ))
}

// ═══════════════════════════════ CALLBACKS DE LOS WORKERS ═══════════════════════════════
fn extraer_objeto(texto: &str) -> Option<Value> {
    static INI: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)^```(?:json)?\s*").expect("re"));
    static FIN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)```\s*$").expect("re"));
    let t = INI.replace(texto.trim(), "").to_string();
    let t = FIN.replace(&t, "").to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&t) {
        return Some(v);
    }
    let ini = t.find('{')?;
    let mut d = 0i32;
    for (i, c) in t[ini..].char_indices() {
        match c {
            '{' => d += 1,
            '}' => {
                d -= 1;
                if d == 0 {
                    return serde_json::from_str(&t[ini..ini + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

fn validar_cierre(b: &Value) -> Result<(String, String), ApiError> {
    let (Some(ejecucion), Some(estado)) = (b["execId"].as_str().filter(|x| !x.is_empty()), b["status"].as_str().filter(|x| !x.is_empty())) else {
        return Err(ApiError::bad_request("execId y status son requeridos"));
    };
    if estado != "DONE" && estado != "FAILED" {
        return Err(ApiError::bad_request("status debe ser DONE o FAILED"));
    }
    Ok((ejecucion.to_string(), estado.to_string()))
}

async fn explicacion_completa(State(st): State<AppState>, o: Opcional, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(Json(b)) = cuerpo else { return Err(ApiError::bad_request("Body inválido")) };
    let (ejecucion, estado) = validar_cierre(&b)?;
    let log = if b["toolLog"].is_array() { b["toolLog"].clone() } else { json!([]) };
    exec(
        &st.pool,
        r#"UPDATE "TaskExplanation" SET status = $2, resultado = $3, "toolLog" = $4::jsonb, "updatedAt" = NOW() WHERE "execId" = $1"#,
        &[B::T(ejecucion), B::T(estado), B::T(b["resultSummary"].as_str().unwrap_or("").to_string()), B::T(log.to_string())],
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn plan_completo(State(st): State<AppState>, o: Opcional, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(Json(b)) = cuerpo else { return Err(ApiError::bad_request("Body inválido")) };
    let (ejecucion, estado) = validar_cierre(&b)?;
    let resumen = b["resultSummary"].as_str().unwrap_or("").to_string();
    let parseado = if estado == "DONE" && b["resultSummary"].is_string() { extraer_objeto(&resumen) } else { None };
    // Si no se pudo parsear un plan bien formado, se guarda igual como texto (planJson en null).
    let plan = parseado.filter(|p| p.is_object() && p["pasos"].is_array());
    exec(
        &st.pool,
        r#"UPDATE "TaskRemediationPlan" SET status = $2, resultado = $3, "planJson" = $4::jsonb, "updatedAt" = NOW() WHERE "execId" = $1"#,
        &[B::T(ejecucion), B::T(estado), B::T(resumen), B::OT(plan.map(|p| p.to_string()))],
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ ESTADOS (solo lectura) ═══════════════════════════════
async fn explicacion_estado(State(st): State<AppState>, o: Opcional, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(e) = q.get("execId").filter(|x| !x.is_empty()) else { return Err(ApiError::bad_request("execId requerido")) };
    fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('status', status, 'resultado', resultado, 'toolLog', "toolLog") FROM "TaskExplanation" WHERE "execId" = $1"#, &[B::T(e.clone())])
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No encontrada"))
}

async fn plan_estado(State(st): State<AppState>, o: Opcional, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(e) = q.get("execId").filter(|x| !x.is_empty()) else { return Err(ApiError::bad_request("execId requerido")) };
    fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', id, 'status', status, 'resultado', resultado, 'planJson', "planJson", 'appliedAt', "appliedAt") FROM "TaskRemediationPlan" WHERE "execId" = $1"#,
        &[B::T(e.clone())],
    )
    .await?
    .map(|mut v| {
        crate::util::fix_dates(&mut v);
        Json(v)
    })
    .ok_or_else(|| ApiError::not_found("No encontrado"))
}

async fn deploy_estado(State(st): State<AppState>, o: Opcional, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('deployUrl', "deployUrl", 'deployStatus', "deployStatus", 'deployedAt', "deployedAt", 'deployPort', "deployPort") FROM "Solucion" WHERE id = $1"#,
        &[B::T(id)],
    )
    .await?
    .map(|mut v| {
        crate::util::fix_dates(&mut v);
        Json(v)
    })
    .ok_or_else(|| ApiError::not_found("No encontrado"))
}

async fn db_estado(State(st): State<AppState>, o: Opcional, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('dbStatus', "dbStatus", 'dbProvisionedAt', "dbProvisionedAt") FROM "Solucion" WHERE id = $1"#, &[B::T(id)])
        .await?
        .map(|mut v| {
            crate::util::fix_dates(&mut v);
            Json(v)
        })
        .ok_or_else(|| ApiError::not_found("No encontrado"))
}
