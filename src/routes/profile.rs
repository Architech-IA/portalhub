//! /api/profile — perfil del usuario autenticado (MASD PHUB-0001-0002).
//!
//! Paridad con `src/app/api/profile/route.ts`. Única diferencia deliberada: la respuesta de GET
//! ya no incluye `googleAccessToken` / `microsoftAccessToken` (tokens OAuth en crudo que Next
//! mandaba al navegador y que el cliente no usa; solo necesita `googleConnected` /
//! `microsoftConnected`).

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    routes::users::{es_hash_bcrypt, hashear},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, s, B},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/profile", get(obtener).put(accion))
}

async fn obtener(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    let uid = sesion.require_user()?.to_string();

    let user = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(u) FROM (
             SELECT id, name, email, role::text AS role, avatar, "createdAt",
                    ("googleAccessToken" IS NOT NULL) AS "googleConnected",
                    ("microsoftAccessToken" IS NOT NULL) AS "microsoftConnected"
             FROM "User" WHERE id = $1) u"#,
        &[B::T(uid.clone())],
    )
    .await?
    .unwrap_or_else(|| json!({ "googleConnected": false, "microsoftConnected": false }));

    let stats = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'leads',         (SELECT COUNT(*) FROM "Lead" WHERE "userId" = $1),
             'proposals',     (SELECT COUNT(*) FROM "Proposal" WHERE "userId" = $1),
             'projects',      (SELECT COUNT(*) FROM "Project" p
                               WHERE EXISTS (SELECT 1 FROM "ProjectUser" pu
                                             WHERE pu."projectId" = p.id AND pu."userId" = $1)),
             'pipelineValue', COALESCE((SELECT SUM("estimatedValue") FROM "Lead"
                               WHERE "userId" = $1 AND NOT (status = 'RESULT' AND outcome = 'LOST')), 0),
             'leadsGanados',  (SELECT COUNT(*) FROM "Lead"
                               WHERE "userId" = $1 AND status = 'RESULT' AND outcome = 'WON'))"#,
        &[B::T(uid.clone())],
    )
    .await?;

    let actividad = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a."createdAt" DESC), '[]'::jsonb)
           FROM (SELECT * FROM "Activity" WHERE "userId" = $1 ORDER BY "createdAt" DESC LIMIT 10) a"#,
        &[B::T(uid)],
    )
    .await?;

    Ok(Json(json!({ "user": user, "stats": stats, "recentActivity": actividad })))
}

async fn accion(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let uid = sesion.require_user()?.to_string();
    let accion = s(&body, "action").unwrap_or_default();

    match accion.as_str() {
        "updateProfile" => {
            let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
            let mut binds: Vec<B> = vec![B::T(uid.clone())];

            if let Some(n) = s(&body, "name") {
                binds.push(B::T(n));
                sets.push(format!("name = ${}", binds.len()));
            }
            if body.get("avatar").is_some() {
                // `avatar || null`: vacío o null borra el avatar.
                let a = s(&body, "avatar").filter(|x| !x.is_empty());
                binds.push(B::OT(a));
                sets.push(format!("avatar = ${}", binds.len()));
            }
            if let Some(e) = s(&body, "email") {
                let otro = fetch_text_opt(
                    &st.pool,
                    r#"SELECT id FROM "User" WHERE email = $1"#,
                    &[B::T(e.clone())],
                )
                .await?;
                if otro.is_some_and(|id| id != uid) {
                    return Err(ApiError::bad_request("El email ya está en uso"));
                }
                binds.push(B::T(e));
                sets.push(format!("email = ${}", binds.len()));
            }

            let sql = format!(
                r#"WITH up AS (UPDATE "User" SET {} WHERE id = $1
                     RETURNING id, name, email, role::text AS role, avatar, "createdAt")
                   SELECT to_jsonb(up) FROM up"#,
                sets.join(", ")
            );
            let user = fetch_json_opt(&st.pool, &sql, &binds)
                .await?
                .ok_or_else(|| ApiError::internal("Error al actualizar"))?;
            Ok(Json(json!({ "success": true, "user": user })))
        }

        "changePassword" => {
            let actual = s(&body, "currentPassword").filter(|x| !x.is_empty());
            let nueva = s(&body, "newPassword").filter(|x| !x.is_empty());
            let (Some(actual), Some(nueva)) = (actual, nueva) else {
                return Err(ApiError::bad_request("Contraseña actual y nueva son requeridas"));
            };
            if nueva.chars().count() < 6 {
                return Err(ApiError::bad_request("La nueva contraseña debe tener al menos 6 caracteres"));
            }
            let guardada = fetch_text_opt(&st.pool, r#"SELECT password FROM "User" WHERE id = $1"#, &[B::T(uid.clone())])
                .await?
                .ok_or_else(|| ApiError::not_found("Usuario no encontrado"))?;

            let ok = tokio::task::spawn_blocking(move || {
                es_hash_bcrypt(&guardada) && bcrypt::verify(actual, &guardada).unwrap_or(false)
            })
            .await
            .unwrap_or(false);
            if !ok {
                return Err(ApiError::bad_request("Contraseña actual incorrecta"));
            }

            let hash = hashear(nueva).await?;
            exec(
                &st.pool,
                r#"UPDATE "User" SET password = $2, "updatedAt" = NOW() WHERE id = $1"#,
                &[B::T(uid), B::T(hash)],
            )
            .await?;
            Ok(Json(json!({ "success": true })))
        }

        "disconnectGoogle" => {
            exec(
                &st.pool,
                r#"UPDATE "User" SET "googleAccessToken" = NULL, "googleRefreshToken" = NULL,
                     "googleTokenExpiry" = NULL, "googleCalendarId" = NULL, "updatedAt" = NOW()
                   WHERE id = $1"#,
                &[B::T(uid)],
            )
            .await?;
            Ok(Json(json!({ "success": true })))
        }

        "disconnectMicrosoft" => {
            exec(
                &st.pool,
                r#"UPDATE "User" SET "microsoftAccessToken" = NULL, "microsoftRefreshToken" = NULL,
                     "microsoftTokenExpiry" = NULL, "microsoftAccountEmail" = NULL, "updatedAt" = NOW()
                   WHERE id = $1"#,
                &[B::T(uid)],
            )
            .await?;
            Ok(Json(json!({ "success": true })))
        }

        _ => Err(ApiError::bad_request("Acción no válida")),
    }
}
