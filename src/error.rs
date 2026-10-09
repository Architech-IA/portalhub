use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

/// Error de API con el mismo formato que devuelven hoy las rutas de Next: `{"error": "..."}`
/// (más campos extra opcionales, p. ej. la versión vigente en un conflicto de edición).
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String, pub Option<Value>);

impl ApiError {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self(status, msg.into(), None)
    }
    pub fn con_extra(mut self, extra: Value) -> Self {
        self.2 = Some(extra);
        self
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "No autorizado")
    }
    pub fn unauthorized_msg(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, msg)
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, msg)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut cuerpo = json!({ "error": self.1 });
        if let (Some(o), Some(Value::Object(extra))) = (cuerpo.as_object_mut(), self.2) {
            for (k, v) in extra {
                o.insert(k, v);
            }
        }
        (self.0, Json(cuerpo)).into_response()
    }
}

/// Un error de base de datos no filtrado al cliente: se loguea completo del lado del servidor y
/// se responde un mensaje genérico (las rutas que necesitan un mensaje propio lo mapean ellas).
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("error de base de datos: {e}");
        Self::internal("Error interno")
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
