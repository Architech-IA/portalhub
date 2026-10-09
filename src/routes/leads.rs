//! /api/leads (núcleo) — MASD PHUB-0001-0005.
//! Paridad con `src/app/api/leads/route.ts`, `[id]/route.ts`, `[id]/notes`, `[id]/interactions`,
//! `[id]/proposal` y `[id]/backlog-items`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{
        exec, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, new_id, numero, parse_float, presente, s,
        s_no_vacio, s_o_nulo, B,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/leads", get(listar).post(crear))
        .route("/api/leads/{id}", get(obtener).put(actualizar).delete(eliminar))
        .route("/api/leads/{id}/notes", get(notas_listar).post(nota_crear))
        .route("/api/leads/{id}/interactions", get(interacciones_listar).post(interaccion_crear))
        .route(
            "/api/leads/{id}/proposal",
            get(propuesta_obtener).post(propuesta_guardar).patch(propuesta_estado),
        )
        .route("/api/leads/{id}/backlog-items", get(backlog_listar).patch(backlog_estado))
}

const ESTADOS_LEAD: [&str; 7] =
    ["NEW", "CONTACTED", "DIAGNOSIS", "DEMO_VALIDATION", "PROPOSAL_SENT", "NEGOTIATION", "RESULT"];
const ESTADOS_PROPUESTA: [&str; 5] = ["DRAFT", "SENT", "UNDER_REVIEW", "ACCEPTED", "REJECTED"];
const TIPOS_ACTIVIDAD: [&str; 10] = [
    "CREATED",
    "UPDATED",
    "STATUS_CHANGED",
    "NOTE_ADDED",
    "PROPOSAL_SENT",
    "MILESTONE_COMPLETED",
    "CALL",
    "EMAIL",
    "MEETING",
    "WHATSAPP",
];

/// `include: { user: {id,name,email}, cliente: {id,nombre} }`.
const CON_USER_CLIENTE: &str = r#"to_jsonb(l) || jsonb_build_object(
  'user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email) FROM "User" u WHERE u.id = l."userId"),
  'cliente', (SELECT jsonb_build_object('id', c.id, 'nombre', c.nombre) FROM "Cliente" c WHERE c.id = l."clienteId"))"#;

fn sin_sesion_de_usuario(s: &Session, msg: &str) -> ApiResult<()> {
    if s.is_service || s.id.is_empty() {
        Err(ApiError::new(StatusCode::UNAUTHORIZED, msg))
    } else {
        Ok(())
    }
}

async fn listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(l) || jsonb_build_object(
             'user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email) FROM "User" u WHERE u.id = l."userId"),
             'solucion', (SELECT jsonb_build_object('id', s.id) FROM "Solucion" s WHERE s."leadId" = l.id),
             'cliente', (SELECT jsonb_build_object('id', c.id, 'nombre', c.nombre) FROM "Cliente" c WHERE c.id = l."clienteId"))
             ORDER BY l."createdAt" DESC), '[]'::jsonb)
           FROM "Lead" l"#,
        &[],
    )
    .await?;
    Ok(Json(v))
}

