//! /api/meetings — Calendar (MASD PHUB-0001-0003).
//! Paridad con `src/app/api/meetings/**` (6 archivos de ruta de Next).

use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    google, llm,
    session::Session,
    state::AppState,
    texto,
    util::{
        exec, fetch_json, fetch_json_opt, log_activity, new_id, presente, s, s_no_vacio, s_o_nulo, truthy, ts_utc5,
        ts_utc5_opt, B,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/meetings", get(listar).post(crear))
        .route("/api/meetings/{id}", put(actualizar).delete(eliminar))
        .route("/api/meetings/{id}/actions", get(acciones_listar).post(accion_crear))
        .route(
            "/api/meetings/{id}/actions/{action_id}",
            put(accion_actualizar).delete(accion_eliminar),
        )
        .route("/api/meetings/{id}/hub", put(hub_guardar))
        .route("/api/meetings/{id}/acta-generate", post(acta_generar))
}

/// `include: { user: { select: { id, name, email } } }` de Prisma.
const CON_USER: &str = r#"to_jsonb(m) || jsonb_build_object('user',
    (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email) FROM "User" u WHERE u.id = m."userId"))"#;

async fn listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg({CON_USER} ORDER BY m."date" DESC), '[]'::jsonb) FROM "Meeting" m"#
    );
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    // Usuario de la sesión; si no hay (llamada de servicio), cae al body (compatibilidad).
    let uid = (if sesion.is_service { s_no_vacio(&body, "userId") } else { Some(sesion.id.clone()) })
        .ok_or_else(|| ApiError::new(axum::http::StatusCode::UNAUTHORIZED, "Usuario no autenticado"))?;

    let (Some(title), Some(date)) = (s_no_vacio(&body, "title"), s_no_vacio(&body, "date")) else {
        return Err(ApiError::bad_request("title y date son requeridos"));
    };

    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "Meeting" (id, title, description, "type", "date", "endDate", location, link, attendees,
                                    status, notes, "actaFile", "actaFileName", "userId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, {fecha}, {fin}, $7, $8, $9, $10, $11, $12, $13, $14, NOW(), NOW())
             RETURNING *)
           SELECT {CON_USER} FROM ins m"#,
        fecha = ts_utc5(5),
        fin = ts_utc5_opt(6),
    );
    let reunion = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::T(title.clone()),
            B::OT(s(&body, "description")),
            B::T(s_no_vacio(&body, "type").unwrap_or_else(|| "INTERNAL".into())),
            B::T(date),
            B::OT(s_o_nulo(&body, "endDate")),
            B::OT(s_o_nulo(&body, "location")),
            B::OT(s_o_nulo(&body, "link")),
            B::OT(s_o_nulo(&body, "attendees")),
            B::T(s_no_vacio(&body, "status").unwrap_or_else(|| "SCHEDULED".into())),
            B::OT(s_o_nulo(&body, "notes")),
            B::OT(s_o_nulo(&body, "actaFile")),
            B::OT(s_o_nulo(&body, "actaFileName")),
            B::T(uid.clone()),
        ],
    )
    .await?;

    let mid = reunion["id"].as_str().unwrap_or_default().to_string();
    google::crear(st.clone(), mid.clone());
    log_activity(&st.pool, "CREATED", &format!("creó la reunión {title}"), "meeting", &mid, Some(&uid), None).await;
    Ok(Json(reunion))
}

