//! Respaldo hacia Next: toda ruta que Rust no implementa (todavía) se reenvía tal cual al portal
//! en Next.js (127.0.0.1:3003). Así Nginx puede mandar un área completa a Rust (p. ej. todo
//! `/api/backlog`) aunque algunas de sus sub-rutas sigan en Next (disparo de tareas, IA...).
//!
//! No sirve para respuestas en streaming (SSE): esas rutas se excluyen en Nginx y nunca llegan acá.

use std::{sync::LazyLock, time::Duration};

use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Response},
};

const NEXT: &str = "http://127.0.0.1:3003";

static CLIENTE: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(180))
        .build()
        .expect("cliente http hacia Next")
});

/// Cabeceras que no se copian entre saltos.
fn salto(n: &HeaderName) -> bool {
    matches!(
        n.as_str(),
        "host" | "connection" | "content-length" | "transfer-encoding" | "keep-alive" | "upgrade" | "te" | "trailer"
    )
}

pub async fn a_next(req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, 40 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "cuerpo demasiado grande").into_response(),
    };
    let pq = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let mut rb = CLIENTE.request(parts.method.clone(), format!("{NEXT}{pq}"));
    for (k, v) in parts.headers.iter() {
        if !salto(k) {
            rb = rb.header(k, v);
        }
    }
    if !bytes.is_empty() {
        rb = rb.body(bytes);
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("respaldo hacia Next falló en {pq}: {e}");
            return (StatusCode::BAD_GATEWAY, "El portal no respondió").into_response();
        }
    };
    let status = resp.status();
    let headers = resp.headers().clone();
    let cuerpo = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("respaldo hacia Next: error leyendo respuesta de {pq}: {e}");
            return (StatusCode::BAD_GATEWAY, "El portal no respondió").into_response();
        }
    };
    let mut out = Response::builder().status(status);
    for (k, v) in headers.iter() {
        if !salto(k) {
            out = out.header(k, v);
        }
    }
    out = out.header(header::CONTENT_LENGTH, cuerpo.len());
    out.body(Body::from(cuerpo)).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}
