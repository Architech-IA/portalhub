//! Consejo SAGE: propuestas, debate ponderado, ajustes, planificación, negociación y creación del
//! backlog — MASD PHUB-0001-0011. Paridad con `src/app/api/council/**` de Next.
//!
//! Los motores (debate, ajustes, plan, negociación) corren en segundo plano con `tokio::spawn`,
//! igual que los `fire-and-forget` de Next. Hablan con OpenCode por HTTP directo.
//!
//! Notas de la migración:
//! - Next llamaba a OpenCode GO sin la cabecera `x-opencode-session` que la API exige (400
//!   MissingSessionID, ver llm.rs); acá se envía una por propuesta.
//! - `plan/approve` dispara el grafo de tareas del Motor (`runTaskChain`) llamando a la ruta
//!   `executor/dispatch-chain` de Next mientras el executor siga allí.
//! - `host-gateway:8649` (acciones, disparadores, ejecución, trazas) y los agentes `:8644-8648`
//!   no resuelven en este servidor: esas rutas devuelven lo mismo que Next (listas vacías / offline).
//! - `council/status` ya no devuelve la clave de cada agente (Next la incluía en la respuesta).

use std::{sync::LazyLock, time::Duration};

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_i64, fetch_json, fetch_json_opt, fetch_text_opt, new_id, s, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/council/proposals", get(propuestas_listar).post(propuesta_crear))
        .route("/api/council/proposals/{id}", get(propuesta_obtener).patch(propuesta_actualizar).delete(propuesta_eliminar))
        .route("/api/council/proposals/{id}/messages", get(mensajes_listar).post(mensaje_crear))
        .route("/api/council/proposals/{id}/votes", get(votos_listar).post(voto_crear))
        .route("/api/council/proposals/{id}/debate/start", post(debate_iniciar))
        .route("/api/council/proposals/{id}/adjust/start", post(ajuste_iniciar))
        .route("/api/council/proposals/{id}/plan/start", post(plan_iniciar))
        .route("/api/council/proposals/{id}/plan/approve", post(plan_aprobar))
        .route("/api/council/proposals/{id}/negotiate", post(negociar))
        .route("/api/council/proposals/{id}/finalize", post(finalizar))
        .route("/api/council/proposals/{id}/create-backlog", post(crear_backlog))
        .route("/api/council/chat", post(chat))
        .route("/api/council/chat/extract", post(chat_extraer))
        .route("/api/council/chat/attach", post(chat_adjuntar))
        .route("/api/council/document/process", post(documento_procesar))
        .route("/api/council/actions", get(acciones_listar).post(accion_disparar))
        .route("/api/council/actions/{id}/{action}", post(accion_resolver))
        .route("/api/council/run", post(ejecutar))
        .route("/api/council/traces", get(trazas))
        .route("/api/council/triggers", get(disparadores))
        .route("/api/council/status", get(estado_agentes))
        .route("/api/council/triggers/whatsapp", get(whatsapp_estado).post(whatsapp_recibir))
        .route("/api/council/triggers/whatsapp/messages-upsert", get(whatsapp_estado).post(whatsapp_recibir))
}

const GO_URL: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const ZEN_URL: &str = "https://opencode.ai/zen/v1/chat/completions";
const HOST_API: &str = "http://host-gateway:8649";
const UMBRAL: i64 = 5;
const RONDA_PLAN: i64 = 10;
const RONDA_AJUSTE: i64 = 20;

fn err500(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e.to_string())
}

// ── Utilidades de texto y de modelo ──────────────────────────────────────────────────────────
static RE_THINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<think>.*?</think>").expect("re"));
static RE_THINK_ABIERTO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<think>.*").expect("re"));

/// Algunos modelos razonadores devuelven su cadena de pensamiento inline entre `<think>…</think>`.
pub fn quitar_razonamiento(t: &str) -> String {
    let limpio = RE_THINK.replace_all(t, "").to_string();
    RE_THINK_ABIERTO.replace(&limpio, "").trim().to_string()
}

/// `text.match(/\{[\s\S]*\}/)`: del primer `{` al último `}`.
pub fn primer_json(t: &str) -> Option<&str> {
    let a = t.find('{')?;
    let b = t.rfind('}')?;
    if b >= a {
        Some(&t[a..=b])
    } else {
        None
    }
}

/// Llamada al modelo (GO por defecto; `opencode/` usa la API normal). Devuelve el texto sin
/// razonamiento o el mismo mensaje de error que Next.
async fn llm(st: &AppState, system: &str, user: &str, modelo: Option<&str>, max_tokens: u32, timeout_s: u64, sesion: &str) -> Result<String, String> {
    let (url, id_modelo) = match modelo {
        Some(m) if m.starts_with("opencode-go/") => (GO_URL, m["opencode-go/".len()..].to_string()),
        Some(m) if m.starts_with("opencode/") => (ZEN_URL, m["opencode/".len()..].to_string()),
        _ => (GO_URL, st.cfg.opencode_executor_model.clone()),
    };
    let r = st
        .http
        .post(url)
        .bearer_auth(st.cfg.opencode_api_key.clone().unwrap_or_default())
        .header("x-opencode-session", sesion)
        .timeout(Duration::from_secs(timeout_s))
        .json(&json!({
            "model": id_modelo,
            "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
            "max_tokens": max_tokens,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        let code = r.status().as_u16();
        let cuerpo = r.text().await.unwrap_or_default();
        return Err(format!("OpenCode API error {code}: {}", cuerpo.chars().take(300).collect::<String>()));
    }
    let d: Value = r.json().await.map_err(|e| e.to_string())?;
    Ok(quitar_razonamiento(d.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("")))
}

fn hoy() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let z = d.div_euclid(86400) + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dia = doy - (153 * mp + 2) / 5 + 1;
    let mes = if mp < 10 { mp + 3 } else { mp - 9 };
    let anio = yoe + era * 400 + if mes <= 2 { 1 } else { 0 };
    format!("{anio:04}-{mes:02}-{dia:02}")
}

// ── Código de solución (lib/solucionCode.ts) ─────────────────────────────────────────────────
pub fn generar_codigo_solucion(nombre: &str) -> String {
    const STOP: [&str; 16] = ["de", "del", "la", "el", "los", "las", "y", "e", "a", "en", "por", "para", "con", "the", "of", "and"];
    let palabras = nombre
        .split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_ascii_alphanumeric()) && !STOP.contains(&w.to_lowercase().as_str()) && *w != "&");
    palabras.filter_map(|w| w.chars().next()).flat_map(|c| c.to_uppercase()).take(6).collect()
}

pub async fn codigo_solucion_unico(st: &AppState, base: &str) -> Result<String, sqlx::Error> {
    let mut candidato = base.to_string();
    let mut sufijo = 2;
    loop {
        if fetch_text_opt(&st.pool, r#"SELECT id FROM "Solucion" WHERE "solucionCode" = $1 LIMIT 1"#, &[B::T(candidato.clone())]).await?.is_none() {
            return Ok(candidato);
        }
        candidato = format!("{}{}", base.chars().take(5).collect::<String>(), sufijo);
        sufijo += 1;
    }
}

// ── Acceso a datos ───────────────────────────────────────────────────────────────────────────
async fn propuesta_json(st: &AppState, id: &str) -> Result<Option<Value>, sqlx::Error> {
    fetch_json_opt(&st.pool, r#"SELECT to_jsonb(p) FROM "CouncilProposal" p WHERE p.id = $1"#, &[B::T(id.into())]).await
}

async fn estado_propuesta(st: &AppState, id: &str, estado: &str) {
    let _ = exec(&st.pool, r#"UPDATE "CouncilProposal" SET status = $2, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(id.into()), B::T(estado.into())]).await;
}

async fn metadata_y_estado(st: &AppState, id: &str, estado: &str, meta: Value) {
    let r = exec(
        &st.pool,
        r#"UPDATE "CouncilProposal" SET status = $3, metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb, "updatedAt" = NOW() WHERE id = $1"#,
        &[B::T(id.into()), B::T(meta.to_string()), B::T(estado.into())],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("[council] no se pudo actualizar la propuesta {id}: {e}");
    }
}

async fn insertar_mensaje(st: &AppState, propuesta: &str, agente_id: &str, nombre: &str, slug: &str, contenido: &str, ronda: i64) -> Result<(), sqlx::Error> {
    exec(
        &st.pool,
        r#"INSERT INTO "DebateMessage" ("proposalId", "agentId", "agentName", "agentSlug", content, round) VALUES ($1, $2, $3, $4, $5, $6::int)"#,
        &[B::T(propuesta.into()), B::T(agente_id.into()), B::T(nombre.into()), B::T(slug.into()), B::T(contenido.into()), B::I(ronda)],
    )
    .await?;
    Ok(())
}

fn items_de(p: &Value) -> Vec<Value> {
    p["items"].as_array().cloned().unwrap_or_default()
}

fn capitalizar(slug: &str) -> String {
    let mut c = slug.chars();
    match c.next() {
        Some(p) => p.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Agentes del consejo guardados en la base (`id`, `systemPrompt`, `llmModel`, `name`).
async fn agentes_db(st: &AppState, slugs: &[&str]) -> Vec<Value> {
    let lista = slugs.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'name', name, 'slug', slug, 'systemPrompt', "systemPrompt", 'llmModel', "llmModel")), '[]'::jsonb) FROM "Agent" WHERE slug IN ({lista})"#
    );
    fetch_json(&st.pool, &sql, &[]).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

// ═══════════════════════════════ PROPUESTAS ═══════════════════════════════
async fn propuestas_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let estado = q.get("status").filter(|x| !x.is_empty()).map(|x| x.replace('\'', ""));
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC), '[]'::jsonb) FROM
               (SELECT id, title, description, status, "inputChannel", items, round, "epicId", "sprintId", "solucionId", "createdByAgentId", "createdByAgentName",
                       metadata, "createdAt", "updatedAt"
                FROM "CouncilProposal" WHERE ($1::text IS NULL OR status = $1) ORDER BY "createdAt" DESC LIMIT 50) x"#,
            &[B::OT(estado)],
        )
        .await?,
    ))
}

async fn propuesta_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let Some(titulo) = s(&body, "title").filter(|t| !t.is_empty()) else {
        return Err(ApiError::bad_request("title requerido"));
    };
    let mut solucion_id = s(&body, "solucionId").filter(|x| !x.is_empty());
    // Si el humano eligió "crear Solución nueva" en el panel de Propuesta Extraída, se crea ahora.
    if solucion_id.is_none() {
        if let Some(nombre) = body.pointer("/solucionPropuesta/name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()) {
            let codigo = codigo_solucion_unico(&st, &generar_codigo_solucion(nombre)).await?;
            let sp = &body["solucionPropuesta"];
            let id = new_id_uuid();
            fetch_text_opt(
                &st.pool,
                r#"INSERT INTO "Solucion" (id, nombre, descripcion, estado, tipo, repositorio, "solucionCode", "createdAt", "updatedAt")
                   VALUES ($1, $2, $3, 'ACTIVO', 'PRODUCT', $4, $5, NOW(), NOW()) RETURNING id"#,
                &[B::T(id.clone()), B::T(nombre.into()), B::OT(s(sp, "description")), B::T(s(sp, "repositorio").unwrap_or_else(|| "portal-architechia".into())), B::T(codigo)],
            )
            .await?;
            solucion_id = Some(id);
        }
    }
    let items = body.get("items").cloned().filter(|i| !i.is_null()).unwrap_or_else(|| json!([]));
    let metadata = match body.get("metadata") {
        Some(m) if truthy_v(m) => Some(m.to_string()),
        _ => None,
    };
    let fila = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "CouncilProposal" (title, description, "inputChannel", items, "epicId", "sprintId", "solucionId", "createdByAgentId", "createdByAgentName", metadata)
             VALUES ($1, $2, COALESCE($3, 'CONVERSATION'), $4::jsonb, $5, $6, $7, $8, $9, $10::jsonb) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(titulo),
            B::OT(s(&body, "description")),
            B::OT(s(&body, "inputChannel")),
            B::T(items.to_string()),
            B::OT(s(&body, "epicId")),
            B::OT(s(&body, "sprintId")),
            B::OT(solucion_id),
            B::OT(s(&body, "createdByAgentId")),
            B::OT(s(&body, "createdByAgentName")),
            B::OT(metadata),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(fila)))
}

