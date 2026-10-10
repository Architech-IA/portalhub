//! Propuestas, soluciones, iniciativas, hitos y riesgos de solución, productos — MASD PHUB-0001-0010
//! (bloque 2). Paridad con `src/app/api/{proposals,soluciones,iniciativas,hitos,riesgos,productos}/**`.
//! Siguen en Next (por el respaldo de `proxy.rs`): alta de solución (dispara una propuesta del
//! consejo con la CLI de IA), `soluciones/backfill`, las rutas de IA de `soluciones/{id}/*` y
//! `proposals/{id}/documents` (convierte archivos con LibreOffice).

use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{
        exec, fecha_cuerpo, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, log_activity_full, new_id, num_o_nulo, parse_float, presente, s,
        s_o_nulo, truthy, ts_js_opt, uid, Upd, B,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        // Propuestas
        .route("/api/proposals", get(propuestas_listar).post(propuesta_crear))
        .route("/api/proposals/{id}", put(propuesta_actualizar).delete(propuesta_eliminar))
        .route("/api/proposals/{id}/tasks", get(ptareas_listar).post(ptarea_crear))
        .route("/api/proposals/{id}/tasks/{task_id}", patch(ptarea_marcar).delete(ptarea_eliminar))
        // Soluciones
        .route("/api/soluciones", get(soluciones_listar).post(crate::routes::triggers::solucion_crear))
        .route("/api/soluciones/{id}", get(solucion_obtener).put(crate::routes::solucion_hub::solucion_actualizar).delete(crate::routes::solucion_hub::solucion_eliminar))
        // Iniciativas
        .route("/api/iniciativas", get(iniciativas_listar).post(iniciativa_crear))
        .route("/api/iniciativas/delete-requests", get(solicitudes_listar).post(solicitud_crear))
        .route("/api/iniciativas/delete-requests/{id}", put(solicitud_resolver))
        .route("/api/iniciativas/{id}", put(iniciativa_actualizar).delete(iniciativa_eliminar))
        .route("/api/iniciativas/{id}/convertir", post(iniciativa_convertir))
        // Hitos y riesgos de una solución
        .route("/api/hitos", get(hitos_listar).post(hito_crear))
        .route("/api/hitos/{id}", put(hito_actualizar).delete(hito_eliminar))
        .route("/api/riesgos", get(riesgos_listar).post(riesgo_crear))
        .route("/api/riesgos/{id}", put(riesgo_actualizar).delete(riesgo_eliminar))
        // Productos
        .route("/api/productos", get(productos_listar).post(producto_crear))
        .route("/api/productos/{id}", put(producto_actualizar).delete(producto_eliminar))
}

fn no_autenticado(sesion: &Session) -> Result<&str, ApiError> {
    sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autenticado"))
}

/// Valor de texto que Prisma recibiría: `x || defecto`. Un valor que no es texto (objeto, número)
/// hace fallar la consulta en Next, y acá también.
fn texto_o(body: &Value, k: &str, defecto: Option<&str>) -> Result<Option<String>, ApiError> {
    match body.get(k) {
        None | Some(Value::Null) => Ok(defecto.map(String::from)),
        Some(Value::String(x)) if x.is_empty() => Ok(defecto.map(String::from)),
        Some(Value::String(x)) => Ok(Some(x.clone())),
        Some(_) => Err(ApiError::internal("Error interno")),
    }
}

// ═══════════════════════════════ PROPUESTAS ═══════════════════════════════
const PROPUESTA_JSON: &str = r#"to_jsonb(p) || jsonb_build_object(
    'lead', (SELECT to_jsonb(l) FROM "Lead" l WHERE l.id = p."leadId"),
    'user', (SELECT jsonb_build_object('id', u.id, 'name', u.name, 'email', u.email) FROM "User" u WHERE u.id = p."userId"))"#;

