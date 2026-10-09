//! Documentos de una propuesta (`/api/proposals/{id}/documents`) y `soluciones/backfill`.
//! GET y DELETE solo tocan la base; el POST (que convierte Office → PDF con LibreOffice) lo atiende el
//! servicio privilegiado `portalhub-motor` (ver `motor/mod.rs`), sin el tope de memoria del servicio público.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    routes::ejecutor::a_motor,
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proposals/{id}/documents", get(documentos_listar).delete(documento_eliminar).post(a_motor))
        .route("/api/soluciones/backfill", post(backfill))
}

async fn documentos_listar(State(st): State<AppState>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    if let Some(doc) = q.get("docId").filter(|x| !x.is_empty()) {
        return fetch_json_opt(&st.pool, r#"SELECT to_jsonb(d) FROM "ProposalDocument" d WHERE d.id = $1 AND d."proposalId" = $2"#, &[B::T(doc.clone()), B::T(id)])
            .await?
            .map(Json)
            .ok_or_else(|| ApiError::not_found("No encontrado"));
    }
    let etapa = q.get("stage").filter(|x| !x.is_empty()).cloned();
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(d) ORDER BY d."createdAt" ASC), '[]'::jsonb) FROM "ProposalDocument" d WHERE d."proposalId" = $1 AND ($2::text IS NULL OR d.stage = $2)"#,
            &[B::T(id), B::OT(etapa)],
        )
        .await?,
    ))
}

async fn documento_eliminar(State(st): State<AppState>, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    // `deleteMany` con docId indefinido borra todos los de la propuesta; acá se exige el id.
    let doc = b["docId"].as_str().unwrap_or("").to_string();
    exec(&st.pool, r#"DELETE FROM "ProposalDocument" WHERE id = $1 AND "proposalId" = $2"#, &[B::T(doc), B::T(id)]).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Crea las soluciones que faltan para los leads con `solucionAsociada` (solo administradores).
async fn backfill(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    if !sesion.is_admin() {
        return Err(ApiError::forbidden("No autorizado"));
    }
    let leads = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(l)), '[]'::jsonb) FROM "Lead" l
            WHERE l."solucionAsociada" IS NOT NULL AND NOT EXISTS (SELECT 1 FROM "Solucion" s WHERE s."leadId" = l.id)"#,
        &[],
    )
    .await?;
    let leads = leads.as_array().cloned().unwrap_or_default();
    let mut creadas = 0;
    for lead in &leads {
        let Some(asociada) = lead["solucionAsociada"].as_str().filter(|x| !x.is_empty()) else { continue };
        let nombre = format!("{} — {asociada}", lead["companyName"].as_str().unwrap_or(""));
        let codigo = crate::routes::council::codigo_solucion_unico(&st, &crate::routes::council::generar_codigo_solucion(&nombre)).await?;
        let tipo = match asociada {
            "Project" => "PROJECT",
            "Demo" => "DEMO",
            "Partnership" => "PARTNERSHIP",
            "Products" => "PRODUCT",
            _ => "PROJECT",
        };
        exec(
            &st.pool,
            r#"INSERT INTO "Solucion" (id, nombre, descripcion, tipo, "valorEstimado", "leadId", "solucionCode", "createdAt", "updatedAt") VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())"#,
            &[
                B::T(new_id()),
                B::T(nombre),
                B::OT(lead["scope"].as_str().filter(|x| !x.is_empty()).map(String::from)),
                B::T(tipo.into()),
                B::F(lead["estimatedValue"].as_f64().unwrap_or(0.0)),
                B::T(lead["id"].as_str().unwrap_or("").to_string()),
                B::T(codigo),
            ],
        )
        .await?;
        creadas += 1;
    }
    Ok(Json(json!({ "created": creadas, "total": leads.len() })))
}
