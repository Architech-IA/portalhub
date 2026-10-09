//! Cliente del servicio privilegiado `portalhub-motor` (puerto 3101, corre como root): lo que toca el
//! sistema de archivos, docker o GitHub no se hace desde el proceso público.

use serde_json::{json, Value};

use crate::state::AppState;

fn base() -> String {
    std::env::var("MOTOR_URL").unwrap_or_else(|_| "http://127.0.0.1:3101".to_string())
}

/// Llama a una ruta interna del motor con la clave interna.
pub async fn llamar(st: &AppState, ruta: &str, cuerpo: Value) -> Result<Value, String> {
    let clave = st.cfg.internal_api_key.clone().unwrap_or_default();
    let r = st
        .http
        .post(format!("{}{ruta}", base()))
        .header("x-api-key", clave)
        .json(&cuerpo)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("el motor no responde: {e}"))?;
    let estado = r.status();
    let v: Value = r.json().await.unwrap_or_else(|_| json!({}));
    if estado.is_success() {
        Ok(v)
    } else {
        Err(v["error"].as_str().unwrap_or("error del motor").to_string())
    }
}

/// `crear_repositorio` del asistente de proyectos.
pub async fn pedir_repositorio(st: &AppState, solucion_id: &str, nombre: &str, privado: bool) -> Result<Value, String> {
    llamar(st, "/internal/crear-repositorio", json!({ "solucionId": solucion_id, "nombre": nombre, "privado": privado })).await
}