async fn propuestas_listar(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    if let Some(id) = q.get("id").filter(|x| !x.is_empty()) {
        let sql = format!(
            r#"SELECT ({PROPUESTA_JSON}) || jsonb_build_object('activities', COALESCE((
                  SELECT jsonb_agg(to_jsonb(a) || jsonb_build_object('user', (SELECT jsonb_build_object('name', u2.name) FROM "User" u2 WHERE u2.id = a."userId")) ORDER BY a."createdAt" DESC)
                  FROM "Activity" a WHERE a."proposalId" = p.id), '[]'::jsonb))
               FROM "Proposal" p WHERE p.id = $1"#
        );
        return Ok(Json(fetch_json_opt(&st.pool, &sql, &[B::T(id.clone())]).await?.unwrap_or(Value::Null)));
    }
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({PROPUESTA_JSON} ORDER BY p."createdAt" DESC), '[]'::jsonb) FROM "Proposal" p"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn propuesta_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let sql = format!(
        r#"WITH p AS (INSERT INTO "Proposal" (id, title, description, status, amount, "leadId", "userId", "sentDate", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, COALESCE($4::"ProposalStatus", 'DRAFT'), $5, $6, $7, {}, NOW(), NOW()) RETURNING *)
           SELECT {PROPUESTA_JSON} FROM p"#,
        ts_js_opt(8)
    );
    let propuesta = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::OT(s(&body, "title")),
            B::OT(s(&body, "description")),
            B::OT(s(&body, "status")),
            B::F(parse_float(body.get("amount"))),
            B::OT(s(&body, "leadId")),
            B::OT(s(&body, "userId")),
            B::OT(fecha_cuerpo(&body, "sentDate")),
        ],
    )
    .await?;
    let pid = propuesta["id"].as_str().unwrap_or_default().to_string();
    log_activity_full(
        &st.pool,
        "CREATED",
        &format!("creó la propuesta \"{}\"", s(&body, "title").unwrap_or_default()),
        "proposal",
        &pid,
        s(&body, "userId").as_deref(),
        s_o_nulo(&body, "leadId").as_deref(),
        Some(&pid),
        None,
    )
    .await;
    Ok(Json(propuesta))
}

async fn propuesta_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let fallo = |e: sqlx::Error| {
        tracing::error!("propuesta_actualizar: {e}");
        ApiError::internal("Error al actualizar")
    };
    let previo = fetch_text_opt(&st.pool, r#"SELECT status::text FROM "Proposal" WHERE id = $1"#, &[B::T(id.clone())]).await.map_err(fallo)?;
    let mut up = Upd::new(&id);
    if let Some(v) = s(&body, "title") {
        up.set("title", B::T(v));
    }
    if let Some(v) = s(&body, "description") {
        up.set("description", B::T(v));
    }
    if let Some(v) = s(&body, "status") {
        up.set_expr("status", B::T(v), "{n}::\"ProposalStatus\"");
    }
    up.set("amount", B::F(parse_float(body.get("amount"))));
    up.set("leadId", B::OT(s_o_nulo(&body, "leadId")));
    if let Some(v) = s(&body, "userId") {
        up.set("userId", B::T(v));
    }
    up.set_expr("sentDate", B::OT(fecha_cuerpo(&body, "sentDate")), &ts_js_opt(0).replace("$0", "{n}"));
    let sql = up.con("Proposal", &format!("SELECT {PROPUESTA_JSON} FROM up p"));
    let propuesta = fetch_json_opt(&st.pool, &sql, &up.binds).await.map_err(fallo)?.ok_or_else(|| ApiError::internal("Error al actualizar"))?;

    let actor = if !sesion.id.is_empty() { Some(sesion.id.clone()) } else { s(&body, "userId") };
    let titulo = s(&body, "title").unwrap_or_default();
    let estado = s(&body, "status");
    if previo.as_deref() != estado.as_deref() {
        log_activity_full(&st.pool, "STATUS_CHANGED", &format!("cambió propuesta \"{titulo}\" a estado {}", estado.unwrap_or_default()), "proposal", &id, actor.as_deref(), None, Some(&id), None).await;
    } else {
        log_activity_full(&st.pool, "UPDATED", &format!("actualizó la propuesta \"{titulo}\""), "proposal", &id, actor.as_deref(), None, Some(&id), None).await;
    }
    Ok(Json(propuesta))
}

async fn propuesta_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let fallo = |e: sqlx::Error| {
        tracing::error!("propuesta_eliminar: {e}");
        ApiError::internal("Error al eliminar")
    };
    exec(&st.pool, r#"DELETE FROM "Activity" WHERE "proposalId" = $1"#, &[B::T(id.clone())]).await.map_err(fallo)?;
    if exec(&st.pool, r#"DELETE FROM "Proposal" WHERE id = $1"#, &[B::T(id)]).await.map_err(fallo)? == 0 {
        return Err(ApiError::internal("Error al eliminar"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn ptareas_listar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.completed ASC, t."createdAt" ASC), '[]'::jsonb) FROM "ProposalTask" t WHERE t."proposalId" = $1"#,
            &[B::T(id)],
        )
        .await?,
    ))
}