async fn crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let (Some(company), Some(contacto), Some(email), Some(origen), Some(user_id)) = (
        s_no_vacio(&body, "companyName"),
        s(&body, "contactName"),
        s(&body, "email"),
        s(&body, "source"),
        s_no_vacio(&body, "userId"),
    ) else {
        return Err(ApiError::bad_request("Faltan companyName, contactName, email, source o userId"));
    };
    let status = s_no_vacio(&body, "status").unwrap_or_else(|| "NEW".into());
    if !ESTADOS_LEAD.contains(&status.as_str()) {
        return Err(ApiError::bad_request("Estado inválido"));
    }
    let valor = parse_float(body.get("estimatedValue"));

    // Vincula con un Cliente existente (por nombre) o crea uno nuevo — nunca duplicado sin relación
    let cliente_id = match fetch_text_opt(
        &st.pool,
        r#"SELECT id FROM "Cliente" WHERE lower(nombre) = lower($1) ORDER BY ctid LIMIT 1"#,
        &[B::T(company.clone())],
    )
    .await?
    {
        Some(id) => id,
        None => {
            let id = new_id();
            exec(
                &st.pool,
                r#"INSERT INTO "Cliente" (id, nombre, contacto, email, industria, pais, estado, "valorTotal", "createdAt", "updatedAt")
                   VALUES ($1, $2, $3, $4, 'Sin especificar', 'Sin especificar', 'Activo', $5, NOW(), NOW())"#,
                &[B::T(id.clone()), B::T(company.clone()), B::T(contacto.clone()), B::T(email.clone()), B::F(valor)],
            )
            .await?;
            id
        }
    };

    let outcome = if status == "RESULT" { s_o_nulo(&body, "outcome") } else { None };
    let mut lead = fetch_json(
        &st.pool,
        &format!(
            r#"WITH ins AS (
                 INSERT INTO "Lead" (id, "companyName", "contactName", email, phone, status, outcome, source,
                                     "estimatedValue", scope, repository, notes, "userId", tipo, "solucionAsociada",
                                     "clienteId", "createdAt", "updatedAt")
                 VALUES ($1, $2, $3, $4, $5, $6::"LeadStatus", $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, NOW(), NOW())
                 RETURNING *)
               SELECT {} FROM ins l"#,
            CON_USER_CLIENTE
        ),
        &[
            B::T(new_id()),
            B::T(company.clone()),
            B::T(contacto),
            B::T(email),
            B::OT(s(&body, "phone")),
            B::T(status),
            B::OT(outcome),
            B::T(origen),
            B::F(valor),
            B::OT(s_o_nulo(&body, "scope")),
            B::OT(s_o_nulo(&body, "repository")),
            B::OT(s(&body, "notes")),
            B::T(user_id.clone()),
            B::OT(s_o_nulo(&body, "tipo")),
            B::OT(s_o_nulo(&body, "solucionAsociada")),
            B::T(cliente_id),
        ],
    )
    .await?;

    let lid = lead["id"].as_str().unwrap_or_default().to_string();
    log_activity(&st.pool, "CREATED", &format!("creó el lead {company}"), "lead", &lid, Some(&user_id), Some(&lid)).await;
    // Todo lead nace con su Solución y su motor de fases (el lead ya quedó guardado: si esto falla, se puede iniciar desde Oficina > Motor).
    let nombre_actor = if sesion.name.is_empty() { sesion.email.clone() } else { sesion.name.clone() };
    match crate::routes::fases::proyecto_para_lead(&st, &lid, &user_id, &nombre_actor).await {
        Ok(sol) => lead["solucionId"] = json!(sol),
        Err(e) => tracing::error!("iniciar el motor del lead nuevo: {}", e.1),
    }
    Ok(Json(lead))
}

async fn obtener(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sin_sesion_de_usuario(&sesion, "No autenticado")?;
    let sql = format!(r#"SELECT {CON_USER_CLIENTE} FROM "Lead" l WHERE l.id = $1"#);
    fetch_json_opt(&st.pool, &sql, &[B::T(id)])
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No encontrado"))
}

fn tipo_solucion(nombre: &str) -> &'static str {
    match nombre {
        "Project" => "PROJECT",
        "Demo" => "DEMO",
        "Partnership" => "PARTNERSHIP",
        "Products" => "PRODUCT",
        "Intern" => "INTERN",
        _ => "PROJECT",
    }
}

