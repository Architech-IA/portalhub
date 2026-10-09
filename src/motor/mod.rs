//! Servicio privilegiado `portalhub-motor` (127.0.0.1:3101, corre como root). Concentra lo que toca el
//! sistema de archivos, git, Docker, Nginx o los secretos del servidor: ejecutor de tareas, despliegues,
//! bases de datos y variables de los proyectos, creación de repositorios.
//!
//! No está expuesto a internet: solo lo llaman (a) el servicio público `portalhub`, que ya validó la sesión
//! de la persona y reenvía la petición, y (b) los workers de Python del Motor. En los dos casos viaja la
//! clave interna (`x-api-key`); sin ella todo responde 401.

pub mod despliegue;
pub mod diseno;
pub mod ejecutor;
pub mod grafo;
pub mod repo;

use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, B},
};

pub fn router(st: AppState) -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "status": "ok", "service": "portalhub-motor" })) }))
        .route("/api/executor/dispatch", post(dispatch))
        .route("/api/executor/dispatch-chain", post(dispatch_chain))
        .route("/api/executor/complete", post(complete))
        .route("/api/backlog/task/{task_id}/explain", post(explicar))
        .route("/api/backlog/task/{task_id}/plan", post(planificar))
        .route("/api/backlog/task/{task_id}/apply-plan", post(aplicar_plan))
        .route("/api/backlog/sprint/{sprint_id}/reactivate-blocked", post(reactivar_bloqueadas))
        .route("/api/backlog/sprints/{id}/approve", post(aprobar_sprint))
        .route("/api/proyectos/{id}/deploy", post(proyecto_deploy))
        .route("/api/proyectos/{id}/db", post(proyecto_db))
        .route("/api/proyectos/{id}/db/migrar", post(proyecto_migrar))
        .route("/api/proyectos/{id}/env", get(env_listar).post(env_guardar).delete(env_borrar))
        .route("/api/proposals/{id}/documents", post(documento_crear))
        .route("/internal/crear-repositorio", post(crear_repositorio))
        .route_layer(middleware::from_fn_with_state(st, exigir_clave))
}

/// Toda ruta del motor (salvo /health) exige la clave interna.
async fn exigir_clave(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let esperada = st.cfg.internal_api_key.clone().unwrap_or_default();
    let recibida = req.headers().get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if esperada.is_empty() || !iguales_const(esperada.as_bytes(), recibida.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "No autorizado" }))).into_response();
    }
    next.run(req).await
}

fn iguales_const(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn json_err(estado: StatusCode, msg: impl Into<String>) -> Response {
    (estado, Json(json!({ "error": msg.into() }))).into_response()
}

fn texto(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().filter(|x| !x.is_empty()).map(String::from)
}

fn lista_pg(ids: &[String]) -> String {
    format!("{{{}}}", ids.iter().map(|i| format!("\"{}\"", i.replace('"', ""))).collect::<Vec<_>>().join(","))
}

// ═══════════════════════════════ EJECUTOR ═══════════════════════════════
async fn dispatch(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(id) = texto(&body, "taskId") else { return json_err(StatusCode::BAD_REQUEST, "taskId required") };
    match ejecutor::despachar(&st, &id, None).await {
        Ok(r) => (StatusCode::ACCEPTED, Json(r)).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e),
    }
}

