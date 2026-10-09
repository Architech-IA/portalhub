//! Aplicaciones de clientes (apps, tipos, eventos), métricas de los VPS, repos de GitHub,
//! resumen público, pantalla de métricas y estado de servicios externos — MASD PHUB-0001-0011.
//! Paridad con `src/app/api/{apps,app-types,eventos-app,vps,vps2,github,metrics,public-summary,status}/**`.
//!
//! OJO: `vps`, `vps2`, `github`, `public-summary` y `metrics` son rutas PÚBLICAS en Next
//! (`PUBLIC_PATHS` de proxy.ts: no exigen sesión). Se conserva igual para que la pantalla de
//! métricas y los consumidores externos sigan funcionando; endurecerlo es una decisión aparte.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, new_id, s, truthy, uid, Upd, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/apps", get(apps_listar).post(app_crear))
        .route("/api/apps/by-slug/{slug}", get(app_por_slug))
        .route("/api/apps/{id}", get(app_obtener).patch(app_actualizar).delete(app_eliminar))
        .route("/api/app-types", get(tipos_listar))
        .route("/api/eventos-app", get(eventos_listar).post(evento_crear))
        .route("/api/vps/stats", get(|s: State<AppState>| vps_stats(s, 0)))
        .route("/api/vps/docker", get(|s: State<AppState>| vps_docker(s, 0)))
        .route("/api/vps/history", get(|s: State<AppState>| vps_historial(s, 0)))
        .route("/api/vps/logs", get(|s: State<AppState>| vps_logs(s, 0)))
        .route("/api/vps2/stats", get(|s: State<AppState>| vps_stats(s, 1)))
        .route("/api/vps2/docker", get(|s: State<AppState>| vps_docker(s, 1)))
        .route("/api/vps2/history", get(|s: State<AppState>| vps_historial(s, 1)))
        .route("/api/vps2/logs", get(|s: State<AppState>| vps_logs(s, 1)))
        .route("/api/github/repos", get(github_repos))
        .route("/api/metrics/display", get(metricas_pantalla))
        .route("/api/public-summary", get(resumen_publico).options(resumen_opciones))
        .route("/api/status", get(estado_servicios))
}

// ═══════════════════════════════ APLICACIONES ═══════════════════════════════
/// App con su tipo completo y los datos cortos de sus relaciones.
const APP_LISTA: &str = r#"to_jsonb(a) || jsonb_build_object(
    'appType', (SELECT to_jsonb(t) FROM "AppType" t WHERE t.id = a."appTypeId"),
    'owner',    (SELECT jsonb_build_object('name', u.name) FROM "User" u WHERE u.id = a."ownerId"),
    'lead',     (SELECT jsonb_build_object('companyName', l."companyName") FROM "Lead" l WHERE l.id = a."leadId"),
    'proposal', (SELECT jsonb_build_object('title', p.title) FROM "Proposal" p WHERE p.id = a."proposalId"),
    'project',  (SELECT jsonb_build_object('name', pr.name) FROM "Project" pr WHERE pr.id = a."projectId"),
    'cliente',  (SELECT jsonb_build_object('nombre', c.nombre) FROM "Cliente" c WHERE c.id = a."clienteId"))"#;

/// Igual pero con los ids de las relaciones (detalle).
const APP_DETALLE: &str = r#"to_jsonb(a) || jsonb_build_object(
    'appType', (SELECT to_jsonb(t) FROM "AppType" t WHERE t.id = a."appTypeId"),
    'owner',    (SELECT jsonb_build_object('name', u.name) FROM "User" u WHERE u.id = a."ownerId"),
    'lead',     (SELECT jsonb_build_object('id', l.id, 'companyName', l."companyName") FROM "Lead" l WHERE l.id = a."leadId"),
    'proposal', (SELECT jsonb_build_object('id', p.id, 'title', p.title) FROM "Proposal" p WHERE p.id = a."proposalId"),
    'project',  (SELECT jsonb_build_object('id', pr.id, 'name', pr.name) FROM "Project" pr WHERE pr.id = a."projectId"),
    'cliente',  (SELECT jsonb_build_object('id', c.id, 'nombre', c.nombre) FROM "Cliente" c WHERE c.id = a."clienteId"))"#;

