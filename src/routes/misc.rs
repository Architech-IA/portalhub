//! Rutas pequeñas que se piden en casi todas las pantallas: notificaciones, sesiones, búsqueda,
//! agentes, insignia del consejo, estadísticas, reportes, actividad, comentarios, áreas, hitos de
//! proyecto y pipeline — MASD PHUB-0001-0010 (bloque 1).
//! Paridad con las rutas de Next del mismo nombre. Lo que no se porta (chat de agentes, SSE,
//! estado de Hermes) se reenvía a Next con `proxy::a_next`.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{any, get, patch, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    proxy,
    session::Session,
    state::AppState,
    util::{
        exec, fecha_cuerpo, fetch_i64, fetch_json, fetch_json_opt, fetch_json_raw, log_activity_full, new_id, parse_float, presente, s,
        s_o_nulo, truthy, ts_js_opt, uid, Upd, B,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/notifications", get(notif_listar).post(notif_crear).patch(notif_marcar))
        .route("/api/sessions", get(sesiones_listar).post(sesion_crear))
        .route("/api/search", get(buscar))
        .route("/api/agents", get(agentes_listar).post(agente_crear))
        .route("/api/agents/status", any(proxy::a_next))
        .route("/api/agents/{slug}", get(agente_obtener).put(agente_actualizar).delete(agente_desactivar))
        .route("/api/council/badge", get(insignia_consejo))
        .route("/api/user-stats", get(user_stats))
        .route("/api/reportes", get(reportes))
        .route("/api/activities", get(actividades_listar).post(actividad_crear))
        .route("/api/graph", get(grafo))
        .route("/api/projects", get(proyectos_listar))
        .route("/api/comments", get(comentarios_listar).post(comentario_crear))
        .route("/api/areas", get(areas_listar))
        .route("/api/areas/{slug}", patch(area_actualizar))
        .route("/api/areas/{slug}/activity", get(area_actividad))
        .route("/api/milestones", post(hito_crear))
        .route("/api/milestones/{id}", patch(hito_estado).delete(hito_eliminar))
        .route("/api/pipeline/{id}", patch(pipeline_mover))
}

fn fallo(msg: &str) -> impl Fn(sqlx::Error) -> ApiError + '_ {
    move |e| {
        tracing::error!("{msg}: {e}");
        ApiError::internal(msg)
    }
}

// ═══════════════════════════════ NOTIFICACIONES ═══════════════════════════════
static CACHE: std::sync::LazyLock<crate::util::CacheJson> = std::sync::LazyLock::new(Default::default);

async fn notif_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<std::sync::Arc<Value>>> {
    let uid_f = q.get("userId").filter(|x| !x.is_empty()).cloned();
    let clave = format!("notif:{}", uid_f.clone().unwrap_or_default());
    Ok(Json(
        crate::util::fetch_json_cacheado(
            &st.pool,
            &CACHE,
            &clave,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC), '[]'::jsonb)
               FROM (SELECT * FROM "Notification" n WHERE ($1::text IS NULL OR n."userId" = $1) ORDER BY n."createdAt" DESC LIMIT 50) x"#,
            &[B::OT(uid_f)],
        )
        .await?,
    ))
}

async fn notif_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"WITH ins AS (INSERT INTO "Notification" (id, "userId", type, title, message, link, read, "createdAt")
                 VALUES ($1, $2, COALESCE($3, 'info'), $4, $5, $6, false, NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
            &[
                B::T(new_id()),
                B::OT(s(&body, "userId")),
                B::OT(s_o_nulo(&body, "type")),
                B::OT(s(&body, "title")),
                B::OT(s(&body, "message")),
                B::OT(s_o_nulo(&body, "link")),
            ],
        )
        .await?,
    ))
}

async fn notif_marcar(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if let Some(id) = s_o_nulo(&body, "id") {
        // Sin `read` en el cuerpo Prisma no cambia nada (pero sí falla si el id no existe).
        let n = match body.get("read").and_then(|v| v.as_bool()) {
            Some(r) => exec(&st.pool, r#"UPDATE "Notification" SET read = $2 WHERE id = $1"#, &[B::T(id), B::Bo(r)]).await?,
            None => crate::util::fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "Notification" WHERE id = $1"#, &[B::T(id)]).await? as u64,
        };
        if n == 0 {
            return Err(ApiError::internal("Error interno"));
        }
    } else if let Some(u) = s_o_nulo(&body, "userId") {
        // Corrección deliberada: en Next este camino volvía a leer el cuerpo (ya consumido) y
        // fallaba con 500; acá marca todas las notificaciones del usuario como leídas.
        exec(&st.pool, r#"UPDATE "Notification" SET read = true WHERE "userId" = $1 AND read = false"#, &[B::T(u)]).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ SESIONES (registro de acceso) ═══════════════════════════════
async fn sesiones_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let limite: i64 = q.get("limit").and_then(|x| x.parse().ok()).unwrap_or(100);
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC), '[]'::jsonb)
               FROM (SELECT * FROM "SessionLog" ORDER BY "createdAt" DESC LIMIT $1) x"#,
            &[B::I(limite)],
        )
        .await?,
    ))
}

