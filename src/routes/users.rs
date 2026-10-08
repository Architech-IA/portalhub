//! /api/users — gestión de usuarios (MASD PHUB-0001-0002).
//!
//! Paridad con `src/app/api/users/route.ts`, con DOS correcciones deliberadas:
//! 1. POST ahora exige ADMIN/SUPERADMIN (en Next cualquier usuario logueado podía crear usuarios
//!    con rol ADMIN; solo PUT y DELETE validaban el rol).
//! 2. POST guarda la contraseña con bcrypt (en Next se guardaba tal cual llegaba, y el login
//!    compara con bcrypt, así que un usuario creado desde la pantalla de equipo no podía entrar
//!    y su contraseña quedaba en texto plano en la base).

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, s, s_no_vacio, B},
};

const SUPERADMIN_EMAIL: &str = "admin@architechia.co";
pub const ROLES: [&str; 8] = [
    "SUPERADMIN",
    "ADMIN",
    "GERENTE_COMERCIAL",
    "GERENTE_ADMINISTRATIVO",
    "GERENTE_OPERACIONES",
    "ARQUITECTO_SOLUCIONES",
    "PARTNER",
    "COLLABORATOR",
];

pub fn router() -> Router<AppState> {
    Router::new().route("/api/users", get(listar).post(crear).put(actualizar).delete(eliminar))
}

pub fn es_hash_bcrypt(p: &str) -> bool {
    p.len() == 60 && (p.starts_with("$2a$") || p.starts_with("$2b$") || p.starts_with("$2y$"))
}

pub async fn hashear(password: String) -> ApiResult<String> {
    tokio::task::spawn_blocking(move || bcrypt::hash(password, 12))
        .await
        .map_err(|_| ApiError::internal("Error interno"))?
        .map_err(|_| ApiError::internal("Error interno"))
}

async fn listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(u) ORDER BY u.name ASC), '[]'::jsonb)
           FROM (SELECT id, name, email, role::text AS role, avatar, "createdAt" FROM "User") u"#,
        &[],
    )
    .await?;
    Ok(Json(v))
}

async fn crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;

    let role = s(&body, "role");
    // Nadie puede crear otro SUPERADMIN
    if role.as_deref() == Some("SUPERADMIN") {
        return Err(ApiError::forbidden("El rol SUPERADMIN es único y no puede asignarse"));
    }
    let (Some(name), Some(email), Some(password)) =
        (s_no_vacio(&body, "name"), s_no_vacio(&body, "email"), s_no_vacio(&body, "password"))
    else {
        return Err(ApiError::bad_request("Faltan name, email o password"));
    };
    let role = role.unwrap_or_else(|| "ARQUITECTO_SOLUCIONES".to_string());
    if !ROLES.contains(&role.as_str()) {
        return Err(ApiError::bad_request("Rol inválido"));
    }

    let existe = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(1) FROM "User" WHERE email = $1"#,
        &[B::T(email.clone())],
    )
    .await?;
    if existe.is_some() {
        return Err(ApiError::bad_request("El usuario ya existe"));
    }

    let guardada = if es_hash_bcrypt(&password) { password } else { hashear(password).await? };

    let user = fetch_json(
        &st.pool,
        r#"WITH ins AS (
             INSERT INTO "User" (id, name, email, password, role, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5::"Role", NOW(), NOW())
             RETURNING id, name, email, role::text AS role, avatar, "createdAt")
           SELECT to_jsonb(ins) FROM ins"#,
        &[B::T(new_id()), B::T(name), B::T(email), B::T(guardada), B::T(role)],
    )
    .await?;
    Ok(Json(user))
}

async fn actualizar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let Some(id) = s_no_vacio(&body, "id") else {
        return Err(ApiError::internal("Error al actualizar"));
    };

    // Proteger al SUPERADMIN
    let objetivo = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('role', role::text, 'email', email) FROM "User" WHERE id = $1"#,
        &[B::T(id.clone())],
    )
    .await?;
    if objetivo.as_ref().and_then(|o| o["role"].as_str()) == Some("SUPERADMIN") {
        return Err(ApiError::forbidden("El Super Admin no puede ser modificado"));
    }
    // Nadie puede asignar SUPERADMIN
    let role = s(&body, "role");
    if role.as_deref() == Some("SUPERADMIN") {
        return Err(ApiError::forbidden("El rol SUPERADMIN no puede asignarse"));
    }
    if let Some(r) = &role {
        if !ROLES.contains(&r.as_str()) {
            return Err(ApiError::internal("Error al actualizar"));
        }
    }

    // Solo se tocan los campos presentes (Prisma ignora los `undefined`).
    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(id)];
    if let Some(n) = s(&body, "name") {
        binds.push(B::T(n));
        sets.push(format!("name = ${}", binds.len()));
    }
    if let Some(e) = s(&body, "email") {
        binds.push(B::T(e));
        sets.push(format!("email = ${}", binds.len()));
    }
    if let Some(r) = role {
        binds.push(B::T(r));
        sets.push(format!(r#"role = ${}::"Role""#, binds.len()));
    }
    let sql = format!(
        r#"WITH up AS (UPDATE "User" SET {} WHERE id = $1
             RETURNING id, name, email, role::text AS role, avatar, "createdAt")
           SELECT to_jsonb(up) FROM up"#,
        sets.join(", ")
    );
    match fetch_json_opt(&st.pool, &sql, &binds).await {
        Ok(Some(u)) => Ok(Json(u)),
        _ => Err(ApiError::internal("Error al actualizar")),
    }
}

async fn eliminar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let Some(id) = s_no_vacio(&body, "id") else {
        return Err(ApiError::internal("Error al eliminar"));
    };

    // Proteger al SUPERADMIN
    let objetivo = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('role', role::text, 'email', email) FROM "User" WHERE id = $1"#,
        &[B::T(id.clone())],
    )
    .await?;
    if let Some(o) = &objetivo {
        if o["role"].as_str() == Some("SUPERADMIN") || o["email"].as_str() == Some(SUPERADMIN_EMAIL) {
            return Err(ApiError::forbidden("El Super Admin no puede ser eliminado"));
        }
    }
    match exec(&st.pool, r#"DELETE FROM "User" WHERE id = $1"#, &[B::T(id)]).await {
        Ok(n) if n > 0 => Ok(Json(json!({ "ok": true }))),
        _ => Err(ApiError::internal("Error al eliminar")),
    }
}
