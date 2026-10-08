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
