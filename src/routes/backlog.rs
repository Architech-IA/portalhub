//! Backlog, épicas, sprints, registros de estado y ejecuciones — MASD PHUB-0001-0010.
//! Paridad con `src/app/api/backlog/**` de Next. Lo que dispara al Motor (aprobar sprint,
//! reactivar bloqueadas, explicar/planear/aplicar plan) o llama a la CLI de IA (alta de épica,
//! que genera una propuesta del consejo) NO se porta: se reenvía a Next (`proxy::a_next`).

use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{
        exec, fetch_i64, fetch_json, fetch_json_opt, fetch_text_opt, fecha_cuerpo, log_activity, new_id, num_o_nulo, presente, s,
        s_o_nulo, truthy, ts_js_opt, uid, Upd, B,
    },
};

const ORION_AGENT_ID: &str = "cmsii11qf0003l0w1jikaxygb";
const BACKLOG_HUB_AREA_ID: &str = "area_backlog_hub_001";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/backlog", get(items_listar).post(item_crear))
        .route("/api/backlog/epics", get(epics_listar).post(crate::routes::triggers::epic_crear).put(epic_actualizar).delete(epic_eliminar))
        .route("/api/backlog/sprints", get(sprints_listar).post(sprint_crear).put(sprint_estado))
        .route("/api/backlog/sprints/edit", put(sprint_editar))
        .route("/api/backlog/sprints/{id}/areas", get(sprint_areas_obtener).put(sprint_areas_guardar))
        .route("/api/backlog/reorder", post(reordenar))
        .route("/api/backlog/solution", get(soluciones_arbol))
        .route("/api/backlog/logs", get(logs_listar).post(logs_crear))
        .route("/api/backlog/sprint/{sprint_id}/graph", get(sprint_grafo))
        .route("/api/backlog/{id}", get(item_obtener).put(item_actualizar).delete(item_eliminar))
        .route("/api/backlog/{id}/executions", get(ejecuciones_listar).post(ejecucion_crear))
        .route("/api/backlog/{id}/resultado", axum::routing::patch(resultado_guardar))
}

// ── Fragmentos SQL ──────────────────────────────────────────────────────────────────────────
/// Ítem de backlog con `solucion` y `sprint` anidados (alias de la tabla: `b`). Se quitan las
/// dos columnas que existen en la base pero no en el esquema de Prisma.
const ITEM_JSON: &str = r#"(to_jsonb(b) - 'createdByAgentId' - 'createdByAgentName') || jsonb_build_object(
    'solucion', (SELECT jsonb_build_object('id', so.id, 'nombre', so.nombre, 'tipo', so.tipo) FROM "Solucion" so WHERE so.id = b."solucionId"),
    'sprint',   (SELECT jsonb_build_object('id', sp.id, 'sprintCode', sp."sprintCode", 'name', sp.name) FROM "Sprint" sp WHERE sp.id = b."sprintId"))"#;

/// Lo mismo SIN `description` ni `resultado` (≈50 % del peso de la lista) y con un extracto de la descripción para las
/// tarjetas del kanban. La ficha completa se pide aparte (`GET /api/backlog/{id}`).
const ITEM_LIGERO_JSON: &str = r#"(to_jsonb(b) - 'createdByAgentId' - 'createdByAgentName' - 'description' - 'resultado') || jsonb_build_object(
    'descripcionResumen', left(b.description, 160),
    'solucion', (SELECT jsonb_build_object('id', so.id, 'nombre', so.nombre, 'tipo', so.tipo) FROM "Solucion" so WHERE so.id = b."solucionId"),
    'sprint',   (SELECT jsonb_build_object('id', sp.id, 'sprintCode', sp."sprintCode", 'name', sp.name) FROM "Sprint" sp WHERE sp.id = b."sprintId"))"#;

/// Sprint (alias `s`) con `_count.items`, `solucion` y `epic`; sin la columna `metadata`.
const SPRINT_BASICO: &str = r#"(to_jsonb(s) - 'metadata') || jsonb_build_object(
    '_count', jsonb_build_object('items', (SELECT COUNT(*) FROM "BacklogItem" i WHERE i."sprintId" = s.id)),
    'solucion', (SELECT jsonb_build_object('id', so.id, 'solucionCode', so."solucionCode", 'nombre', so.nombre) FROM "Solucion" so WHERE so.id = s."solucionId"),
    'epic',     (SELECT jsonb_build_object('id', e.id, 'name', e.name, 'color', e.color) FROM "Epic" e WHERE e.id = s."epicId"))"#;

const AREA_CORTA: &str = "jsonb_build_object('id', a.id, 'name', a.name, 'slug', a.slug, 'color', a.color)";

/// Equivalente a `orionLog`: nunca falla hacia afuera.
async fn orion_log(st: &AppState, message: &str, action: &str, item_id: Option<&str>, title: Option<&str>, code: Option<&str>, meta: Option<Value>) {
    let r = exec(
        &st.pool,
        r#"INSERT INTO "OrionLog" (message, "actionType", "backlogItemId", "backlogItemTitle", "backlogItemCode", metadata)
           VALUES ($1, $2, $3, $4, $5, $6::jsonb)"#,
        &[
            B::T(message.to_string()),
            B::T(action.to_string()),
            B::OT(item_id.map(String::from)),
            B::OT(title.map(String::from)),
            B::OT(code.map(String::from)),
            B::OT(meta.map(|m| m.to_string())),
        ],
    )
    .await;
    if let Err(e) = r {
        tracing::warn!("orionLog: {e}");
    }
}