fn new_id_uuid() -> String {
    // `crypto.randomUUID()`: uuid v4 a partir de bytes aleatorios.
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn truthy_v(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

async fn propuesta_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    use sqlx::Row;
    // Mismas uniones que Next: `json_agg(DISTINCT fila)`; las fechas anidadas quedan como las da
    // PostgreSQL (sin normalizar), igual que en Next.
    let fila = sqlx::query(
        r#"SELECT to_jsonb(p) AS p,
                  (SELECT COALESCE(json_agg(DISTINCT dm.*) FILTER (WHERE dm.id IS NOT NULL), '[]') FROM "DebateMessage" dm WHERE dm."proposalId" = p.id) AS messages,
                  (SELECT COALESCE(json_agg(DISTINCT av.*) FILTER (WHERE av.id IS NOT NULL), '[]') FROM "AgentVote" av WHERE av."proposalId" = p.id) AS votes
           FROM "CouncilProposal" p WHERE p.id = $1"#,
    )
    .bind(&id)
    .fetch_optional(&st.pool)
    .await?;
    let Some(fila) = fila else { return Err(ApiError::not_found("Not found")) };
    let mut p: Value = fila.try_get("p")?;
    crate::util::fix_dates(&mut p);
    p["messages"] = fila.try_get::<Value, _>("messages")?;
    p["votes"] = fila.try_get::<Value, _>("votes")?;
    Ok(Json(p))
}

async fn propuesta_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let estado = s(&body, "status").filter(|x| !x.is_empty());
    let ronda = body.get("round").and_then(|r| r.as_f64()).filter(|r| *r != 0.0);
    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(id)];
    if let Some(e) = estado {
        binds.push(B::T(e));
        sets.push(format!("status = ${}", binds.len()));
    }
    if let Some(r) = ronda {
        binds.push(B::I(r as i64));
        sets.push(format!("round = ${}::int", binds.len()));
    }
    let sql = format!(r#"WITH up AS (UPDATE "CouncilProposal" SET {} WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#, sets.join(", "));
    fetch_json_opt(&st.pool, &sql, &binds).await?.map(Json).ok_or_else(|| ApiError::not_found("Not found"))
}

async fn propuesta_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exec(&st.pool, r#"DELETE FROM "DebateMessage" WHERE "proposalId" = $1"#, &[B::T(id.clone())]).await?;
    exec(&st.pool, r#"DELETE FROM "AgentVote" WHERE "proposalId" = $1"#, &[B::T(id.clone())]).await?;
    exec(&st.pool, r#"DELETE FROM "CouncilProposal" WHERE id = $1"#, &[B::T(id)]).await?;
    Ok(Json(json!({ "deleted": true })))
}

// ═══════════════════════════════ MENSAJES Y VOTOS ═══════════════════════════════
async fn mensajes_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.round, m."createdAt"), '[]'::jsonb) FROM "DebateMessage" m WHERE m."proposalId" = $1"#,
            &[B::T(id)],
        )
        .await?,
    ))
}

async fn mensaje_crear(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (Some(agente_id), Some(nombre), Some(contenido)) =
        (s(&body, "agentId").filter(|x| !x.is_empty()), s(&body, "agentName").filter(|x| !x.is_empty()), s(&body, "content").filter(|x| !x.is_empty()))
    else {
        return Err(ApiError::bad_request("agentId, agentName y content requeridos"));
    };
    let ronda = body.get("round").and_then(|r| r.as_i64()).unwrap_or(1);
    let fila = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "DebateMessage" ("proposalId", "agentId", "agentName", "agentSlug", content, round) VALUES ($1, $2, $3, $4, $5, $6::int) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[B::T(id), B::T(agente_id), B::T(nombre), B::OT(s(&body, "agentSlug")), B::T(contenido), B::I(ronda)],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(fila)))
}

async fn votos_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let votos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY v.round, v."createdAt"), '[]'::jsonb) FROM "AgentVote" v WHERE v."proposalId" = $1"#, &[B::T(id)]).await?;
    let total: i64 = votos.as_array().cloned().unwrap_or_default().iter().filter(|v| v["vote"] == true).map(|v| v["weight"].as_i64().unwrap_or(0)).sum();
    Ok(Json(json!({ "votes": votos, "weightedScore": total, "threshold": UMBRAL, "approved": total >= UMBRAL })))
}

/// ¿Alguno de los `areaId` de los items de la propuesta pertenece al área de este agente?
async fn area_coincide(st: &AppState, propuesta: &Value, slug: &str) -> Result<bool, sqlx::Error> {
    let area_ids: Vec<String> = items_de(propuesta).iter().filter_map(|i| i["areaId"].as_str().filter(|x| !x.is_empty()).map(String::from)).collect();
    if area_ids.is_empty() {
        return Ok(false);
    }
    let agentes = sqlx::query_scalar::<_, String>(r#"SELECT "agentSlug" FROM "Area" WHERE id = ANY($1::text[]) AND "agentSlug" IS NOT NULL"#)
        .bind(&area_ids)
        .fetch_all(&st.pool)
        .await?;
    Ok(agentes.iter().any(|a| a == slug))
}

async fn voto_crear(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (Some(agente_id), Some(nombre)) = (s(&body, "agentId").filter(|x| !x.is_empty()), s(&body, "agentName").filter(|x| !x.is_empty())) else {
        return Err(ApiError::bad_request("agentId, agentName y vote requeridos"));
    };
    let Some(voto) = body.get("vote").and_then(|v| v.as_bool()).or_else(|| body.get("vote").filter(|v| !v.is_null()).map(truthy_v)) else {
        return Err(ApiError::bad_request("agentId, agentName y vote requeridos"));
    };
    let ronda = body.get("round").and_then(|r| r.as_i64()).unwrap_or(1);
    let slug_original = s(&body, "agentSlug");
    let slug = slug_original.clone().unwrap_or_default().to_lowercase();
    let propuesta = propuesta_json(&st, &id).await?.unwrap_or(Value::Null);
    let peso = if slug == "orion" {
        3
    } else if area_coincide(&st, &propuesta, slug_original.as_deref().unwrap_or("")).await? {
        2
    } else {
        1
    };
    let fila = fetch_json(
        &st.pool,
        r#"WITH up AS (INSERT INTO "AgentVote" ("proposalId", "agentId", "agentName", "agentSlug", weight, vote, argument, round)
             VALUES ($1, $2, $3, $4, $5::int, $6, $7, $8::int)
             ON CONFLICT ("proposalId", "agentId", round) DO UPDATE SET vote = $6, argument = $7, weight = $5::int, "createdAt" = NOW() RETURNING *)
           SELECT to_jsonb(up) FROM up"#,
        &[B::T(id.clone()), B::T(agente_id.clone()), B::T(nombre.clone()), B::OT(slug_original), B::I(peso), B::Bo(voto), B::OT(s(&body, "argument")), B::I(ronda)],
    )
    .await?;

    let todos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(v)), '[]'::jsonb) FROM "AgentVote" v WHERE v."proposalId" = $1 AND v.round = $2::int"#, &[B::T(id.clone()), B::I(ronda)]).await?;
    let todos = todos.as_array().cloned().unwrap_or_default();
    let votaron: Vec<String> = todos.iter().filter_map(|v| v["agentSlug"].as_str().map(|x| x.to_lowercase())).filter(|x| !x.is_empty()).collect();
    let todos_votaron = ["orion", "ares", "atlas", "iris", "vesta"].iter().all(|s| votaron.iter().any(|v| v == s));
    let puntaje: i64 = todos.iter().filter(|v| v["vote"] == true).map(|v| v["weight"].as_i64().unwrap_or(0)).sum();

    let mut nuevo_estado: Option<&str> = None;
    if todos_votaron {
        if puntaje >= UMBRAL {
            nuevo_estado = Some("APPROVED");
            estado_propuesta(&st, &id, "APPROVED").await;
            crear_items_aprobados(&st, &id, &agente_id, &nombre).await?;
        } else if ronda >= 2 {
            nuevo_estado = Some("ESCALATED");
            estado_propuesta(&st, &id, "ESCALATED").await;
        } else {
            nuevo_estado = Some("REVISED");
            estado_propuesta(&st, &id, "REVISED").await;
            crear_propuesta_ronda2(&st, &id).await?;
        }
    }
    Ok((StatusCode::CREATED, Json(json!({ "vote": fila, "weightedScore": puntaje, "threshold": UMBRAL, "newStatus": nuevo_estado, "allVoted": todos_votaron }))))
}