async fn actualizar(
    State(st): State<AppState>,
    sesion: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;

    let status_body = s(&body, "status");
    let outcome_body = s(&body, "outcome");
    let es_perdido = status_body.as_deref() == Some("RESULT") && outcome_body.as_deref() == Some("LOST");
    // Defensa en profundidad: sin motivo, un LOST no aporta nada a los reportes.
    if es_perdido && s(&body, "lostReason").map(|m| m.trim().is_empty()).unwrap_or(true) {
        return Err(ApiError::bad_request("El motivo de pérdida es obligatorio."));
    }

    let fallo = || ApiError::internal("Error al actualizar el lead");
    let previo = fetch_text_opt(&st.pool, r#"SELECT status::text FROM "Lead" WHERE id = $1"#, &[B::T(id.clone())])
        .await
        .map_err(|_| fallo())?;

    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(id.clone())];
    for (k, col) in [
        ("companyName", r#""companyName""#),
        ("contactName", r#""contactName""#),
        ("email", "email"),
        ("source", "source"),
        ("userId", r#""userId""#),
    ] {
        if let Some(v) = s(&body, k) {
            binds.push(B::T(v));
            sets.push(format!("{col} = ${}", binds.len()));
        }
    }
    if let Some(stt) = &status_body {
        if !ESTADOS_LEAD.contains(&stt.as_str()) {
            return Err(fallo());
        }
        binds.push(B::T(stt.clone()));
        sets.push(format!(r#"status = ${}::"LeadStatus""#, binds.len()));
    }
    // Estos campos se SOBREESCRIBEN siempre (null si no vienen), igual que en Next.
    let es_result = status_body.as_deref() == Some("RESULT");
    let lost_reason = if es_perdido { s(&body, "lostReason").map(|m| m.trim().to_string()) } else { None };
    for (col, val) in [
        ("phone", B::OT(s_o_nulo(&body, "phone"))),
        ("outcome", B::OT(if es_result { s_o_nulo(&body, "outcome") } else { None })),
        (r#""lostReason""#, B::OT(lost_reason)),
        (r#""estimatedValue""#, B::F(parse_float(body.get("estimatedValue")))),
        ("scope", B::OT(s_o_nulo(&body, "scope"))),
        ("repository", B::OT(s_o_nulo(&body, "repository"))),
        ("notes", B::OT(s_o_nulo(&body, "notes"))),
        ("tipo", B::OT(s_o_nulo(&body, "tipo"))),
        (r#""solucionAsociada""#, B::OT(s_o_nulo(&body, "solucionAsociada"))),
    ] {
        binds.push(val);
        sets.push(format!("{col} = ${}", binds.len()));
    }

    let sql = format!(
        r#"WITH up AS (UPDATE "Lead" SET {} WHERE id = $1 RETURNING *) SELECT {CON_USER_CLIENTE} FROM up l"#,
        sets.join(", ")
    );
    let lead = match fetch_json_opt(&st.pool, &sql, &binds).await {
        Ok(Some(l)) => l,
        Ok(None) => return Err(fallo()),
        Err(e) => {
            tracing::error!("{e}");
            return Err(fallo());
        }
    };
    let empresa = lead["companyName"].as_str().unwrap_or_default().to_string();

    // Crear o actualizar la Solución asociada (o quitarla si se vació el campo)
    if let Some(asociada) = s_o_nulo(&body, "solucionAsociada") {
        let tipo = tipo_solucion(&asociada);
        let r = exec(
            &st.pool,
            r#"INSERT INTO "Solucion" (id, nombre, descripcion, tipo, "valorEstimado", "leadId", "updatedAt")
               VALUES ($1, $2, $3, $4, $5, $6, NOW())
               ON CONFLICT ("leadId") DO UPDATE
                 SET nombre = EXCLUDED.nombre, descripcion = EXCLUDED.descripcion, tipo = EXCLUDED.tipo,
                     "valorEstimado" = EXCLUDED."valorEstimado", "updatedAt" = NOW()"#,
            &[
                B::T(new_id()),
                B::T(format!("{empresa} — {asociada}")),
                B::OT(s_o_nulo(&body, "scope")),
                B::T(tipo.to_string()),
                B::F(parse_float(body.get("estimatedValue"))),
                B::T(id.clone()),
            ],
        )
        .await;
        if let Err(e) = r {
            tracing::error!("{e}");
            return Err(fallo());
        }
    }
    // Sin solución asociada la Solución se queda (con su motor de fases): ya no se borra al vaciar el campo.

    let actor = if sesion.id.is_empty() { s(&body, "userId").unwrap_or_default() } else { sesion.id.clone() };
    let nuevo = status_body.clone().or_else(|| previo.clone());
    if previo != nuevo {
        log_activity(
            &st.pool,
            "STATUS_CHANGED",
            &format!("cambió el estado de {empresa} a {}", nuevo.unwrap_or_default()),
            "lead",
            &id,
            Some(&actor),
            Some(&id),
        )
        .await;
    } else {
        log_activity(&st.pool, "UPDATED", &format!("actualizó el lead {empresa}"), "lead", &id, Some(&actor), Some(&id))
            .await;
    }
    let nombre_actor = if sesion.name.is_empty() { sesion.email.clone() } else { sesion.name.clone() };
    crate::routes::fases::tras_actualizar_lead(&st, &id, &actor, &nombre_actor).await;
    Ok(Json(lead))
}

async fn eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let fallo = || ApiError::internal("Error al eliminar el lead");

    let empresa = fetch_text_opt(&st.pool, r#"SELECT "companyName" FROM "Lead" WHERE id = $1"#, &[B::T(id.clone())])
        .await
        .map_err(|_| fallo())?;

    // Las tres escrituras van en una transacción (en Next eran sentencias sueltas: un fallo a
    // mitad dejaba la actividad borrada y el lead vivo).
    let mut tx = st.pool.begin().await.map_err(|_| fallo())?;
    let pasos = async {
        sqlx::query(r#"DELETE FROM "Activity" WHERE "leadId" = $1"#).bind(&id).execute(&mut *tx).await?;
        sqlx::query(r#"UPDATE "Proposal" SET "leadId" = NULL WHERE "leadId" = $1"#).bind(&id).execute(&mut *tx).await?;
        sqlx::query(r#"DELETE FROM "Lead" WHERE id = $1"#).bind(&id).execute(&mut *tx).await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    if let Err(e) = pasos {
        tracing::error!("{e}");
        return Err(fallo());
    }
    tx.commit().await.map_err(|_| fallo())?;

    log_activity(
        &st.pool,
        "UPDATED",
        &format!("eliminó el lead {}", empresa.unwrap_or_default()),
        "lead",
        &id,
        Some(&sesion.id),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ── Notas ───────────────────────────────────────────────────────────────────────────────────
const ACTIVIDAD_CON_USER_ID_NAME: &str = r#"to_jsonb(a) || jsonb_build_object('user',
  (SELECT jsonb_build_object('id', u.id, 'name', u.name) FROM "User" u WHERE u.id = a."userId"))"#;

async fn notas_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg({ACTIVIDAD_CON_USER_ID_NAME} ORDER BY a."createdAt" DESC), '[]'::jsonb)
           FROM "Activity" a WHERE a."leadId" = $1 AND a.type = 'NOTE_ADDED'"#
    );
    Ok(Json(fetch_json(&st.pool, &sql, &[B::T(id)]).await?))
}

async fn nota_crear(
    State(st): State<AppState>,
    sesion: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    sin_sesion_de_usuario(&sesion, "No autorizado")?;
    let texto = s(&body, "text").map(|t| t.trim().to_string()).unwrap_or_default();
    if texto.is_empty() {
        return Err(ApiError::bad_request("Nota vacía"));
    }
    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "Activity" (id, type, description, "entityType", "entityId", "userId", "leadId", "createdAt")
             VALUES ($1, 'NOTE_ADDED', $2, 'lead', $3, $4, $3, NOW())
             RETURNING *)
           SELECT {} FROM ins a"#,
        ACTIVIDAD_CON_USER_ID_NAME
    );
    let nota = fetch_json(&st.pool, &sql, &[B::T(new_id()), B::T(texto), B::T(id), B::T(sesion.id.clone())]).await?;
    Ok(Json(nota))
}

// ── Interacciones ───────────────────────────────────────────────────────────────────────────
const ACTIVIDAD_INTERACCION: &str = r#"to_jsonb(a) || jsonb_build_object(
  'user', (SELECT jsonb_build_object('name', u.name) FROM "User" u WHERE u.id = a."userId"),
  'meeting', (SELECT jsonb_build_object('id', m.id, 'title', m.title, 'type', m."type", 'status', m.status,
        'date', m."date", 'endDate', m."endDate", 'link', m.link, 'attendees', m.attendees, 'location', m.location)
      FROM "Meeting" m WHERE m.id = a."meetingId"))"#;

async fn interacciones_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg({ACTIVIDAD_INTERACCION} ORDER BY a."createdAt" DESC), '[]'::jsonb)
           FROM "Activity" a WHERE a."leadId" = $1 AND a.type IN ('CALL', 'EMAIL', 'MEETING', 'WHATSAPP')"#
    );
    Ok(Json(fetch_json(&st.pool, &sql, &[B::T(id)]).await?))
}

async fn interaccion_crear(
    State(st): State<AppState>,
    sesion: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    sin_sesion_de_usuario(&sesion, "Unauthorized")?;
    if fetch_text_opt(&st.pool, r#"SELECT id FROM "User" WHERE id = $1"#, &[B::T(sesion.id.clone())]).await?.is_none() {
        return Err(ApiError::not_found("User not found"));
    }

    let tipo = s_no_vacio(&body, "type").unwrap_or_else(|| "MEETING".into());
    if !TIPOS_ACTIVIDAD.contains(&tipo.as_str()) {
        return Err(ApiError::bad_request("Tipo de interacción inválido"));
    }
    let mut descripcion = s(&body, "description").filter(|d| !d.is_empty());
    let mut fecha = s_o_nulo(&body, "date");
    let meeting_id = s_o_nulo(&body, "meetingId");

    // Si se vincula una reunión, la descripción y la fecha se toman de ella.
    if let Some(mid) = &meeting_id {
        if let Some(m) = fetch_json_opt(
            &st.pool,
            r#"SELECT jsonb_build_object('title', title, 'date', "date") FROM "Meeting" WHERE id = $1"#,
            &[B::T(mid.clone())],
        )
        .await?
        {
            descripcion = descripcion.or_else(|| m["title"].as_str().map(|t| t.to_string()));
            fecha = m["date"].as_str().map(|d| d.to_string());
        }
    }
    let Some(descripcion) = descripcion else {
        return Err(ApiError::bad_request("description requerida"));
    };

    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "Activity" (id, type, description, "entityType", "entityId", "userId", "leadId", "date", "meetingId", "createdAt")
             VALUES ($1, $2::"ActivityType", $3, 'lead', $4, $5, $4,
                     CASE WHEN $6::text IS NULL THEN NOW() AT TIME ZONE 'UTC'
                          ELSE $6::text::timestamptz AT TIME ZONE 'UTC' END,
                     $7, NOW())
             RETURNING *)
           SELECT {} FROM ins a"#,
        ACTIVIDAD_INTERACCION
    );
    let act = fetch_json(
        &st.pool,
        &sql,
        &[B::T(new_id()), B::T(tipo), B::T(descripcion), B::T(id), B::T(sesion.id.clone()), B::OT(fecha), B::OT(meeting_id)],
    )
    .await?;
    Ok(Json(act))
}

// ── Propuesta ───────────────────────────────────────────────────────────────────────────────
const PROPUESTA_COMPLETA: &str = r#"to_jsonb(p) || jsonb_build_object(
  'tasks', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY t."createdAt" ASC) FROM "ProposalTask" t WHERE t."proposalId" = p.id), '[]'::jsonb),
  'documents', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', d.id, 'name', d.name, 'url', d.url, 'type', d.type,
        'version', d.version, 'archived', d.archived, 'replacesId', d."replacesId", 'previewUrl', d."previewUrl",
        'createdAt', d."createdAt")) FROM "ProposalDocument" d WHERE d."proposalId" = p.id), '[]'::jsonb),
  'user', (SELECT jsonb_build_object('name', u.name) FROM "User" u WHERE u.id = p."userId"))"#;