/// Aviso de sprint adjudicado en el chat de Orión (`SPRINT_ASSIGNED`).
async fn orion_sprint_asignado(st: &AppState, mensaje: &str, nombre: &str, codigo: &str, meta: Value) -> Result<(), sqlx::Error> {
    exec(
        &st.pool,
        r#"INSERT INTO "OrionLog" (id, message, "actionType", "backlogItemId", "backlogItemTitle", "backlogItemCode", metadata, "createdAt")
           VALUES (gen_random_uuid()::text, $1, 'SPRINT_ASSIGNED', NULL, $2, $3, $4::jsonb, NOW())"#,
        &[B::T(mensaje.to_string()), B::T(nombre.to_string()), B::T(codigo.to_string()), B::T(meta.to_string())],
    )
    .await?;
    Ok(())
}

// ═══════════════════════════════ ÍTEMS ═══════════════════════════════
/// Última lista de ítems (ver `fetch_json_cacheado`): el backlog pesa ~1,3 MB y se pide por polling.
static CACHE: std::sync::LazyLock<crate::util::CacheJson> = std::sync::LazyLock::new(Default::default);

async fn items_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<std::sync::Arc<Value>>> {
    let ligero = q.get("ligero").map(|v| v == "1" || v == "true").unwrap_or(false);
    let (json, clave) = if ligero { (ITEM_LIGERO_JSON, "items-ligero") } else { (ITEM_JSON, "items") };
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({json} ORDER BY b."createdAt" ASC), '[]'::jsonb) FROM "BacklogItem" b"#);
    Ok(Json(crate::util::fetch_json_cacheado(&st.pool, &CACHE, clave, &sql, &[]).await?))
}

/// Un ítem completo (la ficha lo pide al abrirse cuando la lista vino en modo ligero).
async fn item_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    item_por_id(&st, &id).await?.map(Json).ok_or_else(|| ApiError::not_found("No encontrado"))
}

async fn item_por_id(st: &AppState, id: &str) -> Result<Option<Value>, sqlx::Error> {
    let sql = format!(r#"SELECT {ITEM_JSON} FROM "BacklogItem" b WHERE b.id = $1"#);
    fetch_json_opt(&st.pool, &sql, &[B::T(id.to_string())]).await
}

async fn item_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let area_body = s_o_nulo(&body, "areaId");
    let asignado_orion = s(&body, "assigneeId").as_deref() == Some(ORION_AGENT_ID)
        || s(&body, "assigneeName").map(|n| n.to_lowercase().contains("orion")).unwrap_or(false);
    let area_id = area_body.or_else(|| asignado_orion.then(|| BACKLOG_HUB_AREA_ID.to_string()));

    let sprint_id = s_o_nulo(&body, "sprintId");
    let mut task_code: Option<String> = None;
    if let Some(sid) = &sprint_id {
        let codigo = fetch_text_opt(&st.pool, r#"SELECT "sprintCode" FROM "Sprint" WHERE id = $1"#, &[B::T(sid.clone())]).await?;
        let n = fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "BacklogItem" WHERE "sprintId" = $1"#, &[B::T(sid.clone())]).await?;
        task_code = codigo.filter(|c| !c.is_empty()).map(|c| format!("{c}-{:03}", n + 1));
    }

    let puntos = if truthy(&body, "points") { Some(crate::util::numero(body.get("points"))) } else { None };
    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "BacklogItem" (id, title, description, type, priority, status, points, "solucionId", "assigneeId", "assigneeName",
                                         "sprintId", "taskCode", "areaId", "prdRequisitoId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, COALESCE($4, 'TASK'), COALESCE($5, 'MEDIUM'), COALESCE($6, 'BACKLOG'), $7::float8::int, $8, $9, $10, $11, $12, $13, $14, NOW(), NOW())
             RETURNING *)
           SELECT {ITEM_JSON} FROM ins b"#
    );
    let item = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::OT(s(&body, "title")),
            B::OT(s_o_nulo(&body, "description")),
            B::OT(s_o_nulo(&body, "type")),
            B::OT(s_o_nulo(&body, "priority")),
            B::OT(s_o_nulo(&body, "status")),
            B::OF(puntos),
            B::OT(s_o_nulo(&body, "solucionId")),
            B::OT(s_o_nulo(&body, "assigneeId")),
            B::OT(s_o_nulo(&body, "assigneeName")),
            B::OT(sprint_id),
            B::OT(task_code.clone()),
            B::OT(area_id.clone()),
            B::OT(s_o_nulo(&body, "prdRequisitoId")),
        ],
    )
    .await?;
    let id = item["id"].as_str().unwrap_or_default().to_string();
    let titulo = s(&body, "title").unwrap_or_default();

    if area_id.as_deref() == Some(BACKLOG_HUB_AREA_ID) {
        let cod = task_code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default();
        orion_log(
            &st,
            &format!("Se ha recibido la adjudicación de la tarea **{titulo}**{cod} a Oficina Virtual."),
            "RECEIVED",
            Some(&id),
            Some(&titulo),
            task_code.as_deref(),
            Some(json!({ "priority": body.get("priority"), "type": body.get("type"), "status": body.get("status") })),
        )
        .await;
    }
    let cod = task_code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default();
    log_activity(&st.pool, "CREATED", &format!("creó el ítem de backlog {titulo}{cod}"), "backlogItem", &id, uid(&sesion), None).await;
    Ok(Json(item))
}

