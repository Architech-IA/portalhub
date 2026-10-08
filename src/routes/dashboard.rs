//! /api/dashboard y /api/dashboard/personal (MASD PHUB-0001-0004).
//! Paridad con `src/app/api/dashboard/route.ts` y `.../personal/route.ts`.
//!
//! Dos diferencias deliberadas respecto de Next:
//! 1. La caché de 30 s en Next guardaba el resultado COMPLETO, incluido el bloque "Mi día"
//!    (leads y propuestas del usuario que llenó la caché): durante 30 s cualquier otro usuario
//!    veía los datos del primero. Acá se cachea solo la parte compartida y "Mi día" se calcula
//!    siempre por usuario.
//! 2. La tendencia mensual calculaba el fin de cada mes como "YYYY-MM-31", que JavaScript
//!    desborda al mes siguiente en los meses de 30 días (un lead creado el día 1 se contaba en
//!    dos meses). Acá cada mes termina donde termina.

use std::{sync::Mutex, time::{Duration, Instant}};

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{fetch_json, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/dashboard", get(dashboard))
        .route("/api/dashboard/personal", get(personal))
}

static CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Agrupa por una columna conservando el orden de primera aparición en la tabla (como hace el
/// `groupCount` de Next sobre el resultado de `findMany` sin orden).
fn grupo(tabla: &str, col: &str, clave: &str) -> String {
    format!(
        r#"COALESCE((SELECT jsonb_agg(jsonb_build_object('{clave}', g.k, '_count', g.c) ORDER BY g.mn)
                    FROM (SELECT COALESCE({col}::text, 'sin-dato') AS k, COUNT(*) AS c, MIN(ctid) AS mn
                          FROM "{tabla}" GROUP BY 1) g), '[]'::jsonb)"#
    )
}