async fn sesion_crear(State(st): State<AppState>, _s: Session, headers: HeaderMap, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()).map(String::from))
        .unwrap_or_else(|| "unknown".into());
    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("unknown").to_string();
    let exito = body.get("success").map(|v| v != &Value::Bool(false)).unwrap_or(true);
    let fila = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "SessionLog" (id, "userId", email, action, ip, "userAgent", success, details, "createdAt")
             VALUES ($1, $2, $3, COALESCE($4, 'LOGOUT'), $5, $6, $7, $8, NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::OT(s_o_nulo(&body, "userId")),
            B::OT(s_o_nulo(&body, "email")),
            B::OT(s_o_nulo(&body, "action")),
            B::T(ip),
            B::T(ua),
            B::Bo(exito),
            B::OT(s_o_nulo(&body, "details")),
        ],
    )
    .await
    .map_err(fallo("Failed to log session"))?;
    Ok(Json(fila))
}

// ═══════════════════════════════ BÚSQUEDA GLOBAL ═══════════════════════════════
async fn buscar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let q = q.get("q").map(|x| x.trim().to_string()).unwrap_or_default();
    if q.chars().count() < 2 {
        return Ok(Json(json!({ "leads": [], "proposals": [], "projects": [], "clientes": [], "backlog": [], "meetings": [] })));
    }
    // `contains` de Prisma = coincidencia de subcadena sensible a mayúsculas.
    let sql = r#"SELECT jsonb_build_object(
        'leads', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'companyName', x."companyName", 'contactName', x."contactName", 'status', x.status, 'estimatedValue', x."estimatedValue"))
            FROM (SELECT * FROM "Lead" WHERE position($1 in "companyName") > 0 OR position($1 in "contactName") > 0 OR position($1 in email) > 0 LIMIT 5) x), '[]'::jsonb),
        'proposals', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'title', x.title, 'status', x.status, 'amount', x.amount))
            FROM (SELECT * FROM "Proposal" WHERE position($1 in title) > 0 OR position($1 in description) > 0 LIMIT 5) x), '[]'::jsonb),
        'projects', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'name', x.name, 'status', x.status, 'progress', x.progress))
            FROM (SELECT * FROM "Project" WHERE position($1 in name) > 0 OR position($1 in description) > 0 LIMIT 5) x), '[]'::jsonb),
        'clientes', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'nombre', x.nombre, 'industria', x.industria, 'estado', x.estado))
            FROM (SELECT * FROM "Cliente" WHERE position($1 in nombre) > 0 OR position($1 in contacto) > 0 OR position($1 in email) > 0 LIMIT 5) x), '[]'::jsonb),
        'backlog', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'title', x.title, 'status', x.status, 'type', x.type))
            FROM (SELECT * FROM "BacklogItem" WHERE position($1 in title) > 0 OR position($1 in description) > 0 LIMIT 5) x), '[]'::jsonb),
        'meetings', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'title', x.title, 'date', x.date, 'type', x.type))
            FROM (SELECT * FROM "Meeting" WHERE position($1 in title) > 0 OR position($1 in notes) > 0 LIMIT 5) x), '[]'::jsonb))"#;
    Ok(Json(fetch_json(&st.pool, sql, &[B::T(q)]).await?))
}

// ═══════════════════════════════ AGENTES ═══════════════════════════════
async fn agentes_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.name ASC), '[]'::jsonb) FROM "Agent" a"#, &[]).await?))
}

fn lista_texto(body: &Value, k: &str) -> Value {
    match body.get(k) {
        Some(Value::Array(a)) => Value::Array(a.iter().filter(|x| x.is_string()).cloned().collect()),
        _ => json!([]),
    }
}