async fn item_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let asignado_orion = s(&body, "assigneeId").as_deref() == Some(ORION_AGENT_ID)
        || s(&body, "assigneeName").map(|n| n.to_lowercase().contains("orion")).unwrap_or(false);
    // `bodyAreaId !== undefined ? bodyAreaId : (isOrion ? HUB : undefined)`
    let area_resuelta: Option<Option<String>> = if presente(&body, "areaId") {
        Some(s_o_nulo(&body, "areaId"))
    } else if asignado_orion {
        Some(Some(BACKLOG_HUB_AREA_ID.to_string()))
    } else {
        None
    };

    let previo = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(x) FROM (SELECT "sprintId", "taskCode", "areaId" FROM "BacklogItem" WHERE id = $1) x"#, &[B::T(id.clone())]).await?;

    // Código de tarea nuevo si se asigna a un sprint distinto (o aún no tenía).
    let mut codigo_nuevo: Option<String> = None;
    if let Some(sid) = s_o_nulo(&body, "sprintId") {
        let (cod_prev, sprint_prev) = match &previo {
            Some(p) => (p["taskCode"].as_str().map(String::from), p["sprintId"].as_str().map(String::from)),
            None => (None, None),
        };
        if cod_prev.is_none() || sprint_prev.as_deref() != Some(sid.as_str()) {
            let cod = fetch_text_opt(&st.pool, r#"SELECT "sprintCode" FROM "Sprint" WHERE id = $1"#, &[B::T(sid.clone())]).await?;
            let n = fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "BacklogItem" WHERE "sprintId" = $1"#, &[B::T(sid.clone())]).await?;
            codigo_nuevo = cod.filter(|c| !c.is_empty()).map(|c| format!("{c}-{:03}", n + 1));
        }
    }

    let mut up = Upd::new(&id);
    if let Some(t) = s(&body, "title") {
        up.set("title", B::T(t));
    }
    // Si el pedido no trae `description` (la lista ligera no la incluye y la pantalla devuelve el ítem tal cual) se conserva;
    // Next la ponía en NULL. Un `description` explícito (texto, vacío o null) se guarda como siempre.
    if body.get("description").is_some() {
        up.set("description", B::OT(s_o_nulo(&body, "description")));
    }
    if let Some(Value::String(r)) = body.get("resultado") {
        up.set("resultado", B::T(r.clone()));
    }
    up.set_expr("fechaEjecucion", B::OT(fecha_cuerpo(&body, "fechaEjecucion")), &ts_js_opt(0).replace("$0", "{n}"));
    for k in ["type", "priority", "status"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    let puntos = if truthy(&body, "points") { Some(crate::util::numero(body.get("points"))) } else { None };
    up.set_expr("points", B::OF(puntos), "{n}::float8::int");
    if presente(&body, "solucionId") {
        up.set("solucionId", B::OT(s_o_nulo(&body, "solucionId")));
    }
    up.set("assigneeId", B::OT(s_o_nulo(&body, "assigneeId")));
    up.set("assigneeName", B::OT(s_o_nulo(&body, "assigneeName")));
    if let Some(a) = &area_resuelta {
        up.set("areaId", B::OT(a.clone()));
    }
    if presente(&body, "sprintId") {
        up.set("sprintId", B::OT(s_o_nulo(&body, "sprintId")));
        if body["sprintId"].is_null() {
            up.set("taskCode", B::OT(None));
        } else if let Some(c) = &codigo_nuevo {
            up.set("taskCode", B::T(c.clone()));
        }
    }
    let sql = up.con("BacklogItem", &format!("SELECT {ITEM_JSON} FROM up b"));
    let item = fetch_json_opt(&st.pool, &sql, &up.binds).await?.ok_or_else(|| ApiError::internal("Error interno"))?;

    let titulo = item["title"].as_str().unwrap_or_default().to_string();
    let cod_item = item["taskCode"].as_str().map(String::from);

    // Orión — DISPATCHED: el ítem sale del Hub de Backlog hacia otra área.
    let area_body = s_o_nulo(&body, "areaId");
    let area_prev = previo.as_ref().and_then(|p| p["areaId"].as_str().map(String::from));
    if let Some(destino) = &area_body {
        if destino != BACKLOG_HUB_AREA_ID && area_prev.as_deref() == Some(BACKLOG_HUB_AREA_ID) {
            let nombre = fetch_text_opt(&st.pool, r#"SELECT name FROM "Area" WHERE id = $1"#, &[B::T(destino.clone())]).await?.unwrap_or_else(|| destino.clone());
            let cod = cod_item.as_deref().map(|c| format!(" ({c})")).unwrap_or_default();
            orion_log(
                &st,
                &format!("Se ha asignado la tarea **{titulo}**{cod} al área de **{nombre}**."),
                "DISPATCHED",
                Some(&id),
                Some(&titulo),
                cod_item.as_deref(),
                Some(json!({ "toArea": nombre, "toAreaId": destino })),
            )
            .await;
        }
    }
    // Orión — avisos de estado mientras el ítem está en el Hub de Backlog.
    let area_item = item["areaId"].as_str().map(String::from);
    let en_hub = area_item.as_deref() == Some(BACKLOG_HUB_AREA_ID) || matches!(&area_resuelta, Some(Some(a)) if a == BACKLOG_HUB_AREA_ID);
    if en_hub {
        if let Some(estado) = s_o_nulo(&body, "status").filter(|e| e != "BACKLOG") {
            let etiqueta = match estado.as_str() {
                "IN_PROGRESS" => "🔄 En progreso",
                "DONE" => "✅ Completada",
                "REVIEW" => "🔍 En revisión",
                "CANCELLED" => "❌ Cancelada",
                "BACKLOG" => "📋 En backlog",
                "TODO" => "📌 Por hacer",
                otro => otro,
            };
            let (msg, accion) = if estado == "DONE" {
                (format!("La tarea **{titulo}** ha sido completada exitosamente. Cerrando el ciclo de coordinación."), "COMPLETED")
            } else {
                (format!("He actualizado el estado de **{titulo}** a **{etiqueta}**."), "STATUS_CHANGED")
            };
            orion_log(&st, &msg, accion, Some(&id), Some(&titulo), None, Some(json!({ "status": estado }))).await;
        }
    }
    if presente(&body, "status") {
        let estado = s(&body, "status").unwrap_or_default();
        log_activity(&st.pool, "STATUS_CHANGED", &format!("cambió el estado de {titulo} a {estado}"), "backlogItem", &id, uid(&sesion), None).await;
    } else {
        log_activity(&st.pool, "UPDATED", &format!("actualizó el ítem de backlog {titulo}"), "backlogItem", &id, uid(&sesion), None).await;
    }
    Ok(Json(item))
}

async fn item_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let titulo = fetch_text_opt(&st.pool, r#"SELECT title FROM "BacklogItem" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    if exec(&st.pool, r#"DELETE FROM "BacklogItem" WHERE id = $1"#, &[B::T(id.clone())]).await? == 0 {
        return Err(ApiError::internal("Error interno"));
    }
    log_activity(&st.pool, "UPDATED", &format!("eliminó el ítem de backlog {}", titulo.unwrap_or_default()), "backlogItem", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "ok": true })))
}

