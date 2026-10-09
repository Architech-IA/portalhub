//! Flujos SSE del portal: notificaciones sin leer y comentarios de una entidad (`/api/notifications/sse`
//! y `/api/comments/sse`). Igual que en Next, cada flujo consulta la base cada pocos segundos y envía el
//! estado completo; un comentario de keep-alive mantiene viva la conexión.

use std::{collections::HashMap, convert::Infallible, time::Duration};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    error::ApiError,
    session::Opcional,
    state::AppState,
    util::{fetch_json, B},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/notifications/sse", get(notificaciones)).route("/api/comments/sse", get(comentarios))
}

const SQL_NOTIFICACIONES: &str = r#"SELECT COALESCE(jsonb_agg(to_jsonb(n) ORDER BY n."createdAt" DESC), '[]'::jsonb) FROM (SELECT * FROM "Notification" WHERE read = false ORDER BY "createdAt" DESC LIMIT 20) n"#;

const SQL_COMENTARIOS: &str = r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) || jsonb_build_object(
      'user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'avatar', u.avatar) FROM "User" u WHERE u.id = c."userId"),
      'replies', COALESCE((SELECT jsonb_agg(to_jsonb(r) || jsonb_build_object('user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'avatar', u.avatar) FROM "User" u WHERE u.id = r."userId"))
                            ORDER BY r."createdAt" ASC) FROM "Comment" r WHERE r."parentId" = c.id), '[]'::jsonb))
      ORDER BY c."createdAt" ASC), '[]'::jsonb)
   FROM "Comment" c WHERE c."entityType" = $1 AND c."entityId" = $2 AND c."parentId" IS NULL"#;

/// Flujo SSE: consulta cada `cada` y envía el resultado; `keepalive` mantiene la conexión. Termina al desconectarse el cliente.
fn flujo(st: AppState, sql: &'static str, binds: Vec<B>, cada: Duration, keepalive: Duration) -> Response {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
    tokio::spawn(async move {
        let mut datos = tokio::time::interval(cada);
        let mut vivo = tokio::time::interval(keepalive);
        vivo.tick().await;
        loop {
            tokio::select! {
                _ = datos.tick() => {
                    // Un fallo consultando no corta el flujo (igual que el `catch {}` de Next).
                    if let Ok(mut v) = fetch_json(&st.pool, sql, &binds).await {
                        crate::util::fix_dates(&mut v);
                        let linea: String = format!("data: {}\n\n", serde_json::to_string::<Value>(&v).unwrap_or_else(|_| "[]".into()));
                        if tx.send(linea.into_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
                _ = vivo.tick() => {
                    if tx.send(b": keepalive\n\n".to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let cuerpo = Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|b| (Ok::<_, Infallible>(b), rx)) }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(cuerpo)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn notificaciones(State(st): State<AppState>, o: Opcional) -> Response {
    if o.0.is_none() {
        return ApiError::unauthorized_msg("No autenticado").into_response();
    }
    flujo(st, SQL_NOTIFICACIONES, vec![], Duration::from_secs(10), Duration::from_secs(30))
}

async fn comentarios(State(st): State<AppState>, o: Opcional, Query(q): Query<HashMap<String, String>>) -> Response {
    if o.0.is_none() {
        return ApiError::unauthorized_msg("No autenticado").into_response();
    }
    let tipo = q.get("entityType").cloned().unwrap_or_default();
    let id = q.get("entityId").cloned().unwrap_or_default();
    flujo(st, SQL_COMENTARIOS, vec![B::T(tipo), B::T(id)], Duration::from_secs(4), Duration::from_secs(25))
}