/// En Next estas inserciones usaban columnas que no existen (`itemType`, `epicId` en BacklogItem) y
/// no pasaban el id, así que fallaban siempre. Acá se corrigen (`type`, sin `epicId`, con id).
async fn crear_items_aprobados(st: &AppState, propuesta_id: &str, agente_id: &str, agente_nombre: &str) -> Result<(), sqlx::Error> {
    let Some(p) = propuesta_json(st, propuesta_id).await? else { return Ok(()) };
    for item in items_de(&p) {
        match item["type"].as_str() {
            Some("task") => {
                exec(
                    &st.pool,
                    r#"INSERT INTO "BacklogItem" (id, title, description, status, priority, type, "areaId", "sprintId", "createdByAgentId", "createdByAgentName", "createdAt", "updatedAt")
                       VALUES ($1, $2, $3, 'BACKLOG', $4, 'TASK', $5, $6, $7, $8, NOW(), NOW())"#,
                    &[
                        B::T(new_id()),
                        B::T(s(&item, "title").unwrap_or_else(|| "Task sin título".into())),
                        B::OT(s(&item, "description")),
                        B::T(s(&item, "priority").unwrap_or_else(|| "MEDIUM".into())),
                        B::OT(s(&item, "areaId")),
                        B::OT(s(&p, "sprintId").or_else(|| s(&item, "sprintId"))),
                        B::T(agente_id.into()),
                        B::T(agente_nombre.into()),
                    ],
                )
                .await?;
            }
            Some("sprint") => {
                exec(
                    &st.pool,
                    r#"INSERT INTO "Sprint" (id, name, goal, status, "epicId", "ownerAreaId", "responsibleId", "responsibleName", "createdAt")
                       VALUES ($1, $2, $3, 'PLANNED', $4, $5, $6, $7, NOW())"#,
                    &[
                        B::T(new_id()),
                        B::T(s(&item, "title").unwrap_or_else(|| "Sprint sin título".into())),
                        B::OT(s(&item, "goal")),
                        B::OT(s(&p, "epicId").or_else(|| s(&item, "epicId"))),
                        B::OT(s(&item, "areaId")),
                        B::T(agente_id.into()),
                        B::T(agente_nombre.into()),
                    ],
                )
                .await?;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn crear_propuesta_ronda2(st: &AppState, original_id: &str) -> Result<(), sqlx::Error> {
    let Some(o) = propuesta_json(st, original_id).await? else { return Ok(()) };
    let mut meta = json!({ "originalProposalId": original_id, "revisedRound": 2 });
    if let Some(m) = o["metadata"].as_object() {
        for (k, v) in m {
            meta[k] = v.clone();
        }
    }
    exec(
        &st.pool,
        r#"INSERT INTO "CouncilProposal" (title, description, status, "inputChannel", items, round, "epicId", "sprintId", "solucionId", "createdByAgentId", "createdByAgentName", metadata)
           VALUES ($1, $2, 'PENDING', $3, $4::jsonb, 2, $5, $6, $7, $8, $9, $10::jsonb)"#,
        &[
            B::T(format!("[Revisada] {}", o["title"].as_str().unwrap_or(""))),
            B::OT(s(&o, "description")),
            B::T(s(&o, "inputChannel").unwrap_or_else(|| "CONVERSATION".into())),
            B::T(o["items"].to_string()),
            B::OT(s(&o, "epicId")),
            B::OT(s(&o, "sprintId")),
            B::OT(s(&o, "solucionId")),
            B::OT(s(&o, "createdByAgentId")),
            B::OT(s(&o, "createdByAgentName")),
            B::T(meta.to_string()),
        ],
    )
    .await?;
    Ok(())
}

// ═══════════════════════════════ DEBATE ═══════════════════════════════
const AGENTES_CONSEJO: [(&str, &str, &str, &str); 5] = [
    ("agent_orion_001", "Orión", "orion", "Eres Orión, el agente estratégico central de ArchiTechIA. Actúas como CEO operacional del consejo de agentes.\nTu función: evaluar si una propuesta es coherente con la visión estratégica de ArchiTechIA, si se alinea con las soluciones activas y si tiene viabilidad sistémica a largo plazo.\nEres reflexivo, decisivo y hablas con autoridad. Tu peso de voto es 3 (el más alto del consejo).\nSi rechazas, tu argumento guía la reformulación en ronda 2.\nResponde EXCLUSIVAMENTE con JSON sin markdown: {\"argument\": \"análisis en 2-3 oraciones\", \"vote\": true o false}"),
    ("agent_ares_001", "Ares", "ares", "Eres Ares, el agente comercial de ArchiTechIA. Rol: Sales & Presales Lead.\nTu función: evaluar si la propuesta tiene impacto comercial real, genera ingresos directos o abre oportunidades de negocio concretas.\nEres agresivo, orientado a conversión y piensas en el pipeline. Si algo no mueve ventas o no protege clientes actuales, eres escéptico.\nResponde EXCLUSIVAMENTE con JSON sin markdown: {\"argument\": \"análisis en 2-3 oraciones\", \"vote\": true o false}"),
    ("agent_atlas_001", "Atlas", "atlas", "Eres Atlas, el agente operativo de ArchiTechIA. Rol: Operations Manager.\nTu función: evaluar si la propuesta es ejecutable con los recursos actuales del equipo, si tiene dependencias bloqueantes y si el timeline es realista.\nEres analítico, exiges datos concretos y no especulas. Si falta información crítica para estimar esfuerzo, lo señalas y rechazas por precaución.\nResponde EXCLUSIVAMENTE con JSON sin markdown: {\"argument\": \"análisis en 2-3 oraciones\", \"vote\": true o false}"),
    ("agent_iris_001", "Iris", "iris", "Eres Iris, la agente de marketing y marca de ArchiTechIA. Rol: Marketing & Brand Lead.\nTu función: evaluar si la propuesta es coherente con la identidad de marca ArchiTechIA, si la comunicación externa es adecuada y si refuerza el posicionamiento en el mercado.\nEres creativa pero rigurosa con la identidad. Preguntas: ¿cómo lo vería un cliente? ¿refuerza o diluye la marca?\nResponde EXCLUSIVAMENTE con JSON sin markdown: {\"argument\": \"análisis en 2-3 oraciones\", \"vote\": true o false}"),
    ("agent_vesta_001", "Vesta", "vesta", "Eres Vesta, la agente de finanzas y legal de ArchiTechIA. Rol: Finance & Legal Lead.\nTu función: evaluar la viabilidad financiera de la propuesta, si hay presupuesto disponible, si el ROI justifica la inversión y si hay riesgos legales o de cumplimiento.\nEres conservadora y precisa. Si el costo no está justificado o hay riesgo legal sin mitigación, rechazas.\nResponde EXCLUSIVAMENTE con JSON sin markdown: {\"argument\": \"análisis en 2-3 oraciones\", \"vote\": true o false}"),
];

const INSTRUCCION_VOTO: &str = "\n\nResponde EXCLUSIVAMENTE con JSON sin markdown: {\"argument\": \"análisis en 2-3 oraciones\", \"vote\": true o false}";

fn peso_base(slug: &str) -> i64 {
    match slug {
        "orion" => 3,
        "atlas" => 2,
        _ => 1,
    }
}

async fn voto_del_agente(st: &AppState, sistema: &str, mensaje: &str, modelo: Option<&str>, sesion: &str) -> Result<(String, bool), String> {
    let contenido = llm(st, sistema, mensaje, modelo, 2048, 90, sesion).await?;
    let json_txt = primer_json(&contenido).ok_or_else(|| format!("No JSON in response: {}", contenido.chars().take(200).collect::<String>()))?;
    let v: Value = serde_json::from_str(json_txt).map_err(|e| e.to_string())?;
    Ok((v["argument"].as_str().map(String::from).unwrap_or_else(|| "Sin argumento generado".into()), truthy_v(&v["vote"])))
}

async fn motor_debate(st: AppState, propuesta_id: String, ronda: i64) {
    let sesion = format!("council-{propuesta_id}");
    let db = agentes_db(&st, &["orion", "ares", "atlas", "iris", "vesta"]).await;
    let Ok(Some(propuesta)) = propuesta_json(&st, &propuesta_id).await else { return };
    let resumen = items_de(&propuesta).iter().map(|i| format!("- {}: {}", i["type"].as_str().unwrap_or("undefined"), i["title"].as_str().unwrap_or("undefined"))).collect::<Vec<_>>().join("\n");
    let resumen = if resumen.is_empty() { "Sin items específicos".to_string() } else { resumen };
    let mut previos: Vec<(String, String)> = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('n', "agentName", 'c', content) ORDER BY "createdAt"), '[]'::jsonb) FROM "DebateMessage" WHERE "proposalId" = $1 AND round = $2::int"#,
        &[B::T(propuesta_id.clone()), B::I(ronda)],
    )
    .await
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default()
    .iter()
    .map(|m| (m["n"].as_str().unwrap_or("").to_string(), m["c"].as_str().unwrap_or("").to_string()))
    .collect();

    for (id_fb, nombre_fb, slug, prompt_fb) in AGENTES_CONSEJO {
        let d = db.iter().find(|a| a["slug"].as_str() == Some(slug));
        let id = d.and_then(|a| a["id"].as_str()).unwrap_or(id_fb).to_string();
        let nombre = d.and_then(|a| a["name"].as_str()).unwrap_or(nombre_fb).to_string();
        let sistema = match d.and_then(|a| a["systemPrompt"].as_str()).filter(|p| !p.is_empty()) {
            Some(p) => format!("{p}{INSTRUCCION_VOTO}"),
            None => prompt_fb.to_string(),
        };
        let modelo = d.and_then(|a| a["llmModel"].as_str()).map(String::from);
        let r: Result<(), String> = async {
            let previo = if previos.is_empty() {
                String::new()
            } else {
                format!("\n\nDebate previo en esta ronda:\n{}", previos.iter().map(|(n, c)| format!("{n}: {c}")).collect::<Vec<_>>().join("\n"))
            };
            let mensaje = format!(
                "PROPUESTA A EVALUAR:\nTítulo: {}\nDescripción: {}\nCanal de entrada: {}\nItems propuestos:\n{}{}\n\nAnaliza esta propuesta desde tu perspectiva y emite tu voto. Recuerda responder solo con JSON.",
                propuesta["title"].as_str().unwrap_or(""),
                propuesta["description"].as_str().unwrap_or("Sin descripción"),
                propuesta["inputChannel"].as_str().unwrap_or(""),
                resumen,
                previo
            );
            let (argumento, voto) = voto_del_agente(&st, &sistema, &mensaje, modelo.as_deref(), &sesion).await?;
            insertar_mensaje(&st, &propuesta_id, &id, &nombre, slug, &argumento, ronda).await.map_err(|e| e.to_string())?;
            let mut peso = peso_base(slug);
            if slug != "orion" && area_coincide(&st, &propuesta, slug).await.map_err(|e| e.to_string())? {
                peso = 2;
            }
            exec(
                &st.pool,
                r#"INSERT INTO "AgentVote" ("proposalId", "agentId", "agentName", "agentSlug", weight, vote, argument, round)
                   VALUES ($1, $2, $3, $4, $5::int, $6, $7, $8::int)
                   ON CONFLICT ("proposalId", "agentId", round) DO UPDATE SET vote = $6, argument = $7, weight = $5::int, "createdAt" = NOW()"#,
                &[B::T(propuesta_id.clone()), B::T(id.clone()), B::T(nombre.clone()), B::T(slug.into()), B::I(peso), B::Bo(voto), B::T(argumento.clone()), B::I(ronda)],
            )
            .await
            .map_err(|e| e.to_string())?;
            previos.push((nombre.clone(), argumento));
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::error!("[DebateEngine] Error with agent {nombre}: {e}");
        }
    }

    let puntaje = fetch_i64(
        &st.pool,
        r#"SELECT COALESCE(SUM(weight) FILTER (WHERE vote), 0)::bigint FROM "AgentVote" WHERE "proposalId" = $1 AND round = $2::int"#,
        &[B::T(propuesta_id.clone()), B::I(ronda)],
    )
    .await
    .unwrap_or(0);
    if puntaje >= UMBRAL {
        estado_propuesta(&st, &propuesta_id, "PLANNING").await;
        tokio::spawn(motor_plan(st.clone(), propuesta_id.clone(), None));
    } else if ronda >= 2 {
        estado_propuesta(&st, &propuesta_id, "ESCALATED").await;
    } else {
        estado_propuesta(&st, &propuesta_id, "ADJUSTING").await;
        tokio::spawn(motor_ajuste(st.clone(), propuesta_id.clone(), None));
    }
}

async fn debate_iniciar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let ronda = body.get("round").and_then(|r| r.as_i64()).unwrap_or(1);
    let Some(p) = propuesta_json(&st, &id).await? else { return Err(ApiError::not_found("Propuesta no encontrada")) };
    let estado = p["status"].as_str().unwrap_or("");
    if !["PENDING", "REVISED"].contains(&estado) {
        return Err(ApiError::bad_request(format!("No se puede iniciar debate con status: {estado}")));
    }
    estado_propuesta(&st, &id, "DEBATING").await;
    tokio::spawn(motor_debate(st.clone(), id.clone(), ronda));
    Ok(Json(json!({ "started": true, "status": "DEBATING", "proposalId": id, "round": ronda })))
}

// ═══════════════════════════════ AJUSTES ═══════════════════════════════
const AGENTES_AJUSTE: [(&str, &str); 5] = [
    ("ares", "Eres Ares, Sales Lead. La propuesta fue rechazada.\nAnaliza las razones de rechazo desde la perspectiva comercial:\n- Que aspectos no tienen justificacion de negocio suficiente\n- Que cambios aumentarian el impacto comercial\n- Que se podria reducir o simplificar sin perder valor\nPropone cambios concretos. Responde en prosa, 3-4 oraciones."),
    ("atlas", "Eres Atlas, Operations Manager. La propuesta fue rechazada.\nAnaliza las razones de rechazo desde la perspectiva operativa:\n- Que parte del alcance es inviable o demasiado amplia\n- Que simplificaciones harían el scope ejecutable\n- Que dependencias bloqueantes hay que resolver primero\nPropone ajustes concretos. Responde en prosa, 3-4 oraciones."),
    ("vesta", "Eres Vesta, Finance Lead. La propuesta fue rechazada.\nAnaliza las razones de rechazo desde la perspectiva financiera:\n- Que aspectos tienen riesgo financiero no justificado\n- Como se podria reducir el costo o riesgo manteniendo el valor\n- Que ajuste al presupuesto o alcance mejoraria la aprobacion\nPropone ajustes concretos. Responde en prosa, 3-4 oraciones."),
    ("iris", "Eres Iris, Marketing Lead. La propuesta fue rechazada.\nAnaliza las razones de rechazo desde branding/comunicacion:\n- Que aspectos no estan bien comunicados o alineados con la marca\n- Como se podria reformular para que sea mas convincente\n- Que ajustes de enfoque o narrativa mejorarian la aceptacion\nResponde en prosa, 3-4 oraciones."),
    ("orion", "Eres Orion, CEO operacional. Con los inputs de todos los agentes,\nsintetiza los ajustes definitivos a la propuesta rechazada.\nQue cambios concretos y especificos se deben hacer para que sea aprobable.\nResponde EXCLUSIVAMENTE con JSON valido sin markdown."),
];