async fn agente_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (Some(slug), Some(name), Some(role), Some(area), Some(pers)) = (
        s_o_nulo(&body, "slug"),
        s_o_nulo(&body, "name"),
        s_o_nulo(&body, "role"),
        s_o_nulo(&body, "area"),
        s_o_nulo(&body, "personality"),
    ) else {
        return Err(ApiError::bad_request("Faltan campos obligatorios"));
    };
    let vault = s(&body, "vaultPath").unwrap_or_else(|| format!("/agents/{slug}/"));
    let fila = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "Agent" (id, slug, name, role, area, personality, "systemPrompt", "llmModel", "taskTypes", repos, "discordUserId", "vaultPath", status, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                     COALESCE(ARRAY(SELECT jsonb_array_elements_text($9::jsonb)), '{}'), COALESCE(ARRAY(SELECT jsonb_array_elements_text($10::jsonb)), '{}'),
                     $11, $12, 'ACTIVE', NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::T(slug),
            B::T(name),
            B::T(role),
            B::T(area),
            B::T(pers),
            B::OT(s(&body, "systemPrompt")),
            B::OT(s_o_nulo(&body, "llmModel")),
            B::J(lista_texto(&body, "taskTypes")),
            B::J(lista_texto(&body, "repos")),
            B::OT(s(&body, "discordUserId")),
            B::T(vault),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(fila)))
}

async fn agente_obtener(State(st): State<AppState>, _s: Session, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    fetch_json_opt(&st.pool, r#"SELECT to_jsonb(a) FROM "Agent" a WHERE a.slug = $1"#, &[B::T(slug)])
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No encontrado"))
}

async fn agente_actualizar(State(st): State<AppState>, _s: Session, Path(slug): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&slug);
    for k in ["slug", "name", "role", "area", "personality", "systemPrompt", "llmModel", "discordUserId", "vaultPath", "status", "areaId"] {
        match body.get(k) {
            Some(Value::String(v)) => up.set(k, B::T(v.clone())),
            Some(Value::Null) => up.set(k, B::OT(None)),
            _ => {}
        }
    }
    for k in ["taskTypes", "repos"] {
        if body.get(k).map(|v| v.is_array()).unwrap_or(false) {
            up.set_expr(k, B::J(lista_texto(&body, k)), "COALESCE(ARRAY(SELECT jsonb_array_elements_text({n}::jsonb)), '{}')");
        }
    }
    let sql = up.sql("Agent").replace("WHERE id = $1", "WHERE slug = $1");
    fetch_json_opt(&st.pool, &sql, &up.binds).await?.map(Json).ok_or_else(|| ApiError::internal("Error interno"))
}

async fn agente_desactivar(State(st): State<AppState>, _s: Session, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    if exec(&st.pool, r#"UPDATE "Agent" SET status = 'INACTIVE', "updatedAt" = NOW() WHERE slug = $1"#, &[B::T(slug)]).await? == 0 {
        return Err(ApiError::internal("Error interno"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ INSIGNIA DEL CONSEJO ═══════════════════════════════
async fn insignia_consejo(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT jsonb_build_object('debating', d, 'escalated', e, 'total', d + e)
               FROM (SELECT COUNT(*) FILTER (WHERE status = 'DEBATING') d, COUNT(*) FILTER (WHERE status = 'ESCALATED') e FROM "CouncilProposal") t"#,
            &[],
        )
        .await?,
    ))
}

// ═══════════════════════════════ ESTADÍSTICAS DE USUARIO ═══════════════════════════════
async fn user_stats(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let Some(user_id) = q.get("userId").filter(|x| !x.is_empty()).cloned() else {
        return Err(ApiError::bad_request("userId requerido"));
    };
    let sql = r#"SELECT jsonb_build_object(
        'user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email, 'role', u.role, 'avatar', u.avatar, 'createdAt', u."createdAt", 'updatedAt', u."updatedAt",
                    '_count', jsonb_build_object(
                        'leads', (SELECT COUNT(*) FROM "Lead" WHERE "userId" = u.id),
                        'proposals', (SELECT COUNT(*) FROM "Proposal" WHERE "userId" = u.id),
                        'projects', (SELECT COUNT(*) FROM "ProjectUser" WHERE "userId" = u.id),
                        'activities', (SELECT COUNT(*) FROM "Activity" WHERE "userId" = u.id)))
                 FROM "User" u WHERE u.id = $1),
        'stats', jsonb_build_object(
            'totalLeadValue', COALESCE((SELECT SUM("estimatedValue") FROM "Lead" WHERE "userId" = $1), 0),
            'totalProposalAmount', COALESCE((SELECT SUM(amount) FROM "Proposal" WHERE "userId" = $1), 0),
            'acceptedProposals', (SELECT COUNT(*) FROM "Proposal" WHERE "userId" = $1 AND status = 'ACCEPTED')),
        'leads', COALESCE((SELECT jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC) FROM (SELECT id, "companyName", "contactName", status, "estimatedValue", "createdAt" FROM "Lead" WHERE "userId" = $1 ORDER BY "createdAt" DESC LIMIT 5) x), '[]'::jsonb),
        'proposals', COALESCE((SELECT jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC) FROM (SELECT id, title, status, amount, "createdAt" FROM "Proposal" WHERE "userId" = $1 ORDER BY "createdAt" DESC LIMIT 5) x), '[]'::jsonb),
        'projects', COALESCE((SELECT jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC) FROM (SELECT p.id, p.name, p.status, p.priority, p.progress, p."createdAt" FROM "Project" p
                       WHERE EXISTS (SELECT 1 FROM "ProjectUser" pu WHERE pu."projectId" = p.id AND pu."userId" = $1) ORDER BY p."createdAt" DESC LIMIT 5) x), '[]'::jsonb))"#;
    let mut v = fetch_json(&st.pool, sql, &[B::T(user_id)]).await?;
    if v["user"].is_null() {
        return Err(ApiError::not_found("Usuario no encontrado"));
    }
    let usuario = v["user"].take();
    let mut out = serde_json::Map::new();
    out.insert("user".into(), usuario);
    for k in ["stats", "leads", "proposals", "projects"] {
        out.insert(k.into(), v[k].take());
    }
    Ok(Json(Value::Object(out)))
}