async fn actualizar(
    State(st): State<AppState>,
    sesion: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let existente = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('userId', "userId") FROM "Meeting" WHERE id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrada"))?;

    let fallo = || ApiError::internal("Error al actualizar");
    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(id.clone())];

    for (k, col) in [("title", r#""title""#), ("type", r#""type""#), ("status", "status")] {
        if presente(&body, k) {
            binds.push(B::T(s(&body, k).ok_or_else(fallo)?));
            sets.push(format!("{col} = ${}", binds.len()));
        }
    }
    for (k, col) in [
        ("description", "description"),
        ("location", "location"),
        ("link", "link"),
        ("attendees", "attendees"),
        ("notes", "notes"),
        ("actaFile", r#""actaFile""#),
        ("actaFileName", r#""actaFileName""#),
    ] {
        if presente(&body, k) {
            binds.push(B::OT(s_o_nulo(&body, k)));
            sets.push(format!("{col} = ${}", binds.len()));
        }
    }
    if presente(&body, "date") {
        binds.push(B::T(s(&body, "date").ok_or_else(fallo)?));
        sets.push(format!(r#""date" = {}"#, ts_utc5(binds.len())));
    }
    if presente(&body, "endDate") {
        binds.push(B::OT(s_o_nulo(&body, "endDate")));
        sets.push(format!(r#""endDate" = {}"#, ts_utc5_opt(binds.len())));
    }

    let sql = format!(
        r#"WITH up AS (UPDATE "Meeting" SET {} WHERE id = $1 RETURNING *)
           SELECT {CON_USER} FROM up m"#,
        sets.join(", ")
    );
    let reunion = match fetch_json_opt(&st.pool, &sql, &binds).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(fallo()),
        Err(e) => {
            tracing::error!("{e}");
            return Err(fallo());
        }
    };

    if ["status", "title", "date", "description", "location", "link", "attendees", "endDate"]
        .iter()
        .any(|k| truthy(&body, k))
    {
        google::actualizar(st.clone(), id.clone());
    }

    let titulo = reunion["title"].as_str().unwrap_or_default();
    let dueno = existente["userId"].as_str().filter(|u| !u.is_empty()).unwrap_or(&sesion.id);
    if presente(&body, "status") {
        let estado = match body.get("status") {
            Some(Value::String(x)) => x.clone(),
            Some(otro) => otro.to_string(),
            None => String::new(),
        };
        log_activity(
            &st.pool,
            "STATUS_CHANGED",
            &format!("cambió el estado de la reunión {titulo} a {estado}"),
            "meeting",
            &id,
            Some(dueno),
            None,
        )
        .await;
    } else {
        log_activity(&st.pool, "UPDATED", &format!("actualizó la reunión {titulo}"), "meeting", &id, Some(dueno), None)
            .await;
    }
    Ok(Json(reunion))
}

async fn eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let reunion = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('title', title, 'userId', "userId", 'googleEventId', "googleEventId")
           FROM "Meeting" WHERE id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrada"))?;
    let dueno = reunion["userId"].as_str().unwrap_or_default().to_string();

    // El id del evento se lee ANTES de borrar la reunión (en Next la lectura corría en paralelo
    // con el borrado y podía llegar tarde).
    if let Some(evento) = reunion["googleEventId"].as_str().filter(|e| !e.is_empty()) {
        google::eliminar(st.clone(), dueno.clone(), evento.to_string());
    }
    match exec(&st.pool, r#"DELETE FROM "Meeting" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(n) if n > 0 => {}
        _ => return Err(ApiError::internal("Error al eliminar")),
    }
    let actor = if dueno.is_empty() { sesion.id.clone() } else { dueno };
    log_activity(
        &st.pool,
        "UPDATED",
        &format!("eliminó la reunión {}", reunion["title"].as_str().unwrap_or_default()),
        "meeting",
        &id,
        Some(&actor),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ── Acciones (pendientes) de la reunión ─────────────────────────────────────────────────────
async fn acciones_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a."createdAt" ASC), '[]'::jsonb)
           FROM "MeetingAction" a WHERE a."meetingId" = $1"#,
        &[B::T(id)],
    )
    .await?;
    Ok(Json(v))
}

fn fecha_limite(body: &Value) -> Option<String> {
    // fechaLimite llega como "YYYY-MM-DD" (fecha local UTC-5) → fin del día
    s_o_nulo(body, "fechaLimite").map(|f| format!("{f}T23:59"))
}

async fn accion_crear(
    State(st): State<AppState>,
    _s: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let texto = s(&body, "texto").map(|t| t.trim().to_string()).unwrap_or_default();
    if texto.is_empty() {
        return Err(ApiError::bad_request("texto requerido"));
    }
    if fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Meeting" WHERE id = $1"#, &[B::T(id.clone())])
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("Reunión no encontrada"));
    }
    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "MeetingAction" (id, "meetingId", texto, responsable, "fechaLimite", estado, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, {}, 'PENDIENTE', NOW(), NOW())
             RETURNING *)
           SELECT to_jsonb(ins) FROM ins"#,
        ts_utc5_opt(5)
    );
    let accion = fetch_json(
        &st.pool,
        &sql,
        &[B::T(new_id()), B::T(id), B::T(texto), B::OT(s_o_nulo(&body, "responsable")), B::OT(fecha_limite(&body))],
    )
    .await?;
    Ok(Json(accion))
}