fn sql_compartido() -> String {
    let leads_por_estado = grupo("Lead", "status", "status");
    let propuestas_por_estado = grupo("Proposal", "status", "status");
    let proyectos_por_estado = grupo("Project", "status", "status");
    let industria = grupo("Lead", "source", "source");
    format!(
        r#"
SELECT jsonb_build_object(
  'counts', jsonb_build_object(
      'leads', (SELECT COUNT(*) FROM "Lead"),
      'proposals', (SELECT COUNT(*) FROM "Proposal"),
      'projects', (SELECT COUNT(*) FROM "Project"),
      'activities', (SELECT COUNT(*) FROM "Activity")),
  'leadsByStatus', {leads_por_estado},
  'proposalsByStatus', {propuestas_por_estado},
  'projectsByStatus', {proyectos_por_estado},
  'totalEstimatedValue', (SELECT COALESCE(SUM("estimatedValue"), 0) FROM "Lead"),
  'leadsGanados', (SELECT COUNT(*) FROM "Lead" WHERE status = 'RESULT' AND outcome = 'WON'),
  'conversionRate', (SELECT CASE WHEN COUNT(*) > 0
        THEN ROUND(100.0 * COUNT(*) FILTER (WHERE status = 'RESULT' AND outcome = 'WON') / COUNT(*))::int
        ELSE 0 END FROM "Lead"),
  'leadsInactivos', COALESCE((SELECT jsonb_agg(jsonb_build_object(
        'id', l.id, 'companyName', l."companyName", 'status', l.status::text, 'updatedAt', l."updatedAt"))
      FROM (SELECT * FROM "Lead" WHERE status <> 'RESULT'
              AND "updatedAt" < (NOW() AT TIME ZONE 'UTC') - interval '7 days' ORDER BY ctid LIMIT 5) l), '[]'::jsonb),
  'propuestasSinRespuesta', COALESCE((SELECT jsonb_agg(jsonb_build_object(
        'id', p.id, 'title', p.title, 'amount', p.amount, 'sentDate', p."sentDate"))
      FROM (SELECT * FROM "Proposal" WHERE status = 'SENT' AND "sentDate" IS NOT NULL
              AND "sentDate" < (NOW() AT TIME ZONE 'UTC') - interval '5 days' ORDER BY ctid LIMIT 5) p), '[]'::jsonb),
  'proximosDeadlines', COALESCE((SELECT jsonb_agg(jsonb_build_object(
        'id', d.id, 'name', d.name, 'endDate', d."endDate", 'progress', d.progress, 'priority', d.priority::text)
        ORDER BY d."endDate" ASC)
      FROM (SELECT * FROM "Project" WHERE status NOT IN ('COMPLETED', 'CANCELLED') AND "endDate" IS NOT NULL
              AND "endDate" >= (NOW() AT TIME ZONE 'UTC') AND "endDate" <= (NOW() AT TIME ZONE 'UTC') + interval '7 days'
              ORDER BY "endDate" ASC LIMIT 5) d), '[]'::jsonb),
  'topSocios', COALESCE((SELECT jsonb_agg(jsonb_build_object(
        'id', t.id, 'name', t.name, 'role', t.role::text,
        '_count', jsonb_build_object('leads', t.l, 'proposals', t.p, 'projects', t.pr)) ORDER BY t.l DESC)
      FROM (SELECT u.id, u.name, u.role,
                   (SELECT COUNT(*) FROM "Lead" x WHERE x."userId" = u.id) AS l,
                   (SELECT COUNT(*) FROM "Proposal" x WHERE x."userId" = u.id) AS p,
                   (SELECT COUNT(*) FROM "ProjectUser" x WHERE x."userId" = u.id) AS pr
            FROM "User" u ORDER BY l DESC LIMIT 4) t), '[]'::jsonb),
  'embudo', (SELECT jsonb_agg(jsonb_build_object('status', e.s, 'count', COALESCE(c.n, 0), 'valor', COALESCE(c.v, 0)) ORDER BY e.o)
      FROM unnest(ARRAY['NEW','CONTACTED','DIAGNOSIS','DEMO_VALIDATION','PROPOSAL_SENT','NEGOTIATION','RESULT'])
           WITH ORDINALITY AS e(s, o)
      LEFT JOIN (SELECT status::text AS s, COUNT(*) AS n, SUM("estimatedValue") AS v FROM "Lead" GROUP BY status) c ON c.s = e.s),
  'industriaLeads', {industria},
  'metaMensual', 30000,
  'ingresosMes', (SELECT COALESCE(SUM(monto), 0) FROM "RegistroFinanciero"
      WHERE tipo = 'ingreso' AND estado <> 'cancelado'
        AND fecha >= to_char(NOW() AT TIME ZONE 'UTC', 'YYYY-MM-01')
        AND fecha <= to_char(NOW() AT TIME ZONE 'UTC', 'YYYY-MM-31')),
  'registrosPendientes', COALESCE((SELECT jsonb_agg(jsonb_build_object(
        'id', r.id, 'concepto', r.concepto, 'monto', r.monto, 'moneda', r.moneda, 'tipo', r.tipo))
      FROM (SELECT * FROM "RegistroFinanciero" WHERE estado = 'pendiente' ORDER BY ctid LIMIT 5) r), '[]'::jsonb),
  'recentActivities', COALESCE((SELECT jsonb_agg(to_jsonb(a) || jsonb_build_object('user',
        jsonb_build_object('name', (SELECT u.name FROM "User" u WHERE u.id = a."userId"))) ORDER BY a."createdAt" DESC)
      FROM (SELECT * FROM "Activity" ORDER BY "createdAt" DESC LIMIT 8) a), '[]'::jsonb),
  'tendencias', (SELECT jsonb_agg(jsonb_build_object(
        'mes', (ARRAY['ene','feb','mar','abr','may','jun','jul','ago','sept','oct','nov','dic'])[EXTRACT(MONTH FROM m.ini)::int],
        'leads', (SELECT COUNT(*) FROM "Lead" WHERE "createdAt" >= m.ini AND "createdAt" < m.ini + interval '1 month'),
        'proyectos', (SELECT COUNT(*) FROM "Project" WHERE "createdAt" >= m.ini AND "createdAt" < m.ini + interval '1 month'),
        'ingresos', (SELECT COALESCE(SUM(monto), 0) FROM "RegistroFinanciero"
            WHERE tipo = 'ingreso' AND estado <> 'cancelado'
              AND fecha >= to_char(m.ini, 'YYYY-MM-DD') AND fecha <= to_char(m.ini, 'YYYY-MM') || '-31')) ORDER BY m.ini)
      FROM (SELECT date_trunc('month', NOW() AT TIME ZONE 'UTC') - (n || ' months')::interval AS ini
            FROM generate_series(5, 0, -1) AS n) m),
  'backlogStats', jsonb_build_object(
      'total', (SELECT COUNT(*) FROM "BacklogItem"),
      'pendientes', (SELECT COUNT(*) FROM "BacklogItem" WHERE status = 'BACKLOG'),
      'enProgreso', (SELECT COUNT(*) FROM "BacklogItem" WHERE status = 'IN_PROGRESS'),
      'completados', (SELECT COUNT(*) FROM "BacklogItem" WHERE status = 'DONE'),
      'puntosTotales', (SELECT COALESCE(SUM(COALESCE(points, 0)), 0) FROM "BacklogItem"),
      'sprintActivo', (SELECT jsonb_build_object('name', s.name, 'endDate', s."endDate",
            'items', (SELECT COUNT(*) FROM "BacklogItem" b WHERE b."sprintId" = s.id))
          FROM (SELECT * FROM "Sprint" WHERE status = 'ACTIVE' ORDER BY ctid LIMIT 1) s),
      'sprintBurndown', COALESCE((SELECT jsonb_agg(jsonb_build_object('status', b.status, 'points', COALESCE(b.points, 0)) ORDER BY b.ctid)
          FROM "BacklogItem" b
          WHERE b."sprintId" = (SELECT id FROM "Sprint" WHERE status = 'ACTIVE' ORDER BY ctid LIMIT 1)), '[]'::jsonb))
)"#
    )
}