async fn apps_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let f = |k: &str| q.get(k).filter(|x| !x.is_empty()).cloned();
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg({APP_LISTA} ORDER BY a."updatedAt" DESC), '[]'::jsonb) FROM "AppInstance" a
           WHERE ($1::text IS NULL OR a.status = $1) AND ($2::text IS NULL OR a."appTypeId" = $2)
             AND ($3::text IS NULL OR position($3 in a.name) > 0 OR position($3 in COALESCE(a.description, '')) > 0)
             AND ($4::text IS NULL OR EXISTS (SELECT 1 FROM "AppType" t WHERE t.id = a."appTypeId" AND t.category = $4))"#
    );
    Ok(Json(
        fetch_json(&st.pool, &sql, &[B::OT(f("status")), B::OT(f("appTypeId")), B::OT(f("q")), B::OT(f("category"))]).await?,
    ))
}

static RE_NO_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9]+").expect("regex"));

fn slugify(nombre: &str) -> String {
    let t = RE_NO_ALNUM.replace_all(nombre.to_lowercase().trim(), "-").to_string();
    t.trim_matches('-').to_string()
}

async fn app_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let user = sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autenticado"))?.to_string();
    let nombre = s(&body, "name").map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let tipo_id = s(&body, "appTypeId").filter(|n| !n.is_empty());
    let (Some(nombre), Some(tipo_id)) = (nombre, tipo_id) else {
        return Err(ApiError::bad_request("Nombre y tipo de app son obligatorios"));
    };
    let Some(config) = fetch_json_opt(&st.pool, r#"SELECT "defaultConfig" FROM "AppType" WHERE id = $1"#, &[B::T(tipo_id.clone())]).await? else {
        return Err(ApiError::not_found("Tipo de app no encontrado"));
    };
    let mut slug = match s(&body, "slug").map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()) {
        Some(sl) => sl,
        None => slugify(&nombre),
    };
    if slug.is_empty() {
        slug = format!("app-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
    }
    let mut unico = slug.clone();
    let mut n = 1;
    while fetch_text_opt(&st.pool, r#"SELECT id FROM "AppInstance" WHERE slug = $1"#, &[B::T(unico.clone())]).await?.is_some() {
        unico = format!("{slug}-{n}");
        n += 1;
    }
    let desc = s(&body, "description").map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
    let id_opt = |k: &str| s(&body, k).filter(|x| !x.is_empty());
    let sql = r#"WITH a AS (INSERT INTO "AppInstance" (id, name, description, slug, "appTypeId", status, config, "leadId", "proposalId", "projectId", "clienteId", "ownerId", "createdAt", "updatedAt")
          VALUES ($1, $2, $3, $4, $5, 'DRAFT', $6::jsonb, $7, $8, $9, $10, $11, NOW(), NOW()) RETURNING *)
        SELECT to_jsonb(a) || jsonb_build_object(
          'appType', (SELECT to_jsonb(t) FROM "AppType" t WHERE t.id = a."appTypeId"),
          'owner',   (SELECT jsonb_build_object('name', u.name) FROM "User" u WHERE u.id = a."ownerId")) FROM a"#;
    let app = fetch_json(
        &st.pool,
        sql,
        &[
            B::T(new_id()),
            B::T(nombre.clone()),
            B::OT(desc),
            B::T(unico),
            B::T(tipo_id),
            B::T(config.to_string()),
            B::OT(id_opt("leadId")),
            B::OT(id_opt("proposalId")),
            B::OT(id_opt("projectId")),
            B::OT(id_opt("clienteId")),
            B::T(user.clone()),
        ],
    )
    .await?;
    let id = app["id"].as_str().unwrap_or_default().to_string();
    log_activity(&st.pool, "CREATED", &format!("creó la app {nombre}"), "app", &id, Some(&user), None).await;
    Ok((StatusCode::CREATED, Json(app)))
}