async fn ptarea_crear(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"WITH ins AS (INSERT INTO "ProposalTask" (id, title, "proposalId", completed, "createdAt", "updatedAt") VALUES ($1, $2, $3, false, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
            &[B::T(new_id()), B::OT(s(&body, "title")), B::T(id)],
        )
        .await?,
    ))
}

async fn ptarea_marcar(State(st): State<AppState>, _s: Session, Path((_id, tarea)): Path<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&tarea);
    if let Some(c) = body.get("completed").and_then(|v| v.as_bool()) {
        up.set("completed", B::Bo(c));
    }
    fetch_json_opt(&st.pool, &up.sql("ProposalTask"), &up.binds).await?.map(Json).ok_or_else(|| ApiError::internal("Error interno"))
}

async fn ptarea_eliminar(State(st): State<AppState>, _s: Session, Path((_id, tarea)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    if exec(&st.pool, r#"DELETE FROM "ProposalTask" WHERE id = $1"#, &[B::T(tarea)]).await? == 0 {
        return Err(ApiError::internal("Error interno"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════ SOLUCIONES ═══════════════════════════════
pub const SOLUCION_JSON: &str = r#"to_jsonb(so) || jsonb_build_object(
    'lead', (SELECT jsonb_build_object('id', l.id, 'companyName', l."companyName", 'contactName', l."contactName", 'status', l.status) FROM "Lead" l WHERE l.id = so."leadId"))"#;

async fn soluciones_listar(State(st): State<AppState>, _s: Session, req: Request) -> Response {
    // `ensureInternSolution` de Next crea la solución interna si falta (necesita generar su código
    // único): ese caso raro se deja a Next.
    let existe = fetch_text_opt(
        &st.pool,
        r#"SELECT id FROM "Solucion" WHERE tipo = 'INTERN' AND nombre = 'Portal Interno ArchitechIA' LIMIT 1"#,
        &[],
    )
    .await;
    match existe {
        Ok(Some(_)) => {}
        Ok(None) => {
            // `ensureInternSolution`: la solución interna del portal se crea la primera vez.
            let nombre = "Portal Interno ArchitechIA";
            let r: Result<(), sqlx::Error> = async {
                let codigo = crate::routes::council::codigo_solucion_unico(&st, &crate::routes::council::generar_codigo_solucion(nombre)).await?;
                exec(
                    &st.pool,
                    r#"INSERT INTO "Solucion" (id, nombre, descripcion, tipo, estado, "valorEstimado", "solucionCode", "createdAt", "updatedAt")
                       VALUES ($1, $2, 'Solución interna que agrupa el portal, herramientas y plataformas de ArchiTechIA.', 'INTERN', 'ACTIVO', 0, $3, NOW(), NOW())"#,
                    &[B::T(new_id()), B::T(nombre.into()), B::T(codigo)],
                )
                .await?;
                Ok(())
            }
            .await;
            if let Err(e) = r {
                tracing::error!("soluciones_listar (solución interna): {e}");
                return ApiError::internal("Error interno").into_response();
            }
        }
        Err(e) => {
            tracing::error!("soluciones_listar: {e}");
            return ApiError::internal("Error interno").into_response();
        }
    }
    let tipo = Query::<HashMap<String, String>>::try_from_uri(req.uri()).ok().and_then(|q| q.0.get("tipo").cloned()).filter(|t| !t.is_empty());
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({SOLUCION_JSON} ORDER BY so."createdAt" DESC), '[]'::jsonb) FROM "Solucion" so WHERE ($1::text IS NULL OR so.tipo = $1)"#);
    static CACHE: std::sync::LazyLock<crate::util::CacheJson> = std::sync::LazyLock::new(Default::default);
    let clave = tipo.clone().unwrap_or_default();
    match crate::util::fetch_json_cacheado(&st.pool, &CACHE, &clave, &sql, &[B::OT(tipo)]).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}

async fn solucion_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT {SOLUCION_JSON} FROM "Solucion" so WHERE so.id = $1"#);
    fetch_json_opt(&st.pool, &sql, &[B::T(id)]).await?.map(Json).ok_or_else(|| ApiError::not_found("No encontrado"))
}

// ═══════════════════════════════ INICIATIVAS ═══════════════════════════════
/// `tecnologias` se guarda como texto JSON; la API la devuelve como arreglo.
fn con_tecnologias(mut v: Value) -> Value {
    let arr = v["tecnologias"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()).filter(|a| a.is_array()).unwrap_or_else(|| json!([]));
    v["tecnologias"] = arr;
    v
}

async fn iniciativas_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let v = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(i) ORDER BY i."createdAt" DESC), '[]'::jsonb) FROM "Iniciativa" i"#, &[]).await?;
    Ok(Json(Value::Array(v.as_array().cloned().unwrap_or_default().into_iter().map(con_tecnologias).collect())))
}