// ═══════════════════════════════ REPORTES ═══════════════════════════════
fn dias_desde_civil(y: i64, m: i64, d: i64) -> i64 {
    // Algoritmo de Howard Hinnant (días desde 1970-01-01).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_desde_dias(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn dias_del_mes(y: i64, m: i64) -> i64 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    dias_desde_civil(ny, nm, 1) - dias_desde_civil(y, m, 1)
}

/// `Math.round` de JavaScript.
fn redondear(x: f64) -> f64 {
    (x + 0.5).floor()
}

async fn reportes(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let datos = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
            'leads', COALESCE((SELECT jsonb_agg(jsonb_build_object('status', status, 'outcome', outcome, 'valor', "estimatedValue",
                         'creado', EXTRACT(EPOCH FROM "createdAt"))) FROM "Lead"), '[]'::jsonb),
            'propuestas', COALESCE((SELECT jsonb_agg(jsonb_build_object('status', status, 'monto', amount)) FROM "Proposal"), '[]'::jsonb),
            'registros', COALESCE((SELECT jsonb_agg(jsonb_build_object('monto', monto, 'fecha', fecha, 'categoria', categoria))
                                   FROM "RegistroFinanciero" WHERE tipo = 'ingreso' AND estado <> 'cancelado'), '[]'::jsonb),
            'socios', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'name', x.name, 'role', x.role,
                                    '_count', jsonb_build_object('leads', (SELECT COUNT(*) FROM "Lead" WHERE "userId" = x.id), 'proposals', (SELECT COUNT(*) FROM "Proposal" WHERE "userId" = x.id))))
                                FROM (SELECT id, name, role FROM "User" LIMIT 6) x), '[]'::jsonb))"#,
        &[],
    )
    .await?;
    let leads = datos["leads"].as_array().cloned().unwrap_or_default();
    let props = datos["propuestas"].as_array().cloned().unwrap_or_default();
    let regs = datos["registros"].as_array().cloned().unwrap_or_default();
    let f = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
    let st_of = |v: &Value| v["status"].as_str().unwrap_or("").to_string();
    let out_of = |v: &Value| v["outcome"].as_str().map(String::from);
    let ganado = |l: &Value| st_of(l) == "RESULT" && out_of(l).as_deref() == Some("WON");
    let perdido = |l: &Value| st_of(l) == "RESULT" && out_of(l).as_deref() == Some("LOST");

    let total_leads = leads.len() as f64;
    let n_ganados = leads.iter().filter(|l| ganado(l)).count() as f64;
    let n_perdidos = leads.iter().filter(|l| perdido(l)).count() as f64;
    let win_rate = if total_leads > 0.0 { redondear(n_ganados / total_leads * 100.0) } else { 0.0 };
    let avg_deal = if n_ganados > 0.0 { redondear(leads.iter().filter(|l| ganado(l)).map(|l| f(l, "valor")).sum::<f64>() / n_ganados) } else { 0.0 };
    let total_pipeline: f64 = leads.iter().filter(|l| !perdido(l)).map(|l| f(l, "valor")).sum();
    let total_props = props.len() as f64;
    let aceptadas = props.iter().filter(|p| st_of(p) == "ACCEPTED").count() as f64;
    let acept_rate = if total_props > 0.0 { redondear(aceptadas / total_props * 100.0) } else { 0.0 };
    let total_ingresos: f64 = regs.iter().map(|r| f(r, "monto")).sum();

    // Últimos 6 meses (con el desborde de `setMonth` de JavaScript cuando el día no existe).
    let hoy_dias = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64 / 86400).unwrap_or(0);
    let (hy, hm, hd) = civil_desde_dias(hoy_dias);
    const MESES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
    let mut revenue_mensual = vec![];
    for i in 0..6i64 {
        let idx = (hy * 12 + (hm - 1)) - (5 - i);
        let (ty, tm) = (idx.div_euclid(12), idx.rem_euclid(12) + 1);
        let dias = dia_desborde(ty, tm, hd);
        let (y, m, _) = civil_desde_dias(dias);
        let ini = format!("{y}-{m:02}-01");
        let fin = format!("{y}-{m:02}-31");
        let ingresos: f64 = regs.iter().filter(|r| r["fecha"].as_str().map(|x| x >= ini.as_str() && x <= fin.as_str()).unwrap_or(false)).map(|r| f(r, "monto")).sum();
        // `new Date(`${y}-${m}-31T23:59:59`)` se desborda al mes siguiente si el mes tiene menos de 31 días.
        let ini_e = (dias_desde_civil(y, m, 1) * 86400) as f64;
        let fin_e = ini_e + (30 * 86400 + 86399) as f64;
        let deals = leads.iter().filter(|l| ganado(l) && f(l, "creado") >= ini_e && f(l, "creado") <= fin_e).count();
        revenue_mensual.push(json!({ "mes": format!("{} {:02}", MESES[(m - 1) as usize], y % 100), "ingresos": ingresos, "deals": deals }));
    }

    let etapas = [("NEW", "Nuevo"), ("CONTACTED", "Contactado"), ("DIAGNOSIS", "Diagnóstico"), ("DEMO_VALIDATION", "Demo"), ("PROPOSAL_SENT", "Propuesta"), ("NEGOTIATION", "Negociación")];
    let mut pipeline: Vec<Value> = etapas
        .iter()
        .map(|(c, label)| {
            let del: Vec<&Value> = leads.iter().filter(|l| st_of(l) == *c).collect();
            json!({ "status": c, "label": label, "count": del.len(), "valor": del.iter().map(|l| f(l, "valor")).sum::<f64>() })
        })
        .collect();
    let g: Vec<&Value> = leads.iter().filter(|l| ganado(l)).collect();
    let p: Vec<&Value> = leads.iter().filter(|l| perdido(l)).collect();
    let sd: Vec<&Value> = leads.iter().filter(|l| st_of(l) == "RESULT" && out_of(l).is_none()).collect();
    pipeline.push(json!({ "status": "RESULT_WON", "label": "Ganado", "count": g.len(), "valor": g.iter().map(|l| f(l, "valor")).sum::<f64>() }));
    pipeline.push(json!({ "status": "RESULT_LOST", "label": "Perdido", "count": p.len(), "valor": p.iter().map(|l| f(l, "valor")).sum::<f64>() }));
    pipeline.push(json!({ "status": "RESULT_PENDING", "label": "Resultado (sin definir)", "count": sd.len(), "valor": sd.iter().map(|l| f(l, "valor")).sum::<f64>() }));
    pipeline.retain(|e| e["count"].as_u64().unwrap_or(0) > 0);

    let mut cats: Vec<(String, f64)> = vec![];
    for r in &regs {
        let c = r["categoria"].as_str().unwrap_or("").to_string();
        match cats.iter_mut().find(|(k, _)| *k == c) {
            Some(e) => e.1 += f(r, "monto"),
            None => cats.push((c, f(r, "monto"))),
        }
    }
    cats.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let revenue_cat: Vec<Value> = cats.into_iter().take(6).map(|(cat, total)| json!({ "cat": cat, "total": total })).collect();

    let prop_estados: Vec<Value> = ["DRAFT", "SENT", "UNDER_REVIEW", "ACCEPTED", "REJECTED"]
        .iter()
        .map(|e| {
            let del: Vec<&Value> = props.iter().filter(|p| st_of(p) == *e).collect();
            json!({ "status": e, "count": del.len(), "valor": del.iter().map(|p| f(p, "monto")).sum::<f64>() })
        })
        .collect();

    Ok(Json(json!({
        "kpis": { "totalLeads": total_leads, "leadsGanados": n_ganados, "leadsPerdidos": n_perdidos, "winRate": win_rate, "avgDealSize": avg_deal,
                  "totalPipeline": total_pipeline, "totalProposals": total_props, "propAceptadas": aceptadas, "acceptanceRate": acept_rate,
                  "totalIngresos": total_ingresos },
        "revenueMensual": revenue_mensual,
        "pipelineEtapas": pipeline,
        "revenueCategorias": revenue_cat,
        "propEstados": prop_estados,
        "topSocios": datos["socios"],
    })))
}