async fn app_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT {APP_DETALLE} FROM "AppInstance" a WHERE a.id = $1"#);
    fetch_json_opt(&st.pool, &sql, &[B::T(id)]).await?.map(Json).ok_or_else(|| ApiError::not_found("App no encontrada"))
}

async fn app_por_slug(State(st): State<AppState>, _s: Session, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT {APP_DETALLE} FROM "AppInstance" a WHERE a.slug = $1"#);
    fetch_json_opt(&st.pool, &sql, &[B::T(slug)]).await?.map(Json).ok_or_else(|| ApiError::not_found("App no encontrada"))
}

/// `String(x)` de JavaScript para un valor del cuerpo.
fn a_texto(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Null => "null".into(),
        otro => otro.to_string(),
    }
}

async fn app_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autenticado"))?;
    let mut up = Upd::new(&id);
    if let Some(v) = body.get("name") {
        up.set("name", B::T(a_texto(v).trim().to_string()));
    }
    if let Some(v) = body.get("description") {
        up.set("description", B::OT(if truthy(&body, "description") { Some(a_texto(v).trim().to_string()) } else { None }));
    }
    if let Some(v) = body.get("status") {
        up.set("status", B::T(a_texto(v)));
    }
    for k in ["leadId", "proposalId", "projectId", "clienteId"] {
        if body.get(k).is_some() {
            up.set(k, B::OT(s(&body, k).filter(|x| !x.is_empty())));
        }
    }
    if let Some(c) = body.get("config") {
        up.set_expr("config", B::T(c.to_string()), "{n}::jsonb");
    }
    let sql = up.con("AppInstance", &format!("SELECT {APP_DETALLE} FROM up a"));
    let app = fetch_json_opt(&st.pool, &sql, &up.binds).await?.ok_or_else(|| ApiError::internal("Error interno"))?;
    let nombre = app["name"].as_str().unwrap_or_default().to_string();
    if body.get("status").is_some() {
        log_activity(&st.pool, "STATUS_CHANGED", &format!("cambió el estado de la app {nombre} a {}", a_texto(&body["status"])), "app", &id, uid(&sesion), None).await;
    } else {
        log_activity(&st.pool, "UPDATED", &format!("actualizó la app {nombre}"), "app", &id, uid(&sesion), None).await;
    }
    Ok(Json(app))
}

async fn app_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autenticado"))?;
    let nombre = fetch_text_opt(&st.pool, r#"SELECT name FROM "AppInstance" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    if exec(&st.pool, r#"DELETE FROM "AppInstance" WHERE id = $1"#, &[B::T(id.clone())]).await? == 0 {
        return Err(ApiError::internal("Error interno"));
    }
    log_activity(&st.pool, "UPDATED", &format!("eliminó la app {}", nombre.unwrap_or_default()), "app", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "ok": true })))
}

async fn tipos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.name ASC), '[]'::jsonb) FROM "AppType" t WHERE t."isActive" = true"#, &[]).await?,
    ))
}

// ═══════════════════════════════ EVENTOS DE APPS CLIENTE ═══════════════════════════════
fn ip_privada(ip: &str) -> bool {
    ip == "127.0.0.1"
        || ip == "::1"
        || ip.starts_with("10.")
        || ip.starts_with("192.168.")
        || ip.strip_prefix("172.").and_then(|r| r.split('.').next()).and_then(|n| n.parse::<u8>().ok()).map(|n| (16..=31).contains(&n)).unwrap_or(false)
}

/// Geolocalización best-effort (ip-api.com); nunca bloquea el registro del evento.
async fn resolver_ubicacion(st: &AppState, ip: Option<&str>) -> Option<String> {
    let ip = ip.filter(|i| !i.is_empty() && !ip_privada(i))?;
    let url = format!("http://ip-api.com/json/{ip}?fields=status,city,regionName,country");
    let r = st.http.get(url).timeout(Duration::from_millis(2500)).send().await.ok()?;
    let d: Value = r.json().await.ok()?;
    if d["status"].as_str() != Some("success") {
        return None;
    }
    let partes: Vec<&str> = ["city", "regionName", "country"].iter().filter_map(|k| d[*k].as_str()).filter(|x| !x.is_empty()).collect();
    Some(partes.join(", "))
}