async fn mi_dia(st: &AppState, uid: &str) -> Result<Value, sqlx::Error> {
    let leads = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', l.id, 'companyName', l."companyName",
                 'contactName', l."contactName", 'status', l.status::text, 'updatedAt', l."updatedAt")
                 ORDER BY l."updatedAt" ASC), '[]'::jsonb)
           FROM (SELECT * FROM "Lead" WHERE "userId" = $1 AND status IN ('NEW', 'CONTACTED')
                 ORDER BY "updatedAt" ASC LIMIT 5) l"#,
        &[B::T(uid.to_string())],
    )
    .await?;
    let propuestas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title, 'status', p.status::text,
                 'amount', p.amount) ORDER BY p."createdAt" DESC), '[]'::jsonb)
           FROM (SELECT * FROM "Proposal" WHERE "userId" = $1 AND status IN ('DRAFT', 'SENT', 'UNDER_REVIEW')
                 ORDER BY "createdAt" DESC LIMIT 5) p"#,
        &[B::T(uid.to_string())],
    )
    .await?;
    Ok(json!({ "leadsContactar": leads, "propuestasPendientes": propuestas, "tareasVencidas": [] }))
}

async fn dashboard(State(st): State<AppState>, sesion: Session) -> ApiResult<Response> {
    let (mut compartido, hit) = {
        let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            Some((ts, v)) if ts.elapsed() < CACHE_TTL => (Some(v.clone()), true),
            _ => (None, false),
        }
    };
    if compartido.is_none() {
        let v = fetch_json(&st.pool, &sql_compartido(), &[]).await?;
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.clone()));
        compartido = Some(v);
    }
    let mut resultado = compartido.unwrap_or_else(|| json!({}));

    let dia = if sesion.is_service || sesion.id.is_empty() {
        json!({ "leadsContactar": [], "propuestasPendientes": [], "tareasVencidas": [] })
    } else {
        mi_dia(&st, &sesion.id).await?
    };
    if let Some(o) = resultado.as_object_mut() {
        o.insert("myDay".into(), dia);
        let inactivos = o.get("leadsInactivos").cloned().unwrap_or(json!([]));
        o.insert("staleLeads".into(), inactivos);
    }
    Ok(([("X-Cache", if hit { "HIT" } else { "MISS" })], Json(resultado)).into_response())
}