async fn resultado_guardar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let fila = fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "BacklogItem" SET resultado = $2, "updatedAt" = NOW() WHERE id = $1 RETURNING resultado) SELECT to_jsonb(up) FROM up"#,
        &[B::T(id), B::OT(s(&body, "resultado"))],
    )
    .await?
    .ok_or_else(|| ApiError::internal("Error interno"))?;
    Ok(Json(json!({ "ok": true, "resultado": fila["resultado"] })))
}

// ═══════════════════════════════ ÉPICAS ═══════════════════════════════
async fn epics_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<std::sync::Arc<Value>>> {
    let sql = r#"SELECT COALESCE(jsonb_agg(to_jsonb(e) || jsonb_build_object(
          'roadmap',  (SELECT jsonb_build_object('id', r.id, 'name', r.name, 'quarter', r.quarter) FROM "Roadmap" r WHERE r.id = e."roadmapId"),
          'solucion', (SELECT jsonb_build_object('id', so.id, 'nombre', so.nombre) FROM "Solucion" so WHERE so.id = e."solucionId"),
          'sprints',  COALESCE((SELECT jsonb_agg((to_jsonb(s) - 'metadata') || jsonb_build_object(
                          '_count', jsonb_build_object('items', (SELECT COUNT(*) FROM "BacklogItem" i WHERE i."sprintId" = s.id)),
                          'items', COALESCE((SELECT jsonb_agg(jsonb_build_object('status', i2.status)) FROM "BacklogItem" i2 WHERE i2."sprintId" = s.id), '[]'::jsonb))
                          ORDER BY s."createdAt" DESC) FROM "Sprint" s WHERE s."epicId" = e.id), '[]'::jsonb),
          '_count',   jsonb_build_object('sprints', (SELECT COUNT(*) FROM "Sprint" s2 WHERE s2."epicId" = e.id)))
        ORDER BY e."createdAt" DESC), '[]'::jsonb) FROM "Epic" e"#;
    Ok(Json(crate::util::fetch_json_cacheado(&st.pool, &CACHE, "epics", sql, &[]).await?))
}

async fn epic_actualizar(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let id = s(&body, "id").ok_or_else(|| ApiError::internal("Error interno"))?;
    let mut up = Upd::new(&id);
    for k in ["name", "description", "status", "priority", "color", "roadmapId", "solucionId"] {
        match body.get(k) {
            Some(Value::String(v)) => up.set(k, B::T(v.clone())),
            Some(Value::Null) => up.set(k, B::OT(None)),
            _ => {}
        }
    }
    for k in ["startDate", "endDate"] {
        if presente(&body, k) {
            up.set_expr(k, B::OT(fecha_cuerpo(&body, k)), &ts_js_opt(0).replace("$0", "{n}"));
        }
    }
    let epic = fetch_json_opt(&st.pool, &up.sql("Epic"), &up.binds).await?.ok_or_else(|| ApiError::internal("Error interno"))?;
    Ok(Json(epic))
}