async fn motor_ajuste(st: AppState, propuesta_id: String, comentario: Option<String>) {
    let sesion = format!("council-{propuesta_id}");
    let Ok(Some(propuesta)) = propuesta_json(&st, &propuesta_id).await else { return };
    let db = agentes_db(&st, &["orion", "atlas", "vesta", "ares", "iris"]).await;
    let votos = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('n', "agentName", 'a', argument, 'v', vote, 'w', weight) ORDER BY "createdAt"), '[]'::jsonb) FROM "AgentVote" WHERE "proposalId" = $1"#,
        &[B::T(propuesta_id.clone())],
    )
    .await
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    let razones = votos.iter().filter(|v| v["v"] != true).map(|v| format!("  - {}: {}", v["n"].as_str().unwrap_or(""), v["a"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n");
    let todos = votos
        .iter()
        .map(|v| format!("  - {} ({}): {}", v["n"].as_str().unwrap_or(""), if v["v"] == true { "VOTO A FAVOR" } else { "RECHAZO" }, v["a"].as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n");
    let humano = comentario.as_ref().map(|c| format!("\n\nCOMENTARIO DEL USUARIO:\n{c}")).unwrap_or_default();
    let items_json = serde_json::to_string_pretty(&propuesta["items"]).unwrap_or_else(|_| "[]".into());
    let base = format!(
        "PROPUESTA RECHAZADA:\nTitulo: {}\nDescripcion: {}\nItems originales: {}\n\nRAZONES DE RECHAZO:\n{}\n\nTODOS LOS ARGUMENTOS DEL DEBATE:\n{}\n{}",
        propuesta["title"].as_str().unwrap_or(""),
        propuesta["description"].as_str().unwrap_or("Sin descripcion"),
        items_json,
        if razones.is_empty() { "  (no disponible)".to_string() } else { razones },
        if todos.is_empty() { "  (no disponible)".to_string() } else { todos },
        humano
    );
    let _ = exec(&st.pool, r#"DELETE FROM "DebateMessage" WHERE "proposalId" = $1 AND round = $2::int"#, &[B::T(propuesta_id.clone()), B::I(RONDA_AJUSTE)]).await;

    let mut historial: Vec<String> = vec![];
    let mut final_ajuste: Option<Value> = None;
    for (slug, prompt) in AGENTES_AJUSTE {
        let d = db.iter().find(|a| a["slug"].as_str() == Some(slug));
        let id = d.and_then(|a| a["id"].as_str()).map(String::from).unwrap_or_else(|| format!("agent_{slug}_001"));
        let nombre = capitalizar(slug);
        let modelo = d.and_then(|a| a["llmModel"].as_str()).map(String::from);
        let previo = if historial.is_empty() { String::new() } else { format!("\n\nDEBATE DE AJUSTES HASTA AHORA:\n{}", historial.join("\n\n")) };
        let mensaje = if slug == "orion" {
            format!("{base}{previo}\n\nCon todo lo anterior, define los ajustes definitivos. Formato JSON exacto:\n{}", r#"{"needsMoreInfo":false,"questions":[],"adjustmentRationale":"por que estos ajustes resuelven el rechazo","titleAdjusted":null,"descriptionAdjusted":null,"keyChanges":[{"aspect":"que cambia","from":"estado actual","to":"propuesta de cambio","agentSupporting":"nombre agente","rationale":"por que"}],"agentConsensus":"resumen del consenso"}"#)
        } else {
            format!("{base}{previo}\n\n{prompt}")
        };
        match llm(&st, prompt, &mensaje, modelo.as_deref(), 4096, 120, &sesion).await {
            Ok(respuesta) => {
                if slug == "orion" {
                    if let Some(j) = primer_json(&respuesta) {
                        final_ajuste = serde_json::from_str::<Value>(j).ok();
                    }
                    let prosa = RE_JSON_BLOQUE.replace(&respuesta, "").trim().to_string();
                    let prosa = if prosa.is_empty() { "Ajustes sintetizados.".to_string() } else { prosa };
                    let _ = insertar_mensaje(&st, &propuesta_id, &id, &format!("{nombre} (Ajustes)"), slug, &prosa, RONDA_AJUSTE).await;
                } else {
                    let _ = insertar_mensaje(&st, &propuesta_id, &id, &format!("{nombre} (Ajustes)"), slug, &respuesta, RONDA_AJUSTE).await;
                    historial.push(format!("{nombre}: {respuesta}"));
                }
            }
            Err(e) => tracing::error!("[AdjustmentEngine] Error with {slug}: {e}"),
        }
    }
    match final_ajuste {
        Some(a) if a["needsMoreInfo"].as_bool().unwrap_or(false) && a["questions"].as_array().map(|q| !q.is_empty()).unwrap_or(false) => {
            metadata_y_estado(&st, &propuesta_id, "ADJUST_QUESTIONS", json!({ "adjustmentQuestions": a["questions"], "adjustmentProposal": a })).await
        }
        Some(a) => metadata_y_estado(&st, &propuesta_id, "ADJUST_READY", json!({ "adjustmentProposal": a })).await,
        None => metadata_y_estado(&st, &propuesta_id, "ADJUST_READY", json!({ "adjustError": "No se pudo generar ajustes — revisa los logs del servidor." })).await,
    }
}

static RE_JSON_BLOQUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\{.*\}").expect("re"));

async fn ajuste_iniciar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let comentario = s(&body, "humanComment");
    let Some(p) = propuesta_json(&st, &id).await? else { return Err(ApiError::not_found("Propuesta no encontrada")) };
    let estado = p["status"].as_str().unwrap_or("");
    if !["ADJUSTING", "ADJUST_QUESTIONS", "ADJUST_READY"].contains(&estado) {
        return Err(ApiError::bad_request(format!("Estado invalido: {estado}")));
    }
    estado_propuesta(&st, &id, "ADJUSTING").await;
    tokio::spawn(motor_ajuste(st.clone(), id.clone(), comentario));
    Ok(Json(json!({ "started": true, "status": "ADJUSTING", "proposalId": id })))
}

// ═══════════════════════════════ PLANIFICACIÓN ═══════════════════════════════
const AGENTES_PLAN: [(&str, &str); 5] = [
    ("atlas", "Eres Atlas, Operations Manager de ArchiTechIA.\nAnaliza la propuesta aprobada y define el alcance operativo:\n- Cuantos sprints necesita y de cuanto tiempo\n- Que areas del equipo deben involucrarse y por que\n- Que dependencias tecnicas existen\n- Riesgos operativos y como mitigarlos\nSé concreto. Propone nombres de sprints y areas responsables.\nResponde en prosa, 3-5 oraciones."),
    ("vesta", "Eres Vesta, Finance & Legal Lead de ArchiTechIA.\nAnaliza el plan operativo propuesto y evalua:\n- Distribucion de esfuerzo por area (alta/media/baja)\n- Si el alcance es financieramente razonable\n- Que tasks tienen mayor ROI y cuales son nice-to-have\n- Prioridades desde la perspectiva de costo-beneficio\nAjusta o valida lo propuesto por Atlas. Responde en prosa, 3-4 oraciones."),
    ("ares", "Eres Ares, Sales Lead de ArchiTechIA.\nDefine el angulo comercial del plan:\n- Que sprint o entregable genera valor visible para el cliente primero\n- Como se conecta esto con el pipeline actual\n- Que tasks de demos, presales o comunicacion comercial son necesarias\n- Propone tasks especificas con el area Sales & Presales\nResponde en prosa, 3-4 oraciones."),
    ("iris", "Eres Iris, Marketing & Brand Lead de ArchiTechIA.\nDefine el angulo de comunicacion del plan:\n- Como se comunica el lanzamiento de esta iniciativa (interno y externo)\n- Que tasks de marketing o documentacion son necesarias\n- En que sprint deberia incluirse el componente de comunicacion\n- Propone tasks concretas con el area Marketing & Brand\nResponde en prosa, 3-4 oraciones."),
    ("orion", "Eres Orion, CEO operacional de ArchiTechIA.\nCon los inputs de Atlas (operativo), Vesta (financiero), Ares (comercial) e Iris (marketing),\nsintetiza el plan de ejecucion definitivo como JSON.\nCRITICO: cada task DEBE tener areaSlug. Si el area no existe, proponla con prefix \"new:\".\nCRITICO: cada task DEBE tener un \"localId\" corto y unico dentro del plan (ej. \"s1-t1\").\nSi una task necesita el trabajo de OTRA task para poder arrancar (ej. \"construir el\nformulario\" necesita que \"crear el modelo en la base de datos\" ya este hecho), marcala\ncon \"dependsOnLocalId\" apuntando al localId de esa otra task — SOLO puede apuntar a una\ntask que ya aparecio antes en el plan, nunca a una futura. Si no depende de nada, null.\nNo inventes dependencias que no sean reales — la mayoria de las tasks no dependen de nada.\nResponde EXCLUSIVAMENTE con JSON valido sin markdown ni texto extra."),
];

const INSTRUCCION_ORION_PLAN: &str = r##"Con todo lo anterior, genera el JSON del plan de ejecucion definitivo.
Incluye: solucionId (o solucionPropuesta), epic, sprints con tasks.
Cada task DEBE tener areaSlug. Puedes proponer areas con "new:nombre-area".

DIMENSIONAMIENTO (solo si estas proponiendo una Solucion nueva, es decir
solucionPropuesta != null): solucionPropuesta DEBE incluir tambien un
campo "repositorio". Usa el literal "portal-architechia" si esto es un
modulo/feature que va a vivir DENTRO del portal ArchiTechIA (la mayoria de
los casos). Si de la conversacion o el contexto surge que esto es un
producto, demo o MVP independiente que deberia tener su propio repo y
desplegarse por separado, proponé un nombre de repo nuevo en kebab-case
(ej: "oficina-virtual-inbox") en vez de "portal-architechia". Si no está
claro, usá "portal-architechia" por default — no lo dejes vacío ni null.

Cada task DEBE tener tambien agentSlug asignado: buscá en AGENTES
DISPONIBLES el agente cuyo area coincida con el areaSlug de esa task (por
ejemplo, un task con areaSlug "infra" va con el agente cuya area sea
"infra", no importa si ese agente no participo del debate). Dejá
agentSlug en null UNICAMENTE si ningun agente de la lista tiene esa area.

GRANULARIDAD DE TASKS DE CODIGO (areaSlug dev/infra/qa/security/data — las
que va a ejecutar un agente escribiendo codigo real): el agente que ejecuta
estas tasks tiene un presupuesto acotado de pasos por tarea. Una task de
codigo NUNCA debe pedir tocar mas de 2-3 archivos o una sola pieza cohesiva
de funcionalidad. Si lo que describis requiere crear/modificar mas de eso
(ej. "integracion OAuth2 completa" = cliente+endpoints+UI son 3 piezas
distintas), DIVIDILA en varias tasks secuenciales mas chicas encadenadas
con dependsOnLocalId, cada una con su propio alcance acotado (ej. "Crear
cliente X", luego "Crear endpoints de X" dependiendo de la anterior, luego
"Integrar X en la UI" dependiendo de esa). Preferí 3 tasks chicas y
verificables a 1 sola task ancha que probablemente no termine.

Formato JSON exacto:
{"needsMoreInfo":false,"questions":[],"planRationale":"resumen del plan","solucionId":null,"solucionPropuesta":{"name":"...","description":"...","repositorio":"portal-architechia"},"epic":{"name":"...","description":"...","estimatedWeeks":4},"sprints":[{"name":"Sprint 1 - ...","goal":"...","areaSlug":"...","estimatedWeeks":2,"tasks":[{"localId":"s1-t1","dependsOnLocalId":null,"title":"...","description":"...","areaSlug":"...","agentSlug":"slug-del-agente-de-esa-area","priority":"HIGH","estimatedHours":8,"rationaleArea":"por que esta area"}]}]}"##;

async fn motor_plan(st: AppState, propuesta_id: String, comentario: Option<String>) {
    let sesion = format!("council-{propuesta_id}");
    let Ok(Some(propuesta)) = propuesta_json(&st, &propuesta_id).await else { return };
    let db = agentes_db(&st, &["orion", "atlas", "vesta", "ares", "iris"]).await;
    let soluciones = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'nombre', nombre, 'descripcion', descripcion) ORDER BY "createdAt" DESC), '[]'::jsonb) FROM (SELECT * FROM "Solucion" ORDER BY "createdAt" DESC LIMIT 15) x"#, &[]).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let areas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('name', name, 'slug', slug) ORDER BY name), '[]'::jsonb) FROM (SELECT * FROM "Area" ORDER BY name LIMIT 30) x"#, &[]).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let agentes = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('name', ag.name, 'slug', ag.slug, 'areaSlug', ar.slug)), '[]'::jsonb) FROM (SELECT * FROM "Agent" WHERE status = 'ACTIVE' LIMIT 20) ag LEFT JOIN "Area" ar ON ar.id = ag."areaId""#, &[]).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let lista_sol = if soluciones.is_empty() {
        "  (ninguna aun)".to_string()
    } else {
        soluciones.iter().map(|x| format!("  - ID:\"{}\" Nombre:\"{}\"{}", x["id"].as_str().unwrap_or(""), x["nombre"].as_str().unwrap_or(""), x["descripcion"].as_str().filter(|d| !d.is_empty()).map(|d| format!(" - {d}")).unwrap_or_default())).collect::<Vec<_>>().join("\n")
    };
    let lista_areas = if areas.is_empty() {
        "  (ninguna - proponer con \"new:nombre\")".to_string()
    } else {
        areas.iter().map(|a| format!("  - slug:\"{}\" Nombre:\"{}\"", a["slug"].as_str().unwrap_or(""), a["name"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n")
    };
    let lista_agentes = if agentes.is_empty() {
        "  (ninguno)".to_string()
    } else {
        agentes.iter().map(|a| format!("  - slug:\"{}\" Nombre:\"{}\" area:\"{}\"", a["slug"].as_str().unwrap_or(""), a["name"].as_str().unwrap_or(""), a["areaSlug"].as_str().unwrap_or("sin area"))).collect::<Vec<_>>().join("\n")
    };
    let votos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('n', "agentName", 'a', argument, 'v', vote) ORDER BY "createdAt"), '[]'::jsonb) FROM "AgentVote" WHERE "proposalId" = $1"#, &[B::T(propuesta_id.clone())]).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let resumen_votos = votos.iter().map(|v| format!("  {} ({}): {}", v["n"].as_str().unwrap_or(""), if v["v"] == true { "APROBO" } else { "RECHAZO" }, v["a"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n");
    let humano = comentario.as_ref().map(|c| format!("\n\nCOMENTARIO DEL USUARIO:\n{c}")).unwrap_or_default();
    let previo_plan = match propuesta.pointer("/metadata/councilPlan") {
        Some(p) if !p.is_null() => format!("\n\nPLAN PREVIO (refinar con el comentario del usuario):\n{}", serde_json::to_string_pretty(p).unwrap_or_default()),
        _ => String::new(),
    };
    let fijada = propuesta["solucionId"].as_str().filter(|x| !x.is_empty()).map(|sid| format!("\n\nSOLUCION YA DECIDIDA (fijada por un humano al extraer la propuesta, NO la cambies): solucionId = \"{sid}\"")).unwrap_or_default();
    let base = format!(
        "PROPUESTA APROBADA:\nTitulo: {}\nDescripcion: {}\n\nDEBATE DE VOTACION (por que fue aprobada):\n{}\n{}\n{}\n{}\n\nSOLUCIONES EXISTENTES:\n{}\n\nAREAS DISPONIBLES (usar estos slugs en las tasks):\n{}\n\nAGENTES DISPONIBLES:\n{}",
        propuesta["title"].as_str().unwrap_or(""),
        propuesta["description"].as_str().unwrap_or("Sin descripcion"),
        if resumen_votos.is_empty() { "  (no disponible)".to_string() } else { resumen_votos },
        humano,
        previo_plan,
        fijada,
        lista_sol,
        lista_areas,
        lista_agentes
    );
    let _ = exec(&st.pool, r#"DELETE FROM "DebateMessage" WHERE "proposalId" = $1 AND round = $2::int"#, &[B::T(propuesta_id.clone()), B::I(RONDA_PLAN)]).await;

    let mut historial: Vec<String> = vec![];
    let mut plan_final: Option<Value> = None;
    for (slug, prompt) in AGENTES_PLAN {
        let d = db.iter().find(|a| a["slug"].as_str() == Some(slug));
        let id = d.and_then(|a| a["id"].as_str()).map(String::from).unwrap_or_else(|| format!("agent_{slug}_001"));
        let nombre = capitalizar(slug);
        let modelo = d.and_then(|a| a["llmModel"].as_str()).map(String::from);
        let previo = if historial.is_empty() { String::new() } else { format!("\n\nDEBATE DE PLANIFICACION HASTA AHORA:\n{}", historial.join("\n\n")) };
        let mensaje = if slug == "orion" {
            format!("{base}{previo}\n\n{INSTRUCCION_ORION_PLAN}")
        } else {
            format!("{base}{previo}\n\n{prompt}\n\nEnfocate en tu area de expertise. Sé especifico con nombres de areas y tasks.")
        };
        match llm(&st, prompt, &mensaje, modelo.as_deref(), 4096, 120, &sesion).await {
            Ok(respuesta) => {
                if slug == "orion" {
                    if let Some(j) = primer_json(&respuesta) {
                        if let Ok(mut p) = serde_json::from_str::<Value>(j) {
                            // La Solución decidida por un humano manda siempre.
                            if let Some(sid) = propuesta["solucionId"].as_str().filter(|x| !x.is_empty()) {
                                p["solucionId"] = json!(sid);
                                p["solucionPropuesta"] = Value::Null;
                            }
                            plan_final = Some(p);
                        }
                    }
                    let prosa = RE_JSON_BLOQUE.replace(&respuesta, "").trim().to_string();
                    let prosa = if prosa.is_empty() { "Plan sintetizado y definido.".to_string() } else { prosa };
                    let _ = insertar_mensaje(&st, &propuesta_id, &id, &format!("{nombre} (Planificacion)"), slug, &prosa, RONDA_PLAN).await;
                } else {
                    let _ = insertar_mensaje(&st, &propuesta_id, &id, &format!("{nombre} (Planificacion)"), slug, &respuesta, RONDA_PLAN).await;
                    historial.push(format!("{nombre}: {respuesta}"));
                }
            }
            Err(e) => tracing::error!("[PlanningEngine] Error with {slug}: {e}"),
        }
    }
    match plan_final {
        Some(p) if p["needsMoreInfo"].as_bool().unwrap_or(false) && p["questions"].as_array().map(|q| !q.is_empty()).unwrap_or(false) => {
            metadata_y_estado(&st, &propuesta_id, "PLAN_QUESTIONS", json!({ "councilQuestions": p["questions"], "councilPlanDraft": p })).await
        }
        Some(p) => metadata_y_estado(&st, &propuesta_id, "PLAN_READY", json!({ "councilPlan": p })).await,
        None => metadata_y_estado(&st, &propuesta_id, "PLAN_READY", json!({ "planError": "No se pudo generar plan estructurado — revisa los logs del servidor." })).await,
    }
}

async fn plan_iniciar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let comentario = s(&body, "humanComment");
    let Some(p) = propuesta_json(&st, &id).await? else { return Err(ApiError::not_found("Propuesta no encontrada")) };
    let estado = p["status"].as_str().unwrap_or("");
    if !["APPROVED", "PLANNING", "PLAN_QUESTIONS", "PLAN_READY", "ADJUST_READY"].contains(&estado) {
        return Err(ApiError::bad_request(format!("Estado invalido: {estado}")));
    }
    estado_propuesta(&st, &id, "PLANNING").await;
    tokio::spawn(motor_plan(st.clone(), id.clone(), comentario));
    Ok(Json(json!({ "started": true, "status": "PLANNING", "proposalId": id })))
}

// ═══════════════════════════════ NEGOCIACIÓN ═══════════════════════════════
const AGENTES_NEGOCIACION: [(&str, &str, &str, &str); 4] = [
    ("agent_ares_001", "Ares", "ares", "Sales & Presales Lead. Evaluás impacto comercial, ingresos y protección de clientes."),
    ("agent_atlas_001", "Atlas", "atlas", "Operations Manager. Evaluás ejecutabilidad, recursos disponibles y timelines realistas."),
    ("agent_iris_001", "Iris", "iris", "Marketing & Brand Lead. Evaluás coherencia de marca y posicionamiento externo."),
    ("agent_vesta_001", "Vesta", "vesta", "Finance & Legal Lead. Evaluás viabilidad financiera, ROI y riesgos legales."),
];

const ORION_APERTURA: &str = "Eres Orión, CEO y orquestador del Consejo de ArchiTechIA.\nEl consejo no alcanzó consenso en el debate previo. Tu rol ahora es ABRIR la negociación:\n1. Resumí las principales objeciones de cada miembro\n2. Proponé ajustes concretos a la propuesta que podrían resolver esas objeciones\n3. Invitá a cada miembro a responder\n\nTono: directo, constructivo, orientado a consenso. No sos el que decide — sos el facilitador.\nRespondé en 3-5 oraciones.";

fn sistema_negociacion(nombre: &str, rol: &str) -> String {
    format!("Eres {nombre} del Consejo de ArchiTechIA. Rol: {rol}\nOrión acaba de proponer ajustes para desbloquear el consenso en una propuesta escalada.\nTu tarea: respondé concretamente —\n- Qué ajustes de Orión aceptás\n- Qué ajuste adicional específico (1 como máximo) necesitás vos para aprobar\n- Si los ajustes ya cubren tus preocupaciones, decí que aprobás con los cambios propuestos\n\nSé conciso (2-4 oraciones). No repitas el debate anterior — solo tu posición actual.")
}

fn cierre_orion() -> String {
    format!(
        "Eres Orión, CEO del Consejo de ArchiTechIA.\nLeíste las respuestas de todos los miembros del consejo en la negociación.\nTu tarea: CERRAR el debate con el plan final consensuado.\n\nPrimero escribí 2 oraciones declarando el consenso alcanzado y los ajustes incorporados.\nLuego respondé con el siguiente JSON (sin markdown):\n\n{}\n\nReglas: 1 épica, 2-4 sprints de 2 semanas, 2-5 tasks por sprint. Hoy es {}.",
        r#"{
  "epic": {
    "name": "nombre de la épica",
    "description": "qué resuelve y por qué importa",
    "startDate": "YYYY-MM-DD",
    "endDate": "YYYY-MM-DD"
  },
  "sprints": [
    {
      "name": "Sprint N — nombre",
      "goal": "objetivo concreto",
      "startDate": "YYYY-MM-DD",
      "endDate": "YYYY-MM-DD",
      "areaSlug": "dev|data|infra|qa|sales|operations|finance|marketing|people|delivery|security",
      "tasks": [
        {
          "title": "título accionable",
          "description": "qué hacer",
          "priority": "LOW|MEDIUM|HIGH|CRITICAL",
          "areaSlug": "...",
          "assigneeName": null
        }
      ]
    }
  ]
}"#,
        hoy()
    )
}