async fn dispatch_chain(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let ids: Vec<String> = body["taskIds"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if ids.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "taskIds (array) requerido");
    }
    match grafo::correr_cadena(&st, ids).await {
        Ok(r) => Json(json!({ "ok": true, "results": r })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Callback del worker al terminar una tarea.
async fn complete(State(st): State<AppState>, cuerpo: Option<Json<Value>>) -> Response {
    let Some(Json(b)) = cuerpo else { return json_err(StatusCode::BAD_REQUEST, "Body inválido") };
    let (tarea, ejecucion, estado) = (texto(&b, "taskId"), texto(&b, "execId"), texto(&b, "status"));
    let (Some(tarea), Some(ejecucion), Some(estado)) = (tarea, ejecucion, estado) else {
        return json_err(StatusCode::BAD_REQUEST, "taskId, execId y status son requeridos");
    };
    if estado != "DONE" && estado != "FAILED" {
        return json_err(StatusCode::BAD_REQUEST, "status debe ser DONE o FAILED");
    }
    let duracion = b["durationMs"].as_f64().or_else(|| b["durationMs"].as_str().and_then(|s| s.parse().ok())).filter(|d| d.is_finite()).unwrap_or(0.0) as i64;
    let cierre = ejecutor::Cierre {
        tarea,
        ejecucion,
        estado_final: estado,
        resumen: b["resultSummary"].as_str().unwrap_or("").to_string(),
        duracion_ms: duracion,
        contexto_usado: b["contextUsed"].as_str().map(String::from),
        tool_log: b["toolLog"].as_array().cloned().unwrap_or_default(),
        uso: b.get("usage").filter(|u| u.is_object()).cloned(),
    };
    match ejecutor::finalizar(&st, cierre).await {
        Ok(r) => {
            let mut v = json!({ "ok": true });
            if let (Some(o), Some(x)) = (v.as_object_mut(), r.as_object()) {
                for (k, val) in x {
                    o.insert(k.clone(), val.clone());
                }
            }
            Json(v).into_response()
        }
        Err(e) => {
            tracing::error!("[EXECUTOR_COMPLETE] Error: {e}");
            json_err(StatusCode::INTERNAL_SERVER_ERROR, "Error finalizando ejecución")
        }
    }
}

// ── Explicar / planificar una tarea fallida ──────────────────────────────────────────────────
const EXPLAIN_SYSTEM: &str = r#"Sos un agente investigador de diagnóstico dentro del motor SAGE/MASD de ArchiTechIA.
Tu única tarea es EXPLICAR — en español claro, sin jerga innecesaria — por qué una tarea de desarrollo
quedó FAILED o BLOCKED, investigando el REPOSITORIO REAL con tus herramientas (find_symbol,
get_file_summary, get_dependents, list_files, grep_files, read_file) antes de responder.

Preferí find_symbol/get_file_summary/get_dependents primero (consultas exactas a un índice de código
ya parseado). Usá list_files/grep_files/read_file cuando necesites explorar por carpeta o ver el código
completo de un archivo. Tenés también write_file disponible, pero tu rol es diagnosticar, NO corregir —
no escribas ni modifiques ningún archivo salvo que sea estrictamente imprescindible para confirmar algo
(por ejemplo, nunca deberías necesitarlo para explicar un error de compilación o un bloqueo por
dependencia).

Tu respuesta final (texto plano, sin markdown pesado) debe cubrir, en este orden:
1. Causa raíz real (no el síntoma) — señalá el archivo y la línea exacta si la encontraste.
2. Por qué pasó esto (qué asunción rota, qué cambio en otro lado lo provocó, si es un problema de la
   propia tarea o de una dependencia).
3. Cómo se resolvería en la práctica — sé concreto: "cambiar tal tipo en tal archivo", "reintentar tal
   cual", "dividir la tarea en dos", etc.
No inventes nada que no hayas verificado leyendo el repo real."#;

const PLAN_SYSTEM: &str = r#"Sos un agente de planificación de remediación dentro del motor SAGE/MASD de ArchiTechIA.
Tu tarea es investigar el REPOSITORIO REAL (con find_symbol, get_file_summary, get_dependents, list_files,
grep_files, read_file) para entender por qué una tarea quedó FAILED o BLOCKED, y proponer un PLAN DE TRABAJO
concreto y bien desglosado para resolverlo — NO ejecutar nada todavía. Tenés write_file disponible pero NO
debés usarlo: tu única salida es el plan, en JSON.

Respondé ÚNICAMENTE con un objeto JSON válido (sin markdown, sin ```, sin texto antes o después), con esta forma exacta:
{
  "resumen": "1-2 frases de la causa raíz real, ya investigada en el repo",
  "automatizable": true | false,
  "motivoNoAutomatizable": "si automatizable=false, por qué un agente de código no puede resolver esto solo (ej. requiere una acción humana externa, una decisión de negocio, acceso a un sistema que el agente no tiene). Si automatizable=true, dejalo en null.",
  "pasos": [
    {
      "titulo": "Título corto del paso (una línea)",
      "descripcion": "Qué hacer exactamente, con el detalle suficiente para que otro agente lo ejecute sin re-investigar todo desde cero",
      "archivos": ["ruta/relativa/al/archivo.ts"],
      "riesgo": "bajo" | "medio" | "alto"
    }
  ]
}

Reglas:
- Cada paso debe ser accionable y verificable por separado — no un paso vago tipo "arreglar el bug".
- Ordená los pasos en el orden real en que deberían aplicarse.
- "automatizable" es CRÍTICO — un botón del portal usa este campo para decidir si se le permite a un agente
  de código ejecutar este plan automáticamente. Poné false si CUALQUIER paso requiere: una acción humana
  real (reunión, llamada, aprobación de negocio, enviar un email/contrato), completar otra tarea del
  backlog que depende de una persona, o acceso a un sistema externo que el agente no tiene. Poné true SOLO
  si TODOS los pasos son cambios de código/configuración que un agente puede hacer solo en este repo.
- Si automatizable=false, los "pasos" igual deben describir qué hay que hacer (aunque sea manualmente) —
  simplemente no se van a poder ejecutar con el botón de auto-ejecución.
- No inventes nada que no hayas verificado leyendo el repo real."#;

fn prompt_usuario(t: &Value, sprint: Option<&Value>) -> String {
    let s = |v: &Value, k: &str| v[k].as_str().map(String::from);
    let mut lineas: Vec<String> = vec![format!("Tarea: {} — {}", s(t, "taskCode").unwrap_or_else(|| "(sin código)".into()), s(t, "title").unwrap_or_default())];
    if let Some(d) = s(t, "description").filter(|x| !x.is_empty()) {
        lineas.push(format!("Descripción: {d}"));
    }
    if let Some(sp) = sprint {
        lineas.push(format!(
            "Sprint: {} · Épica: {} · Solución: {}",
            s(sp, "sprintCode").unwrap_or_else(|| "?".into()),
            s(sp, "epicName").unwrap_or_else(|| "?".into()),
            s(sp, "solucionNombre").unwrap_or_else(|| "?".into())
        ));
    }
    lineas.push(format!("Estado actual: {}", s(t, "status").unwrap_or_default()));
    lineas.push(String::new());
    lineas.push(if t["status"] == "FAILED" { "Error / motivo de la falla (tal cual lo guardó el motor):".into() } else { "Motivo del bloqueo (tal cual lo guardó el motor):".into() });
    lineas.push(s(t, "resultado").unwrap_or_else(|| "(sin detalle guardado)".into()));
    lineas.join("\n")
}

async fn investigar(st: AppState, task_id: String, tipo: &'static str) -> Response {
    let (tabla, tipo_harness, agente, sistema, aviso) = if tipo == "explain" {
        ("TaskExplanation", "explain_task", "explicador", EXPLAIN_SYSTEM, "Solo se puede explicar una tarea FAILED o BLOCKED")
    } else {
        ("TaskRemediationPlan", "plan_task", "planificador", PLAN_SYSTEM, "Solo se puede proponer un plan para una tarea FAILED o BLOCKED")
    };
    let t = match fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', id, 'taskCode', "taskCode", 'title', title, 'description', description, 'status', status, 'resultado', resultado, 'solucionId', "solucionId", 'sprintId', "sprintId") FROM "BacklogItem" WHERE id = $1"#,
        &[B::T(task_id.clone())],
    )
    .await
    {
        Ok(Some(t)) => t,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "Tarea no encontrada"),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if t["status"] != "FAILED" && t["status"] != "BLOCKED" {
        return json_err(StatusCode::BAD_REQUEST, aviso);
    }
    let sprint = match texto(&t, "sprintId") {
        Some(sid) => fetch_json_opt(
            &st.pool,
            r#"SELECT jsonb_build_object('sprintCode', s."sprintCode", 'epicName', e.name, 'solucionNombre', sol.nombre)
               FROM "Sprint" s LEFT JOIN "Epic" e ON e.id = s."epicId" LEFT JOIN "Solucion" sol ON sol.id = COALESCE(s."solucionId", e."solucionId") WHERE s.id = $1"#,
            &[B::T(sid)],
        )
        .await
        .ok()
        .flatten(),
        None => None,
    };
    let repo_path = match repo::resolver_repo(&st, texto(&t, "solucionId").as_deref()).await {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let exec_id = uuid_v4();
    let usuario = prompt_usuario(&t, sprint.as_ref());
    if let Err(e) = exec(&st.pool, &format!(r#"INSERT INTO "{tabla}" (id, "taskId", "execId", status, "createdAt", "updatedAt") VALUES ($1, $2, $3, 'RUNNING', NOW(), NOW())"#), &[B::T(uuid_v4()), B::T(task_id.clone()), B::T(exec_id.clone())]).await {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let url_harness = std::env::var("HARNESS_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8767".to_string());
    let envio = st
        .http
        .post(format!("{url_harness}/dispatch"))
        .timeout(std::time::Duration::from_secs(10))
        .json(&json!({
            "type": tipo_harness, "agent": agente, "priority": "MEDIUM",
            "payload": { "taskId": task_id, "execId": exec_id, "apiUrl": "https://opencode.ai/zen/go/v1/chat/completions", "modelId": st.cfg.opencode_executor_model, "systemPrompt": sistema, "userPrompt": usuario, "repoPath": repo_path }
        }))
        .send()
        .await;
    if let Err(e) = envio {
        let _ = exec(&st.pool, &format!(r#"UPDATE "{tabla}" SET status = 'FAILED', resultado = $2, "updatedAt" = NOW() WHERE "execId" = $1"#), &[B::T(exec_id), B::T(format!("No se pudo encolar en el Harness: {e}"))]).await;
        return json_err(StatusCode::BAD_GATEWAY, if tipo == "explain" { "No se pudo encolar la explicación" } else { "No se pudo encolar la propuesta de plan" });
    }
    (StatusCode::ACCEPTED, Json(json!({ "execId": exec_id }))).into_response()
}

fn uuid_v4() -> String {
    use rand::Rng;
    let mut r = rand::thread_rng();
    let mut b: [u8; 16] = r.gen();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

async fn explicar(State(st): State<AppState>, Path(task_id): Path<String>) -> Response {
    investigar(st, task_id, "explain").await
}

async fn planificar(State(st): State<AppState>, Path(task_id): Path<String>) -> Response {
    investigar(st, task_id, "plan").await
}

async fn aplicar_plan(State(st): State<AppState>, Path(task_id): Path<String>, cuerpo: Option<Json<Value>>) -> Response {
    let b = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let Some(exec_id) = texto(&b, "execId") else { return json_err(StatusCode::BAD_REQUEST, "execId requerido (el plan a ejecutar)") };
    let plan = match fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('status', status, 'resultado', resultado, 'planJson', "planJson") FROM "TaskRemediationPlan" WHERE "execId" = $1 AND "taskId" = $2"#,
        &[B::T(exec_id.clone()), B::T(task_id.clone())],
    )
    .await
    {
        Ok(Some(p)) => p,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "Plan no encontrado"),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if plan["status"] != "DONE" {
        return json_err(StatusCode::BAD_REQUEST, "El plan todavía no está listo");
    }
    let plan_json = &plan["planJson"];
    if plan_json.is_object() && plan_json["automatizable"] == false {
        return json_err(
            StatusCode::CONFLICT,
            format!("Este plan no es automatizable: {}", plan_json["motivoNoAutomatizable"].as_str().unwrap_or("requiere una acción humana antes de poder ejecutarse.")),
        );
    }
    let texto_plan = if plan_json.is_null() { plan["resultado"].as_str().unwrap_or("").to_string() } else { serde_json::to_string_pretty(plan_json).unwrap_or_default() };
    let original = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('resultado', resultado) FROM "BacklogItem" WHERE id = $1"#, &[B::T(task_id.clone())]).await.ok().flatten();
    let guia = [
        original.as_ref().and_then(|o| texto(o, "resultado")).map(|r| format!("ERROR / MOTIVO DE BLOQUEO ORIGINAL de esta tarea (texto crudo tal cual lo guardó el motor, antes de que se propusiera el plan):\n{r}")),
        Some(texto_plan).filter(|t| !t.is_empty()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n\n---\n\n");
    match ejecutor::despachar(&st, &task_id, Some(&guia)).await {
        Ok(r) => {
            let _ = exec(&st.pool, r#"UPDATE "TaskRemediationPlan" SET "appliedAt" = NOW(), "updatedAt" = NOW() WHERE "execId" = $1"#, &[B::T(exec_id)]).await;
            (StatusCode::ACCEPTED, Json(r)).into_response()
        }
        Err(e) => json_err(StatusCode::BAD_REQUEST, e),
    }
}

async fn reactivar_bloqueadas(State(st): State<AppState>, Path(sprint_id): Path<String>) -> Response {
    let r = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', bi.id, 'taskCode', bi."taskCode")), '[]'::jsonb)
             FROM "BacklogItem" bi JOIN "BacklogItem" dep ON bi."dependsOnTaskId" = dep.id WHERE bi."sprintId" = $1 AND bi.status = 'BLOCKED' AND dep.status = 'DONE'"#,
        &[B::T(sprint_id.clone())],
    )
    .await;
    let reactivables = match r {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if reactivables.is_empty() {
        return Json(json!({ "reactivated": 0, "dispatched": 0 })).into_response();
    }
    let ids: Vec<String> = reactivables.iter().filter_map(|x| x["id"].as_str().map(String::from)).collect();
    // `resultado` se limpia: tenía el motivo de bloqueo viejo.
    if let Err(e) = exec(&st.pool, r#"UPDATE "BacklogItem" SET status = 'BACKLOG', resultado = NULL WHERE id = ANY($1::text[])"#, &[B::T(lista_pg(&ids))]).await {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let todas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(id), '[]'::jsonb) FROM "BacklogItem" WHERE "sprintId" = $1 AND status = 'BACKLOG'"#, &[B::T(sprint_id.clone())]).await.map(|v| v.as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    // No se espera a que termine toda la cadena (puede tardar minutos).
    let (st2, cadena, sprint) = (st.clone(), todas.clone(), sprint_id.clone());
    tokio::spawn(async move {
        if let Err(e) = grafo::correr_cadena(&st2, cadena).await {
            tracing::error!("[REACTIVATE_BLOCKED] Error en runTaskChain para sprint {sprint}: {e}");
        }
    });
    Json(json!({ "reactivated": ids.len(), "taskCodes": reactivables.iter().map(|x| x["taskCode"].clone()).collect::<Vec<_>>(), "dispatched": todas.len() })).into_response()
}

async fn aprobar_sprint(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    let r: Result<Response, String> = async {
        let sprint = fetch_json_opt(
            &st.pool,
            r#"WITH u AS (UPDATE "Sprint" SET status='CLOSED' WHERE id=$1 AND status='REVIEW_PENDING' RETURNING id, "epicId", "solucionId", "sprintCode") SELECT to_jsonb(u) FROM u"#,
            &[B::T(id.clone())],
        )
        .await
        .map_err(|e| e.to_string())?;
        let Some(sprint) = sprint else { return Ok(json_err(StatusCode::BAD_REQUEST, "Sprint not found or not in REVIEW_PENDING")) };
        let siguiente = fetch_json_opt(
            &st.pool,
            r#"SELECT jsonb_build_object('id', id, 'name', name, 'sprintCode', "sprintCode") FROM "Sprint" WHERE "epicId" = $1 AND status = 'PLANNED' ORDER BY "createdAt" ASC LIMIT 1"#,
            &[B::OT(texto(&sprint, "epicId"))],
        )
        .await
        .map_err(|e| e.to_string())?;
        let (mut info, mut despachada) = (Value::Null, Value::Null);
        if let Some(sig) = siguiente {
            let sid = sig["id"].as_str().unwrap_or("").to_string();
            exec(&st.pool, r#"UPDATE "Sprint" SET status='ACTIVE' WHERE id=$1"#, &[B::T(sid.clone())]).await.map_err(|e| e.to_string())?;
            let primera = crate::util::fetch_text_opt(&st.pool, r#"SELECT id FROM "BacklogItem" WHERE "sprintId"=$1 AND status='BACKLOG' ORDER BY "createdAt" LIMIT 1"#, &[B::T(sid)]).await.map_err(|e| e.to_string())?;
            if let Some(p) = primera {
                despachada = ejecutor::despachar(&st, &p, None).await?;
            }
            info = sig;
        }
        Ok(Json(json!({ "closed": sprint["sprintCode"], "nextSprint": info, "dispatched": despachada })).into_response())
    }
    .await;
    match r {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("approve sprint: {e}");
            json_err(StatusCode::INTERNAL_SERVER_ERROR, "Error approving sprint")
        }
    }
}

// ═══════════════════════════════ PROYECTOS: DESPLIEGUE ═══════════════════════════════
fn corto500(e: String) -> String {
    e.chars().take(500).collect()
}

async fn proyecto_deploy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match despliegue::desplegar(&st, &id).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, corto500(e)),
    }
}

async fn proyecto_db(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match despliegue::aprovisionar_base(&st, &id).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, corto500(e)),
    }
}

async fn proyecto_migrar(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match despliegue::aplicar_migraciones(&st, &id).await {
        Ok(r) => {
            let ok = r["ok"] == true;
            (if ok { StatusCode::OK } else { StatusCode::BAD_REQUEST }, Json(r)).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": corto500(e) }))).into_response(),
    }
}

async fn nombre_proyecto(st: &AppState, id: &str) -> Option<String> {
    crate::util::fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Solucion" WHERE id = $1"#, &[B::T(id.into())]).await.ok().flatten()
}

async fn env_listar(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(n) = nombre_proyecto(&st, &id).await else { return json_err(StatusCode::NOT_FOUND, "Proyecto no encontrado") };
    Json(json!({ "variables": despliegue::listar_variables(&n) })).into_response()
}

async fn env_guardar(State(st): State<AppState>, Path(id): Path<String>, cuerpo: Option<Json<Value>>) -> Response {
    let Some(n) = nombre_proyecto(&st, &id).await else { return json_err(StatusCode::NOT_FOUND, "Proyecto no encontrado") };
    let b = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let (Some(nombre), Some(valor)) = (b["nombre"].as_str().map(str::trim).filter(|x| !x.is_empty()), b["valor"].as_str()) else {
        return json_err(StatusCode::BAD_REQUEST, "Faltan \"nombre\" y/o \"valor\".");
    };
    match despliegue::guardar_variable(&n, nombre, valor) {
        Ok(()) => Json(json!({ "variables": despliegue::listar_variables(&n) })).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e),
    }
}

async fn env_borrar(State(st): State<AppState>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(n) = nombre_proyecto(&st, &id).await else { return json_err(StatusCode::NOT_FOUND, "Proyecto no encontrado") };
    let Some(v) = q.get("variable").filter(|x| !x.is_empty()) else { return json_err(StatusCode::BAD_REQUEST, "Falta \"variable\".") };
    if let Err(e) = despliegue::borrar_variable(&n, v) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    Json(json!({ "variables": despliegue::listar_variables(&n) })).into_response()
}

async fn crear_repositorio(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    let (Some(sol), Some(nombre)) = (texto(&b, "solucionId"), texto(&b, "nombre")) else { return json_err(StatusCode::BAD_REQUEST, "solucionId y nombre requeridos") };
    match repo::crear_repositorio_para_solucion(&st, &sol, &nombre, b["privado"] != false).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e),
    }
}

// ═══════════════════════════════ DOCUMENTOS DE PROPUESTA ═══════════════════════════════
const MAX_DOCUMENTO: usize = 10 * 1024 * 1024;

fn extension_office(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-excel" => "xls",
        _ => return None,
    })
}

/// Vista previa en PDF de un documento de Office (data URL) con LibreOffice; `None` si no aplica o falla.
async fn convertir_a_pdf(url: &str) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    if !url.starts_with("data:") {
        return None;
    }
    let mime = url.get(5..url.find(';')?)?;
    let ext = extension_office(mime)?;
    let b64 = &url[url.find(',')? + 1..];
    let bytes = STANDARD.decode(b64).ok()?;
    let id = uuid_v4();
    let tmp = std::env::temp_dir();
    let trabajo = tmp.join(format!("docprev_{id}"));
    let perfil = tmp.join(format!("loprofile_{id}"));
    let entrada = trabajo.join(format!("input.{ext}"));
    let r: Option<String> = async {
        tokio::fs::create_dir_all(&trabajo).await.ok()?;
        tokio::fs::write(&entrada, bytes).await.ok()?;
        repo::sh(
            "soffice",
            &["--headless", "--convert-to", "pdf", "--outdir", &trabajo.to_string_lossy(), &format!("-env:UserInstallation=file://{}", perfil.to_string_lossy()), &entrada.to_string_lossy()],
            None,
            45,
        )
        .await
        .ok()?;
        let pdf = tokio::fs::read(trabajo.join("input.pdf")).await.ok()?;
        Some(format!("data:application/pdf;base64,{}", STANDARD.encode(pdf)))
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&trabajo).await;
    let _ = tokio::fs::remove_dir_all(&perfil).await;
    r
}

async fn documento_crear(State(st): State<AppState>, Path(propuesta): Path<String>, Json(b): Json<Value>) -> Response {
    let (Some(nombre), Some(url)) = (texto(&b, "name"), texto(&b, "url")) else { return json_err(StatusCode::BAD_REQUEST, "name y url son requeridos") };
    if url.starts_with("data:") && url.len() * 3 / 4 > MAX_DOCUMENTO {
        return json_err(StatusCode::BAD_REQUEST, "Archivo muy grande (máx 10MB)");
    }
    let reemplaza = texto(&b, "replacesId");
    let mut version = 1i64;
    if let Some(r) = &reemplaza {
        match fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('version', version) FROM "ProposalDocument" WHERE id = $1 AND "proposalId" = $2"#, &[B::T(r.clone()), B::T(propuesta.clone())]).await {
            Ok(Some(prev)) => version = prev["version"].as_i64().unwrap_or(1) + 1,
            Ok(None) => return json_err(StatusCode::NOT_FOUND, "Documento a reemplazar no encontrado"),
            Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    }
    let vista = convertir_a_pdf(&url).await;
    let r: Result<Value, sqlx::Error> = async {
        let mut tx = st.pool.begin().await?;
        let doc: Value = sqlx::query_scalar(
            r#"WITH ins AS (INSERT INTO "ProposalDocument" (id, name, url, type, stage, "proposalId", version, "replacesId", "previewUrl", "createdAt") VALUES ($1, $2, $3, $4, $5, $6, $7::int, $8, $9, NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        )
        .bind(new_id())
        .bind(&nombre)
        .bind(&url)
        .bind(texto(&b, "type").unwrap_or_else(|| "otro".into()))
        .bind(texto(&b, "stage"))
        .bind(&propuesta)
        .bind(version)
        .bind(&reemplaza)
        .bind(&vista)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(r) = &reemplaza {
            sqlx::query(r#"UPDATE "ProposalDocument" SET archived = true WHERE id = $1"#).bind(r).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(doc)
    }
    .await;
    match r {
        Ok(mut d) => {
            crate::util::fix_dates(&mut d);
            Json(d).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