async fn epic_eliminar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let Some(id) = q.get("id").filter(|x| !x.is_empty()) else {
        return Err(ApiError::bad_request("id requerido"));
    };
    if exec(&st.pool, r#"DELETE FROM "Epic" WHERE id = $1"#, &[B::T(id.clone())]).await? == 0 {
        return Err(ApiError::internal("Error interno"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ SPRINTS ═══════════════════════════════
/// `SP-0000-0001`: prefijo de la solución, número de la épica dentro de la solución y orden del
/// sprint. `excluir` es el sprint que se está editando (conserva su lugar si ya está en el grupo).
async fn codigo_sprint(st: &AppState, solucion_id: Option<&str>, epic_id: Option<&str>, excluir: Option<&str>) -> Result<String, sqlx::Error> {
    let prefijo = match solucion_id {
        Some(id) => fetch_text_opt(&st.pool, r#"SELECT "solucionCode" FROM "Solucion" WHERE id = $1"#, &[B::T(id.to_string())]).await?.filter(|c| !c.is_empty()),
        None => None,
    }
    .unwrap_or_else(|| "SP".to_string());

    let epic_num = match epic_id {
        Some(eid) => {
            let idx = fetch_i64(
                &st.pool,
                r#"SELECT COALESCE((SELECT rn FROM (SELECT id, row_number() OVER (ORDER BY "createdAt") rn FROM "Epic"
                                     WHERE ($1::text IS NULL OR "solucionId" = $1)) t WHERE id = $2), 0)::bigint"#,
                &[B::OT(solucion_id.map(String::from)), B::T(eid.to_string())],
            )
            .await?;
            if idx > 0 {
                format!("{idx:04}")
            } else {
                "0000".to_string()
            }
        }
        None => "0000".to_string(),
    };

    let filtro = r#"($1::text IS NULL OR "solucionId" = $1) AND ($2::text IS NULL OR "epicId" = $2)"#;
    let b = [B::OT(solucion_id.map(String::from)), B::OT(epic_id.map(String::from))];
    let numero = match excluir {
        Some(self_id) => {
            let sql = format!(r#"SELECT COALESCE((SELECT rn FROM (SELECT id, row_number() OVER (ORDER BY "createdAt") rn FROM "Sprint" WHERE {filtro}) t WHERE id = $3), -1)::bigint"#);
            let pos = fetch_i64(&st.pool, &sql, &[b[0].clone(), b[1].clone(), B::T(self_id.to_string())]).await?;
            if pos > 0 {
                pos
            } else {
                fetch_i64(&st.pool, &format!(r#"SELECT COUNT(*) FROM "Sprint" WHERE {filtro}"#), &b).await? + 1
            }
        }
        None => fetch_i64(&st.pool, &format!(r#"SELECT COUNT(*) FROM "Sprint" WHERE {filtro}"#), &b).await? + 1,
    };
    Ok(format!("{prefijo}-{epic_num}-{numero:04}"))
}

async fn sprints_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<std::sync::Arc<Value>>> {
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg(({SPRINT_BASICO}) || jsonb_build_object(
              'ownerArea', (SELECT {AREA_CORTA} FROM "Area" a WHERE a.id = s."ownerAreaId"),
              'sprintAreas', COALESCE((SELECT jsonb_agg(to_jsonb(sa) || jsonb_build_object('area', (SELECT {AREA_CORTA} FROM "Area" a WHERE a.id = sa."areaId")))
                                       FROM "SprintArea" sa WHERE sa."sprintId" = s.id), '[]'::jsonb))
              ORDER BY s."createdAt" DESC), '[]'::jsonb) FROM "Sprint" s"#
    );
    Ok(Json(crate::util::fetch_json_cacheado(&st.pool, &CACHE, "sprints", &sql, &[]).await?))
}

async fn sprint_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let solucion_id = s_o_nulo(&body, "solucionId");
    let epic_id = s_o_nulo(&body, "epicId");
    let codigo = codigo_sprint(&st, solucion_id.as_deref(), epic_id.as_deref(), None).await?;
    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "Sprint" (id, "sprintCode", name, goal, "startDate", "endDate", status, "solucionId", "epicId", "responsibleId", "responsibleName", "createdAt")
             VALUES ($1, $2, $3, $4, {ini}, {fin}, 'PLANNED', $7, $8, $9, $10, NOW()) RETURNING *)
           SELECT {SPRINT_BASICO} FROM ins s"#,
        ini = ts_js_opt(5),
        fin = ts_js_opt(6),
    );
    let sprint = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::T(codigo),
            B::OT(s(&body, "name")),
            B::OT(s_o_nulo(&body, "goal")),
            B::OT(fecha_cuerpo(&body, "startDate")),
            B::OT(fecha_cuerpo(&body, "endDate")),
            B::OT(solucion_id),
            B::OT(epic_id),
            B::OT(s_o_nulo(&body, "responsibleId")),
            B::OT(s_o_nulo(&body, "responsibleName")),
        ],
    )
    .await?;
    if let Some(resp) = s_o_nulo(&body, "responsibleName") {
        let etiqueta = sprint["sprintCode"].as_str().or(sprint["name"].as_str()).unwrap_or_default().to_string();
        let nombre = sprint["name"].as_str().unwrap_or_default().to_string();
        let msg = format!("Se ha recibido la adjudicación del **{etiqueta} — {nombre}** a Oficina Virtual.");
        orion_sprint_asignado(
            &st,
            &msg,
            &nombre,
            sprint["sprintCode"].as_str().unwrap_or_default(),
            json!({ "sprintId": sprint["id"], "responsibleId": body.get("responsibleId"), "responsibleName": resp }),
        )
        .await?;
    }
    Ok(Json(sprint))
}