async fn motor_negociacion(st: AppState, id: String) {
    let sesion = format!("council-{id}");
    let r: Result<(), String> = async {
        let Some(propuesta) = propuesta_json(&st, &id).await.map_err(err_s)? else { return Ok(()) };
        let historial = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('n', "agentName", 'c', content, 'r', round) ORDER BY round, "createdAt"), '[]'::jsonb) FROM "DebateMessage" WHERE "proposalId" = $1"#, &[B::T(id.clone())]).await.map_err(err_s)?;
        let historial = historial.as_array().cloned().unwrap_or_default();
        let votos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('n', "agentName", 'v', vote, 'a', argument, 'w', weight) ORDER BY round, "createdAt"), '[]'::jsonb) FROM "AgentVote" WHERE "proposalId" = $1"#, &[B::T(id.clone())]).await.map_err(err_s)?;
        let votos = votos.as_array().cloned().unwrap_or_default();
        let max_ronda = historial.iter().filter_map(|m| m["r"].as_i64()).max().unwrap_or(0);
        let ronda = max_ronda + 1;
        let resumen_debate = historial.iter().map(|m| format!("[R{}] {}: {}", m["r"], m["n"].as_str().unwrap_or(""), m["c"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n");
        let resumen_votos = votos
            .iter()
            .map(|v| format!("{} (x{}): {} — {}", v["n"].as_str().unwrap_or(""), v["w"], if v["v"] == true { "APROBÓ" } else { "RECHAZÓ" }, v["a"].as_str().unwrap_or("null")))
            .collect::<Vec<_>>()
            .join("\n");
        let items = items_de(&propuesta).iter().map(|i| format!("- {}", i["title"].as_str().unwrap_or("undefined"))).collect::<Vec<_>>().join("\n");
        let contexto = [
            format!("PROPUESTA: {}", propuesta["title"].as_str().unwrap_or("")),
            propuesta["description"].as_str().unwrap_or("").to_string(),
            format!("Items: {items}"),
            String::new(),
            "DEBATE PREVIO:".into(),
            resumen_debate,
            String::new(),
            "VOTOS:".into(),
            resumen_votos,
        ]
        .join("\n");

        let apertura = llm(&st, ORION_APERTURA, &contexto, None, 4096, 90, &sesion).await?;
        insertar_mensaje(&st, &id, "agent_orion_001", "Orión", "orion", &apertura, ronda).await.map_err(err_s)?;
        let mut respuestas: Vec<String> = vec![];
        for (aid, nombre, slug, rol) in AGENTES_NEGOCIACION {
            let prompt = format!("{contexto}\n\nPROPUESTA DE AJUSTES DE ORIÓN:\n{apertura}");
            match llm(&st, &sistema_negociacion(nombre, rol), &prompt, None, 4096, 90, &sesion).await {
                Ok(resp) => {
                    insertar_mensaje(&st, &id, aid, nombre, slug, &resp, ronda).await.map_err(err_s)?;
                    respuestas.push(format!("{nombre}: {resp}"));
                }
                Err(e) => tracing::error!("[negotiate] {nombre} failed: {e}"),
            }
        }
        let cierre_prompt = [
            contexto.clone(),
            String::new(),
            "PROPUESTA DE AJUSTES DE ORIÓN:".into(),
            apertura.clone(),
            String::new(),
            "RESPUESTAS DEL CONSEJO:".into(),
            respuestas.join("\n\n"),
            String::new(),
            "Cerrá la negociación con el plan final consensuado.".into(),
        ]
        .join("\n");
        let cierre = llm(&st, &cierre_orion(), &cierre_prompt, None, 4096, 90, &sesion).await?;
        insertar_mensaje(&st, &id, "agent_orion_001", "Orión", "orion", &cierre, ronda).await.map_err(err_s)?;
        let plan = primer_json(&cierre).and_then(|j| serde_json::from_str::<Value>(j).ok());
        let mut meta = propuesta["metadata"].as_object().cloned().unwrap_or_default();
        meta.insert("negotiatedPlan".into(), plan.unwrap_or(Value::Null));
        exec(&st.pool, r#"UPDATE "CouncilProposal" SET status = 'ESCALATED', metadata = $2::jsonb, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(id.clone()), B::T(Value::Object(meta).to_string())]).await.map_err(err_s)?;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::error!("[negotiate] runNegotiation failed: {e}");
        estado_propuesta(&st, &id, "ESCALATED").await;
    }
}

fn err_s(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn negociar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Response> {
    let Some(p) = propuesta_json(&st, &id).await? else { return Err(ApiError::not_found("Not found")) };
    if p["status"].as_str() == Some("DEBATING") {
        return Ok((StatusCode::ACCEPTED, Json(json!({ "status": "DEBATING", "message": "Negociación ya en curso" }))).into_response());
    }
    estado_propuesta(&st, &id, "DEBATING").await;
    tokio::spawn(motor_negociacion(st.clone(), id));
    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "DEBATING" }))).into_response())
}