async fn personal(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    if sesion.is_service || sesion.id.is_empty() {
        return Err(ApiError::new(axum::http::StatusCode::UNAUTHORIZED, "Unauthorized"));
    }
    let uid = sesion.id.clone();

    let datos = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
  'myLeads', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', l.id, 'companyName', l."companyName",
        'contactName', l."contactName", 'status', l.status::text, 'estimatedValue', l."estimatedValue",
        'updatedAt', l."updatedAt", 'source', l.source) ORDER BY l."updatedAt" DESC)
      FROM "Lead" l WHERE l."userId" = $1), '[]'::jsonb),
  'myProjects', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', p.id, 'name', p.name, 'status', p.status::text,
        'priority', p.priority::text, 'progress', p.progress, 'endDate', p."endDate", 'description', p.description,
        'projectRole', pu.role::text) ORDER BY pu.ctid)
      FROM "ProjectUser" pu JOIN "Project" p ON p.id = pu."projectId" WHERE pu."userId" = $1), '[]'::jsonb),
  'myBacklog', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', b.id, 'title', b.title, 'description', b.description,
        'type', b.type, 'priority', b.priority, 'status', b.status, 'points', b.points, 'solucionId', b."solucionId",
        'assigneeId', b."assigneeId", 'assigneeName', b."assigneeName", 'createdAt', b."createdAt",
        'solucion', CASE WHEN s.id IS NULL THEN NULL ELSE jsonb_build_object('id', s.id, 'nombre', s.nombre, 'tipo', s.tipo) END)
        ORDER BY b.status ASC, b.priority DESC)
      FROM "BacklogItem" b LEFT JOIN "Solucion" s ON s.id = b."solucionId"
      WHERE b."assigneeId" = $1 AND b.status <> 'DONE'), '[]'::jsonb),
  'upcomingMeetings', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', m.id, 'title', m.title, 'type', m."type",
        'date', m."date", 'endDate', m."endDate", 'location', m.location, 'link', m.link,
        'attendees', m.attendees, 'status', m.status) ORDER BY m."date" ASC)
      FROM (SELECT * FROM "Meeting"
            WHERE "date" >= (NOW() AT TIME ZONE 'UTC') AND "date" <= (NOW() AT TIME ZONE 'UTC') + interval '14 days'
              AND ("userId" = $1
                   OR (attendees IS NOT NULL AND position($2 in attendees) > 0)
                   OR (attendees IS NOT NULL AND position($3 in attendees) > 0))
            ORDER BY "date" ASC LIMIT 10) m), '[]'::jsonb)
)"#,
        &[B::T(uid.clone()), B::T(sesion.email.clone()), B::T(sesion.name.clone())],
    )
    .await?;

    let mis_leads = datos["myLeads"].as_array().cloned().unwrap_or_default();
    let activos: Vec<&Value> = mis_leads.iter().filter(|l| l["status"].as_str() != Some("RESULT")).collect();
    let pipeline: f64 = activos.iter().map(|l| l["estimatedValue"].as_f64().unwrap_or(0.0)).sum();
    let backlog = datos["myBacklog"].as_array().cloned().unwrap_or_default();
    let en_progreso = backlog.iter().filter(|i| i["status"].as_str() == Some("IN_PROGRESS")).count();
    let proyectos = datos["myProjects"].as_array().map(|a| a.len()).unwrap_or(0);
    let reuniones = datos["upcomingMeetings"].as_array().map(|a| a.len()).unwrap_or(0);

    Ok(Json(json!({
        "user": { "id": uid, "email": sesion.email, "name": sesion.name },
        "kpis": {
            "leadsActivos": activos.len(),
            "proyectos": proyectos,
            "backlogPendientes": backlog.len(),
            "backlogInProgress": en_progreso,
            "reunionesPróximas": reuniones,
            "pipelineValue": pipeline,
        },
        "myLeads": datos["myLeads"],
        "myProjects": datos["myProjects"],
        "myBacklog": datos["myBacklog"],
        "upcomingMeetings": datos["upcomingMeetings"],
    })))
}
