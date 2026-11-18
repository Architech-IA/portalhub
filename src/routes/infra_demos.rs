//! Panel de infraestructura de las demos (solo ADMIN): estado del servidor y de los contenedores de cada demo, y las
//! acciones dormir, despertar, reiniciar, ver logs, fijar encendida y reglas por demo. El navegador nunca habla con Docker:
//! este servicio valida la sesión, deja registro de cada acción y se lo pide al servicio privilegiado, que solo toca
//! contenedores de demos. Además atiende la página pública «Despertando la demo…» que Nginx muestra cuando una demo dormida recibe una visita.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/infra/demos", get(estado))
        .route("/api/infra/config", put(config))
        .route("/api/infra/demos/{id}/fijar", put(fijar))
        .route("/api/infra/demos/{id}/reglas", put(reglas))
        .route("/api/infra/memoria", get(memoria))
        .route("/api/infra/demos/{id}/historial", get(historial))
        .route("/api/infra/demos/{id}/{accion}", post(accion))
        .route("/api/publico/despertar/{slug}", get(despertar))
}

const DOMINIO: &str = "demos.architechia.co";
const MODOS: [&str; 3] = ["apagado", "simulacro", "activo"];

async fn modo(st: &AppState) -> String {
    fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('v', valor) FROM "InfraConfig" WHERE clave = 'autoDormir'"#, &[]).await.ok().flatten().and_then(|v| v["v"].as_str().map(String::from)).unwrap_or_else(|| "simulacro".into())
}

async fn estado(State(st): State<AppState>, se: Session) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let mut v = crate::routes::motor::llamar_t(&st, "/api/infra/estado", json!({}), 60).await.map_err(ApiError::bad_request)?;
    v["autoDormir"] = json!(modo(&st).await);
    Ok(Json(v))
}

async fn registrar(st: &AppState, usuario: Option<&str>, id: &str, accion: &str, ok: bool, detalle: &str) {
    let _ = exec(
        &st.pool,
        r#"INSERT INTO "InfraAccion" (id, "solucionId", "usuarioId", accion, ok, detalle) VALUES ($1,$2,$3,$4,$5,$6)"#,
        &[B::T(new_id()), B::T(id.into()), B::OT(usuario.map(String::from)), B::T(accion.into()), B::Bo(ok), B::T(detalle.chars().take(300).collect())],
    )
    .await;
}

async fn accion(State(st): State<AppState>, se: Session, Path((id, accion)): Path<(String, String)>, body: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let usuario = se.require_user()?.to_string();
    if !["detener", "iniciar", "reiniciar", "logs"].contains(&accion.as_str()) {
        return Err(ApiError::bad_request("Acción desconocida"));
    }
    let cuerpo = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let mut c = json!({ "solucionId": id, "accion": accion });
    for k in ["contenedor", "lineas"] {
        if let Some(v) = cuerpo.get(k) {
            c[k] = v.clone();
        }
    }
    let r = crate::routes::motor::llamar_t(&st, "/api/infra/accion", c, 150).await;
    if accion != "logs" {
        match &r {
            Ok(v) => registrar(&st, Some(&usuario), &id, &accion, true, v["mensaje"].as_str().unwrap_or("")).await,
            Err(e) => registrar(&st, Some(&usuario), &id, &accion, false, e).await,
        }
    }
    Ok(Json(r.map_err(ApiError::bad_request)?))
}