async fn evento_crear(State(st): State<AppState>, headers: HeaderMap, body: Option<Json<Value>>) -> ApiResult<(StatusCode, Json<Value>)> {
    // Solo máquina a máquina: NO acepta la sesión de un usuario del portal.
    let key = headers.get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    if key.is_empty() || st.cfg.internal_api_key.as_deref() != Some(key) {
        return Err(ApiError::unauthorized_msg("No autenticado"));
    }
    let Some(Json(body)) = body else {
        return Err(ApiError::bad_request("Body inválido"));
    };
    let g = |k: &str| s(&body, k).filter(|x| !x.is_empty());
    let (Some(app_slug), Some(tipo), Some(actor_n), Some(actor_u)) = (g("appSlug"), g("tipo"), g("actorNombre"), g("actorUsuario")) else {
        return Err(ApiError::bad_request("appSlug, tipo, actorNombre y actorUsuario son requeridos"));
    };
    if !["LOGIN", "LOGOUT", "ACCION", "VISTA", "API"].contains(&tipo.as_str()) {
        return Err(ApiError::bad_request(format!("tipo inválido: {tipo}")));
    }
    let ip = s(&body, "ip");
    let ubicacion = resolver_ubicacion(&st, ip.as_deref()).await;
    let meta = match body.get("metadata") {
        Some(Value::Null) | None => None,
        Some(m) => Some(m.to_string()),
    };
    let id = fetch_text_opt(
        &st.pool,
        r#"INSERT INTO "AppEvento" (id, "appSlug", tipo, "actorNombre", "actorUsuario", entidad, accion, detalle, metadata, ip, ubicacion, "userAgent", "creadoEn")
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb, $10, $11, $12, NOW()) RETURNING id"#,
        &[
            B::T(new_id()),
            B::T(app_slug),
            B::T(tipo),
            B::T(actor_n),
            B::T(actor_u),
            B::OT(s(&body, "entidad")),
            B::OT(s(&body, "accion")),
            B::OT(s(&body, "detalle")),
            B::OT(meta),
            B::OT(ip),
            B::OT(ubicacion),
            B::OT(s(&body, "userAgent")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "ok": true, "id": id }))))
}

async fn eventos_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let app_slug = q.get("appSlug").filter(|x| !x.is_empty()).cloned();
    let take = q.get("take").and_then(|t| t.parse::<f64>().ok()).unwrap_or(100.0).min(500.0).max(0.0) as i64;
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT jsonb_build_object(
                 'eventos', COALESCE((SELECT jsonb_agg(to_jsonb(x) ORDER BY x."creadoEn" DESC) FROM
                     (SELECT * FROM "AppEvento" WHERE ($1::text IS NULL OR "appSlug" = $1) ORDER BY "creadoEn" DESC LIMIT $2) x), '[]'::jsonb),
                 'apps', COALESCE((SELECT jsonb_agg(jsonb_build_object('appSlug', g."appSlug", '_count', jsonb_build_object('_all', g.n), '_max', jsonb_build_object('creadoEn', g.ultimo)))
                                   FROM (SELECT "appSlug", COUNT(*) n, MAX("creadoEn") ultimo FROM "AppEvento" GROUP BY "appSlug") g), '[]'::jsonb))"#,
            &[B::OT(app_slug), B::I(take)],
        )
        .await?,
    ))
}

// ═══════════════════════════════ MÉTRICAS DE LOS VPS (públicas) ═══════════════════════════════
fn vps_de(st: &AppState, n: usize) -> (Option<String>, Option<String>, &'static str) {
    let (u, t) = st.cfg.vps[n].clone();
    (u, t, if n == 0 { "VPS_METRICS_URL" } else { "VPS2_METRICS_URL" })
}

fn autorizar(rb: reqwest::RequestBuilder, token: &Option<String>) -> reqwest::RequestBuilder {
    match token {
        Some(t) => rb.header(header::AUTHORIZATION, format!("Bearer {t}")),
        None => rb,
    }
}