fn texto_tecnologias(body: &Value) -> String {
    match body.get("tecnologias") {
        Some(Value::Array(a)) => Value::Array(a.clone()).to_string(),
        _ => "[]".to_string(),
    }
}

fn trim_o_nulo(body: &Value, k: &str) -> Option<String> {
    s(body, k).map(|x| x.trim().to_string()).filter(|x| !x.is_empty())
}

async fn iniciativa_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let (Some(nombre), Some(descripcion)) = (s(&body, "nombre").filter(|x| !x.trim().is_empty()), s(&body, "descripcion").filter(|x| !x.trim().is_empty())) else {
        return Err(ApiError::bad_request("Nombre y descripción son obligatorios"));
    };
    let fila = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "Iniciativa" (id, nombre, descripcion, categoria, estado, prioridad, sector, problema, beneficios, tecnologias,
                                 "costoMin", "costoMax", "tiempoEstimado", "roiEstimado", color, responsable, "responsableId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, COALESCE($4, 'IA/ML'), COALESCE($5, 'IDEA'), COALESCE($6, 'MEDIA'), $7, $8, $9, $10, $11, $12, $13, $14,
                     COALESCE($15, 'from-orange-500 to-red-600'), $16, $17, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::T(nombre.clone()),
            B::T(descripcion),
            B::OT(s_o_nulo(&body, "categoria")),
            B::OT(s_o_nulo(&body, "estado")),
            B::OT(s_o_nulo(&body, "prioridad")),
            B::OT(s_o_nulo(&body, "sector")),
            B::OT(s_o_nulo(&body, "problema")),
            B::OT(s_o_nulo(&body, "beneficios")),
            B::T(texto_tecnologias(&body)),
            B::OF(num_o_nulo(&body, "costoMin")),
            B::OF(num_o_nulo(&body, "costoMax")),
            B::OT(trim_o_nulo(&body, "tiempoEstimado")),
            B::OT(s_o_nulo(&body, "roiEstimado")),
            B::OT(s_o_nulo(&body, "color")),
            B::OT(Some(sesion.name.clone()).filter(|n| !n.is_empty())),
            B::OT(Some(sesion.id.clone()).filter(|n| !n.is_empty())),
        ],
    )
    .await?;
    let iid = fila["id"].as_str().unwrap_or_default().to_string();
    log_activity(&st.pool, "CREATED", &format!("creó la iniciativa {nombre}"), "iniciativa", &iid, uid(&sesion), None).await;
    Ok(Json(con_tecnologias(fila)))
}

async fn iniciativa_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let fallo = || ApiError::internal("Error al actualizar");
    let mut up = Upd::new(&id);
    for k in ["nombre", "descripcion", "categoria", "estado", "prioridad"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    for k in ["sector", "problema", "beneficios"] {
        up.set(k, B::OT(s_o_nulo(&body, k)));
    }
    if presente(&body, "tecnologias") {
        up.set("tecnologias", B::T(texto_tecnologias(&body)));
    }
    up.set("costoMin", B::OF(num_o_nulo(&body, "costoMin")));
    up.set("costoMax", B::OF(num_o_nulo(&body, "costoMax")));
    up.set("tiempoEstimado", B::OT(trim_o_nulo(&body, "tiempoEstimado")));
    up.set("roiEstimado", B::OT(s_o_nulo(&body, "roiEstimado")));
    if let Some(c) = s_o_nulo(&body, "color") {
        up.set("color", B::T(c));
    }
    let fila = match fetch_json_opt(&st.pool, &up.sql("Iniciativa"), &up.binds).await {
        Ok(Some(v)) => v,
        _ => return Err(fallo()),
    };
    let nombre = s(&body, "nombre").unwrap_or_default();
    if presente(&body, "estado") {
        log_activity(&st.pool, "STATUS_CHANGED", &format!("cambió el estado de la iniciativa {nombre} a {}", s(&body, "estado").unwrap_or_default()), "iniciativa", &id, uid(&sesion), None).await;
    } else {
        log_activity(&st.pool, "UPDATED", &format!("actualizó la iniciativa {nombre}"), "iniciativa", &id, uid(&sesion), None).await;
    }
    Ok(Json(con_tecnologias(fila)))
}