async fn fijar(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let usuario = se.require_user()?.to_string();
    let fijada = b["fijada"] == true;
    let n = exec(&st.pool, r#"UPDATE "Solucion" SET "infraFijada"=$2, "updatedAt"=NOW() WHERE id=$1 AND "deployContainerName" LIKE 'demo-%'"#, &[B::T(id.clone()), B::Bo(fijada)]).await?;
    if n == 0 {
        return Err(ApiError::not_found("Esa Solución no tiene una demo desplegada"));
    }
    registrar(&st, Some(&usuario), &id, if fijada { "fijar" } else { "soltar" }, true, "").await;
    Ok(Json(json!({ "ok": true, "fijada": fijada })))
}

/// Reglas de una demo: días sin visitas antes de dormirla y tope de memoria de la aplicación (opcional).
async fn reglas(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let usuario = se.require_user()?.to_string();
    let dias = b["dias"].as_i64().unwrap_or(7);
    if !(1..=365).contains(&dias) {
        return Err(ApiError::bad_request("Los días sin visitas deben estar entre 1 y 365"));
    }
    let mem = b["memoriaMb"].as_i64();
    if mem.map(|m| !(64..=4096).contains(&m)).unwrap_or(false) {
        return Err(ApiError::bad_request("El tope de memoria debe estar entre 64 y 4096 MB"));
    }
    let n = exec(&st.pool, r#"UPDATE "Solucion" SET "infraDiasInactiva"=$2::int, "infraMemoriaMb"=$3::int, "updatedAt"=NOW() WHERE id=$1 AND "deployContainerName" LIKE 'demo-%'"#, &[B::T(id.clone()), B::I(dias), match mem { Some(m) => B::I(m), None => B::OT(None) }]).await?;
    if n == 0 {
        return Err(ApiError::not_found("Esa Solución no tiene una demo desplegada"));
    }
    let mut mensaje = format!("Regla: dormir tras {dias} días sin visitas");
    if let Some(m) = mem {
        match crate::routes::motor::llamar_t(&st, "/api/infra/accion", json!({ "solucionId": id, "accion": "limite", "mb": m }), 60).await {
            Ok(_) => mensaje.push_str(&format!(" · tope {m} MB")),
            Err(e) => {
                registrar(&st, Some(&usuario), &id, "reglas", false, &e).await;
                return Err(ApiError::bad_request(e));
            }
        }
    }
    registrar(&st, Some(&usuario), &id, "reglas", true, &mensaje).await;
    Ok(Json(json!({ "ok": true, "mensaje": mensaje })))
}

async fn config(State(st): State<AppState>, se: Session, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let usuario = se.require_user()?.to_string();
    let m = b["autoDormir"].as_str().unwrap_or("");
    if !MODOS.contains(&m) {
        return Err(ApiError::bad_request("Modo desconocido"));
    }
    exec(&st.pool, r#"INSERT INTO "InfraConfig" (clave, valor) VALUES ('autoDormir', $1) ON CONFLICT (clave) DO UPDATE SET valor = EXCLUDED.valor"#, &[B::T(m.into())]).await?;
    registrar(&st, Some(&usuario), "", "config", true, &format!("Dormir frías automáticamente: {m}")).await;
    Ok(Json(json!({ "ok": true, "autoDormir": m })))
}

/// Histórico de memoria: una muestra cada 10 min (servidor y por demo) de las últimas `horas` (24 por defecto, máximo 720).
async fn memoria(State(st): State<AppState>, se: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let horas = q.get("horas").and_then(|h| h.parse::<i64>().ok()).unwrap_or(24).clamp(1, 720);
    let desde = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) - horas * 3600;
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('ts', ts, 'ram', "ramMb", 'swap', "swapMb", 'demos', demos) ORDER BY ts), '[]'::jsonb) FROM "InfraMuestra" WHERE ts >= $1"#,
        &[B::I(desde)],
    )
    .await?;
    Ok(Json(json!({ "muestras": v, "desde": desde })))
}

async fn historial(State(st): State<AppState>, se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    se.require_admin()?;
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(x ORDER BY x."createdAt" DESC), '[]'::jsonb) FROM (
             SELECT a.accion, a.ok, a.detalle, a."createdAt", u.name AS "usuario" FROM "InfraAccion" a LEFT JOIN "User" u ON u.id = a."usuarioId" WHERE a."solucionId" = $1 ORDER BY a."createdAt" DESC LIMIT 20) x"#,
        &[B::T(id)],
    )
    .await?;
    Ok(Json(v))
}

// ── Despertar al abrir la URL ───────────────────────────────────────────────────────────────

fn enfriamiento() -> &'static Mutex<HashMap<String, Instant>> {
    static M: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn slug_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Las comprobaciones de salud del propio servidor y los robots NO despiertan demos: solo una persona que las abre.