/// Día (como número de días desde 1970) de `y-m-d` con el desborde de JavaScript si `d` no existe en ese mes.
fn dia_desborde(y: i64, m: i64, d: i64) -> i64 {
    dias_desde_civil(y, m, 1) + (d - 1)
}

// ═══════════════════════════════ ACTIVIDAD ═══════════════════════════════
async fn actividades_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<std::sync::Arc<Value>>> {
    Ok(Json(
        crate::util::fetch_json_cacheado(
            &st.pool,
            &CACHE,
            "actividades",
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) || jsonb_build_object('user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email) FROM "User" u WHERE u.id = x."userId"))
                                        ORDER BY x."createdAt" DESC), '[]'::jsonb)
               FROM (SELECT * FROM "Activity" ORDER BY "createdAt" DESC LIMIT 50) x"#,
            &[],
        )
        .await?,
    ))
}

async fn actividad_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"WITH ins AS (INSERT INTO "Activity" (id, type, description, "entityType", "entityId", "userId", "createdAt")
                 VALUES ($1, $2::"ActivityType", $3, $4, $5, $6, NOW()) RETURNING *)
               SELECT to_jsonb(ins) || jsonb_build_object('user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email) FROM "User" u WHERE u.id = ins."userId")) FROM ins"#,
            &[
                B::T(new_id()),
                B::OT(s(&body, "type")),
                B::OT(s(&body, "description")),
                B::OT(s(&body, "entityType")),
                B::OT(s(&body, "entityId")),
                B::OT(s(&body, "userId")),
            ],
        )
        .await?,
    ))
}

