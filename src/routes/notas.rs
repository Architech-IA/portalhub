//! Notas y archivos por fase dentro del proyecto — MASD-0024. Sustituyen a las notas del Lead Hub, que desaparece:
//! cada proyecto guarda lo que se escribe en cada una de sus fases (texto enriquecido en pestañas, igual que antes) y
//! los archivos de esa fase. Las vistas `ProyectoNotaLead` / `ProyectoNotaArchivoLead` le dan a lo que ya leía el Lead
//! (IA, contexto de la propuesta, entregables) la misma forma de siempre, ahora con las notas del proyecto.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, new_id, B},
};

const MAX_TEXTO: usize = 400_000;
const MAX_ARCHIVO: usize = 5 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proyectos/{id}/notas", get(listar))
        .route("/api/proyectos/{id}/notas/{fase}", axum::routing::put(guardar))
        .route("/api/proyectos/{id}/archivos-nota", post(subir))
        .route("/api/proyectos/{id}/archivos-nota/{aid}", get(bajar).delete(quitar))
        .route("/api/leads/{id}/proyecto", get(proyecto_de_lead))
}

fn fase_valida(f: &str) -> bool {
    (2..=40).contains(&f.len()) && f.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

async fn existe(st: &AppState, sol: &str) -> Result<(), ApiError> {
    if fetch_text_opt(&st.pool, r#"SELECT id FROM "Solucion" WHERE id = $1"#, &[B::T(sol.into())]).await?.is_none() {
        return Err(ApiError::not_found("Proyecto no encontrado"));
    }
    Ok(())
}

async fn listar(State(st): State<AppState>, _se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    existe(&st, &id).await?;
    let notas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_object_agg(n.fase, jsonb_build_object('contenido', n.contenido, 'actualizadoPor', n."actualizadoPor", 'actualizadoEn', n."updatedAt",
               'archivos', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', a.id, 'nombre', a.nombre, 'size', a.size, 'mimeType', a."mimeType") ORDER BY a."createdAt") FROM "ProyectoNotaArchivo" a WHERE a."notaId" = n.id), '[]'::jsonb))), '{}'::jsonb)
             FROM "ProyectoNota" n WHERE n."solucionId" = $1"#,
        &[B::T(id)],
    )
    .await?;
    Ok(Json(json!({ "notas": notas })))
}

/// Deja la nota de la fase (la crea vacía si no existía) y devuelve su id.
async fn asegurar_nota(st: &AppState, sol: &str, fase: &str, quien: &str) -> Result<String, ApiError> {
    exec(
        &st.pool,
        r#"INSERT INTO "ProyectoNota" (id, "solucionId", fase, contenido, "actualizadoPor") VALUES ($1, $2, $3, '', $4) ON CONFLICT ("solucionId", fase) DO NOTHING"#,
        &[B::T(format!("nota_{}", new_id())), B::T(sol.into()), B::T(fase.into()), B::T(quien.into())],
    )
    .await?;
    Ok(fetch_text_opt(&st.pool, r#"SELECT id FROM "ProyectoNota" WHERE "solucionId" = $1 AND fase = $2"#, &[B::T(sol.into()), B::T(fase.into())]).await?.unwrap_or_default())
}

fn quien(se: &Session) -> String {
    if se.name.is_empty() { se.email.clone() } else { se.name.clone() }
}

async fn guardar(State(st): State<AppState>, se: Session, Path((id, fase)): Path<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    existe(&st, &id).await?;
    if !fase_valida(&fase) {
        return Err(ApiError::bad_request("Fase inválida"));
    }
    let contenido = body["contenido"].as_str().ok_or_else(|| ApiError::bad_request("Falta el contenido"))?;
    if contenido.len() > MAX_TEXTO {
        return Err(ApiError::bad_request("La nota es demasiado larga"));
    }
    let q = quien(&se);
    asegurar_nota(&st, &id, &fase, &q).await?;
    exec(&st.pool, r#"UPDATE "ProyectoNota" SET contenido = $3, "actualizadoPor" = $4, "updatedAt" = NOW() WHERE "solucionId" = $1 AND fase = $2"#, &[B::T(id), B::T(fase), B::T(contenido.into()), B::T(q)]).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn subir(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    existe(&st, &id).await?;
    let fase = body["fase"].as_str().unwrap_or_default();
    if !fase_valida(fase) {
        return Err(ApiError::bad_request("Fase inválida"));
    }
    let nombre = body["nombre"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad_request("Falta el nombre del archivo"))?;
    let b64 = body["base64"].as_str().filter(|b| !b.is_empty()).ok_or_else(|| ApiError::bad_request("Falta el archivo"))?;
    let bytes = b64.len() / 4 * 3;
    if bytes > MAX_ARCHIVO {
        return Err(ApiError::bad_request("Máximo 5 MB por archivo"));
    }
    let nota = asegurar_nota(&st, &id, fase, &quien(&se)).await?;
    let aid = new_id();
    exec(
        &st.pool,
        r#"INSERT INTO "ProyectoNotaArchivo" (id, "notaId", nombre, size, "mimeType", base64, "subidoPor") VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        &[B::T(aid.clone()), B::T(nota), B::T(nombre.chars().take(200).collect()), B::I(bytes as i64), B::OT(body["mimeType"].as_str().map(String::from)), B::T(b64.into()), B::T(quien(&se))],
    )
    .await?;
    Ok(Json(json!({ "id": aid, "nombre": nombre, "size": bytes, "mimeType": body["mimeType"] })))
}

/// El archivo debe pertenecer a una nota del proyecto indicado.
async fn del_proyecto(st: &AppState, sol: &str, aid: &str) -> Result<(), ApiError> {
    let ok = fetch_text_opt(&st.pool, r#"SELECT a.id FROM "ProyectoNotaArchivo" a JOIN "ProyectoNota" n ON n.id = a."notaId" WHERE a.id = $1 AND n."solucionId" = $2"#, &[B::T(aid.into()), B::T(sol.into())]).await?;
    ok.map(|_| ()).ok_or_else(|| ApiError::not_found("Archivo no encontrado"))
}

async fn bajar(State(st): State<AppState>, _se: Session, Path((id, aid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    del_proyecto(&st, &id, &aid).await?;
    let f = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('nombre', nombre, 'mimeType', "mimeType", 'base64', base64) FROM "ProyectoNotaArchivo" WHERE id = $1"#, &[B::T(aid)]).await?;
    f.map(Json).ok_or_else(|| ApiError::not_found("Archivo no encontrado"))
}

async fn quitar(State(st): State<AppState>, _se: Session, Path((id, aid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    del_proyecto(&st, &id, &aid).await?;
    exec(&st.pool, r#"DELETE FROM "ProyectoNotaArchivo" WHERE id = $1"#, &[B::T(aid)]).await?;
    Ok(Json(json!({ "ok": true })))
}

/// A qué proyecto pertenece un lead (lo usa el Lead Hub para mandar a quien llega ahí hacia el proyecto).
async fn proyecto_de_lead(State(st): State<AppState>, _se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sol = fetch_text_opt(&st.pool, r#"SELECT id FROM "Solucion" WHERE "leadId" = $1"#, &[B::T(id)]).await?.ok_or_else(|| ApiError::not_found("Este lead no tiene proyecto"))?;
    Ok(Json(json!({ "solucionId": sol })))
}