async fn sprint_estado(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let id = s(&body, "id").ok_or_else(|| ApiError::internal("Error interno"))?;
    let sql = format!(r#"WITH up AS (UPDATE "Sprint" SET status = $2 WHERE id = $1 RETURNING *) SELECT {SPRINT_BASICO} FROM up s"#);
    let sprint = fetch_json_opt(&st.pool, &sql, &[B::T(id), B::OT(s(&body, "status"))]).await?.ok_or_else(|| ApiError::internal("Error interno"))?;
    Ok(Json(sprint))
}

async fn sprint_editar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    // En Next esta ruta exige token de sesión (no acepta la API key).
    sesion.require_user()?;
    let id = s(&body, "id").ok_or_else(|| ApiError::internal("Error interno"))?;
    let actual = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(x) FROM (SELECT "solucionId", "epicId", "sprintCode" FROM "Sprint" WHERE id = $1) x"#,
        &[B::T(id.clone())],
    )
    .await?;
    let sol = s_o_nulo(&body, "solucionId");
    let epic = s_o_nulo(&body, "epicId");
    let (sol_act, epic_act, cod_act) = match &actual {
        Some(a) => (a["solucionId"].as_str().map(String::from), a["epicId"].as_str().map(String::from), a["sprintCode"].as_str().map(String::from)),
        None => (None, None, None),
    };
    let cambio = sol != sol_act || epic != epic_act;
    // También se rehace si el código actual no tiene el formato `X-0000-0000` (formato viejo).
    let formato_viejo = cod_act.as_deref().map(|c| c.split('-').count() != 3).unwrap_or(true);
    let codigo_nuevo = if cambio || formato_viejo { Some(codigo_sprint(&st, sol.as_deref(), epic.as_deref(), Some(&id)).await?) } else { None };

    let mut up = Upd::sin_updated(&id);
    if let Some(n) = s(&body, "name") {
        up.set("name", B::T(n));
    }
    up.set("goal", B::OT(s_o_nulo(&body, "goal")));
    up.set_expr("startDate", B::OT(fecha_cuerpo(&body, "startDate")), &ts_js_opt(0).replace("$0", "{n}"));
    up.set_expr("endDate", B::OT(fecha_cuerpo(&body, "endDate")), &ts_js_opt(0).replace("$0", "{n}"));
    up.set("epicId", B::OT(epic));
    up.set("responsibleId", B::OT(s_o_nulo(&body, "responsibleId")));
    up.set("responsibleName", B::OT(s_o_nulo(&body, "responsibleName")));
    up.set("solucionId", B::OT(sol));
    if let Some(c) = &codigo_nuevo {
        up.set("sprintCode", B::T(c.clone()));
    }
    let sql = up.con("Sprint", &format!("SELECT {SPRINT_BASICO} FROM up s"));
    let sprint = fetch_json_opt(&st.pool, &sql, &up.binds).await?.ok_or_else(|| ApiError::internal("Error interno"))?;

    // Si cambió el código del sprint, los códigos de sus tareas se renumeran en orden de creación.
    if let Some(c) = &codigo_nuevo {
        let ids = fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(id ORDER BY "createdAt" ASC), '[]'::jsonb) FROM "BacklogItem" WHERE "sprintId" = $1"#,
            &[B::T(id.clone())],
        )
        .await?;
        for (i, tid) in ids.as_array().cloned().unwrap_or_default().iter().enumerate() {
            let tid = tid.as_str().unwrap_or_default().to_string();
            exec(&st.pool, r#"UPDATE "BacklogItem" SET "taskCode" = $2, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(tid), B::T(format!("{c}-{:03}", i + 1))]).await?;
        }
    }
    if let Some(resp) = s_o_nulo(&body, "responsibleName") {
        let etiqueta = sprint["sprintCode"].as_str().or(sprint["name"].as_str()).unwrap_or_default().to_string();
        let nombre = sprint["name"].as_str().unwrap_or_default().to_string();
        orion_sprint_asignado(
            &st,
            &format!("Se ha recibido la adjudicación del **{etiqueta} — {nombre}** a Oficina Virtual."),
            &nombre,
            sprint["sprintCode"].as_str().unwrap_or_default(),
            json!({ "sprintId": sprint["id"], "responsibleId": body.get("responsibleId"), "responsibleName": resp }),
        )
        .await?;
    }
    Ok(Json(sprint))
}

async fn areas_de_sprint(st: &AppState, id: &str) -> Result<Option<Value>, sqlx::Error> {
    let sql = format!(
        r#"SELECT jsonb_build_object(
              'ownerArea', (SELECT {AREA_CORTA} FROM "Area" a WHERE a.id = s."ownerAreaId"),
              'participantAreas', COALESCE((SELECT jsonb_agg({AREA_CORTA}) FROM "SprintArea" sa JOIN "Area" a ON a.id = sa."areaId" WHERE sa."sprintId" = s.id), '[]'::jsonb))
           FROM "Sprint" s WHERE s.id = $1"#
    );
    fetch_json_opt(&st.pool, &sql, &[B::T(id.to_string())]).await
}

async fn sprint_areas_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    areas_de_sprint(&st, &id).await?.map(Json).ok_or_else(|| ApiError::not_found("Sprint no encontrado"))
}