async fn vps_stats(State(st): State<AppState>, n: usize) -> Response {
    let (url, token, var) = vps_de(&st, n);
    let Some(url) = url else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": format!("{var} no configurada") }))).into_response();
    };
    match autorizar(st.http.get(format!("{url}/metrics")), &token).timeout(Duration::from_secs(30)).send().await {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(v) => Json(v).into_response(),
            Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() }))).into_response(),
        },
        Ok(r) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("VPS respondió {}", r.status().as_u16()) }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn vps_docker(State(st): State<AppState>, n: usize) -> Response {
    let (url, token, var) = vps_de(&st, n);
    let Some(url) = url else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": format!("{var} no configurada") }))).into_response();
    };
    // Este endpoint manda siempre la cabecera Authorization (aunque el token esté vacío).
    let rb = st.http.get(format!("{url}/docker")).header(header::AUTHORIZATION, format!("Bearer {}", token.unwrap_or_default()));
    match rb.send().await {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(v) => Json(v).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
        },
        Ok(r) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("Agent error {}", r.status().as_u16()) }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn vps_lista(st: &AppState, n: usize, ruta: &str, segundos: u64, vacio: Value) -> Response {
    let (url, token, _) = vps_de(st, n);
    let Some(url) = url else { return Json(vacio).into_response() };
    match autorizar(st.http.get(format!("{url}/{ruta}")), &token).timeout(Duration::from_secs(segundos)).send().await {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(v) => Json(v).into_response(),
            Err(_) => Json(vacio).into_response(),
        },
        _ => Json(vacio).into_response(),
    }
}

async fn vps_historial(State(st): State<AppState>, n: usize) -> Response {
    vps_lista(&st, n, "history", 8, json!({ "snapshots": [] })).await
}

async fn vps_logs(State(st): State<AppState>, n: usize) -> Response {
    vps_lista(&st, n, "logs", 5, json!({ "lines": [] })).await
}

// ═══════════════════════════════ REPOS DE GITHUB (pública) ═══════════════════════════════
/// Next cachea estas llamadas 60 s (`revalidate: 60`); acá se cachea la respuesta final lo mismo.
static CACHE_GITHUB: LazyLock<Mutex<Option<(Instant, Value)>>> = LazyLock::new(|| Mutex::new(None));

async fn github_get(st: &AppState, url: &str) -> Result<reqwest::Response, reqwest::Error> {
    let mut rb = st.http.get(url).header(header::ACCEPT, "application/vnd.github+json").header(header::USER_AGENT, "portalhub");
    if let Some(t) = &st.cfg.github_token {
        rb = rb.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    rb.send().await
}

async fn github_repos(State(st): State<AppState>) -> Response {
    if let Some((cuando, v)) = CACHE_GITHUB.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if cuando.elapsed() < Duration::from_secs(60) {
            return Json(v.clone()).into_response();
        }
    }
    let r = match github_get(&st, "https://api.github.com/user/repos?per_page=100&sort=updated&affiliation=owner,collaborator,organization_member").await {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    };
    if !r.status().is_success() {
        return (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("GitHub API error {}", r.status().as_u16()) }))).into_response();
    }
    let repos: Vec<Value> = match r.json().await {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    };
    let mut set = tokio::task::JoinSet::new();
    for (i, repo) in repos.iter().cloned().enumerate() {
        let st = st.clone();
        set.spawn(async move { (i, enriquecer(&st, repo).await) });
    }
    let mut out: Vec<(usize, Value)> = vec![];
    while let Some(Ok(x)) = set.join_next().await {
        out.push(x);
    }
    out.sort_by_key(|(i, _)| *i);
    let resp = json!({ "repos": out.into_iter().map(|(_, v)| v).collect::<Vec<_>>(), "username": st.cfg.github_username });
    *CACHE_GITHUB.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), resp.clone()));
    Json(resp).into_response()
}