// ═══════════════════════════════ SÍNTESIS FINAL ═══════════════════════════════
fn sistema_sintesis() -> String {
    format!(
        "Eres el Consejo de ArchiTechIA en modo síntesis. Analizaste una propuesta en múltiples rondas y ahora debés definir el plan de ejecución final.\n\nTu tarea: basándote en el debate completo, producir una estructura de trabajo CONCRETA y EJECUTABLE.\n\nResponde SOLO con este JSON (sin markdown):\n\n{}\n\nReglas:\n- 1 épica, 2-4 sprints, 2-5 tasks por sprint\n- Sprints de 2 semanas cada uno, comenzando desde hoy\n- assigneeName: solo si el contexto del debate menciona personas concretas, si no null\n- Responde SOLO con el JSON",
        r#"{
  "epic": {
    "name": "nombre de la épica (objetivo de negocio)",
    "description": "qué resuelve y por qué importa",
    "startDate": "YYYY-MM-DD",
    "endDate": "YYYY-MM-DD"
  },
  "sprints": [
    {
      "name": "Sprint 1 — nombre descriptivo",
      "goal": "objetivo concreto y medible",
      "startDate": "YYYY-MM-DD",
      "endDate": "YYYY-MM-DD",
      "areaSlug": "dev | data | infra | qa | sales | operations | finance | marketing | people | delivery | security",
      "tasks": [
        {
          "title": "título accionable",
          "description": "qué hay que hacer exactamente",
          "priority": "LOW | MEDIUM | HIGH | CRITICAL",
          "areaSlug": "dev | data | infra | qa | sales | operations | finance | marketing | people | delivery | security",
          "assigneeName": "nombre sugerido o null"
        }
      ]
    }
  ]
}"#
    )
}

async fn finalizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Response> {
    let Some(p) = propuesta_json(&st, &id).await? else { return Err(ApiError::not_found("Not found")) };
    let mensajes = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('n', "agentName", 'c', content, 'r', round) ORDER BY round, "createdAt"), '[]'::jsonb) FROM "DebateMessage" WHERE "proposalId" = $1"#, &[B::T(id.clone())]).await?;
    let votos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('n', "agentName", 'v', vote, 'a', argument, 'w', weight) ORDER BY round, "createdAt"), '[]'::jsonb) FROM "AgentVote" WHERE "proposalId" = $1"#, &[B::T(id.clone())]).await?;
    let debate = mensajes.as_array().cloned().unwrap_or_default().iter().map(|m| format!("[Ronda {}] {}: {}", m["r"], m["n"].as_str().unwrap_or(""), m["c"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n");
    let votos_txt = votos.as_array().cloned().unwrap_or_default().iter().map(|v| format!("{} (×{}): {} — {}", v["n"].as_str().unwrap_or(""), v["w"], if v["v"] == true { "APROBÓ" } else { "RECHAZÓ" }, v["a"].as_str().unwrap_or("null"))).collect::<Vec<_>>().join("\n");
    let items = items_de(&p).iter().map(|i| format!("- {}: {}", i["type"].as_str().unwrap_or("undefined"), i["title"].as_str().unwrap_or("undefined"))).collect::<Vec<_>>().join("\n");
    let prompt = format!(
        "PROPUESTA:\nTítulo: {}\nDescripción: {}\nItems originales:\n{}\n\nDEBATE DEL CONSEJO:\n{}\n\nVOTOS FINALES:\n{}\n\nFecha de hoy: {}\n\nBasándote en todo el debate, definí el plan de ejecución final con Épica, Sprints y Tasks concretas.",
        p["title"].as_str().unwrap_or(""),
        p["description"].as_str().unwrap_or(""),
        items,
        debate,
        votos_txt,
        hoy()
    );
    let raw = llm(&st, &sistema_sintesis(), &prompt, None, 4096, 90, &format!("council-{id}")).await.map_err(err500)?;
    let Some(j) = primer_json(&raw) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "No se pudo sintetizar", "raw": raw }))).into_response());
    };
    match serde_json::from_str::<Value>(j) {
        Ok(v) => Ok(Json(v).into_response()),
        Err(_) => Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "JSON inválido", "raw": raw }))).into_response()),
    }
}

// ═══════════════════════════════ APROBAR PLAN → BACKLOG ═══════════════════════════════
async fn resolver_area(st: &AppState, slug: &str) -> Result<Option<String>, sqlx::Error> {
    if let Some(resto) = slug.strip_prefix("new:") {
        let nuevo = resto.trim().split_whitespace().collect::<Vec<_>>().join("-").to_lowercase();
        if let Some(id) = fetch_text_opt(&st.pool, r#"SELECT id FROM "Area" WHERE slug = $1 LIMIT 1"#, &[B::T(nuevo.clone())]).await? {
            return Ok(Some(id));
        }
        let nombre = capitalizar(&nuevo);
        // En Next esta inserción no pasaba id ni updatedAt (obligatorios) y fallaba: acá sí.
        return fetch_text_opt(
            &st.pool,
            r#"INSERT INTO "Area" (id, name, slug, color, "updatedAt") VALUES ($1, $2, $3, '#6366f1', NOW()) RETURNING id"#,
            &[B::T(new_id()), B::T(nombre), B::T(nuevo)],
        )
        .await;
    }
    if slug.is_empty() {
        return Ok(None);
    }
    fetch_text_opt(&st.pool, r#"SELECT id FROM "Area" WHERE slug = $1 LIMIT 1"#, &[B::T(slug.into())]).await
}

async fn plan_aprobar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Response> {
    let Some(p) = propuesta_json(&st, &id).await? else { return Err(ApiError::not_found("Propuesta no encontrada")) };
    if p["status"].as_str() != Some("PLAN_READY") {
        return Err(ApiError::bad_request(format!("Plan no listo (status: {})", p["status"].as_str().unwrap_or(""))));
    }
    let plan = p.pointer("/metadata/councilPlan").cloned().filter(|x| !x.is_null());
    let Some(plan) = plan else { return Err(ApiError::bad_request("No hay plan en metadata")) };

    let r: Result<Value, String> = async {
        let mut creados_epics: Vec<String> = vec![];
        let mut creados_sprints: Vec<String> = vec![];
        let mut tareas_total: i64 = 0;
        let mut todas: Vec<String> = vec![];
        let mut local_a_real: HashMap<String, String> = HashMap::new();

        let mut solucion_id: Option<String> = s(&plan, "solucionId").filter(|x| !x.is_empty());
        if solucion_id.is_none() {
            if let Some(nombre) = plan.pointer("/solucionPropuesta/name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()) {
                let codigo = codigo_solucion_unico(&st, &generar_codigo_solucion(nombre)).await.map_err(err_s)?;
                let sp = &plan["solucionPropuesta"];
                solucion_id = fetch_text_opt(
                    &st.pool,
                    r#"INSERT INTO "Solucion" (id, nombre, descripcion, estado, tipo, repositorio, "solucionCode", "createdAt", "updatedAt")
                       VALUES ($1, $2, $3, 'ACTIVO', 'PRODUCT', $4, $5, NOW(), NOW()) RETURNING id"#,
                    &[B::T(new_id_uuid()), B::T(nombre.into()), B::OT(s(sp, "description")), B::T(s(sp, "repositorio").unwrap_or_else(|| "portal-architechia".into())), B::T(codigo)],
                )
                .await
                .map_err(err_s)?;
            }
        }

        let mut epic_id: Option<String> = None;
        if let Some(nombre) = plan.pointer("/epic/name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()) {
            epic_id = fetch_text_opt(
                &st.pool,
                r#"INSERT INTO "Epic" (id, name, description, status, priority, "solucionId", "updatedAt") VALUES (gen_random_uuid()::text, $1, $2, 'ACTIVE', 'HIGH', $3, NOW()) RETURNING id"#,
                &[B::T(nombre.into()), B::OT(s(&plan["epic"], "description")), B::OT(solucion_id.clone())],
            )
            .await
            .map_err(err_s)?;
            if let Some(e) = &epic_id {
                creados_epics.push(e.clone());
            }
        }

        for sp in plan["sprints"].as_array().cloned().unwrap_or_default() {
            let area_slug = s(&sp, "areaSlug").unwrap_or_default();
            let area_id = resolver_area(&st, &area_slug).await.map_err(err_s)?;

            let mut prefijo = "SP".to_string();
            if let Some(sid) = &solucion_id {
                if let Some(c) = fetch_text_opt(&st.pool, r#"SELECT "solucionCode" FROM "Solucion" WHERE id = $1 LIMIT 1"#, &[B::T(sid.clone())]).await.map_err(err_s)?.filter(|c| !c.is_empty()) {
                    prefijo = c;
                }
            }
            let mut num_epica = "0000".to_string();
            if let (Some(eid), Some(sid)) = (&epic_id, &solucion_id) {
                let idx = fetch_i64(
                    &st.pool,
                    r#"SELECT COALESCE((SELECT rn FROM (SELECT id, row_number() OVER (ORDER BY "createdAt") rn FROM "Epic" WHERE "solucionId" = $1) t WHERE id = $2), 0)::bigint"#,
                    &[B::T(sid.clone()), B::T(eid.clone())],
                )
                .await
                .map_err(err_s)?;
                if idx > 0 {
                    num_epica = format!("{idx:04}");
                }
            }
            let cuenta = match &solucion_id {
                Some(sid) => fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "Sprint" WHERE "solucionId" = $1"#, &[B::T(sid.clone())]).await.map_err(err_s)?,
                None => 0,
            };
            let codigo_sprint = format!("{prefijo}-{num_epica}-{:04}", cuenta + 1);

            let sprint_id = fetch_text_opt(
                &st.pool,
                r#"INSERT INTO "Sprint" (id, name, goal, status, "epicId", "solucionId", "ownerAreaId", "responsibleId", "responsibleName", "sprintCode")
                   VALUES (gen_random_uuid()::text, $1, $2, 'PLANNED', $3, $4, $5, 'agent_orion_001', 'Consejo', $6) RETURNING id"#,
                &[B::T(s(&sp, "name").unwrap_or_else(|| "Sprint".into())), B::OT(s(&sp, "goal")), B::OT(epic_id.clone()), B::OT(solucion_id.clone()), B::OT(area_id.clone()), B::T(codigo_sprint.clone())],
            )
            .await
            .map_err(err_s)?;
            if let Some(sid) = &sprint_id {
                creados_sprints.push(sid.clone());
            }

            for task in sp["tasks"].as_array().cloned().unwrap_or_default() {
                let mut area_tarea = area_id.clone();
                let slug_tarea = s(&task, "areaSlug").unwrap_or_default();
                if !slug_tarea.is_empty() && slug_tarea != area_slug {
                    area_tarea = resolver_area(&st, &slug_tarea).await.map_err(err_s)?;
                }
                let (mut agente_id, mut agente_nombre) = (None, None);
                if let Some(slug) = s(&task, "agentSlug").filter(|x| !x.is_empty() && !x.starts_with("new:")) {
                    if let Some(a) = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('id', id, 'name', name) FROM "Agent" WHERE slug = $1 LIMIT 1"#, &[B::T(slug)]).await.map_err(err_s)? {
                        agente_id = a["id"].as_str().map(String::from);
                        agente_nombre = a["name"].as_str().map(String::from);
                    }
                }
                let depende = s(&task, "dependsOnLocalId").and_then(|l| local_a_real.get(&l).cloned());
                let nueva_id = new_id_uuid();
                exec(
                    &st.pool,
                    r#"INSERT INTO "BacklogItem" (id, title, description, status, priority, type, "areaId", "sprintId", "solucionId", "createdByAgentId", "createdByAgentName",
                                                  "assigneeId", "assigneeName", "updatedAt", "taskCode", "dependsOnTaskId")
                       VALUES ($1, $2, $3, 'BACKLOG', $4, 'TASK', $5, $6, $7, 'agent_orion_001', 'Consejo', $8, $9, NOW(), $10, $11)"#,
                    &[
                        B::T(nueva_id.clone()),
                        B::T(s(&task, "title").unwrap_or_else(|| "Task".into())),
                        B::OT(s(&task, "description").or_else(|| s(&task, "rationaleArea"))),
                        B::T(s(&task, "priority").unwrap_or_else(|| "MEDIUM".into())),
                        B::OT(area_tarea),
                        B::OT(sprint_id.clone()),
                        B::OT(solucion_id.clone()),
                        B::OT(agente_id),
                        B::OT(agente_nombre),
                        B::T(format!("{codigo_sprint}-{:03}", tareas_total + 1)),
                        B::OT(depende),
                    ],
                )
                .await
                .map_err(err_s)?;
                if let Some(l) = s(&task, "localId").filter(|x| !x.is_empty()) {
                    local_a_real.insert(l, nueva_id.clone());
                }
                todas.push(nueva_id);
                tareas_total += 1;
            }
        }

        let creados = json!({ "epics": creados_epics, "sprints": creados_sprints, "tasks": tareas_total });
        metadata_y_estado(&st, &id, "EXECUTING", json!({ "executionResult": creados, "solucionId": solucion_id, "epicId": epic_id })).await;

        // Auto-despacho: el grafo de tareas lo corre el servicio privilegiado `portalhub-motor` (executor/dispatch-chain).
        if !todas.is_empty() {
            let st2 = st.clone();
            let ids = todas.clone();
            let propuesta = id.clone();
            tokio::spawn(async move {
                let r = st2
                    .http
                    .post("http://127.0.0.1:3101/api/executor/dispatch-chain")
                    .header("x-api-key", st2.cfg.internal_api_key.clone().unwrap_or_default())
                    .timeout(Duration::from_secs(6 * 3600))
                    .json(&json!({ "taskIds": ids }))
                    .send()
                    .await;
                if let Err(e) = r {
                    tracing::error!("[Plan/approve] Error en auto-dispatch de la propuesta {propuesta}: {e}");
                }
            });
        }
        Ok(json!({ "ok": true, "created": creados, "epicId": epic_id, "solucionId": solucion_id, "autoDispatched": todas.len() }))
    }
    .await;
    match r {
        Ok(v) => Ok(Json(v).into_response()),
        Err(e) => {
            tracing::error!("[Plan/approve] Error: {e}");
            Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))).into_response())
        }
    }
}

