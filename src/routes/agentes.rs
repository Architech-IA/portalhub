//! Chats de agentes (`/api/agents/**/chat`, `/api/agents/*/history`): puerto de las rutas de Next.
//!
//! - `/api/agents/{slug}/chat`: respuesta de un turno con el modelo del agente (OpenCode).
//! - `/api/agents/nexus/chat`: reenvío en streaming al agente Hermes (Nexus).
//! - `/api/agents/orion/chat` y `/api/agents/sage/chat`: llaman a `/api/orion/chat` de este mismo
//!   servicio con la cookie de la persona (así comparten identidad y persistencia).
//! - `/api/agents/{nexus,sage}/history`: sesiones y mensajes de Hermes.

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Opcional,
    state::AppState,
    util::{fetch_json_opt, s, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agents/status", get(estado_agentes))
        .route("/api/agents/{slug}/chat", post(chat_agente))
        .route("/api/agents/nexus/chat", post(chat_nexus))
        .route("/api/agents/nexus/history", get(historial_nexus))
        .route("/api/agents/orion/chat", post(chat_orion))
        .route("/api/agents/sage/chat", post(chat_sage))
        .route("/api/agents/sage/history", get(historial_sage))
}

fn autenticado(o: &Opcional) -> Result<(), ApiError> {
    if o.0.is_some() {
        Ok(())
    } else {
        Err(ApiError::unauthorized_msg("No autenticado"))
    }
}

fn host_hermes() -> String {
    std::env::var("HERMES_HOST").unwrap_or_else(|_| "172.16.0.1".to_string())
}

fn clave(var: &str) -> String {
    std::env::var(var).unwrap_or_default()
}

async fn chat_agente(State(st): State<AppState>, o: Opcional, Path(slug): Path<String>, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let mensaje = body.get("message").and_then(|m| m.as_str()).unwrap_or("");
    if mensaje.trim().is_empty() {
        return Err(ApiError::bad_request("Mensaje vacío"));
    }
    let Some(agente) = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(a) FROM "Agent" a WHERE a.slug = $1"#, &[B::T(slug.clone())]).await? else {
        return Err(ApiError::not_found("Agente no encontrado"));
    };
    let g = |k: &str| agente[k].as_str().filter(|x| !x.is_empty());
    let prompt = g("systemPrompt").map(String::from).unwrap_or_else(|| {
        format!("Eres {}, agente de ArchiTechIA. Rol: {}. Área: {}.", agente["name"].as_str().unwrap_or(""), agente["role"].as_str().unwrap_or(""), g("area").unwrap_or("General"))
    });
    let modelo = g("llmModel").unwrap_or("opencode-go/qwen3.8-max").to_string();
    let (url, es_go) = if modelo.starts_with("opencode-go/") {
        (st.cfg.opencode_url.clone(), true)
    } else if modelo.starts_with("opencode/") {
        ("https://opencode.ai/zen/v1/chat/completions".to_string(), false)
    } else {
        // Next llamaba al CLI de `claude` para otros modelos; ningún agente lo usa hoy. El servicio no
        // tiene ese CLI (corre sin privilegios), así que se responde con el modelo por defecto.
        (st.cfg.opencode_url.clone(), true)
    };
    let id_modelo = if es_go && !modelo.starts_with("opencode-go/") { st.cfg.opencode_model.clone() } else { modelo.rsplit('/').next().unwrap_or(&modelo).to_string() };
    let mut mensajes = vec![json!({ "role": "system", "content": prompt })];
    let historia = body.get("history").and_then(|h| h.as_array()).cloned().unwrap_or_default();
    let desde = historia.len().saturating_sub(10);
    for m in &historia[desde..] {
        mensajes.push(json!({ "role": if m["role"] == "user" { "user" } else { "assistant" }, "content": m["content"] }));
    }
    mensajes.push(json!({ "role": "user", "content": mensaje }));
    let r = st
        .http
        .post(&url)
        .header("Authorization", format!("Bearer {}", st.cfg.opencode_api_key.clone().unwrap_or_default()))
        .header("x-opencode-session", format!("agente-{slug}"))
        .json(&json!({ "model": id_modelo, "messages": mensajes, "max_tokens": 2048 }))
        .timeout(std::time::Duration::from_secs(90))
        .send()
        .await;
    match r {
        Err(e) => {
            tracing::error!("[AgentChat] OpenCode fetch error {}", e.to_string().chars().take(200).collect::<String>());
            Err(ApiError::internal("Error de conexión con OpenCode."))
        }
        Ok(res) if !res.status().is_success() => {
            let est = res.status().as_u16();
            let t = res.text().await.unwrap_or_default();
            tracing::error!("[AgentChat] OpenCode API error {est} {}", t.chars().take(200).collect::<String>());
            Err(ApiError::internal(format!("Error API ({est})")))
        }
        Ok(res) => {
            let d: Value = res.json().await.unwrap_or_else(|_| json!({}));
            let reply = d.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("Sin respuesta");
            Ok(Json(json!({ "reply": reply })))
        }
    }
}