async fn enriquecer(st: &AppState, r: Value) -> Value {
    let rama = r["default_branch"].as_str().filter(|x| !x.is_empty()).unwrap_or("main").to_string();
    let Some(nombre) = r["full_name"].as_str().map(String::from) else {
        let mut o = r.clone();
        o["last_commit"] = Value::Null;
        o["last_workflow"] = Value::Null;
        return o;
    };
    let url_commit = format!("https://api.github.com/repos/{nombre}/commits/{rama}");
    let url_runs = format!("https://api.github.com/repos/{nombre}/actions/runs?per_page=1");
    let (cr, wr) = tokio::join!(github_get(st, &url_commit), github_get(st, &url_runs));
    let (Ok(cr), Ok(wr)) = (cr, wr) else {
        let mut o = r.clone();
        o["last_commit"] = Value::Null;
        o["last_workflow"] = Value::Null;
        return o;
    };
    let commit: Option<Value> = if cr.status().is_success() { cr.json().await.ok() } else { None };
    let workflows: Option<Value> = if wr.status().is_success() { wr.json().await.ok() } else { None };
    let last_run = workflows.as_ref().and_then(|w| w["workflow_runs"].get(0)).cloned();
    json!({
        "id": r["id"], "name": r["name"], "full_name": r["full_name"], "description": r["description"], "private": r["private"],
        "language": r["language"], "url": r["html_url"], "default_branch": r["default_branch"], "updated_at": r["updated_at"],
        "pushed_at": r["pushed_at"], "stars": r["stargazers_count"], "forks": r["forks_count"], "open_issues": r["open_issues_count"],
        "last_commit": commit.map(|c| json!({
            "sha": c["sha"].as_str().map(|x| x.chars().take(7).collect::<String>()),
            "message": c["commit"]["message"].as_str().map(|m| m.split('\n').next().unwrap_or("").to_string()),
            "author": c["commit"]["author"]["name"], "date": c["commit"]["author"]["date"],
        })),
        "last_workflow": last_run.map(|w| json!({
            "name": w["name"], "status": w["status"], "conclusion": w["conclusion"], "updated_at": w["updated_at"], "url": w["html_url"],
        })),
    })
}

// ═══════════════════════════════ PANTALLA DE MÉTRICAS Y RESUMEN PÚBLICO ═══════════════════════════════
async fn metricas_pantalla(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT jsonb_build_object(
                 'sprints', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                      'code', COALESCE(x."sprintCode", left(x.name, 12)), 'name', x.name, 'status', x.status,
                      'done', (SELECT COUNT(*) FROM "BacklogItem" i WHERE i."sprintId" = x.id AND i.status = 'DONE'),
                      'total', (SELECT COUNT(*) FROM "BacklogItem" i WHERE i."sprintId" = x.id)) ORDER BY x."createdAt" DESC)
                    FROM (SELECT * FROM "Sprint" WHERE status IN ('ACTIVE', 'IN_PROGRESS') ORDER BY "createdAt" DESC LIMIT 3) x), '[]'::jsonb),
                 'backlog', (SELECT jsonb_build_object('todo', t, 'in_progress', p, 'done', d, 'total', t + p + d)
                             FROM (SELECT COUNT(*) FILTER (WHERE status = 'BACKLOG') t, COUNT(*) FILTER (WHERE status = 'IN_PROGRESS') p, COUNT(*) FILTER (WHERE status = 'DONE') d FROM "BacklogItem") c),
                 'epics', jsonb_build_object('active', (SELECT COUNT(*) FROM "Epic" WHERE status = 'ACTIVE')),
                 'ts', to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'))"#,
            &[],
        )
        .await?,
    ))
}

const CORS: [(&str, &str); 3] = [
    ("access-control-allow-origin", "*"),
    ("access-control-allow-methods", "GET, OPTIONS"),
    ("access-control-allow-headers", "Content-Type"),
];

fn con_cors(mut r: Response) -> Response {
    for (k, v) in CORS {
        r.headers_mut().insert(header::HeaderName::from_static(k), HeaderValue::from_static(v));
    }
    r
}

async fn resumen_opciones() -> Response {
    con_cors(StatusCode::NO_CONTENT.into_response())
}