async fn iniciativa_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if sesion.role != "SUPERADMIN" {
        return Err(ApiError::forbidden("Solo el Super Admin puede eliminar iniciativas. Envía una solicitud de eliminación."));
    }
    let fallo = || ApiError::internal("Error al eliminar");
    let nombre = fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Iniciativa" WHERE id = $1"#, &[B::T(id.clone())]).await.map_err(|_| fallo())?;
    match exec(&st.pool, r#"DELETE FROM "Iniciativa" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(n) if n > 0 => {}
        _ => return Err(fallo()),
    }
    log_activity(&st.pool, "UPDATED", &format!("eliminó la iniciativa {}", nombre.unwrap_or_default()), "iniciativa", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "ok": true })))
}

async fn iniciativa_convertir(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let user = no_autenticado(&sesion)?.to_string();
    let Some(ini) = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(i) FROM "Iniciativa" i WHERE i.id = $1"#, &[B::T(id.clone())]).await? else {
        return Err(ApiError::not_found("Iniciativa no encontrada"));
    };
    if ini["proyectoId"].as_str().map(|x| !x.is_empty()).unwrap_or(false) {
        return Err(ApiError::bad_request("Esta iniciativa ya tiene un proyecto asociado"));
    }
    let prioridad = match ini["prioridad"].as_str().unwrap_or("") {
        "BAJA" => "LOW",
        "MEDIA" => "MEDIUM",
        "ALTA" => "HIGH",
        "CRITICA" => "CRITICAL",
        _ => "MEDIUM",
    };
    let estado_nuevo = match ini["estado"].as_str().unwrap_or("") {
        e @ ("IDEA" | "EVALUACION" | "APROBADA") => {
            let _ = e;
            "EN_EJECUCION".to_string()
        }
        otro => otro.to_string(),
    };
    let proyecto_id = new_id();
    let mut tx = st.pool.begin().await?;
    sqlx::query(r#"INSERT INTO "Project" (id, name, description, status, priority, "createdAt", "updatedAt") VALUES ($1, $2, $3, 'PLANNING', $4::"Priority", NOW(), NOW())"#)
        .bind(&proyecto_id)
        .bind(ini["nombre"].as_str())
        .bind(ini["descripcion"].as_str())
        .bind(prioridad)
        .execute(&mut *tx)
        .await?;
    sqlx::query(r#"INSERT INTO "ProjectUser" ("projectId", "userId", role, "assignedAt") VALUES ($1, $2, 'OWNER', NOW())"#).bind(&proyecto_id).bind(&user).execute(&mut *tx).await?;
    tx.commit().await?;
    let actualizada = fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "Iniciativa" SET "proyectoId" = $2, estado = $3, "updatedAt" = NOW() WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#,
        &[B::T(id), B::T(proyecto_id.clone()), B::T(estado_nuevo)],
    )
    .await?
    .ok_or_else(|| ApiError::internal("Error interno"))?;
    Ok(Json(json!({ "projectId": proyecto_id, "iniciativa": actualizada })))
}

async fn solicitudes_listar(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    if sesion.role != "SUPERADMIN" {
        return Err(ApiError::forbidden("No autorizado"));
    }
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(r) || jsonb_build_object('iniciativa', (SELECT jsonb_build_object('id', i.id, 'nombre', i.nombre) FROM "Iniciativa" i WHERE i.id = r."iniciativaId"))
                                         ORDER BY r."createdAt" DESC), '[]'::jsonb) FROM "IniciativaDeleteRequest" r WHERE r.status = 'PENDIENTE'"#,
            &[],
        )
        .await?,
    ))
}