async fn propuesta_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"SELECT {PROPUESTA_COMPLETA} FROM "Proposal" p WHERE p."leadId" = $1 ORDER BY p."createdAt" DESC LIMIT 1"#
    );
    Ok(Json(fetch_json_opt(&st.pool, &sql, &[B::T(id)]).await?.unwrap_or(Value::Null)))
}

async fn propuesta_guardar(
    State(st): State<AppState>,
    sesion: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    sin_sesion_de_usuario(&sesion, "Unauthorized")?;
    if fetch_text_opt(&st.pool, r#"SELECT id FROM "User" WHERE id = $1"#, &[B::T(sesion.id.clone())]).await?.is_none() {
        return Err(ApiError::not_found("User not found"));
    }
    let monto = numero(body.get("amount"));
    let estado_body = s_no_vacio(&body, "status");
    if let Some(e) = &estado_body {
        if !ESTADOS_PROPUESTA.contains(&e.as_str()) {
            return Err(ApiError::bad_request("Estado de propuesta inválido"));
        }
    }

    let existente = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', id, 'status', status::text) FROM "Proposal" WHERE "leadId" = $1 ORDER BY ctid LIMIT 1"#,
        &[B::T(id.clone())],
    )
    .await?;

    let propuesta_id = if let Some(ex) = existente {
        let pid = ex["id"].as_str().unwrap_or_default().to_string();
        let estado = estado_body.unwrap_or_else(|| ex["status"].as_str().unwrap_or("DRAFT").to_string());
        let mut sets = vec![r#""updatedAt" = NOW()"#.to_string(), "amount = $2".to_string(), r#"status = $3::"ProposalStatus""#.to_string()];
        let mut binds = vec![B::T(pid.clone()), B::F(monto), B::T(estado)];
        for k in ["title", "description"] {
            if let Some(v) = s(&body, k) {
                binds.push(B::T(v));
                sets.push(format!("{k} = ${}", binds.len()));
            }
        }
        exec(&st.pool, &format!(r#"UPDATE "Proposal" SET {} WHERE id = $1"#, sets.join(", ")), &binds).await?;
        pid
    } else {
        let (Some(titulo), Some(descripcion)) = (s(&body, "title"), s(&body, "description")) else {
            return Err(ApiError::bad_request("title y description son requeridos"));
        };
        let pid = new_id();
        exec(
            &st.pool,
            r#"INSERT INTO "Proposal" (id, title, description, amount, "leadId", "userId", "createdAt", "updatedAt")
               VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW())"#,
            &[B::T(pid.clone()), B::T(titulo), B::T(descripcion), B::F(monto), B::T(id), B::T(sesion.id.clone())],
        )
        .await?;
        pid
    };

    let sql = format!(r#"SELECT {PROPUESTA_COMPLETA} FROM "Proposal" p WHERE p.id = $1"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[B::T(propuesta_id)]).await?))
}

async fn propuesta_estado(
    State(st): State<AppState>,
    _s: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let Some(pid) = fetch_text_opt(
        &st.pool,
        r#"SELECT id FROM "Proposal" WHERE "leadId" = $1 ORDER BY ctid LIMIT 1"#,
        &[B::T(id)],
    )
    .await?
    else {
        return Err(ApiError::not_found("Not found"));
    };

    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds = vec![B::T(pid.clone())];
    if let Some(estado) = s_no_vacio(&body, "status") {
        if !ESTADOS_PROPUESTA.contains(&estado.as_str()) {
            return Err(ApiError::internal("Error al actualizar"));
        }
        if estado == "SENT" {
            sets.push(r#""sentDate" = NOW()"#.to_string());
        }
        if estado == "ACCEPTED" {
            sets.push(r#""acceptedDate" = NOW()"#.to_string());
        }
        binds.push(B::T(estado));
        sets.push(format!(r#"status = ${}::"ProposalStatus""#, binds.len()));
    }
    exec(&st.pool, &format!(r#"UPDATE "Proposal" SET {} WHERE id = $1"#, sets.join(", ")), &binds).await?;

    let sql = format!(r#"SELECT {PROPUESTA_COMPLETA} FROM "Proposal" p WHERE p.id = $1"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[B::T(pid)]).await?))
}

// ── Ítems de backlog de la solución del lead ────────────────────────────────────────────────
async fn backlog_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg((to_jsonb(b) - 'createdByAgentId' - 'createdByAgentName') || jsonb_build_object('sprint',
                 jsonb_build_object('name', sp.name, 'sprintCode', sp."sprintCode"))
                 ORDER BY sp."createdAt" ASC, b."order" ASC), '[]'::jsonb)
           FROM "BacklogItem" b
           JOIN "Sprint" sp ON sp.id = b."sprintId"
           WHERE b."solucionId" = (SELECT s.id FROM "Solucion" s WHERE s."leadId" = $1)"#,
        &[B::T(id)],
    )
    .await?;
    Ok(Json(v))
}

async fn backlog_estado(
    State(st): State<AppState>,
    _s: Session,
    Path(_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let (Some(item), Some(estado)) = (s_no_vacio(&body, "itemId"), s(&body, "status")) else {
        return Err(ApiError::internal("Error al actualizar"));
    };
    let _ = presente(&body, "status");
    let r = fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "BacklogItem" SET status = $2, "updatedAt" = NOW() WHERE id = $1 RETURNING *)
           SELECT (to_jsonb(up) - 'createdByAgentId' - 'createdByAgentName') || jsonb_build_object('sprint',
             (SELECT jsonb_build_object('name', sp.name, 'sprintCode', sp."sprintCode") FROM "Sprint" sp WHERE sp.id = up."sprintId"))
           FROM up"#,
        &[B::T(item), B::T(estado)],
    )
    .await?;
    r.map(Json).ok_or_else(|| ApiError::internal("Error al actualizar"))
}