async fn sprint_areas_guardar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let Some(sprint) = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(x) FROM (SELECT id, name, "sprintCode" FROM "Sprint" WHERE id = $1) x"#,
        &[B::T(id.clone())],
    )
    .await?
    else {
        return Err(ApiError::not_found("Sprint no encontrado"));
    };
    let owner = s_o_nulo(&body, "ownerAreaId");
    let participantes: Vec<String> = body
        .get("participantAreaIds")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    exec(&st.pool, r#"UPDATE "Sprint" SET "ownerAreaId" = $2 WHERE id = $1"#, &[B::T(id.clone()), B::OT(owner.clone())]).await?;
    exec(&st.pool, r#"DELETE FROM "SprintArea" WHERE "sprintId" = $1"#, &[B::T(id.clone())]).await?;
    for area in &participantes {
        exec(
            &st.pool,
            r#"INSERT INTO "SprintArea" (id, "sprintId", "areaId", "createdAt") VALUES ($1, $2, $3, NOW()) ON CONFLICT DO NOTHING"#,
            &[B::T(format!("{id}-{area}")), B::T(id.clone()), B::T(area.clone())],
        )
        .await?;
    }

    let nombre_area = |aid: &str| {
        let aid = aid.to_string();
        let pool = st.pool.clone();
        async move { fetch_text_opt(&pool, r#"SELECT name FROM "Area" WHERE id = $1"#, &[B::T(aid)]).await }
    };
    let nombre_dueno = match &owner {
        Some(o) => nombre_area(o).await?.unwrap_or_else(|| "Sin asignar".into()),
        None => "Sin asignar".into(),
    };
    let mut nombres = vec![];
    for p in &participantes {
        if let Some(n) = nombre_area(p).await? {
            nombres.push(n);
        }
    }
    let etiqueta = sprint["sprintCode"].as_str().or(sprint["name"].as_str()).unwrap_or_default().to_string();
    let msg = if nombres.is_empty() {
        format!("He asignado el **{etiqueta}** al área **{nombre_dueno}** como responsable principal.")
    } else {
        format!("He asignado el **{etiqueta}** — área líder: **{nombre_dueno}**, participantes: **{}**.", nombres.join(", "))
    };
    orion_sprint_asignado(
        &st,
        &msg,
        sprint["name"].as_str().unwrap_or_default(),
        sprint["sprintCode"].as_str().unwrap_or_default(),
        json!({ "sprintId": id, "ownerAreaId": owner, "participantAreaIds": participantes }),
    )
    .await?;
    areas_de_sprint(&st, &id).await?.map(Json).ok_or_else(|| ApiError::internal("Error interno"))
}

// ═══════════════════════════════ REORDENAR ═══════════════════════════════
async fn reordenar(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let sprint_id = s_o_nulo(&body, "sprintId");
    let ordenados: Option<Vec<String>> = body.get("orderedIds").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    let (Some(sprint_id), Some(ordenados)) = (sprint_id, ordenados) else {
        return Err(ApiError::bad_request("sprintId and orderedIds required"));
    };

    let mut tx = st.pool.begin().await?;
    let filas = sqlx::query_as::<_, (String, Option<String>)>(r#"SELECT id, "taskCode" FROM "BacklogItem" WHERE "sprintId" = $1 AND id = ANY($2::text[])"#)
        .bind(&sprint_id)
        .bind(&ordenados)
        .fetch_all(&mut *tx)
        .await?;
    // 1) códigos temporales (el código es único: evita choques al intercambiar posiciones)
    sqlx::query(r#"UPDATE "BacklogItem" SET "taskCode" = '__tmp_' || id, "updatedAt" = NOW() WHERE "sprintId" = $1 AND id = ANY($2::text[])"#)
        .bind(&sprint_id)
        .bind(&ordenados)
        .execute(&mut *tx)
        .await?;
    // 2) códigos finales en el orden del arrastre
    let mut finales: Vec<String> = vec![];
    let mut ids_finales: Vec<String> = vec![];
    let mut codigos: Vec<Option<String>> = vec![];
    for (idx, id) in ordenados.iter().enumerate() {
        let Some((_, codigo)) = filas.iter().find(|(fid, _)| fid == id) else { continue };
        let nuevo = match codigo {
            Some(c) => {
                let mut partes: Vec<String> = c.split('-').map(String::from).collect();
                if partes.len() >= 4 {
                    let ult = partes.len() - 1;
                    partes[ult] = format!("{:03}", idx + 1);
                    Some(partes.join("-"))
                } else {
                    Some(c.clone())
                }
            }
            None => None,
        };
        ids_finales.push(id.clone());
        codigos.push(nuevo.clone());
        finales.push(nuevo.unwrap_or_default());
    }
    for (id, cod) in ids_finales.iter().zip(codigos.iter()) {
        sqlx::query(r#"UPDATE "BacklogItem" SET "taskCode" = $2, "updatedAt" = NOW() WHERE id = $1"#).bind(id).bind(cod.as_deref()).execute(&mut *tx).await?;
    }
    tx.commit().await?;

    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg({ITEM_JSON} ORDER BY array_position($1::text[], b.id)), '[]'::jsonb) FROM "BacklogItem" b WHERE b.id = ANY($1::text[]) AND b."sprintId" = $2"#
    );
    let _ = finales;
    let arr = fetch_json_via(&st, &sql, &ids_finales, &sprint_id).await?;
    Ok(Json(arr))
}

async fn fetch_json_via(st: &AppState, sql: &str, ids: &[String], sprint_id: &str) -> Result<Value, sqlx::Error> {
    use sqlx::Row;
    let fila = sqlx::query(sql).bind(ids).bind(sprint_id).fetch_one(&st.pool).await?;
    let mut v: Value = fila.try_get(0)?;
    crate::util::fix_dates(&mut v);
    Ok(v)
}

// ═══════════════════════════════ ÁRBOL SOLUCIÓN → ÉPICAS → SPRINTS ═══════════════════════════════
async fn soluciones_arbol(State(st): State<AppState>, _s: Session) -> ApiResult<Json<std::sync::Arc<Value>>> {
    let sql = r#"SELECT COALESCE(jsonb_agg(to_jsonb(so) || jsonb_build_object('epics', COALESCE((
            SELECT jsonb_agg(to_jsonb(e) || jsonb_build_object(
                'sprints', COALESCE((SELECT jsonb_agg((to_jsonb(s) - 'metadata') || jsonb_build_object(
                                '_count', jsonb_build_object('items', (SELECT COUNT(*) FROM "BacklogItem" i WHERE i."sprintId" = s.id))))
                              FROM "Sprint" s WHERE s."epicId" = e.id), '[]'::jsonb),
                '_count', jsonb_build_object('sprints', (SELECT COUNT(*) FROM "Sprint" s2 WHERE s2."epicId" = e.id)))
              ORDER BY e."createdAt" DESC)
            FROM "Epic" e WHERE e."solucionId" = so.id), '[]'::jsonb)) ORDER BY so."createdAt" DESC), '[]'::jsonb)
        FROM "Solucion" so"#;
    Ok(Json(crate::util::fetch_json_cacheado(&st.pool, &CACHE, "arbol", sql, &[]).await?))
}

// ═══════════════════════════════ REGISTRO DE ESTADOS ═══════════════════════════════
async fn logs_listar(State(st): State<AppState>, sesion: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let Some(item) = q.get("itemId").filter(|x| !x.is_empty()) else {
        return Err(ApiError::bad_request("itemId requerido"));
    };
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(l) ORDER BY l."createdAt" DESC), '[]'::jsonb) FROM "BacklogItemLog" l WHERE l."itemId" = $1"#,
            &[B::T(item.clone())],
        )
        .await?,
    ))
}

async fn logs_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let nombre = if !sesion.name.is_empty() { sesion.name.clone() } else if !sesion.email.is_empty() { sesion.email.clone() } else { "unknown".to_string() };
    let log = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "BacklogItemLog" (id, "itemId", "fromStatus", "toStatus", note, "userName", "createdAt")
             VALUES ($1, $2, $3, $4, $5, $6, NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::OT(s(&body, "itemId")),
            B::OT(s(&body, "fromStatus")),
            B::OT(s(&body, "toStatus")),
            B::OT(s(&body, "note")),
            B::T(nombre),
        ],
    )
    .await?;
    Ok(Json(log))
}