async fn chat_nexus(State(st): State<AppState>, o: Opcional, cuerpo: Option<Json<Value>>) -> Response {
    if let Err(e) = autenticado(&o) {
        return e.into_response();
    }
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let Some(mensaje) = body.get("message").filter(|m| !m.is_null() && **m != json!("") && **m != json!(false) && **m != json!(0)).cloned() else {
        return ApiError::bad_request("message requerido").into_response();
    };
    let sesion = s(&body, "sessionId").filter(|x| !x.is_empty());
    let modelo = s(&body, "model").filter(|x| !x.is_empty()).unwrap_or_else(|| "kimi-k2.5".to_string());
    let mut req = st
        .http
        .post(format!("http://{}:8642/v1/chat/completions", host_hermes()))
        .header("Authorization", format!("Bearer {}", clave("NEXUS_PORTAL_KEY")))
        .json(&json!({ "model": modelo, "messages": [{ "role": "user", "content": mensaje }], "stream": true }));
    if let Some(sid) = &sesion {
        req = req.header("X-Hermes-Session-Id", sid);
    }
    let up = match req.send().await {
        Ok(u) => u,
        Err(e) => return ApiError::internal(e.to_string()).into_response(),
    };
    if !up.status().is_success() {
        let est = StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let t = up.text().await.unwrap_or_default();
        return (est, Json(json!({ "error": t }))).into_response();
    }
    let sid_sale = up.headers().get("X-Hermes-Session-Id").and_then(|v| v.to_str().ok()).map(String::from).or(sesion);
    let mut resp = Response::builder().status(StatusCode::OK).header(header::CONTENT_TYPE, "text/event-stream").header(header::CACHE_CONTROL, "no-cache").header(header::CONNECTION, "keep-alive");
    if let Some(sid) = sid_sale {
        resp = resp.header("X-Hermes-Session-Id", sid);
    }
    resp.body(Body::from_stream(up.bytes_stream())).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn historial_hermes(State(st): State<AppState>, o: Opcional, Query(q): Query<HashMap<String, String>>, puerto: u16, var: &'static str) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let auth = format!("Bearer {}", clave(var));
    let base = format!("http://172.16.0.1:{puerto}");
    let lista = st.http.get(format!("{base}/api/sessions")).header("Authorization", &auth).send().await;
    let lista = match lista {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or_else(|_| json!({})),
        Ok(_) => return Ok(Json(json!({ "sessions": [] }))),
        Err(e) => return Err(ApiError::internal(e.to_string())),
    };
    if let Some(sid) = q.get("sessionId").filter(|x| !x.is_empty()) {
        let m = st.http.get(format!("{base}/api/sessions/{sid}/messages")).header("Authorization", &auth).send().await;
        return match m {
            Ok(r) if r.status().is_success() => {
                let d = r.json::<Value>().await.unwrap_or_else(|_| json!({}));
                Ok(Json(json!({ "messages": d.get("data").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!([])) })))
            }
            Ok(_) => Ok(Json(json!({ "messages": [] }))),
            Err(e) => Err(ApiError::internal(e.to_string())),
        };
    }
    Ok(Json(json!({ "sessions": lista.get("data").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!([])) })))
}