async fn grafo(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let v = fetch_json_raw(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'nodes', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', id, 'path', path, 'title', title, 'folder', folder, 'updated_at', updated_at) ORDER BY folder, title) FROM obsidian_nodes), '[]'::jsonb),
             'links', COALESCE((SELECT jsonb_agg(jsonb_build_object('source_path', source_path, 'target_title', target_title, 'target_path', target_path)) FROM obsidian_links WHERE target_path IS NOT NULL), '[]'::jsonb))"#,
        &[],
    )
    .await?
    .unwrap_or_else(|| json!({ "nodes": [], "links": [] }));
    Ok(Json(v))
}

async fn proyectos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    // Next devuelve [] (200) ante cualquier error.
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', p.id, 'name', p.name, 'status', p.status) ORDER BY p.name ASC), '[]'::jsonb) FROM "Project" p"#,
        &[],
    )
    .await
    .unwrap_or_else(|_| json!([]));
    Ok(Json(v))
}

// ═══════════════════════════════ COMENTARIOS ═══════════════════════════════
const USUARIO_COMENT: &str = r#"(SELECT jsonb_build_object('id', u.id, 'name', u.name, 'avatar', u.avatar) FROM "User" u WHERE u.id = c."userId")"#;

async fn comentarios_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let (Some(tipo), Some(id)) = (q.get("entityType").filter(|x| !x.is_empty()), q.get("entityId").filter(|x| !x.is_empty())) else {
        return Ok(Json(json!([])));
    };
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) || jsonb_build_object('user', {USUARIO_COMENT},
              'replies', COALESCE((SELECT jsonb_agg(to_jsonb(r) || jsonb_build_object('user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'avatar', u.avatar) FROM "User" u WHERE u.id = r."userId"))
                                    ORDER BY r."createdAt" ASC) FROM "Comment" r WHERE r."parentId" = c.id), '[]'::jsonb))
              ORDER BY c."createdAt" DESC), '[]'::jsonb)
           FROM "Comment" c WHERE c."entityType" = $1 AND c."entityId" = $2 AND c."parentId" IS NULL"#
    );
    Ok(Json(fetch_json(&st.pool, &sql, &[B::T(tipo.clone()), B::T(id.clone())]).await?))
}

async fn comentario_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"WITH ins AS (INSERT INTO "Comment" (id, text, "entityType", "entityId", "parentId", "userId", "createdAt")
             VALUES ($1, $2, $3, $4, $5, $6, NOW()) RETURNING *)
           SELECT to_jsonb(c) || jsonb_build_object('user', {USUARIO_COMENT}) FROM ins c"#
    );
    Ok(Json(
        fetch_json(
            &st.pool,
            &sql,
            &[
                B::T(new_id()),
                B::OT(s(&body, "text")),
                B::OT(s(&body, "entityType")),
                B::OT(s(&body, "entityId")),
                B::OT(s_o_nulo(&body, "parentId")),
                B::OT(s(&body, "userId")),
            ],
        )
        .await?,
    ))
}