async fn solicitud_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> Response {
    let r: ApiResult<Response> = async {
        no_autenticado(&sesion)?;
        let Some(inic) = s_o_nulo(&body, "iniciativaId") else {
            return Err(ApiError::bad_request("Falta la iniciativa"));
        };
        if let Some(existente) = fetch_json_opt(
            &st.pool,
            r#"SELECT to_jsonb(r) FROM "IniciativaDeleteRequest" r WHERE r."iniciativaId" = $1 AND r.status = 'PENDIENTE' LIMIT 1"#,
            &[B::T(inic.clone())],
        )
        .await?
        {
            return Ok((StatusCode::CONFLICT, Json(json!({ "error": "Ya existe una solicitud pendiente para esta iniciativa", "request": existente }))).into_response());
        }
        let nombre = if !sesion.name.is_empty() { Some(sesion.name.clone()) } else { Some(sesion.email.clone()).filter(|e| !e.is_empty()) };
        let fila = fetch_json(
            &st.pool,
            r#"WITH ins AS (INSERT INTO "IniciativaDeleteRequest" (id, "iniciativaId", reason, "requestedById", "requesterName", status, "createdAt")
                 VALUES ($1, $2, $3, $4, $5, 'PENDIENTE', NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
            &[B::T(new_id()), B::T(inic), B::OT(s_o_nulo(&body, "reason")), B::T(sesion.id.clone()), B::OT(nombre)],
        )
        .await?;
        Ok(Json(fila).into_response())
    }
    .await;
    r.unwrap_or_else(|e| e.into_response())
}

async fn solicitud_resolver(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if sesion.role != "SUPERADMIN" {
        return Err(ApiError::forbidden("No autorizado"));
    }
    let Some(req) = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(r) FROM "IniciativaDeleteRequest" r WHERE r.id = $1"#, &[B::T(id.clone())]).await? else {
        return Err(ApiError::not_found("Solicitud no encontrada"));
    };
    if req["status"].as_str() != Some("PENDIENTE") {
        return Err(ApiError::bad_request("La solicitud ya fue resuelta"));
    }
    let nombre_resuelve = if !sesion.name.is_empty() { Some(sesion.name.clone()) } else { Some(sesion.email.clone()).filter(|e| !e.is_empty()) };
    let resolver = |estado: &'static str| {
        let st = st.clone();
        let id = id.clone();
        let sesion = sesion.clone();
        let nombre = nombre_resuelve.clone();
        async move {
            fetch_json_opt(
                &st.pool,
                r#"WITH up AS (UPDATE "IniciativaDeleteRequest" SET status = $2, "resolvedById" = $3, "resolvedByName" = $4, "resolvedAt" = NOW() WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#,
                &[B::T(id), B::T(estado.to_string()), B::T(sesion.id.clone()), B::OT(nombre)],
            )
            .await
        }
    };
    match s(&body, "action").as_deref() {
        Some("APROBAR") => {
            resolver("APROBADA").await?;
            let inic = req["iniciativaId"].as_str().unwrap_or_default().to_string();
            match exec(&st.pool, r#"DELETE FROM "Iniciativa" WHERE id = $1"#, &[B::T(inic.clone())]).await {
                Ok(n) if n > 0 => Ok(Json(json!({ "ok": true, "deleted": inic }))),
                _ => Err(ApiError::internal("Error al eliminar la iniciativa")),
            }
        }
        Some("RECHAZAR") => Ok(Json(resolver("RECHAZADA").await?.unwrap_or(Value::Null))),
        _ => Err(ApiError::bad_request("Acción inválida")),
    }
}

// ═══════════════════════════════ HITOS ═══════════════════════════════
async fn hitos_listar(State(st): State<AppState>, sesion: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let Some(sol) = q.get("solucionId").filter(|x| !x.is_empty()) else {
        return Err(ApiError::bad_request("solucionId requerido"));
    };
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(h) || jsonb_build_object('sprint', (SELECT jsonb_build_object('id', s.id, 'sprintCode', s."sprintCode", 'name', s.name,
                   'total', (SELECT COUNT(*) FROM "BacklogItem" i WHERE i."sprintId" = s.id),
                   'hechas', (SELECT COUNT(*) FROM "BacklogItem" i WHERE i."sprintId" = s.id AND i.status = 'DONE')) FROM "Sprint" s WHERE s.id = h."sprintId"))
                 ORDER BY h."fechaComprometida" ASC), '[]'::jsonb) FROM "Hito" h WHERE h."solucionId" = $1"#,
            &[B::T(sol.clone())],
        )
        .await?,
    ))
}

async fn hito_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let (Some(sol), Some(titulo)) = (s_o_nulo(&body, "solucionId"), s(&body, "titulo").map(|t| t.trim().to_string()).filter(|t| !t.is_empty())) else {
        return Err(ApiError::bad_request("solucionId y titulo son requeridos"));
    };
    let sql = format!(
        r#"WITH ins AS (INSERT INTO "Hito" (id, "solucionId", titulo, descripcion, "fechaComprometida", "fechaReal", estado, monto, "estadoPago", "sprintId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, {}, {}, COALESCE($7, 'PENDIENTE'), COALESCE($8::float8, 0), 'PENDIENTE', $9, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        ts_js_opt(5),
        ts_js_opt(6)
    );
    Ok(Json(
        fetch_json(
            &st.pool,
            &sql,
            &[
                B::T(new_id()),
                B::T(sol),
                B::T(titulo),
                B::OT(s_o_nulo(&body, "descripcion")),
                B::OT(fecha_cuerpo(&body, "fechaComprometida")),
                B::OT(fecha_cuerpo(&body, "fechaReal")),
                B::OT(s_o_nulo(&body, "estado")),
                B::OF(num_o_nulo(&body, "monto")),
                B::OT(s_o_nulo(&body, "sprintId")),
            ],
        )
        .await?,
    ))
}