async fn resumen_publico(State(st): State<AppState>) -> Response {
    let sql = r#"SELECT jsonb_build_object(
          'leads', (SELECT COUNT(*) FROM "Lead"),
          'ganados', (SELECT COUNT(*) FROM "Lead" WHERE status = 'RESULT' AND outcome = 'WON'),
          'pipeline', COALESCE((SELECT SUM("estimatedValue") FROM "Lead" WHERE status <> 'RESULT'), 0),
          'props', (SELECT COUNT(*) FROM "Proposal"),
          'aceptadas', (SELECT COUNT(*) FROM "Proposal" WHERE status = 'ACCEPTED'),
          'pendientes', (SELECT COUNT(*) FROM "Proposal" WHERE status IN ('DRAFT', 'SENT')),
          'activos', (SELECT COUNT(*) FROM "Project" WHERE status NOT IN ('COMPLETED', 'CANCELLED')),
          'completados', (SELECT COUNT(*) FROM "Project" WHERE status = 'COMPLETED'),
          'prog_suma', COALESCE((SELECT SUM(progress) FROM "Project" WHERE status NOT IN ('COMPLETED', 'CANCELLED')), 0),
          'actividades', (SELECT COUNT(*) FROM "Activity"),
          'ahora', to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'))"#;
    let cuerpo = match fetch_json(&st.pool, sql, &[]).await {
        Ok(d) => {
            let n = |k: &str| d[k].as_f64().unwrap_or(0.0);
            let redondear = |x: f64| (x + 0.5).floor();
            let win = if n("leads") > 0.0 { redondear(n("ganados") / n("leads") * 100.0) } else { 0.0 };
            let prom = if n("activos") > 0.0 { redondear(n("prog_suma") / n("activos")) } else { 0.0 };
            json!({
                "online": true, "leads": n("leads"), "leads_won": n("ganados"), "win_rate": win, "pipeline_value": n("pipeline"),
                "proposals_total": n("props"), "proposals_accepted": n("aceptadas"), "proposals_pending": n("pendientes"),
                "projects_active": n("activos"), "projects_completed": n("completados"), "avg_progress": prom,
                "activities": n("actividades"), "updated_at": d["ahora"],
            })
        }
        Err(_) => json!({ "online": false }),
    };
    con_cors(Json(cuerpo).into_response())
}

// ═══════════════════════════════ ESTADO DE SERVICIOS EXTERNOS ═══════════════════════════════
async fn estado_servicios(State(st): State<AppState>, _s: Session) -> Json<Value> {
    let servicios = [
        ("Vercel", "https://www.vercel-status.com/api/v2/status.json"),
        ("GitHub", "https://www.githubstatus.com/api/v2/status.json"),
        ("Render", "https://status.render.com/api/v2/status.json"),
        ("Supabase", "https://status.supabase.com/api/v2/status.json"),
    ];
    let mut set = tokio::task::JoinSet::new();
    for (i, (nombre, url)) in servicios.into_iter().enumerate() {
        let http = st.http.clone();
        set.spawn(async move {
            let t0 = Instant::now();
            let ms = |t: Instant| t.elapsed().as_millis() as u64;
            let v = match http.get(url).timeout(Duration::from_secs(5)).send().await {
                Ok(r) => {
                    let lat = ms(t0);
                    if !r.status().is_success() {
                        json!({ "name": nombre, "status": "down", "latency": lat })
                    } else {
                        match r.json::<Value>().await {
                            Ok(d) => {
                                let ind = d["status"]["indicator"].as_str().unwrap_or("");
                                let estado = if ind == "none" { "ok" } else if ind == "major" { "down" } else { "degraded" };
                                json!({ "name": nombre, "status": estado, "latency": lat })
                            }
                            Err(_) => json!({ "name": nombre, "status": "down", "latency": ms(t0) }),
                        }
                    }
                }
                Err(_) => json!({ "name": nombre, "status": "down", "latency": ms(t0) }),
            };
            (i, v)
        });
    }
    let mut out: Vec<(usize, Value)> = vec![];
    while let Some(Ok(x)) = set.join_next().await {
        out.push(x);
    }
    out.sort_by_key(|(i, _)| *i);
    Json(Value::Array(out.into_iter().map(|(_, v)| v).collect()))
}