async fn crear_backlog(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let area = |slug: &str| -> Option<&'static str> {
        Some(match slug {
            "dev" => "947ca771-fe9e-4c3f-bfea-2ef2e27986c6",
            "data" => "698bcc5e-08ba-49bb-a872-ffac31f0e5c9",
            "infra" => "74b21d1d-0954-4757-a1fd-0fabed1e9e3a",
            "qa" => "3695ed86-da91-4327-bdde-b14cfa8a10b5",
            "sales" => "8df9b7a1-9650-4ec2-8240-f0bb350eb97f",
            "operations" => "53999e08-ce6a-4615-82ea-eca49fe33103",
            "finance" => "edd4e3af-76a8-441c-a498-e919da3e7574",
            "marketing" => "74b21d1d-0954-4757-a1fd-0fabed1e9e3a",
            "people" => "9ab2cc55-3888-4cd9-9418-4eca6286a0b6",
            "delivery" => "7b997ca4-1eb1-4684-898b-9e9c860e079e",
            "security" => "195bed20-8d96-41fa-8672-8f2e9892f264",
            _ => return None,
        })
    };
    let epic = body.get("epic").cloned().unwrap_or(Value::Null);
    let solucion_id = s(&body, "solucionId");
    let fecha = |v: &Value, k: &str| s(v, k).filter(|x| !x.is_empty());
    // En Next estas inserciones no pasaban id (obligatorio) y fallaban; acá sí.
    let epic_id = fetch_text_opt(
        &st.pool,
        r#"INSERT INTO "Epic" (id, name, description, status, priority, color, "startDate", "endDate", "solucionId", "createdAt", "updatedAt")
           VALUES ($1, $2, $3, 'ACTIVE', 'HIGH', '#6366f1', $4::text::timestamptz AT TIME ZONE 'UTC', $5::text::timestamptz AT TIME ZONE 'UTC', $6, NOW(), NOW()) RETURNING id"#,
        &[B::T(new_id()), B::OT(s(&epic, "name")), B::OT(s(&epic, "description")), B::OT(fecha(&epic, "startDate")), B::OT(fecha(&epic, "endDate")), B::OT(solucion_id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::internal("Error interno"))?;
    let mut sprints_creados = vec![];
    for sp in body["sprints"].as_array().cloned().unwrap_or_default() {
        let area_sprint = s(&sp, "areaSlug").and_then(|a| area(&a)).map(String::from);
        let sprint_id = fetch_text_opt(
            &st.pool,
            r#"INSERT INTO "Sprint" (id, name, goal, status, "startDate", "endDate", "epicId", "ownerAreaId", "responsibleName", "createdAt")
               VALUES ($1, $2, $3, 'PLANNED', $4::text::timestamptz AT TIME ZONE 'UTC', $5::text::timestamptz AT TIME ZONE 'UTC', $6, $7, 'Orión', NOW()) RETURNING id"#,
            &[B::T(new_id()), B::OT(s(&sp, "name")), B::OT(s(&sp, "goal")), B::OT(fecha(&sp, "startDate")), B::OT(fecha(&sp, "endDate")), B::T(epic_id.clone()), B::OT(area_sprint.clone())],
        )
        .await?
        .ok_or_else(|| ApiError::internal("Error interno"))?;
        let tareas = sp["tasks"].as_array().cloned().unwrap_or_default();
        for t in &tareas {
            let area_tarea = s(t, "areaSlug").and_then(|a| area(&a)).map(String::from).or_else(|| area_sprint.clone());
            exec(
                &st.pool,
                r#"INSERT INTO "BacklogItem" (id, title, description, type, priority, status, "sprintId", "areaId", "assigneeName", "solucionId", "createdAt", "updatedAt")
                   VALUES ($1, $2, $3, 'TASK', $4, 'BACKLOG', $5, $6, $7, $8, NOW(), NOW())"#,
                &[B::T(new_id()), B::OT(s(t, "title")), B::OT(s(t, "description")), B::T(s(t, "priority").unwrap_or_else(|| "MEDIUM".into())), B::T(sprint_id.clone()), B::OT(area_tarea), B::OT(s(t, "assigneeName")), B::OT(solucion_id.clone())],
            )
            .await?;
        }
        sprints_creados.push(json!({ "id": sprint_id, "name": sp["name"], "taskCount": tareas.len() }));
    }
    estado_propuesta(&st, &id, "APPROVED").await;
    Ok(Json(json!({ "epicId": epic_id, "sprints": sprints_creados, "approved": true })))
}

// ═══════════════════════════════ CHAT Y EXTRACCIÓN ═══════════════════════════════
const SISTEMA_CHAT: &str = "Eres Orión, el agente estratégico central de ArchiTechIA. En este canal, conversas directamente con un socio o directivo de la empresa.\n\nTu función aquí es escuchar, entender y estructurar ideas de proyectos, iniciativas o mejoras que el socio quiere proponer. Haces preguntas clarificadoras cuando es necesario: ¿cuál es el objetivo?, ¿qué área lo ejecutaría?, ¿qué tareas concretas implica?, ¿qué prioridad tiene?\n\nSi la iniciativa implica desarrollar software nuevo, siempre preguntá también el dimensionamiento: ¿esto es un módulo/feature que vive DENTRO del portal ArchiTechIA (portal-architechia), o es un producto, demo o MVP independiente que debería vivir en su propio repositorio y desplegarse por separado? Esta decisión determina dónde termina viviendo el código, así que no la asumas sin preguntar salvo que sea obviamente una mejora al portal mismo.\n\nCuando el socio haya terminado de describir la iniciativa, le ofreces extraer la propuesta formal. Eres conciso, estratégico y hablas con autoridad. No te extiendes innecesariamente. Máximo 3-4 oraciones por respuesta.";

async fn chat(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mensajes = body.get("messages").and_then(|m| m.as_array()).filter(|m| !m.is_empty()).ok_or_else(|| ApiError::bad_request("messages requerido"))?;
    let previos: Vec<String> = mensajes[..mensajes.len() - 1]
        .iter()
        .map(|m| format!("{}: {}", if m["role"] == "user" { "Socio" } else { "Orión" }, m["content"].as_str().unwrap_or("")))
        .collect();
    let ultimo = mensajes.last().and_then(|m| m["content"].as_str()).unwrap_or("");
    let ctx = if previos.is_empty() { "El socio abre la conversación con:".to_string() } else { format!("Conversación previa:\n{}\n\nResponde al siguiente mensaje del socio:", previos.join("\n")) };
    let prompt = format!("{ctx}\nSocio: {ultimo}");
    let r = llm(&st, SISTEMA_CHAT, &prompt, None, 2048, 60, "council-chat").await;
    match r {
        Ok(reply) => Ok(Json(json!({ "reply": reply }))),
        Err(e) => {
            let corto = e.strip_prefix("OpenCode API error ").and_then(|x| x.split(':').next()).map(|c| format!("OpenCode API error {c}")).unwrap_or(e);
            Err(ApiError::internal(corto))
        }
    }
}

fn sistema_extraccion(soluciones: &str) -> String {
    format!(
        "Eres Orión, extractor estructurado de propuestas para el consejo de ArchiTechIA.\n\nTu tarea: analizar una conversación y extraer una propuesta formal con jerarquía Épica → Sprints → Tasks, y sugerir a qué Solución pertenece.\n\nResponde SOLO con este JSON exacto (sin markdown, sin explicaciones):\n\n{}\n\nSOLUCIONES EXISTENTES (usar su ID exacto en solucionId si la propuesta pertenece a una de estas; si no encaja en ninguna, proponé una nueva en solucionPropuesta):\n{soluciones}\n\nReglas:\n- Siempre 1 sola épica (el objetivo de negocio principal)\n- Entre 2 y 4 sprints (nunca más)\n- Entre 2 y 5 tasks por sprint (concretas y accionables, no ideas vagas)\n- Si algo no está claro, inferí razonablemente desde el contexto\n- No inventes un solucionId que no esté en la lista\n- Si estás creando una Solución nueva (solucionPropuesta), definí siempre \"repositorio\":\n  usá el literal \"portal-architechia\" si de la conversación se desprende que esto es un\n  módulo/feature que vive DENTRO del portal; si el socio indicó (o la conversación deja\n  claro) que es un producto, demo o MVP independiente, proponé un nombre de repo nuevo en\n  kebab-case (ej: \"oficina-virtual-inbox\"). Si la conversación no lo aclaró, usá\n  \"portal-architechia\" como default, no lo dejes vacío.\n- Responde SOLO con el JSON",
        r#"{
  "title": "título conciso de la propuesta (max 80 chars)",
  "description": "descripción ejecutiva de 2-3 oraciones: contexto, problema y objetivo",
  "solucionSugerida": {
    "solucionId": "ID exacto de una solución existente de la lista, o null si ninguna aplica",
    "solucionPropuesta": { "name": "nombre corto de la solución nueva", "description": "qué es", "repositorio": "portal-architechia | nombre-sugerido-para-repo-nuevo" } o null si usaste solucionId
  },
  "epic": {
    "name": "nombre de la épica (objetivo de negocio principal)",
    "description": "qué resuelve esta épica y por qué importa"
  },
  "sprints": [
    {
      "name": "Sprint 1 — nombre descriptivo",
      "goal": "objetivo concreto y medible de este sprint",
      "tasks": [
        {
          "title": "título de la tarea",
          "description": "qué hay que hacer exactamente",
          "areaSlug": "dev | data | infra | qa | sales | operations | finance | marketing | people | delivery | security",
          "priority": "LOW | MEDIUM | HIGH | CRITICAL"
        }
      ]
    }
  ]
}"#
    )
}

async fn chat_extraer(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Response> {
    let mensajes = body.get("messages").and_then(|m| m.as_array()).filter(|m| !m.is_empty()).ok_or_else(|| ApiError::bad_request("messages requerido"))?;
    let sols = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'nombre', nombre, 'descripcion', descripcion) ORDER BY "createdAt" DESC), '[]'::jsonb) FROM (SELECT * FROM "Solucion" ORDER BY "createdAt" DESC LIMIT 20) x"#, &[]).await?;
    let sols = sols.as_array().cloned().unwrap_or_default();
    let lista = if sols.is_empty() {
        "  (ninguna aún — proponé siempre una nueva)".to_string()
    } else {
        sols.iter().map(|x| format!("  - ID:\"{}\" Nombre:\"{}\"{}", x["id"].as_str().unwrap_or(""), x["nombre"].as_str().unwrap_or(""), x["descripcion"].as_str().filter(|d| !d.is_empty()).map(|d| format!(" - {d}")).unwrap_or_default())).collect::<Vec<_>>().join("\n")
    };
    let transcripcion = mensajes.iter().map(|m| format!("{}: {}", if m["role"] == "user" { "Socio" } else { "Orión" }, m["content"].as_str().unwrap_or("undefined"))).collect::<Vec<_>>().join("\n");
    let prompt = format!("Esta es la conversación completa:\n\n{transcripcion}\n\nExtrae la propuesta formal en JSON con la jerarquía Épica → Sprints → Tasks, y sugerí la Solución.");
    let raw = match llm(&st, &sistema_extraccion(&lista), &prompt, None, 4096, 90, "council-extract").await {
        Ok(r) => r,
        Err(e) => return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))).into_response()),
    };
    let Some(j) = primer_json(&raw) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "No se pudo extraer propuesta", "raw": raw }))).into_response());
    };
    match serde_json::from_str::<Value>(j) {
        Ok(v) => Ok(Json(v).into_response()),
        Err(_) => Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "JSON inválido en respuesta", "raw": raw }))).into_response()),
    }
}

/// Lee el archivo subido (campo `file`) y devuelve nombre + texto extraído.
async fn leer_archivo_subido(mut mp: Multipart) -> Result<(String, String), Response> {
    let mut archivo: Option<(String, Vec<u8>)> = None;
    while let Ok(Some(campo)) = mp.next_field().await {
        if campo.name() == Some("file") {
            let nombre = campo.file_name().unwrap_or("archivo").to_string();
            match campo.bytes().await {
                Ok(b) => archivo = Some((nombre, b.to_vec())),
                Err(e) => return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response()),
            }
        }
    }
    let Some((nombre, bytes)) = archivo else {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "Archivo requerido" }))).into_response());
    };
    let ext = nombre.rfind('.').map(|i| nombre[i..].to_lowercase()).unwrap_or_default();
    if ![".pdf", ".docx", ".txt"].contains(&ext.as_str()) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "Tipo no soportado. Use: .pdf, .docx, .txt" }))).into_response());
    }
    let texto = if ext == ".txt" {
        String::from_utf8_lossy(&bytes).to_string()
    } else {
        use base64::{engine::general_purpose::STANDARD, Engine};
        match crate::extract::texto_de_archivo(&format!("council-{}-{}", nombre, bytes.len()), &nombre, &STANDARD.encode(&bytes)).await {
            Some(t) => t,
            None => return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "Error extrayendo texto", "detail": "No se pudo leer el archivo" }))).into_response()),
        }
    };
    Ok((nombre, texto))
}