async fn hito_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let fallo = || ApiError::internal("Error al actualizar el hito");
    let mut up = Upd::new(&id);
    if let Some(t) = s(&body, "titulo") {
        up.set("titulo", B::T(t));
    }
    if presente(&body, "descripcion") {
        up.set("descripcion", B::OT(s_o_nulo(&body, "descripcion")));
    }
    for k in ["fechaComprometida", "fechaReal"] {
        if presente(&body, k) {
            up.set_expr(k, B::OT(fecha_cuerpo(&body, k)), &ts_js_opt(0).replace("$0", "{n}"));
        }
    }
    if let Some(e) = s(&body, "estado") {
        if e == "CUMPLIDO" && !presente(&body, "fechaReal") {
            up.sets.push(r#""fechaReal" = COALESCE("fechaReal", NOW())"#.to_string());
        }
        up.set("estado", B::T(e));
    }
    if presente(&body, "monto") {
        up.set("monto", B::F(parse_float(body.get("monto"))));
    }
    if presente(&body, "sprintId") {
        up.set("sprintId", B::OT(s_o_nulo(&body, "sprintId")));
    }
    if let Some(p) = s(&body, "estadoPago") {
        if !["PENDIENTE", "FACTURADO", "PAGADO"].contains(&p.as_str()) {
            return Err(ApiError::bad_request("Estado de pago inválido"));
        }
        if p != "PENDIENTE" && !(sesion.is_admin() || sesion.is_service) {
            return Err(ApiError::forbidden("Solo un administrador registra facturas y pagos"));
        }
        up.set("estadoPago", B::T(p));
    }
    // Aceptación del cliente: un objeto { nombre, nota } la registra; null la quita (solo administradores).
    if let Some(a) = body.get("aceptar") {
        if !(sesion.is_admin() || sesion.is_service) {
            return Err(ApiError::forbidden("Solo un administrador registra la aceptación del cliente"));
        }
        if a.is_object() {
            let quien = s_o_nulo(a, "nombre").unwrap_or_else(|| if sesion.name.is_empty() { sesion.email.clone() } else { sesion.name.clone() });
            up.set("aceptadoPor", B::T(quien));
            up.sets.push(r#""aceptadoEn" = NOW()"#.to_string());
            up.set("aceptadoNota", B::OT(s_o_nulo(a, "nota")));
        } else {
            up.sets.push(r#""aceptadoEn" = NULL"#.to_string());
            up.set("aceptadoPor", B::OT(None));
            up.set("aceptadoNota", B::OT(None));
        }
    }
    match fetch_json_opt(&st.pool, &up.sql("Hito"), &up.binds).await {
        Ok(Some(v)) => Ok(Json(v)),
        _ => Err(fallo()),
    }
}

async fn hito_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    match exec(&st.pool, r#"DELETE FROM "Hito" WHERE id = $1"#, &[B::T(id)]).await {
        Ok(n) if n > 0 => Ok(Json(json!({ "ok": true }))),
        _ => Err(ApiError::internal("Error al eliminar el hito")),
    }
}

// ═══════════════════════════════ RIESGOS ═══════════════════════════════
async fn riesgos_listar(State(st): State<AppState>, sesion: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let Some(sol) = q.get("solucionId").filter(|x| !x.is_empty()) else {
        return Err(ApiError::bad_request("solucionId requerido"));
    };
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY r."createdAt" ASC), '[]'::jsonb) FROM "Riesgo" r WHERE r."solucionId" = $1"#,
            &[B::T(sol.clone())],
        )
        .await?,
    ))
}

async fn riesgo_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let (Some(sol), Some(titulo)) = (s_o_nulo(&body, "solucionId"), s(&body, "titulo").map(|t| t.trim().to_string()).filter(|t| !t.is_empty())) else {
        return Err(ApiError::bad_request("solucionId y titulo son requeridos"));
    };
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"WITH ins AS (INSERT INTO "Riesgo" (id, "solucionId", titulo, descripcion, severidad, probabilidad, mitigacion, estado, responsable, "createdAt", "updatedAt")
                 VALUES ($1, $2, $3, $4, COALESCE($5, 'MEDIA'), COALESCE($6, 'MEDIA'), $7, COALESCE($8, 'ABIERTO'), $9, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
            &[
                B::T(new_id()),
                B::T(sol),
                B::T(titulo),
                B::OT(s_o_nulo(&body, "descripcion")),
                B::OT(s_o_nulo(&body, "severidad")),
                B::OT(s_o_nulo(&body, "probabilidad")),
                B::OT(s_o_nulo(&body, "mitigacion")),
                B::OT(s_o_nulo(&body, "estado")),
                B::OT(s_o_nulo(&body, "responsable")),
            ],
        )
        .await?,
    ))
}