fn es_visita_real(cab: &HeaderMap) -> bool {
    let ip = cab.get("x-real-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
    if ["177.7.46.87", "127.0.0.1", "::1", ""].contains(&ip) {
        return false;
    }
    let ua = cab.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    !(ua.is_empty() || ["bot", "crawl", "spider", "curl", "wget", "monitor", "uptime", "headless", "python", "go-http", "scanner", "check"].iter().any(|p| ua.contains(p)))
}

fn pagina(titulo: &str, texto: &str, reintentar: bool) -> Response {
    let html = format!(
        r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{titulo}</title>
<style>html,body{{height:100%;margin:0}}body{{display:flex;align-items:center;justify-content:center;background:#0b1220;color:#e6e9ef;font-family:system-ui,sans-serif}}main{{max-width:420px;padding:24px;text-align:center}}h1{{font-size:20px;font-weight:600;margin:16px 0 8px}}p{{color:#8b97ab;font-size:14px;line-height:1.6;margin:0}}.r{{width:34px;height:34px;border:3px solid #243247;border-top-color:#22d3ee;border-radius:50%;margin:0 auto;animation:g 1s linear infinite}}@keyframes g{{to{{transform:rotate(360deg)}}}}</style></head>
<body><main><div class="r" id="r"></div><h1>{titulo}</h1><p id="t">{texto}</p></main>
<script>(function(){{var k="despertar:"+location.pathname,n=0;try{{n=+sessionStorage.getItem(k)||0}}catch(e){{}}
if({reintentar}){{if(n>=40){{document.getElementById("r").style.display="none";document.querySelector("h1").textContent="La demo no respondió";document.getElementById("t").textContent="Tardó más de lo normal en arrancar. Avisa al equipo de ArchiTechIA para que la revise.";return}}
try{{sessionStorage.setItem(k,String(n+1))}}catch(e){{}}setTimeout(function(){{location.reload()}},3000)}}else{{document.getElementById("r").style.display="none"}}}})();</script></body></html>"#
    );
    (StatusCode::SERVICE_UNAVAILABLE, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::RETRY_AFTER, "3"), (header::CACHE_CONTROL, "no-store")], html).into_response()
}

async fn despertar(State(st): State<AppState>, Path(slug): Path<String>, cab: HeaderMap) -> Response {
    if !slug_ok(&slug) {
        return pagina("Demo no disponible", "Esta dirección no corresponde a una demo.", false);
    }
    let fila = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('id', id, 'nombre', nombre) FROM "Solucion" WHERE "deployUrl" = $1 AND "deployContainerName" LIKE 'demo-%'"#, &[B::T(format!("https://{slug}.{DOMINIO}"))]).await.ok().flatten();
    let Some(f) = fila else { return pagina("Demo no disponible", "Esta dirección no corresponde a una demo.", false) };
    if !es_visita_real(&cab) {
        return pagina("Demo no disponible ahora", "La demo está dormida. Ábrela desde un navegador para despertarla.", false);
    }
    let id = f["id"].as_str().unwrap_or("").to_string();
    let lanzar = {
        let mut m = enfriamiento().lock().unwrap_or_else(|e| e.into_inner());
        let ya = m.get(&slug).map(|t| t.elapsed() < Duration::from_secs(90)).unwrap_or(false);
        if !ya {
            m.insert(slug.clone(), Instant::now());
        }
        !ya
    };
    if lanzar {
        let (st2, id2) = (st.clone(), id.clone());
        tokio::spawn(async move {
            let r = crate::routes::motor::llamar_t(&st2, "/api/infra/accion", json!({ "solucionId": id2, "accion": "iniciar" }), 150).await;
            match r {
                Ok(v) => registrar(&st2, None, &id2, "despertar-visita", true, v["mensaje"].as_str().unwrap_or("")).await,
                Err(e) => registrar(&st2, None, &id2, "despertar-visita", false, &e).await,
            }
        });
    }
    pagina("Despertando la demo…", "Esta demo estaba dormida para ahorrar recursos. Estará lista en unos segundos.", true)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn cab(ip: &str, ua: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", ip.parse().unwrap());
        h.insert(header::USER_AGENT, ua.parse().unwrap());
        h
    }

    #[test]
    fn solo_una_persona_despierta_la_demo() {
        assert!(es_visita_real(&cab("190.1.2.3", "Mozilla/5.0 (Windows NT 10.0) Chrome/120")));
        assert!(!es_visita_real(&cab("177.7.46.87", "Mozilla/5.0 Chrome/120")));
        assert!(!es_visita_real(&cab("127.0.0.1", "Mozilla/5.0")));
        assert!(!es_visita_real(&cab("190.1.2.3", "curl/7.81.0")));
        assert!(!es_visita_real(&cab("66.249.1.1", "Googlebot/2.1")));
        assert!(!es_visita_real(&HeaderMap::new()));
    }

    #[test]
    fn valida_el_slug() {
        assert!(slug_ok("radar-finops-ia"));
        assert!(!slug_ok("../etc"));
        assert!(!slug_ok("Radar"));
        assert!(!slug_ok(""));
    }
}