// ═══════════════════════════════ ÁREAS ═══════════════════════════════
async fn areas_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let filas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(t.j ORDER BY t.ord1 NULLS FIRST, t.ord2 ASC NULLS LAST, t.ord3), '[]'::jsonb) FROM (
             SELECT jsonb_build_object('id', a.id, 'name', a.name, 'slug', a.slug, 'icon', a.icon, 'color', a.color, 'description', a.description,
                        'parentAreaId', a."parentAreaId", 'agentId', ag.id, 'agentName', ag.name, 'agentSlug', ag.slug, 'agentStatus', ag.status,
                        'activeItems', COUNT(bi.id) FILTER (WHERE bi.status NOT IN ('DONE','CANCELLED')),
                        'inProgressItems', COUNT(bi.id) FILTER (WHERE bi.status = 'IN_PROGRESS')) j,
                    a."parentAreaId" ord1, a."sortOrder" ord2, a.name ord3
             FROM "Area" a LEFT JOIN "Agent" ag ON ag."areaId" = a.id LEFT JOIN "BacklogItem" bi ON bi."areaId" = a.id
             GROUP BY a.id, a.name, a.slug, a.icon, a.color, a.description, a."parentAreaId", a."sortOrder", ag.id, ag.name, ag.slug, ag.status) t"#,
        &[],
    )
    .await?;
    let filas = filas.as_array().cloned().unwrap_or_default();
    let (principales, subs): (Vec<Value>, Vec<Value>) = filas.into_iter().partition(|a| a["parentAreaId"].is_null());
    let resultado: Vec<Value> = principales
        .into_iter()
        .map(|mut a| {
            let hijos: Vec<Value> = subs.iter().filter(|s| s["parentAreaId"] == a["id"]).cloned().collect();
            a["subAreas"] = Value::Array(hijos);
            a
        })
        .collect();
    Ok(Json(Value::Array(resultado)))
}

async fn area_actualizar(State(st): State<AppState>, _s: Session, Path(slug): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let fallo = |e: sqlx::Error| {
        tracing::error!("area_actualizar: {e}");
        ApiError::internal("Error updating area")
    };
    exec(
        &st.pool,
        r#"UPDATE "Area" SET "defaultAgentId" = COALESCE($2, "defaultAgentId"), "defaultAgentName" = COALESCE($3, "defaultAgentName"),
                              "executionStrategy" = COALESCE($4, "executionStrategy") WHERE id = $1 OR slug = $1"#,
        &[B::T(slug.clone()), B::OT(s(&body, "defaultAgentId")), B::OT(s(&body, "defaultAgentName")), B::OT(s(&body, "executionStrategy"))],
    )
    .await
    .map_err(fallo)?;
    let fila = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', id, 'name', name, 'slug', slug, 'defaultAgentId', "defaultAgentId", 'defaultAgentName', "defaultAgentName", 'executionStrategy', "executionStrategy")
           FROM "Area" WHERE id = $1 OR slug = $1"#,
        &[B::T(slug)],
    )
    .await
    .map_err(fallo)?;
    Ok(Json(fila.unwrap_or_else(|| json!({ "error": "not found" }))))
}

async fn area_actividad(State(st): State<AppState>, _s: Session, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    let Some(area_id) = crate::util::fetch_text_opt(&st.pool, r#"SELECT id FROM "Area" WHERE slug = $1 LIMIT 1"#, &[B::T(slug)]).await? else {
        return Err(ApiError::not_found("Not found"));
    };
    let filas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', bi.id, 'title', bi.title, 'status', bi.status, 'updatedAt', bi."updatedAt", 'createdAt', bi."createdAt",
                  'type', bi.type, 'priority', bi.priority, 'assigneeName', bi."assigneeName", 'sprintCode', s."sprintCode", 'sprintName', s.name,
                  'diffMs', ABS(EXTRACT(EPOCH FROM (bi."createdAt" - bi."updatedAt"))) * 1000) ORDER BY bi."updatedAt" DESC), '[]'::jsonb)
           FROM (SELECT * FROM "BacklogItem" WHERE "areaId" = $1 OR "areaId" IN (SELECT id FROM "Area" WHERE "parentAreaId" = $1) ORDER BY "updatedAt" DESC LIMIT 40) bi
           LEFT JOIN "Sprint" s ON s.id = bi."sprintId""#,
        &[B::T(area_id.clone())],
    )
    .await?;
    let eventos: Vec<Value> = filas
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|it| {
            let estado = it["status"].as_str().unwrap_or("");
            let (tipo, etiqueta) = match estado {
                "DONE" => ("completed", "Completado"),
                "IN_PROGRESS" => ("started", "En progreso"),
                "BACKLOG" if it["diffMs"].as_f64().unwrap_or(f64::MAX) < 2000.0 => ("created", "Creado"),
                _ => ("updated", "Actualizado"),
            };
            json!({ "id": it["id"], "type": tipo, "label": etiqueta, "title": it["title"], "status": it["status"], "priority": it["priority"],
                    "itemType": it["type"], "sprint": it["sprintCode"], "sprintName": it["sprintName"], "assigneeName": it["assigneeName"],
                    "timestamp": it["updatedAt"] })
        })
        .collect();
    Ok(Json(json!({ "areaId": area_id, "events": eventos })))
}