const ESTADOS: [&str; 3] = ["PENDIENTE", "EN_CURSO", "HECHA"];

async fn accion_actualizar(
    State(st): State<AppState>,
    _s: Session,
    Path((id, action_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let mut sets = vec![r#""updatedAt" = NOW()"#.to_string()];
    let mut binds: Vec<B> = vec![B::T(action_id.clone())];

    if let Some(Value::String(t)) = body.get("texto") {
        binds.push(B::T(t.clone()));
        sets.push(format!("texto = ${}", binds.len()));
    }
    if presente(&body, "responsable") {
        binds.push(B::OT(s_o_nulo(&body, "responsable")));
        sets.push(format!("responsable = ${}", binds.len()));
    }
    if presente(&body, "fechaLimite") {
        binds.push(B::OT(fecha_limite(&body)));
        sets.push(format!(r#""fechaLimite" = {}"#, ts_utc5_opt(binds.len())));
    }
    if presente(&body, "estado") {
        let e = s(&body, "estado").unwrap_or_default();
        if !ESTADOS.contains(&e.as_str()) {
            return Err(ApiError::bad_request("estado inválido"));
        }
        binds.push(B::T(e));
        sets.push(format!("estado = ${}", binds.len()));
    }

    if fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(1) FROM "MeetingAction" WHERE id = $1 AND "meetingId" = $2"#,
        &[B::T(action_id.clone()), B::T(id)],
    )
    .await?
    .is_none()
    {
        return Err(ApiError::not_found("No encontrada"));
    }
    let sql = format!(
        r#"WITH up AS (UPDATE "MeetingAction" SET {} WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#,
        sets.join(", ")
    );
    let accion = fetch_json(&st.pool, &sql, &binds).await?;
    Ok(Json(accion))
}

async fn accion_eliminar(
    State(st): State<AppState>,
    _s: Session,
    Path((id, action_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let n = exec(
        &st.pool,
        r#"DELETE FROM "MeetingAction" WHERE id = $1 AND "meetingId" = $2"#,
        &[B::T(action_id), B::T(id)],
    )
    .await?;
    if n == 0 {
        return Err(ApiError::not_found("No encontrada"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ── Hub de la reunión (autoguardado) ────────────────────────────────────────────────────────
async fn hub_guardar(
    State(st): State<AppState>,
    _s: Session,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let Some(hub) = s(&body, "hub") else {
        return Err(ApiError::bad_request("hub requerido"));
    };
    let r = fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "Meeting" SET hub = $2, "updatedAt" = NOW() WHERE id = $1 RETURNING id, "updatedAt")
           SELECT to_jsonb(up) FROM up"#,
        &[B::T(id), B::T(hub)],
    )
    .await?;
    r.map(Json).ok_or_else(|| ApiError::not_found("No encontrada"))
}

// ── Borrador de acta con IA ─────────────────────────────────────────────────────────────────
const SYSTEM_ACTA: &str = "Eres el secretario de una reunion en una empresa de tecnologia. Redactas el ACTA de la reunion en espanol, con tono formal y claro, usando UNICAMENTE la informacion que se te entrega.

Reglas:
- No inventes hechos, nombres, cifras, fechas ni acuerdos. Si algo no esta en los datos, no lo menciones o indica \"No se registro\".
- No conviertas ideas sueltas en decisiones. Solo lista como decision lo que las notas presenten como acuerdo o decision.
- Manten los nombres y terminos tecnicos tal como aparecen.
- Formato de salida: texto plano con esta marca minima: una linea \"# Titulo\" al inicio, \"## Seccion\" para cada seccion, \"- \" para cada elemento de lista. Sin tablas, sin negritas con asteriscos, sin bloques de codigo, sin emojis.
- Secciones, en este orden: \"## Datos de la reunion\" (fecha, hora, tipo, lugar, asistentes), \"## Temas tratados\" (siguiendo la agenda cuando exista), \"## Desarrollo\" (resumen de lo discutido segun el contenido), \"## Decisiones y acuerdos\", \"## Pendientes\" (cada uno con responsable y fecha limite si existen). Omite \"Desarrollo\" si no hay contenido; en las demas, si no hay datos, escribe \"- No se registraron ...\".
- Responde solo con el acta, sin comentarios previos ni posteriores.";

const MESES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];

/// `hub.notas` es HTML suelto (formato viejo) o JSON `{ tabs: [{ name, content }] }`.
fn contenido_del_hub(notas: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(notas) {
        if let Some(tabs) = v.get("tabs").and_then(|t| t.as_array()) {
            return tabs
                .iter()
                .map(|t| {
                    let n = t.get("name").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).unwrap_or("Nota");
                    let c = texto::html_a_texto_acta(t.get("content").and_then(|x| x.as_str()).unwrap_or(""));
                    (n.to_string(), c)
                })
                .filter(|(_, c)| !c.is_empty())
                .map(|(n, c)| format!("[{n}]\n{c}"))
                .collect::<Vec<_>>()
                .join("\n\n");
        }
    }
    texto::html_a_texto_acta(notas)
}

fn lista_textos(hub: &Value, clave: &str) -> Vec<String> {
    hub.get(clave)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|p| p.get("texto").and_then(|t| t.as_str()).unwrap_or("").trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn fecha_larga(r: &Value, p: &str) -> String {
    let mes = r[format!("{p}Mes")].as_u64().unwrap_or(1).clamp(1, 12) as usize;
    format!(
        "{} {} {}",
        r[format!("{p}Dia")].as_str().unwrap_or(""),
        MESES[mes - 1],
        r[format!("{p}Anio")].as_str().unwrap_or("")
    )
}

async fn acta_generar(
    State(st): State<AppState>,
    _s: Session,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let loc = "AT TIME ZONE 'UTC' AT TIME ZONE 'America/Bogota'";
    let sql = format!(
        r#"SELECT jsonb_build_object(
             'title', title, 'type', "type", 'location', location, 'hub', hub,
             'ini', to_char("date" {loc}, 'HH24:MI'),
             'iniDia', to_char("date" {loc}, 'DD'), 'iniMes', EXTRACT(MONTH FROM "date" {loc})::int,
             'iniAnio', to_char("date" {loc}, 'YYYY'), 'iniYmd', to_char("date" {loc}, 'YYYY-MM-DD'),
             'fin', CASE WHEN "endDate" IS NULL THEN NULL ELSE to_char("endDate" {loc}, 'HH24:MI') END)
           FROM "Meeting" WHERE id = $1"#
    );
    let m = fetch_json_opt(&st.pool, &sql, &[B::T(id.clone())])
        .await?
        .ok_or_else(|| ApiError::not_found("Reunión no encontrada"))?;

    // El cliente manda el hub tal como lo ve (el autoguardado tiene ~1 s de retraso).
    let hub_txt = match body.get("hub") {
        Some(Value::String(h)) => h.clone(),
        _ => m["hub"].as_str().filter(|h| !h.is_empty()).unwrap_or("{}").to_string(),
    };
    let hub: Value = serde_json::from_str(&hub_txt).unwrap_or_else(|_| json!({}));

    let acciones = fetch_json(
        &st.pool,
        &format!(
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(a) || jsonb_build_object('ymd',
                 CASE WHEN a."fechaLimite" IS NULL THEN NULL ELSE to_char(a."fechaLimite" {loc}, 'YYYY-MM-DD') END)
                 ORDER BY a."createdAt" ASC), '[]'::jsonb)
               FROM "MeetingAction" a WHERE a."meetingId" = $1"#
        ),
        &[B::T(id.clone())],
    )
    .await?;

    let puntos = lista_textos(&hub, "puntos");
    let decisiones = lista_textos(&hub, "decisiones");
    let contenido = contenido_del_hub(hub.get("notas").and_then(|n| n.as_str()).unwrap_or(""));
    let asistentes: Vec<String> = body
        .get("asistentes")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let es_daily = m["type"].as_str() == Some("INTERNAL_DAILY");

    let horario = format!(
        "{}{} (UTC-5)",
        m["ini"].as_str().unwrap_or(""),
        m["fin"].as_str().map(|f| format!(" a {f}")).unwrap_or_default()
    );
    let tipo = body
        .get("typeLabel")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string())
        .unwrap_or_else(|| m["type"].as_str().unwrap_or("").to_string());
    let asistentes_txt = if es_daily {
        "ArchitechIA (equipo interno)".to_string()
    } else if !asistentes.is_empty() {
        asistentes.join(", ")
    } else {
        "No registrados".to_string()
    };
    let pendientes = acciones
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    format!(
                        "- {} | responsable: {} | fecha limite: {} | estado: {}",
                        x["texto"].as_str().unwrap_or(""),
                        x["responsable"].as_str().filter(|r| !r.is_empty()).unwrap_or("sin asignar"),
                        x["ymd"].as_str().unwrap_or("sin fecha"),
                        x["estado"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let lista = |v: &[String], vacio: &str| {
        if v.is_empty() { vacio.to_string() } else { v.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n") }
    };

    let datos = [
        format!("Titulo: {}", m["title"].as_str().unwrap_or("")),
        format!("Fecha: {} ({})", fecha_larga(&m, "ini"), m["iniYmd"].as_str().unwrap_or("")),
        format!("Horario: {horario}"),
        format!("Tipo: {tipo}"),
        format!("Lugar: {}", m["location"].as_str().filter(|l| !l.is_empty()).unwrap_or("No especificado")),
        format!("Asistentes: {asistentes_txt}"),
        String::new(),
        format!("AGENDA:\n{}", lista(&puntos, "(sin puntos)")),
        String::new(),
        format!("CONTENIDO (notas de la reunion):\n{}", if contenido.is_empty() { "(vacio)" } else { contenido.as_str() }),
        String::new(),
        format!("DECISIONES REGISTRADAS:\n{}", lista(&decisiones, "(ninguna)")),
        String::new(),
        format!(
            "PENDIENTES:\n{}",
            if pendientes.is_empty() { "(ninguno)".to_string() } else { pendientes.join("\n") }
        ),
    ]
    .join("\n");
    let datos = texto::truncar(&datos, 24000);

    match llm::call_open_code(&st, SYSTEM_ACTA, &datos, &format!("acta-{id}"), 3000, 120).await {
        Ok(t) => Ok(Json(json!({ "texto": t.trim() }))),
        Err(e) => Err(ApiError::new(axum::http::StatusCode::BAD_GATEWAY, e)),
    }
}