/// Llama a `/api/orion/chat` de este mismo servicio con la cookie de la persona.
async fn orion_interno(st: &AppState, cabeceras: &HeaderMap, cuerpo: Value) -> Result<reqwest::Response, ApiError> {
    let cookie = cabeceras.get(header::COOKIE).and_then(|c| c.to_str().ok()).unwrap_or("").to_string();
    let puerto = st.cfg.port;
    st.http
        .post(format!("http://127.0.0.1:{puerto}/api/orion/chat"))
        .header(header::COOKIE, cookie)
        .json(&cuerpo)
        .send()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))
}

async fn chat_orion(State(st): State<AppState>, o: Opcional, cab: HeaderMap, cuerpo: Option<Json<Value>>) -> Response {
    let Some(sesion) = &o.0 else { return ApiError::unauthorized_msg("No autenticado").into_response() };
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let mensaje = body.get("message").and_then(|m| m.as_str()).unwrap_or("").trim().to_string();
    if mensaje.is_empty() {
        return ApiError::bad_request("message requerido").into_response();
    }
    let canal = s(&body, "sessionId").filter(|x| !x.is_empty()).or_else(|| Some(sesion.id.clone()).filter(|x| !x.is_empty())).unwrap_or_else(|| "oficina-anonymous".to_string());
    let up = match orion_interno(&st, &cab, json!({ "message": mensaje, "channelType": "hub", "channelId": canal, "stream": false })).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if !up.status().is_success() {
        let est = StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return (est, Json(json!({ "error": up.text().await.unwrap_or_default() }))).into_response();
    }
    let d: Value = up.json().await.unwrap_or_else(|_| json!({}));
    Json(json!({ "reply": d.get("reply").cloned().unwrap_or(Value::Null) })).into_response()
}

async fn chat_sage(State(st): State<AppState>, o: Opcional, cab: HeaderMap, cuerpo: Option<Json<Value>>) -> Response {
    if let Err(e) = autenticado(&o) {
        return e.into_response();
    }
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let Some(mensaje) = body.get("message").filter(|m| !m.is_null() && **m != json!("")).cloned() else {
        return ApiError::bad_request("message requerido").into_response();
    };
    // Igual que Next: la identidad persistente es el primer usuario con correo (no la de la sesión).
    let primero = crate::util::fetch_text_opt(&st.pool, r#"SELECT id FROM "User" WHERE email IS NOT NULL LIMIT 1"#, &[]).await.ok().flatten();
    let canal = s(&body, "sessionId").filter(|x| !x.is_empty()).or(primero).unwrap_or_else(|| "hub-anonymous".to_string());
    let up = match orion_interno(&st, &cab, json!({ "message": mensaje, "channelType": "hub", "channelId": canal, "stream": true })).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if !up.status().is_success() {
        let est = StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return (est, Json(json!({ "error": up.text().await.unwrap_or_default() }))).into_response();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(up.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn historial_nexus(st: State<AppState>, o: Opcional, q: Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    historial_hermes(st, o, q, 8642, "NEXUS_PORTAL_KEY").await
}

async fn historial_sage(st: State<AppState>, o: Opcional, q: Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    historial_hermes(st, o, q, 8643, "SAGE_PORTAL_KEY").await
}

async fn estado_agentes(State(st): State<AppState>, o: Opcional) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let host = host_hermes();
    let consultas = [("nexus", 8642u16, "NEXUS_PORTAL_KEY"), ("sage", 8643u16, "SAGE_PORTAL_KEY")].map(|(id, puerto, var)| {
        let st = st.clone();
        let host = host.clone();
        async move {
            let ini = std::time::Instant::now();
            let r = st.http.get(format!("http://{host}:{puerto}/health/detailed")).header("Authorization", format!("Bearer {}", clave(var))).timeout(std::time::Duration::from_secs(3)).send().await;
            let ms = ini.elapsed().as_millis() as u64;
            match r {
                Ok(res) if res.status().is_success() => match res.json::<Value>().await {
                    Ok(d) => json!({ "id": id, "status": "online", "latency": ms, "detail": d }),
                    Err(_) => json!({ "id": id, "status": "offline", "latency": ms }),
                },
                Ok(_) => json!({ "id": id, "status": "degraded", "latency": ms }),
                Err(_) => json!({ "id": id, "status": "offline", "latency": ms }),
            }
        }
    });
    let [a, b] = consultas;
    let (a, b) = tokio::join!(a, b);
    Ok(Json(json!([a, b])))
}