// ═══════════════════════════════ HITOS DE PROYECTO ═══════════════════════════════
async fn hito_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let user = sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autorizado"))?.to_string();
    let (Some(name), Some(project_id)) = (s_o_nulo(&body, "name"), s_o_nulo(&body, "projectId")) else {
        return Err(ApiError::bad_request("Faltan campos"));
    };
    let sql = format!(
        r#"WITH ins AS (INSERT INTO "Milestone" (id, name, "projectId", "dueDate", status, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, {}, 'PENDING', NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        ts_js_opt(4)
    );
    let m = fetch_json(&st.pool, &sql, &[B::T(new_id()), B::T(name.clone()), B::T(project_id.clone()), B::OT(fecha_cuerpo(&body, "dueDate"))]).await?;
    let mid = m["id"].as_str().unwrap_or_default().to_string();
    log_activity_full(&st.pool, "CREATED", &format!("creó el hito {name}"), "milestone", &mid, Some(&user), None, None, Some(&project_id)).await;
    Ok(Json(m))
}

async fn hito_estado(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let user = sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autorizado"))?.to_string();
    let estado = s(&body, "status");
    let completado = estado.as_deref() == Some("COMPLETED");
    let m = fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "Milestone" SET status = $2::"MilestoneStatus", "completedDate" = CASE WHEN $3::bool THEN NOW() ELSE NULL END, "updatedAt" = NOW()
             WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#,
        &[B::T(id.clone()), B::OT(estado.clone()), B::Bo(completado)],
    )
    .await?
    .ok_or_else(|| ApiError::internal("Error interno"))?;
    let tipo = if completado { "MILESTONE_COMPLETED" } else { "STATUS_CHANGED" };
    log_activity_full(
        &st.pool,
        tipo,
        &format!("cambió el estado del hito {} a {}", m["name"].as_str().unwrap_or(""), estado.unwrap_or_default()),
        "milestone",
        &id,
        Some(&user),
        None,
        None,
        m["projectId"].as_str(),
    )
    .await;
    Ok(Json(m))
}

async fn hito_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let user = sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autorizado"))?.to_string();
    let previo = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('name', name, 'projectId', "projectId") FROM "Milestone" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    if exec(&st.pool, r#"DELETE FROM "Milestone" WHERE id = $1"#, &[B::T(id.clone())]).await? == 0 {
        return Err(ApiError::internal("Error interno"));
    }
    let nombre = previo.as_ref().and_then(|p| p["name"].as_str()).unwrap_or("undefined");
    log_activity_full(&st.pool, "UPDATED", &format!("eliminó el hito {nombre}"), "milestone", &id, Some(&user), None, None, previo.as_ref().and_then(|p| p["projectId"].as_str())).await;
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ PIPELINE ═══════════════════════════════
async fn pipeline_mover(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autorizado"))?;
    let estado = s(&body, "status");
    // `outcome` solo existe en la fase RESULT; en cualquier otra columna se limpia.
    let outcome = if estado.as_deref() == Some("RESULT") { s(&body, "outcome") } else { None };
    let fila = fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "Lead" SET status = $2::"LeadStatus", outcome = $3, "updatedAt" = NOW() WHERE id = $1 RETURNING *)
           SELECT to_jsonb(up) || jsonb_build_object('user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email) FROM "User" u WHERE u.id = up."userId")) FROM up"#,
        &[B::T(id), B::OT(estado), B::OT(outcome)],
    )
    .await
    .map_err(fallo("Error al actualizar estado"))?
    .ok_or_else(|| ApiError::internal("Error al actualizar estado"))?;
    Ok(Json(fila))
}

#[allow(dead_code)]
fn _no_usados() {
    let _ = (truthy, presente, parse_float, fetch_i64, uid);
}
