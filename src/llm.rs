//! Cliente de OpenCode (API GO) — puerto de `src/lib/opencodeChat.ts` (`callOpenCode` /
//! `callOpenCodeMessages`), con los mismos mensajes de error porque las rutas los muestran tal
//! cual al usuario.
//!
//! Lecciones ya aprendidas en el portal y respetadas acá:
//! - La API GO exige el header `x-opencode-session` (sin él: 400 MissingSessionID).
//! - El modelo va SIN prefijo de proveedor.
//! - El proveedor aplica un filtro de contenido con falsos positivos intermitentes
//!   (`data_inspection_failed`): se reintenta una vez antes de rendirse.
//! - nginx corta a los 180 s, por eso el timeout por defecto queda por debajo (150 s).

use std::time::Duration;

use serde_json::{json, Value};

use crate::state::AppState;

const MAX_INTENTOS: u32 = 2;

fn es_filtro_de_contenido(detalle: &str) -> bool {
    let d = detalle.to_lowercase();
    d.contains("data_inspection_failed") || d.contains("inappropriate content")
}

/// Llamada de varios turnos. Devuelve el texto o el mensaje de error (listo para mostrar).
pub async fn call_open_code_messages(
    st: &AppState,
    system: &str,
    mensajes: &[Value],
    session: &str,
    max_tokens: u32,
    timeout_s: u64,
) -> Result<String, String> {
    let key = st.cfg.opencode_api_key.clone().unwrap_or_default();
    let mut todos = vec![json!({ "role": "system", "content": system })];
    todos.extend(mensajes.iter().cloned());

    for intento in 1..=MAX_INTENTOS {
        let res = st
            .http
            .post(&st.cfg.opencode_url)
            .bearer_auth(&key)
            .header("x-opencode-session", session)
            .timeout(Duration::from_secs(timeout_s))
            .json(&json!({
                "model": st.cfg.opencode_model,
                "messages": todos,
                "max_tokens": max_tokens,
            }))
            .send()
            .await
            .map_err(|e| {
                tracing::error!("[opencode] error de red: {e}");
                if e.is_timeout() {
                    "The operation was aborted due to timeout".to_string()
                } else {
                    "fetch failed".to_string()
                }
            })?;

        let status = res.status();
        if !status.is_success() {
            let detalle = res.text().await.unwrap_or_default();
            if es_filtro_de_contenido(&detalle) {
                tracing::error!("[opencode] filtro de contenido del proveedor (intento {intento}/{MAX_INTENTOS})");
                if intento < MAX_INTENTOS {
                    continue;
                }
                return Err("El filtro de contenido del proveedor de IA bloqueó la respuesta (es un falso positivo intermitente). Vuelve a intentarlo o reformula un poco el pedido.".into());
            }
            let corto: String = detalle.chars().take(200).collect();
            return Err(format!("El modelo respondió {}: {}", status.as_u16(), corto));
        }

        let data: Value = res.json().await.map_err(|_| "El modelo devolvió una respuesta vacía.".to_string())?;
        let contenido = data
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if contenido.trim().is_empty() {
            return Err("El modelo devolvió una respuesta vacía.".into());
        }
        return Ok(contenido);
    }
    Err("No se pudo obtener respuesta del modelo.".into())
}

/// Llamada de un solo turno (`callOpenCode`).
pub async fn call_open_code(
    st: &AppState,
    system: &str,
    user: &str,
    session: &str,
    max_tokens: u32,
    timeout_s: u64,
) -> Result<String, String> {
    call_open_code_messages(st, system, &[json!({ "role": "user", "content": user })], session, max_tokens, timeout_s).await
}

// ── Llamada con herramientas (tool-calling) ───────────────────────────────────────────────────
/// Uso de tokens que reporta el proveedor.
#[derive(Debug, Clone, Default)]
pub struct Uso {
    pub prompt: i64,
    pub cached: i64,
    pub completion: i64,
    pub reasoning: i64,
}

/// Respuesta completa de una llamada (`callOpenCodeChat` de `opencodeChat.ts`): texto, uso, motivo de
/// fin y el mensaje crudo (con `tool_calls` si los hay). No falla si llega vacía.
pub struct RespuestaModelo {
    pub content: String,
    pub usage: Option<Uso>,
    pub finish: Option<String>,
    pub message: Value,
}

pub async fn call_open_code_chat(
    st: &AppState,
    system: &str,
    mensajes: &[Value],
    session: &str,
    max_tokens: u32,
    timeout_s: u64,
    tools: Option<&Value>,
) -> Result<RespuestaModelo, String> {
    let key = st.cfg.opencode_api_key.clone().unwrap_or_default();
    let mut todos = vec![json!({ "role": "system", "content": system })];
    todos.extend(mensajes.iter().cloned());
    let mut cuerpo = json!({ "model": "qwen3.7-max", "messages": todos, "max_tokens": max_tokens });
    if let Some(t) = tools.filter(|t| t.as_array().map(|a| !a.is_empty()).unwrap_or(false)) {
        cuerpo["tools"] = t.clone();
    }
    for intento in 1..=MAX_INTENTOS {
        let res = st
            .http
            .post("https://opencode.ai/zen/go/v1/chat/completions")
            .bearer_auth(&key)
            .header("x-opencode-session", session)
            .timeout(Duration::from_secs(timeout_s))
            .json(&cuerpo)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    "The operation was aborted due to timeout".to_string()
                } else {
                    "fetch failed".to_string()
                }
            })?;
        let status = res.status();
        if !status.is_success() {
            let detalle = res.text().await.unwrap_or_default();
            if es_filtro_de_contenido(&detalle) {
                if intento < MAX_INTENTOS {
                    continue;
                }
                return Err("El filtro de contenido del proveedor de IA bloqueó la respuesta (es un falso positivo intermitente). Vuelve a intentarlo o reformula un poco el pedido.".into());
            }
            let corto: String = detalle.chars().take(200).collect();
            return Err(format!("El modelo respondió {}: {}", status.as_u16(), corto));
        }
        let data: Value = res.json().await.map_err(|_| "El modelo devolvió una respuesta vacía.".to_string())?;
        let choice = data.pointer("/choices/0").cloned().unwrap_or(Value::Null);
        let message = choice.get("message").cloned().unwrap_or_else(|| json!({ "role": "assistant", "content": "" }));
        let content = message.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let usage = data.get("usage").and_then(|u| {
            Some(Uso {
                prompt: u.get("prompt_tokens")?.as_i64()?,
                cached: u.pointer("/prompt_tokens_details/cached_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
                completion: u.get("completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
                reasoning: u.pointer("/completion_tokens_details/reasoning_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
            })
        });
        return Ok(RespuestaModelo { content, usage, finish: choice.get("finish_reason").and_then(|f| f.as_str()).map(String::from), message });
    }
    Err("No se pudo obtener respuesta del modelo.".into())
}