async fn riesgo_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let mut up = Upd::new(&id);
    for k in ["titulo", "severidad", "probabilidad", "estado"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    for k in ["descripcion", "mitigacion", "responsable"] {
        if presente(&body, k) {
            up.set(k, B::OT(s_o_nulo(&body, k)));
        }
    }
    match fetch_json_opt(&st.pool, &up.sql("Riesgo"), &up.binds).await {
        Ok(Some(v)) => Ok(Json(v)),
        _ => Err(ApiError::internal("Error al actualizar el riesgo")),
    }
}

async fn riesgo_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    match exec(&st.pool, r#"DELETE FROM "Riesgo" WHERE id = $1"#, &[B::T(id)]).await {
        Ok(n) if n > 0 => Ok(Json(json!({ "ok": true }))),
        _ => Err(ApiError::internal("Error al eliminar el riesgo")),
    }
}

// ═══════════════════════════════ PRODUCTOS ═══════════════════════════════
async fn productos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let v = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p."createdAt" DESC), '[]'::jsonb) FROM "Producto" p"#, &[]).await?;
    let mut out = vec![];
    for mut p in v.as_array().cloned().unwrap_or_default() {
        for k in ["tecnologias", "caracteristicas"] {
            let parsed = p[k].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()).ok_or_else(|| ApiError::internal("Error interno"))?;
            p[k] = parsed;
        }
        out.push(p);
    }
    Ok(Json(Value::Array(out)))
}

/// `JSON.stringify(x)`: `undefined` (clave ausente) hace fallar la consulta en Next.
fn json_texto(body: &Value, k: &str) -> Result<String, ApiError> {
    body.get(k).map(|v| v.to_string()).ok_or_else(|| ApiError::internal("Error interno"))
}

async fn producto_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let fila = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "Producto" (id, nombre, version, estado, descripcion, tecnologias, caracteristicas, icono, color, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::OT(s(&body, "nombre")),
            B::OT(s(&body, "version")),
            B::OT(s(&body, "estado")),
            B::OT(s(&body, "descripcion")),
            B::T(json_texto(&body, "tecnologias")?),
            B::T(json_texto(&body, "caracteristicas")?),
            B::OT(s(&body, "icono")),
            B::OT(s(&body, "color")),
        ],
    )
    .await?;
    let pid = fila["id"].as_str().unwrap_or_default().to_string();
    log_activity(&st.pool, "CREATED", &format!("creó el producto {}", s(&body, "nombre").unwrap_or_default()), "producto", &pid, uid(&sesion), None).await;
    let mut out = fila;
    out["tecnologias"] = body["tecnologias"].clone();
    out["caracteristicas"] = body["caracteristicas"].clone();
    Ok(Json(out))
}

async fn producto_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al actualizar");
    let mut up = Upd::new(&id);
    for k in ["nombre", "version", "estado", "descripcion", "icono", "color"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    up.set("tecnologias", B::T(json_texto(&body, "tecnologias").map_err(|_| fallo())?));
    up.set("caracteristicas", B::T(json_texto(&body, "caracteristicas").map_err(|_| fallo())?));
    let mut fila = match fetch_json_opt(&st.pool, &up.sql("Producto"), &up.binds).await {
        Ok(Some(v)) => v,
        _ => return Err(fallo()),
    };
    log_activity(&st.pool, "UPDATED", &format!("actualizó el producto {}", s(&body, "nombre").unwrap_or_default()), "producto", &id, uid(&sesion), None).await;
    fila["tecnologias"] = body["tecnologias"].clone();
    fila["caracteristicas"] = body["caracteristicas"].clone();
    Ok(Json(fila))
}

async fn producto_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al eliminar");
    let nombre = fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Producto" WHERE id = $1"#, &[B::T(id.clone())]).await.map_err(|_| fallo())?;
    match exec(&st.pool, r#"DELETE FROM "Producto" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(n) if n > 0 => {}
        _ => return Err(fallo()),
    }
    log_activity(&st.pool, "UPDATED", &format!("eliminó el producto {}", nombre.unwrap_or_default()), "producto", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "ok": true })))
}

#[allow(dead_code)]
fn _no_usados() {
    let _ = truthy;
}