async fn chat_adjuntar(_s: Session, mp: Multipart) -> Response {
    match leer_archivo_subido(mp).await {
        Ok((nombre, texto)) => {
            let total = texto.chars().count();
            let corto: String = texto.chars().take(20000).collect();
            Json(json!({ "text": corto, "fileName": nombre, "truncated": total > 20000 })).into_response()
        }
        Err(r) => r,
    }
}

async fn documento_procesar(State(st): State<AppState>, _s: Session, mp: Multipart) -> Response {
    let (nombre, texto) = match leer_archivo_subido(mp).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let corto: String = texto.chars().take(20000).collect();
    let sistema = format!(
        "Eres Orión, extractor estructurado de propuestas para el consejo de ArchiTechIA.\n\nTu tarea: analizar el contenido de un documento que un socio ha subido, y extraer una propuesta formal con el siguiente formato JSON EXACTO (sin markdown, sin explicaciones):\n\n{}\n\nSi el documento no es suficientemente claro, extrae al menos 1 task genérica. Responde SOLO con el JSON.",
        r#"{
  "title": "título conciso de la propuesta (max 80 chars)",
  "description": "descripción ejecutiva de 2-3 oraciones explicando el por qué y el objetivo",
  "items": [
    {
      "type": "task" o "sprint",
      "title": "título del item",
      "description": "qué implica este item",
      "areaSlug": "slug del área propietaria (operations/sales/finance/marketing/people/delivery/dev/data/infra/security/qa)",
      "priority": "LOW" o "MEDIUM" o "HIGH" o "CRITICAL"
    }
  ]
}"#
    );
    let prompt = format!("El siguiente es el contenido de un documento llamado \"{nombre}\":\n\n{corto}\n\nExtrae la propuesta formal en JSON.");
    let raw = match llm(&st, &sistema, &prompt, None, 4096, 90, "council-doc").await {
        Ok(r) => r,
        Err(e) => {
            return if e.starts_with("OpenCode API error") {
                let code = e.strip_prefix("OpenCode API error ").and_then(|x| x.split(':').next()).unwrap_or("");
                (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("OpenCode API error {code}"), "detail": e.split_once(": ").map(|x| x.1).unwrap_or("") }))).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "Error llamando LLM", "detail": e }))).into_response()
            };
        }
    };
    let Some(j) = primer_json(&raw) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "Orión no pudo extraer propuesta", "raw": raw }))).into_response();
    };
    match serde_json::from_str::<Value>(j) {
        Ok(mut v) => {
            v["_sourceFile"] = json!(nombre);
            Json(v).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "Error llamando LLM", "detail": e.to_string() }))).into_response(),
    }
}

// ═══════════════════════════════ SERVICIOS EXTERNOS DEL CONSEJO ═══════════════════════════════
async fn acciones_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let estado = q.get("status").filter(|x| !x.is_empty()).cloned().unwrap_or_else(|| "pending".into());
    let r = st.http.get(format!("{HOST_API}/actions")).query(&[("status", estado)]).timeout(Duration::from_secs(10)).send().await;
    match r {
        Ok(r) => Json(r.json::<Value>().await.unwrap_or_else(|_| json!({ "actions": [] }))),
        Err(_) => Json(json!({ "actions": [] })),
    }
}

async fn reenviar(st: &AppState, ruta: &str, cuerpo: Value) -> Response {
    match st.http.post(format!("{HOST_API}{ruta}")).json(&cuerpo).timeout(Duration::from_secs(60)).send().await {
        Ok(r) => match r.json::<Value>().await {
            Ok(v) => Json(v).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("TypeError: {e}") }))).into_response(),
    }
}

async fn accion_disparar(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> Response {
    reenviar(&st, "/trigger", json!({ "trigger_id": body.get("trigger_id") })).await
}

async fn accion_resolver(State(st): State<AppState>, _s: Session, Path((id, accion)): Path<(String, String)>) -> Response {
    if !["approve", "reject"].contains(&accion.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid action" }))).into_response();
    }
    reenviar(&st, &format!("/actions/{id}/{accion}"), json!({})).await
}

async fn ejecutar(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> Response {
    let Some(tarea) = body.get("task").filter(|t| truthy_v(t)) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "task requerida" }))).into_response();
    };
    let patron = s(&body, "pattern").filter(|p| !p.is_empty()).unwrap_or_else(|| "solo".into());
    let agentes = body.get("agents").cloned().filter(truthy_v).unwrap_or_else(|| json!([]));
    reenviar(&st, "/run", json!({ "task": tarea, "pattern": patron, "agents": agentes })).await
}

async fn trazas(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let limite = q.get("limit").filter(|x| !x.is_empty()).cloned().unwrap_or_else(|| "50".into());
    let mut params = vec![("limit", limite)];
    if let Some(b) = q.get("q").filter(|x| !x.is_empty()) {
        params.push(("q", b.clone()));
    }
    match st.http.get(format!("{HOST_API}/traces")).query(&params).timeout(Duration::from_secs(10)).send().await {
        Ok(r) => Json(r.json::<Value>().await.unwrap_or_else(|_| json!({ "traces": [] }))),
        Err(_) => Json(json!({ "traces": [] })),
    }
}

async fn disparadores(State(st): State<AppState>, _s: Session) -> Json<Value> {
    match st.http.get(format!("{HOST_API}/triggers")).timeout(Duration::from_secs(10)).send().await {
        Ok(r) => Json(r.json::<Value>().await.unwrap_or_else(|_| json!({ "triggers": [] }))),
        Err(_) => Json(json!({ "triggers": [] })),
    }
}

async fn estado_agentes(State(st): State<AppState>, _s: Session) -> Json<Value> {
    let agentes = [
        ("orion", "Orion", "Admin", 8644, "#7F77DD"),
        ("ares", "Ares", "Sales", 8645, "#E2562A"),
        ("atlas", "Atlas", "Operations", 8646, "#1D9375"),
        ("vesta", "Vesta", "Finance", 8647, "#BA6057"),
        ("iris", "Iris", "Marketing", 8648, "#378C3D"),
    ];
    let mut set = tokio::task::JoinSet::new();
    for (i, (id, nombre, area, puerto, color)) in agentes.into_iter().enumerate() {
        let http = st.http.clone();
        set.spawn(async move {
            let t0 = std::time::Instant::now();
            let r = http.get(format!("http://host-gateway:{puerto}/health")).timeout(Duration::from_secs(3)).send().await;
            let v = match r {
                Ok(r) => json!({ "id": id, "name": nombre, "area": area, "port": puerto, "color": color, "status": if r.status().is_success() { "online" } else { "degraded" }, "latency": t0.elapsed().as_millis() as u64 }),
                Err(_) => json!({ "id": id, "name": nombre, "area": area, "port": puerto, "color": color, "status": "offline", "latency": Value::Null }),
            };
            (i, v)
        });
    }
    let mut out: Vec<(usize, Value)> = vec![];
    while let Some(Ok(x)) = set.join_next().await {
        out.push(x);
    }
    out.sort_by_key(|(i, _)| *i);
    Json(json!({ "agents": out.into_iter().map(|(_, v)| v).collect::<Vec<_>>() }))
}

// La configuración de disparadores (`council-trigger-config.json`) vive en la carpeta del portal, a la
// que este servicio (usuario sin privilegios) no tiene acceso: esas dos rutas siguen en Next.

// ═══════════════════════════════ WHATSAPP → ORIÓN (pública) ═══════════════════════════════
const EVOLUTION_URL: &str = "http://localhost:8080";
const EVOLUTION_KEY: &str = "evo-scheduling-2026";
const INSTANCIA: &str = "orion";
const WHISPER_URL: &str = "http://localhost:9200";

async fn whatsapp_estado() -> Json<Value> {
    Json(json!({ "status": "ok", "agent": "orion" }))
}

async fn transcribir(st: &AppState, datos: &Value) -> Option<String> {
    let r = st
        .http
        .post(format!("{EVOLUTION_URL}/chat/getBase64FromMediaMessage/{INSTANCIA}"))
        .header("apikey", EVOLUTION_KEY)
        .timeout(Duration::from_secs(15))
        .json(&json!({ "message": datos, "convertToMp4": false }))
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        tracing::error!("[WhatsApp→Whisper] Evolution media error {}", r.status());
        return None;
    }
    let j: Value = r.json().await.ok()?;
    let b64 = j["base64"].as_str().filter(|x| !x.is_empty())?;
    let mime = j["mimetype"].as_str().unwrap_or("audio/ogg").to_string();
    let ext = if mime.contains("ogg") { "ogg" } else { "mp3" };
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD.decode(b64).ok()?;
    let parte = reqwest::multipart::Part::bytes(bytes).file_name(format!("audio.{ext}")).mime_str(&mime).ok()?;
    let form = reqwest::multipart::Form::new().part("file", parte).text("model", "Systran/faster-whisper-small").text("response_format", "json");
    let w = st.http.post(format!("{WHISPER_URL}/v1/audio/transcriptions")).multipart(form).timeout(Duration::from_secs(30)).send().await.ok()?;
    if !w.status().is_success() {
        tracing::error!("[WhatsApp→Whisper] Whisper error {}", w.status());
        return None;
    }
    let d: Value = w.json().await.ok()?;
    d["text"].as_str().map(|t| t.trim().to_string())
}

async fn atender_mensaje(st: AppState, remote_jid: String, texto: String, es_audio: bool) {
    let telefono = remote_jid.replace("@s.whatsapp.net", "").trim_start_matches('+').to_string();
    let etiqueta = if es_audio { format!("[Audio transcrito] {texto}") } else { texto.clone() };
    tracing::info!("[WhatsApp→Orión] from={telefono} {}=\"{}\"", if es_audio { "audio" } else { "msg" }, texto.chars().take(80).collect::<String>());
    let respuesta = match tokio::time::timeout(Duration::from_secs(40), super::orion::responder(&st, &etiqueta, "whatsapp", &telefono)).await {
        Ok(Ok((r, _))) => r.trim().to_string(),
        Ok(Err(e)) => {
            tracing::error!("[WhatsApp→Orión] Orion endpoint error {e}");
            return;
        }
        Err(_) => {
            tracing::error!("[WhatsApp→Orión] Orion endpoint error: tiempo agotado");
            return;
        }
    };
    if respuesta.is_empty() {
        return;
    }
    let r = st
        .http
        .post(format!("{EVOLUTION_URL}/message/sendText/{INSTANCIA}"))
        .header("apikey", EVOLUTION_KEY)
        .json(&json!({ "number": remote_jid, "text": respuesta }))
        .send()
        .await;
    match r {
        Ok(_) => tracing::info!("[WhatsApp→Orión] sent reply to {telefono}"),
        Err(e) => tracing::error!("[WhatsApp→Orión] Evolution send error {e}"),
    }
}

async fn whatsapp_recibir(State(st): State<AppState>, cuerpo: Option<Json<Value>>) -> Response {
    let Some(Json(body)) = cuerpo else { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false }))).into_response() };
    let ok = || Json(json!({ "ok": true })).into_response();
    let Some(d) = body.get("data").filter(|d| d.is_object()) else { return ok() };
    let Some(key) = d.get("key").filter(|k| k.is_object()) else { return ok() };
    if truthy_v(&key["fromMe"]) {
        return ok();
    }
    let Some(jid) = key["remoteJid"].as_str().filter(|j| !j.is_empty() && !j.contains("@g.us") && !j.contains("@broadcast")) else { return ok() };
    let jid = jid.to_string();
    let contenido = d.get("message").cloned().unwrap_or(Value::Null);
    if d["messageType"].as_str() == Some("audioMessage") || truthy_v(&contenido["audioMessage"]) {
        let datos = d.clone();
        tokio::spawn(async move {
            match transcribir(&st, &datos).await {
                Some(t) if !t.is_empty() => atender_mensaje(st.clone(), jid, t, true).await,
                _ => tracing::info!("[WhatsApp→Whisper] could not transcribe audio from {jid}"),
            }
        });
        return ok();
    }
    let texto = contenido["conversation"].as_str().filter(|t| !t.is_empty()).or_else(|| contenido.pointer("/extendedTextMessage/text").and_then(|t| t.as_str()).filter(|t| !t.is_empty())).map(|t| t.trim().to_string());
    let Some(texto) = texto.filter(|t| !t.is_empty()) else { return ok() };
    tokio::spawn(atender_mensaje(st, jid, texto, false));
    ok()
}