// ═══════════════════════════════ GRAFO DEL SPRINT (Sala de Control) ═══════════════════════════════
async fn sprint_grafo(State(st): State<AppState>, _s: Session, Path(sprint_id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = r#"SELECT jsonb_build_object(
          'sprint', jsonb_build_object('id', s.id, 'name', s.name, 'goal', s.goal, 'sprintCode', s."sprintCode", 'status', s.status,
                                       'epicName', e.name, 'solucionNombre', so.nombre),
          'tasks', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                'id', bi.id, 'taskCode', bi."taskCode", 'title', bi.title, 'status', bi.status, 'assigneeName', bi."assigneeName",
                'dependsOnTaskId', bi."dependsOnTaskId", 'execId', te.id, 'startedAt', te."startedAt", 'finishedAt', te."finishedAt",
                'resultado', bi.resultado, 'checklist', CASE WHEN jsonb_typeof(te.artifacts) = 'object' THEN te.artifacts -> 'checklist' ELSE NULL END)
                ORDER BY bi."createdAt" ASC)
              FROM "BacklogItem" bi
              LEFT JOIN LATERAL (SELECT id, "startedAt", "finishedAt", artifacts FROM "TaskExecution"
                                 WHERE "backlogItemId" = bi.id ORDER BY "startedAt" DESC LIMIT 1) te ON true
              WHERE bi."sprintId" = s.id), '[]'::jsonb))
        FROM "Sprint" s
        LEFT JOIN "Epic" e ON s."epicId" = e.id
        LEFT JOIN "Solucion" so ON s."solucionId" = so.id
        WHERE s.id = $1"#;
    fetch_json_opt(&st.pool, sql, &[B::T(sprint_id)]).await?.map(Json).ok_or_else(|| ApiError::not_found("Sprint no encontrado"))
}

// ═══════════════════════════════ EJECUCIONES DE TAREA ═══════════════════════════════
async fn ejecuciones_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = r#"SELECT jsonb_build_object('executions', COALESCE(jsonb_agg(jsonb_build_object(
            'id', t.id, 'agentId', t."agentId", 'agentName', t."agentName", 'startedAt', t."startedAt", 'finishedAt', t."finishedAt",
            'status', t.status, 'resultSummary', t."resultSummary", 'durationMs', t."durationMs", 'contextUsed', t."contextUsed",
            'artifacts', t.artifacts, 'createdAt', t."createdAt") ORDER BY t."startedAt" DESC), '[]'::jsonb))
        FROM "TaskExecution" t WHERE t."backlogItemId" = $1"#;
    fetch_json(&st.pool, sql, &[B::T(id)]).await.map(Json).map_err(|e| {
        tracing::error!("ejecuciones: {e}");
        ApiError::internal("Error fetching executions")
    })
}

async fn ejecucion_crear(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<(axum::http::StatusCode, Json<Value>)> {
    let fallo = || ApiError::internal("Error creating execution");
    let estado = s(&body, "status").unwrap_or_else(|| "RUNNING".to_string());
    let resumen = s(&body, "resultSummary");
    let artifacts = match body.get("artifacts") {
        Some(v) if truthy(&body, "artifacts") => v.clone(),
        _ => json!([]),
    };
    let duracion = num_o_nulo(&body, "durationMs").filter(|_| body.get("durationMs").map(|v| !v.is_null()).unwrap_or(false));
    let exec_id = fetch_text_opt(
        &st.pool,
        r#"INSERT INTO "TaskExecution" (id, "backlogItemId", "agentId", "agentName", "startedAt", "finishedAt", status, "resultSummary", artifacts, "durationMs", "contextUsed")
           VALUES (gen_random_uuid()::text, $1, $2, $3, NOW(), CASE WHEN $4 IN ('DONE','FAILED') THEN NOW() ELSE NULL END, $4, $5, $6::text::jsonb, $7::float8::int, $8)
           RETURNING id"#,
        &[
            B::T(id.clone()),
            B::OT(s(&body, "agentId")),
            B::OT(s(&body, "agentName")),
            B::T(estado.clone()),
            B::OT(resumen.clone()),
            B::T(artifacts.to_string()),
            B::OF(duracion),
            B::OT(s(&body, "contextUsed")),
        ],
    )
    .await
    .map_err(|e| {
        tracing::error!("ejecucion_crear: {e}");
        fallo()
    })?
    .ok_or_else(fallo)?;

    // Sincroniza fechas y resultado del ítem según el estado final.
    let r = match estado.as_str() {
        "RUNNING" => exec(&st.pool, r#"UPDATE "BacklogItem" SET status = 'IN_PROGRESS', "fechaInicio" = NOW() WHERE id = $1 AND status = 'BACKLOG'"#, &[B::T(id)]).await,
        "DONE" => exec(&st.pool, r#"UPDATE "BacklogItem" SET status = 'DONE', "fechaFin" = NOW(), resultado = COALESCE($2, resultado) WHERE id = $1"#, &[B::T(id), B::OT(resumen)]).await,
        "FAILED" => exec(&st.pool, r#"UPDATE "BacklogItem" SET status = 'FAILED', "fechaFin" = NOW(), resultado = COALESCE($2, resultado) WHERE id = $1"#, &[B::T(id), B::OT(resumen)]).await,
        _ => Ok(0),
    };
    r.map_err(|e| {
        tracing::error!("ejecucion_crear (sync): {e}");
        fallo()
    })?;
    Ok((axum::http::StatusCode::CREATED, Json(json!({ "id": exec_id, "status": "created" }))))
}
