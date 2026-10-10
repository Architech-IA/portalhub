//! Ciclo de vida de una tarea del Motor: despacho al Harness, contexto, verificación, cierre y resumen del
//! sprint (`lib/executor/taskDispatcher.ts`, `taskVerifier.ts`, `realChecks.ts`, `sprintMonitor.ts`,
//! `traceEvents.ts`, `lib/context/buildTaskContext.ts` y `lib/memory/vaultNotes.ts`).

use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use regex::Regex;
use serde_json::{json, Value};

use super::repo::{self, ErrorMerge, R};
use crate::{
    llm,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, new_id, B},
};

const URL_GO: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const URL_ZEN: &str = "https://opencode.ai/zen/v1/chat/completions";

fn url_harness() -> String {
    std::env::var("HARNESS_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8767".to_string())
}

const AREAS_CODIGO: [&str; 3] = ["947ca771-fe9e-4c3f-bfea-2ef2e27986c6", "74b21d1d-0954-4757-a1fd-0fabed1e9e3a", "3695ed86-da91-4327-bdde-b14cfa8a10b5"];

fn err_db(e: sqlx::Error) -> String {
    e.to_string()
}

// ── Trazas ───────────────────────────────────────────────────────────────────────────────────
/// Guarda un evento de traza para la Sala de Control. Nunca falla: es observabilidad.
pub async fn emitir_traza(st: &AppState, tarea: &str, ejecucion: Option<&str>, tipo: &str, mensaje: &str) {
    let r = exec(
        &st.pool,
        r#"INSERT INTO "TaskExecutionEvent" (id, "taskId", "execId", kind, message, "createdAt") VALUES (gen_random_uuid()::text, $1, $2, $3, $4, NOW())"#,
        &[B::T(tarea.into()), B::OT(ejecucion.map(String::from)), B::T(tipo.into()), B::T(mensaje.into())],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("[TRACE_EVENT] No se pudo guardar el evento de traza (no bloqueante): {e}");
    }
}

// ── Notas del vault ──────────────────────────────────────────────────────────────────────────
fn raiz_vault() -> PathBuf {
    PathBuf::from(std::env::var("SAGE_VAULT_PATH").unwrap_or_else(|_| "/root/sage-vault".to_string()))
}

pub async fn escribir_nota(ruta_rel: &str, frontmatter: &[(&str, Value)], cuerpo: &str) -> R<()> {
    let lineas: Vec<String> = frontmatter
        .iter()
        .map(|(k, v)| match v {
            Value::Array(a) => format!("{k}: [{}]", a.iter().map(|x| format!("'{}'", x.as_str().unwrap_or(""))).collect::<Vec<_>>().join(", ")),
            Value::Number(n) => format!("{k}: {n}"),
            otro => format!("{k}: '{}'", otro.as_str().map(String::from).unwrap_or_else(|| otro.to_string()).replace('\'', "''")),
        })
        .collect();
    let texto = format!("---\n{}\n---\n\n{}\n", lineas.join("\n"), cuerpo.trim());
    let p = raiz_vault().join(ruta_rel);
    if let Some(d) = p.parent() {
        tokio::fs::create_dir_all(d).await.map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&p, texto).await.map_err(|e| e.to_string())
}

/// Cuerpo de una nota (sin el frontmatter), o `None` si no existe.
pub async fn leer_nota(ruta_rel: &str) -> Option<String> {
    let raw = tokio::fs::read_to_string(raiz_vault().join(ruta_rel)).await.ok()?;
    static R: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)^---\n(.*?)\n---\n?(.*)$").expect("re"));
    Some(R.captures(&raw).and_then(|c| c.get(2)).map(|m| m.as_str().to_string()).unwrap_or(raw))
}

// ── Contexto ─────────────────────────────────────────────────────────────────────────────────
// Antes 8.000 (unos 2.000 tokens): quedaba corto al sumar el diseño técnico. Las tareas gastan 8-55 mil tokens, y el bloque estable se reutiliza entre tareas del sprint.
const MAX_CONTEXTO: usize = 40_000;
const MAX_TAREAS_SPRINT: usize = 20;

fn miles_es_ar(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}

fn cortar(t: &str, n: usize) -> String {
    t.chars().take(n).collect()
}

fn sg(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        otro => otro.to_string(),
    }
}

fn so(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(String::from)
}

pub async fn construir_contexto(st: &AppState, tarea: &str) -> R<String> {
    let t = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'taskId', bi.id, 'taskTitle', bi.title, 'taskDescription', bi.description, 'priority', bi.priority, 'taskStatus', bi.status, 'taskCode', bi."taskCode",
             'dependsOnTaskId', bi."dependsOnTaskId", 'areaId', bi."areaId", 'areaName', a.name,
             'sprintId', s.id, 'sprintName', s.name, 'sprintGoal', s.goal, 'sprintCode', s."sprintCode",
             'epicId', e.id, 'epicName', e.name, 'epicDescription', e.description,
             'solId', sol.id, 'solNombre', sol.nombre, 'solDescripcion', sol.descripcion, 'solucionCode', sol."solucionCode", 'solucionDirecta', bi."solucionId")
           FROM "BacklogItem" bi
           LEFT JOIN "Sprint" s ON bi."sprintId" = s.id
           LEFT JOIN "Epic" e ON s."epicId" = e.id
           LEFT JOIN "Solucion" sol ON s."solucionId" = sol.id
           LEFT JOIN "Area" a ON bi."areaId" = a.id
           WHERE bi.id = $1"#,
        &[B::T(tarea.into())],
    )
    .await
    .map_err(err_db)?;
    let Some(t) = t else { return Ok(format!("Task {tarea} not found.")) };

    // Bloque ESTABLE (idéntico entre tareas del mismo sprint/épica/solución).
    let mut estable: Vec<String> = vec!["=== SDD HIERARCHY ===".into()];
    estable.push(format!("SOLUTION: [{}] {}", sg(&t, "solucionCode"), sg(&t, "solNombre")));
    if let Some(d) = so(&t, "solDescripcion").filter(|x| !x.is_empty()) {
        estable.push(format!("  {d}"));
    }
    estable.push(format!("EPIC: {}", sg(&t, "epicName")));
    if let Some(d) = so(&t, "epicDescription").filter(|x| !x.is_empty()) {
        estable.push(format!("  {d}"));
    }
    estable.push(format!("SPRINT: [{}] {}", sg(&t, "sprintCode"), sg(&t, "sprintName")));
    if let Some(g) = so(&t, "sprintGoal").filter(|x| !x.is_empty()) {
        estable.push(format!("  Goal: {g}"));
    }

    // La Solución a la que pertenece la tarea: por su sprint o, en las actividades de fase (sin sprint), directamente.
    let sol_ctx = so(&t, "solId").or_else(|| so(&t, "solucionDirecta"));
    if so(&t, "solId").is_none() {
        if let Some(sid) = &sol_ctx {
            if let Some((codigo, nombre, descripcion)) = super::lead::solucion_basica(st, sid).await {
                estable.push(format!("SOLUTION: [{codigo}] {nombre}"));
                if !descripcion.is_empty() {
                    estable.push(format!("  {descripcion}"));
                }
            }
        }
    }
    // El lead y el cliente: sin esto las tareas de preventa (ficha, briefing, acta, propuesta) no tienen ni el nombre de la empresa.
    if let Some(sid) = &sol_ctx {
        if let Some(b) = super::lead::cargar(st, sid).await {
            estable.push(String::new());
            estable.push(b);
        }
    }
    // Diseño técnico y arquitectura documentados en el hub: la referencia que el agente debe respetar.
    if let Some(sid) = &sol_ctx {
        if let Some(d) = super::diseno::cargar(st, sid).await {
            estable.push(String::new());
            estable.push(d.texto);
        }
    }

    let consejo = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('agentSlug', x."agentSlug", 'content', x.content) ORDER BY x."createdAt" DESC), '[]'::jsonb) FROM
             (SELECT dm."agentSlug", dm.content, dm."createdAt" FROM "DebateMessage" dm JOIN "CouncilProposal" cp ON dm."proposalId" = cp.id
               WHERE cp."solucionId" = $1 AND dm.round = 10 ORDER BY dm."createdAt" DESC LIMIT 5) x"#,
        &[B::OT(so(&t, "solId"))],
    )
    .await
    .map_err(err_db)?;
    let consejo = consejo.as_array().cloned().unwrap_or_default();
    if !consejo.is_empty() {
        estable.push("\n=== COUNCIL DEBATE (PLANNING) ===".into());
        for m in consejo.iter().rev() {
            estable.push(format!("[{}]: {}", sg(m, "agentSlug"), cortar(&sg(m, "content"), 1200)));
        }
    }

    let previo = fetch_text_opt(
        &st.pool,
        r#"SELECT "sprintCode" FROM "Sprint" WHERE "epicId" = $1 AND status IN ('CLOSED', 'REVIEW_PENDING') AND id != $2 ORDER BY "createdAt" DESC LIMIT 1"#,
        &[B::OT(so(&t, "epicId")), B::OT(so(&t, "sprintId"))],
    )
    .await
    .map_err(err_db)?;
    if let Some(codigo) = previo {
        if let Some(nota) = leer_nota(&format!("shared/decisions/sprints/{codigo}.md")).await {
            estable.push(format!("\n=== PREVIOUS SPRINT SUMMARY (memoria: {codigo}) ==="));
            estable.push(cortar(nota.trim(), 3000));
        }
    }

    let historial = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."finishedAt" DESC), '[]'::jsonb) FROM
             (SELECT te."agentName", te."resultSummary", te."durationMs", te."finishedAt" FROM "TaskExecution" te JOIN "BacklogItem" bi ON te."backlogItemId" = bi.id
               WHERE bi."areaId" = $1 AND bi."solucionId" = $2 AND te.status = 'DONE' ORDER BY te."finishedAt" DESC LIMIT 5) x"#,
        &[B::OT(so(&t, "areaId")), B::OT(so(&t, "solId"))],
    )
    .await
    .map_err(err_db)?;
    let historial = historial.as_array().cloned().unwrap_or_default();
    if !historial.is_empty() {
        estable.push("\n=== AREA EXECUTION HISTORY ===".into());
        for h in &historial {
            let dur = h["durationMs"].as_f64().filter(|d| *d != 0.0).map(|d| format!(" ({}s)", (d / 1000.0).round() as i64)).unwrap_or_default();
            let res = so(h, "resultSummary").filter(|x| !x.is_empty()).map(|r| cortar(&r, 500)).unwrap_or_else(|| "—".into());
            estable.push(format!("{}{dur}: {res}", sg(h, "agentName")));
        }
    }

    // Bloque VARIABLE (cambia en cada tarea; nunca se trunca).
    let mut variable: Vec<String> = vec![];
    let sprint_id = so(&t, "sprintId");
    let total = crate::util::fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "BacklogItem" WHERE "sprintId" = $1 AND id != $2"#, &[B::OT(sprint_id.clone()), B::T(tarea.into())]).await.map_err(err_db)?;
    let tareas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC), '[]'::jsonb) FROM
             (SELECT "taskCode", title, status, resultado, "createdAt" FROM "BacklogItem" WHERE "sprintId" = $1 AND id != $2 ORDER BY "createdAt" DESC LIMIT $3) x"#,
        &[B::OT(sprint_id.clone()), B::T(tarea.into()), B::I(MAX_TAREAS_SPRINT as i64)],
    )
    .await
    .map_err(err_db)?;
    let tareas = tareas.as_array().cloned().unwrap_or_default();
    if !tareas.is_empty() {
        variable.push("\n=== SPRINT PROGRESS ===".into());
        let omitidas = total - tareas.len() as i64;
        if omitidas > 0 {
            variable.push(format!("({omitidas} tareas anteriores omitidas por espacio — mostrando las {MAX_TAREAS_SPRINT} mas recientes)"));
        }
        for x in tareas.iter().rev() {
            let res = so(x, "resultado").filter(|r| !r.is_empty()).map(|r| format!(" → {}", cortar(&r, 300))).unwrap_or_default();
            variable.push(format!("[{}] {}: {}{res}", sg(x, "status"), sg(x, "taskCode"), sg(x, "title")));
        }
    }

    let decisiones = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(summary ORDER BY "createdAt" DESC), '[]'::jsonb) FROM (SELECT summary, "createdAt" FROM "SprintDecision" WHERE "sprintId" = $1 ORDER BY "createdAt" DESC LIMIT 10) x"#,
        &[B::OT(sprint_id)],
    )
    .await
    .map_err(err_db)?;
    let decisiones = decisiones.as_array().cloned().unwrap_or_default();
    if !decisiones.is_empty() {
        variable.push("\n=== SPRINT DECISIONS (decisiones ya tomadas en este sprint) ===".into());
        for d in decisiones.iter().rev() {
            variable.push(format!("- {}", d.as_str().unwrap_or("")));
        }
    }

    if let Some(dep_id) = so(&t, "dependsOnTaskId") {
        let dep = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('taskCode', "taskCode", 'title', title, 'resultado', resultado, 'status', status) FROM "BacklogItem" WHERE id = $1"#, &[B::T(dep_id)]).await.map_err(err_db)?;
        if let Some(dep) = dep {
            variable.push(format!("\n=== RESULTADO REAL DE LA TAREA DE LA QUE DEPENDE ({}: {}) ===", sg(&dep, "taskCode"), sg(&dep, "title")));
            let res = so(&dep, "resultado").filter(|r| !r.is_empty());
            variable.push(match (dep["status"].as_str(), res) {
                (Some("DONE"), Some(r)) => r,
                _ => format!("[ADVERTENCIA: la tarea de la que depende ({}) todavia no esta DONE o no tiene resultado — status actual: {}]", sg(&dep, "taskCode"), sg(&dep, "status")),
            });
        }
    }

    variable.push("\n=== TASK ===".into());
    variable.push(format!("[{}] {} ({})", sg(&t, "taskCode"), sg(&t, "taskTitle"), sg(&t, "priority")));
    if let Some(d) = so(&t, "taskDescription").filter(|x| !x.is_empty()) {
        variable.push(d);
    }
    variable.push(format!("Area: {}", so(&t, "areaName").filter(|x| !x.is_empty()).unwrap_or_else(|| sg(&t, "areaId"))));

    let bloque_variable = variable.join("\n");
    let mut bloque_estable = estable.join("\n");
    let presupuesto = MAX_CONTEXTO as i64 - bloque_variable.chars().count() as i64 - 60;
    if bloque_estable.chars().count() as i64 > presupuesto {
        bloque_estable = format!("{}\n[... contexto estable truncado por limite de tamaño ...]", cortar(&bloque_estable, presupuesto.max(0) as usize));
    }
    Ok(format!("{bloque_estable}\n{bloque_variable}"))
}

// ── Agente y modelo ──────────────────────────────────────────────────────────────────────────
fn resolver_modelo(st: &AppState, llm_model: Option<&str>) -> (String, String) {
    if let Some(m) = llm_model.and_then(|m| m.strip_prefix("opencode-go/")) {
        return (URL_GO.to_string(), m.to_string());
    }
    if let Some(m) = llm_model.and_then(|m| m.strip_prefix("opencode/")) {
        return (URL_ZEN.to_string(), m.to_string());
    }
    (URL_GO.to_string(), st.cfg.opencode_executor_model.clone())
}

struct Agente {
    id: String,
    nombre: String,
    estrategia: &'static str,
}

async fn resolver_agente(st: &AppState, t: &Value) -> R<Agente> {
    let area = so(t, "areaId").filter(|x| !x.is_empty());
    if let (Some(aid), Some(an)) = (so(t, "assigneeId").filter(|x| !x.is_empty()), so(t, "assigneeName").filter(|x| !x.is_empty())) {
        let estrategia = if area.as_deref().map(|a| AREAS_CODIGO.contains(&a)).unwrap_or(false) { "CODE" } else { "LLM" };
        return Ok(Agente { id: aid, nombre: an, estrategia });
    }
    if let Some(a) = &area {
        let fila = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('id', "defaultAgentId", 'nombre', "defaultAgentName", 'estrategia', "executionStrategy") FROM "Area" WHERE id = $1"#, &[B::T(a.clone())]).await.map_err(err_db)?;
        if let Some(f) = fila.filter(|f| f["id"].as_str().map(|x| !x.is_empty()).unwrap_or(false)) {
            return Ok(Agente { id: sg(&f, "id"), nombre: so(&f, "nombre").unwrap_or_else(|| "Agent".into()), estrategia: if f["estrategia"].as_str() == Some("CODE") { "CODE" } else { "LLM" } });
        }
    }
    if t["type"].as_str() == Some("TEST_QA") {
        return Ok(Agente { id: "25f3daa0-5849-49d7-a64e-370cc1315286".into(), nombre: "Sigma".into(), estrategia: "CODE" });
    }
    Ok(Agente { id: "cmsii112p0001l0w1kysamv72".into(), nombre: "Atlas".into(), estrategia: "CODE" })
}

pub(super) fn prd_html_a_plano(html: &str) -> String {
    static R1: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").expect("re"));
    static R2: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</(p|div|li)>").expect("re"));
    static R3: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("re"));
    static R4: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").expect("re"));
    let t = R1.replace_all(html, "\n").to_string();
    let t = R2.replace_all(&t, "\n").to_string();
    let t = R3.replace_all(&t, "").to_string();
    let t = t.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
    R4.replace_all(&t, "\n\n").trim().to_string()
}

/// Texto y criterio de aceptación del requisito del PRD del que nació la tarea.
async fn criterio_prd(st: &AppState, solucion: Option<&str>, requisito: Option<&str>) -> Option<(String, String)> {
    let (s, r) = (solucion?, requisito?);
    let prd = fetch_text_opt(&st.pool, r#"SELECT prd FROM "Solucion" WHERE id = $1"#, &[B::T(s.into())]).await.ok().flatten()?;
    let p: Value = serde_json::from_str(&prd).ok()?;
    let req = p["requisitos"].as_array()?.iter().find(|x| x["id"].as_str() == Some(r))?;
    Some((prd_html_a_plano(req["texto"].as_str().unwrap_or("")), prd_html_a_plano(req["criterioAceptacion"].as_str().unwrap_or(""))))
}

fn extraer_decisiones(resumen: &str) -> Vec<String> {
    static R: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)DECISIONES:\s*(.*)$").expect("re"));
    static NINGUNA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^ninguna\.?$").expect("re"));
    static VINETA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-•*]\s*").expect("re"));
    let Some(c) = R.captures(resumen) else { return vec![] };
    let bloque = c[1].trim().to_string();
    if bloque.is_empty() || NINGUNA.is_match(&bloque) {
        return vec![];
    }
    bloque
        .split('\n')
        .map(|l| VINETA.replace(l, "").trim().to_string())
        .filter(|l| !l.is_empty() && !NINGUNA.is_match(l))
        .take(5)
        .collect()
}

// ── Despacho ─────────────────────────────────────────────────────────────────────────────────
/// Rondas de herramientas que tiene un agente de código por tarea (el tope lo aplica el worker).
const RONDAS_AGENTE: i64 = 20;

fn presupuesto_de_rondas() -> String {
    format!(
        "PRESUPUESTO: tienes como máximo {RONDAS_AGENTE} rondas de herramientas; si se acaban sin terminar, el trabajo queda a medias. Planifica así: (1) lee solo lo imprescindible (máximo 5 rondas; usa grep_files y rangos de líneas, no archivos completos); (2) escribe cuanto antes; (3) deja tsc y las pruebas para las últimas rondas. Si ves que no vas a alcanzar, deja el proyecto compilando con lo que lleves y empieza tu resumen con «PARCIAL:» indicando exactamente qué falta."
    )
}

/// Prioridad en la cola del Harness: lo que se le vende a un cliente (compromisos y fechas) va antes que lo interno. La cola ya atiende
/// HIGH, luego MEDIUM y luego LOW; lo interno queda en MEDIUM para que nunca se quede sin turno.
pub(crate) fn prioridad_por_tipo(tipo: &str) -> &'static str {
    match tipo {
        "PRODUCT" | "INTERN" => "MEDIUM",
        _ => "HIGH",
    }
}

/// ¿La ejecución agotó las rondas? El worker lo avisa en el resumen y además el número de llamadas al modelo llega al tope.
fn agoto_rondas(resumen: &str, uso: Option<&Value>) -> bool {
    resumen.contains("se alcanzo el limite de pasos de herramientas")
        || resumen.contains("se alcanzó el límite de pasos de herramientas")
        || uso.and_then(|u| u["calls"].as_i64()).map(|n| n >= RONDAS_AGENTE).unwrap_or(false)
}

pub async fn despachar(st: &AppState, tarea: &str, guia_extra: Option<&str>) -> R<Value> {
    let t = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(x) FROM (SELECT bi.id, bi.title, bi.description, bi."taskCode", bi."areaId", bi."sprintId", bi.type, bi."assigneeId", bi."assigneeName", bi.status,
                  bi."dependsOnTaskId", bi."solucionId", bi."prdRequisitoId", bi.resultado, s."sprintCode", dep."taskCode" AS "dependsOnTaskCode", dep.status AS "dependsOnStatus"
             FROM "BacklogItem" bi LEFT JOIN "Sprint" s ON bi."sprintId" = s.id LEFT JOIN "BacklogItem" dep ON bi."dependsOnTaskId" = dep.id WHERE bi.id = $1) x"#,
        &[B::T(tarea.into())],
    )
    .await
    .map_err(err_db)?;
    let Some(t) = t else { return Err(format!("Task {tarea} not found")) };
    if t["status"] == "DONE" {
        return Err(format!("Task {tarea} ya está DONE"));
    }
    if t["status"] == "IN_PROGRESS" {
        return Err(format!("Task {tarea} ya está IN_PROGRESS — no se puede volver a disparar mientras corre."));
    }
    if t["dependsOnTaskId"].as_str().is_some() && t["dependsOnStatus"].as_str() != Some("DONE") {
        return Err(format!(
            "No se puede ejecutar: depende de la tarea {} (estado actual: {}). Resolvé esa tarea primero.",
            so(&t, "dependsOnTaskCode").unwrap_or_else(|| sg(&t, "dependsOnTaskId")),
            so(&t, "dependsOnStatus").unwrap_or_else(|| "desconocido".into())
        ));
    }
    let agente = resolver_agente(st, &t).await?;
    let perfil = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('llmModel', "llmModel", 'systemPrompt', "systemPrompt", 'slug', slug) FROM "Agent" WHERE id = $1"#, &[B::T(agente.id.clone())]).await.map_err(err_db)?.unwrap_or_else(|| json!({}));

    // Compare-and-swap atómico: solo pasa a IN_PROGRESS si todavía es disparable en este instante.
    let tomada = fetch_text_opt(
        &st.pool,
        r#"UPDATE "BacklogItem" SET status='IN_PROGRESS', "fechaInicio"=NOW(), "fechaEjecucion"=NOW() WHERE id=$1 AND status NOT IN ('IN_PROGRESS','DONE') RETURNING id"#,
        &[B::T(tarea.into())],
    )
    .await
    .map_err(err_db)?;
    if tomada.is_none() {
        return Err(format!("Task {tarea} ya fue tomada por otro dispatch simultáneo (doble click o reintento en paralelo) — no se disparó de nuevo."));
    }
    let exec_id = fetch_text_opt(
        &st.pool,
        r#"INSERT INTO "TaskExecution" (id,"backlogItemId","agentId","agentName",status,"startedAt") VALUES (gen_random_uuid()::text,$1,$2,$3,'RUNNING',NOW()) RETURNING id"#,
        &[B::T(tarea.into()), B::T(agente.id.clone()), B::T(agente.nombre.clone())],
    )
    .await
    .map_err(err_db)?
    .ok_or("no se pudo crear la ejecución")?;

    emitir_traza(st, tarea, Some(&exec_id), "info", &format!("tarea despachada — agente {}, estrategia {}", agente.nombre, agente.estrategia)).await;
    let contexto = construir_contexto(st, tarea).await?;
    emitir_traza(st, tarea, Some(&exec_id), "info", &format!("contexto armado (buildTaskContext) — {} caracteres", miles_es_ar(contexto.chars().count()))).await;
    let criterio = criterio_prd(st, so(&t, "solucionId").as_deref(), so(&t, "prdRequisitoId").as_deref()).await;
    if criterio.is_some() {
        emitir_traza(st, tarea, Some(&exec_id), "info", &format!("criterio de aceptación del PRD encontrado (requisito {})", sg(&t, "prdRequisitoId"))).await;
    }

    let system = so(&perfil, "systemPrompt").filter(|x| !x.is_empty()).unwrap_or_else(|| format!("Eres {}, agente de ArchiTechIA ejecutando una tarea del Motor Agéntico SDD.", agente.nombre));
    let mut partes: Vec<String> = vec![contexto.clone(), "---".into(), "Ejecuta la siguiente tarea:".into(), sg(&t, "title"), so(&t, "description").unwrap_or_default()];
    partes.push(match &criterio {
        Some((texto, crit)) => ["---".to_string(), "CRITERIO DE ACEPTACIÓN (del PRD — esta tarea implementa este requisito):".into(), texto.clone(), String::new(), format!("Se considera terminado cuando: {crit}")].join("\n"),
        None => String::new(),
    });
    // Recordatorio al FINAL del prompt (donde el modelo lo tiene fresco): lo que está al principio de un contexto largo se pierde.
    if contexto.contains(super::diseno::ENCABEZADO) {
        partes.push(super::diseno::recordatorio());
    }
    partes.push(match guia_extra.filter(|g| !g.is_empty()) {
        Some(g) => [
            "---",
            "PLAN DE REMEDIACIÓN YA REVISADO Y APROBADO POR UN HUMANO — seguilo, no re-investigues desde cero:",
            g,
            "",
            "Ejecutá los pasos de este plan EN ORDEN. Si te desviás de algún paso (porque encontraste algo que lo",
            "invalida, o porque no aplica), decilo explícitamente en tu resumen final y por qué. Tu resumen final",
            "debe indicar, paso por paso, cuáles completaste y cuáles no.",
        ]
        .join("\n"),
        None => String::new(),
    });
    let mut usuario = partes.join("\n\n");
    let (api_url, modelo) = resolver_modelo(st, so(&perfil, "llmModel").as_deref());

    // Tareas CODE dentro de un sprint: worktree aislado ramificado de la rama de integración del sprint.
    let mut repo_path: Option<String> = None;
    if agente.estrategia == "CODE" {
        // Presupuesto de rondas explícito, al final del prompt: las tareas que exploran sin escribir se quedan sin rondas y pierden todo.
        usuario.push_str(&format!("\n\n---\n{}", presupuesto_de_rondas()));
        if so(&t, "sprintCode").filter(|x| !x.is_empty()).is_none() {
            // Actividad de fase (sin sprint): ve el repositorio de la Solución, en solo lectura (main), si la Solución tiene uno propio.
            let sol = so(&t, "solucionId");
            let tiene_repo = match &sol {
                Some(s) => fetch_text_opt(&st.pool, r#"SELECT NULLIF(btrim(repositorio), '') FROM "Solucion" WHERE id = $1"#, &[B::T(s.clone())]).await.map_err(err_db)?.map(|r| r != "portal-architechia").unwrap_or(false),
                None => false,
            };
            if tiene_repo {
                let r: R<String> = async {
                    let raiz = repo::resolver_repo(st, sol.as_deref()).await?;
                    let _ = repo::git_con_credenciales(st, &["fetch", "origin", "main"], &raiz).await;
                    let base = if repo::rama_existe("origin/main", &raiz).await { "origin/main" } else { "main" };
                    let codigo = repo::codigo_actividad(tarea);
                    let (_, wt) = repo::crear_worktree_tarea(&codigo, base, &raiz).await?;
                    emitir_traza(st, tarea, Some(&exec_id), "info", &format!("repositorio de la Solución disponible en solo lectura ({base})")).await;
                    Ok(wt.to_string_lossy().to_string())
                }
                .await;
                match r {
                    Ok(p) => {
                        repo_path = Some(p);
                        usuario.push_str("\n\n---\nREPOSITORIO DE LA SOLUCIÓN (SOLO LECTURA): esta actividad es de análisis o documentación. Tienes el código del proyecto en tu directorio de trabajo para consultarlo (list_files, grep_files, read_file). NO escribas ni modifiques archivos del repositorio: cualquier cambio se descarta al terminar. Entrega el resultado completo en tu resumen final.");
                    }
                    Err(e) => emitir_traza(st, tarea, Some(&exec_id), "info", &format!("no se pudo preparar el repositorio de solo lectura: {e}")).await,
                }
            }
        }
        if let Some(codigo_sprint) = so(&t, "sprintCode").filter(|x| !x.is_empty()) {
            let r: R<String> = async {
                let raiz = repo::resolver_repo(st, so(&t, "solucionId").as_deref()).await?;
                let (rama_sprint, _) = repo::asegurar_rama_sprint(&codigo_sprint, &raiz).await?;
                let mut base = rama_sprint;
                if let Some(dep) = so(&t, "dependsOnTaskCode").filter(|x| !x.is_empty()) {
                    if repo::worktree_tarea(&dep).exists() {
                        base = repo::rama_tarea(&dep);
                    }
                }
                let codigo = sg(&t, "taskCode");
                // Un intento anterior que no terminó dejó su trabajo en la rama parcial: se continúa desde ahí.
                let parcial = repo::rama_parcial(&codigo);
                let continuando = repo::rama_existe(&parcial, &raiz).await;
                if continuando {
                    base = parcial.clone();
                }
                let (_, wt) = repo::crear_worktree_tarea(&codigo, &base, &raiz).await?;
                emitir_traza(st, tarea, Some(&exec_id), "info", &format!("worktree creado — rama {} desde {base}", repo::rama_tarea(&codigo))).await;
                if continuando {
                    emitir_traza(st, tarea, Some(&exec_id), "info", "continúa el trabajo parcial de un intento anterior (ya está en el worktree)").await;
                    usuario.push_str(&format!(
                        "\n\n---\nCONTINUACIÓN: un intento anterior de esta tarea NO terminó, pero su trabajo parcial YA está en tu rama (míralo con `git status` y `git diff main --stat` o con list_files; no empieces de cero). Revisa qué falta, corrige lo que esté mal y termina.\nResultado del intento anterior:\n{}",
                        cortar(&so(&t, "resultado").unwrap_or_default(), 1500)
                    ));
                }
                Ok(wt.to_string_lossy().to_string())
            }
            .await;
            match r {
                Ok(p) => repo_path = Some(p),
                Err(e) => {
                    // Igual que en Next, el error del worktree sube y la tarea queda IN_PROGRESS; se revierte para no dejarla varada.
                    let _ = exec(&st.pool, r#"UPDATE "BacklogItem" SET status='BACKLOG' WHERE id=$1"#, &[B::T(tarea.into())]).await;
                    let _ = exec(&st.pool, r#"UPDATE "TaskExecution" SET status='FAILED', "resultSummary"=$2, "finishedAt"=NOW() WHERE id=$1"#, &[B::T(exec_id.clone()), B::T(format!("Error preparando el worktree: {e}"))]).await;
                    return Err(e);
                }
            }
        }
    }

    let agente_slug = so(&perfil, "slug").filter(|x| !x.is_empty()).unwrap_or_else(|| agente.nombre.to_lowercase());
    let prioridad_cola = match so(&t, "solucionId") {
        Some(sid) => fetch_text_opt(&st.pool, r#"SELECT tipo FROM "Solucion" WHERE id = $1"#, &[B::T(sid)]).await.ok().flatten().map(|tp| prioridad_por_tipo(&tp)).unwrap_or("MEDIUM"),
        None => "MEDIUM",
    };
    let envio = st
        .http
        .post(format!("{}/dispatch", url_harness()))
        .timeout(std::time::Duration::from_secs(10))
        .json(&json!({
            "type": "masd_task", "agent": agente_slug, "priority": prioridad_cola,
            "payload": { "taskId": tarea, "execId": exec_id, "strategy": agente.estrategia, "apiUrl": api_url, "modelId": modelo, "systemPrompt": system, "userPrompt": usuario,
                         "contextPreview": cortar(&contexto, 4000), "repoPath": repo_path }
        }))
        .send()
        .await;
    match envio {
        Ok(_) => emitir_traza(st, tarea, Some(&exec_id), "run", &format!("encolada en el Harness (agente {agente_slug}) — esperando ejecución real")).await,
        Err(e) => {
            tracing::error!("[DISPATCH] No se pudo encolar en el Harness para {tarea}: {e}");
            emitir_traza(st, tarea, Some(&exec_id), "fail", &format!("no se pudo encolar en el Harness: {e}")).await;
            let _ = exec(&st.pool, r#"UPDATE "BacklogItem" SET status='BACKLOG' WHERE id=$1"#, &[B::T(tarea.into())]).await;
            let _ = exec(&st.pool, r#"UPDATE "TaskExecution" SET status='FAILED', "resultSummary"=$2, "finishedAt"=NOW() WHERE id=$1"#, &[B::T(exec_id.clone()), B::T(format!("Error encolando en Harness: {e}"))]).await;
            return Err(e.to_string());
        }
    }
    Ok(json!({ "taskId": tarea, "taskCode": t["taskCode"], "agentId": agente.id, "agentName": agente.nombre, "strategy": agente.estrategia, "started": true }))
}

// ── Verificación ─────────────────────────────────────────────────────────────────────────────
struct ResultadoCodigo {
    corrio: bool,
    paso: bool,
    errores: Vec<String>,
}

/// Archivos que una tarea escribió o modificó según su registro de herramientas: `write_file`, `edit_file` y el destino de
/// `move_file`. Se ignoran las llamadas que el worker rechazó (su resultado empieza con "ERROR").
fn archivos_tocados(tool_log: &[Value]) -> Vec<String> {
    tool_log
        .iter()
        .filter(|t| !t["resultPreview"].as_str().unwrap_or("").starts_with("ERROR"))
        .filter_map(|t| match t["tool"].as_str() {
            Some("write_file") | Some("edit_file") => t["args"]["rel_path"].as_str().map(String::from),
            Some("move_file") => t["args"]["rel_to"].as_str().map(String::from),
            _ => None,
        })
        .collect()
}

async fn chequeo_real(tool_log: &[Value], raiz: &Path) -> ResultadoCodigo {
    let escritos: Vec<String> = archivos_tocados(tool_log).into_iter().filter(|f| f.ends_with(".ts") || f.ends_with(".tsx")).collect();
    let uso_comando = tool_log.iter().any(|t| t["tool"] == "run_command");
    if escritos.is_empty() && !uso_comando {
        return ResultadoCodigo { corrio: false, paso: true, errores: vec![] };
    }
    match repo::sh_salida("npx", &["tsc", "--noEmit"], Some(raiz), 180).await {
        Ok((true, _, _)) => ResultadoCodigo { corrio: true, paso: true, errores: vec![] },
        Ok((false, out, err)) => {
            let salida = if !out.is_empty() { out } else if !err.is_empty() { err } else { "tsc terminó con errores".to_string() };
            if escritos.is_empty() {
                return ResultadoCodigo { corrio: true, paso: false, errores: vec![cortar(&salida, 3000)] };
            }
            let relevantes: Vec<String> = salida.lines().filter(|l| escritos.iter().any(|f| l.contains(f.as_str()))).map(String::from).collect();
            let paso = relevantes.is_empty();
            ResultadoCodigo { corrio: true, paso, errores: if paso { vec![cortar(&salida, 2000)] } else { relevantes } }
        }
        Err(e) => {
            // execFile con timeout/ENOENT: el mensaje es el error.
            if escritos.is_empty() {
                return ResultadoCodigo { corrio: true, paso: false, errores: vec![cortar(&e, 3000)] };
            }
            let relevantes: Vec<String> = e.lines().filter(|l| escritos.iter().any(|f| l.contains(f.as_str()))).map(String::from).collect();
            let paso = relevantes.is_empty();
            ResultadoCodigo { corrio: true, paso, errores: if paso { vec![cortar(&e, 2000)] } else { relevantes } }
        }
    }
}

struct Veredicto {
    paso: bool,
    checklist: Vec<Value>,
}

#[allow(clippy::too_many_arguments)]
async fn verificar(st: &AppState, tarea: &str, titulo: &str, descripcion: Option<&str>, criterios: Vec<String>, extra: Option<String>, resumen: &str, compilo: bool, archivos: &[String], cambios: &str) -> Veredicto {
    let mut criterios = if criterios.is_empty() {
        vec![format!(
            "El resultado responde realmente a lo que pide la tarea \"{titulo}\"{}. No alcanza con que el resultado no este vacio — tiene que responder lo pedido.",
            descripcion.filter(|d| !d.is_empty()).map(|d| format!(": {d}")).unwrap_or_default()
        )]
    } else {
        criterios
    };
    if let Some(e) = extra {
        criterios.push(e);
    }
    let contexto_codigo = if archivos.is_empty() && cambios.trim().is_empty() {
        String::new()
    } else {
        // Con el diff real delante, el verificador juzga el CÓDIGO (no solo el texto del resumen que escribió el agente).
        let diff = if cambios.trim().is_empty() {
            String::new()
        } else {
            format!("\n\nCAMBIOS REALES EN EL CÓDIGO (git diff de esta tarea{}):\n{}\n\nJuzgá si se cumplen los criterios mirando ESTE código, no solo lo que dice el resumen.", if cambios.contains("[diff truncado") { ", truncado" } else { "" }, cambios)
        };
        format!(
            "\nCONTEXTO REAL DE EJECUCIÓN (verificado por el sistema, no por el agente): el agente escribió realmente estos archivos en el repo: {}.{} NO le exijas que pegue el código fuente dentro del resumen — el código ya existe y compiló en el repo real; tu trabajo es juzgar si el ALCANCE descrito en el resumen responde razonablemente a la tarea, no el formato del texto.{diff}\n",
            archivos.join(", "),
            if compilo { " El compilador real (tsc --noEmit) confirmó que el código compila sin errores." } else { "" }
        )
    };
    let largo = resumen.chars().count();
    let usuario = format!(
        "Evalúa si la siguiente tarea fue completada exitosamente. No juzgues por la longitud de la respuesta — una respuesta corta puede ser perfectamente correcta, y una respuesta larga puede no responder nada de lo pedido.\n\nTAREA: {titulo}\n{}\n{contexto_codigo}\nCRITERIOS DE ACEPTACIÓN:\n{}\n\nRESULTADO REPORTADO:\n{}\n{}\n\nResponde ÚNICAMENTE con JSON válido en este formato:\n{{\n  \"passed\": true/false,\n  \"checklist\": [\n    {{\"criterion\": \"...\", \"passed\": true/false, \"reason\": \"...\"}}\n  ]\n}}\nSin markdown, sin explicación adicional. Solo el JSON.",
        descripcion.filter(|d| !d.is_empty()).map(|d| format!("DESCRIPCIÓN: {d}")).unwrap_or_default(),
        criterios.iter().enumerate().map(|(i, c)| format!("{}. {c}", i + 1)).collect::<Vec<_>>().join("\n"),
        if resumen.is_empty() { "(el resultado llegó vacío)".to_string() } else { cortar(resumen, 8000) },
        if largo > 8000 { "\n[... resumen truncado aca solo para el verificador, el resultado real completo es mas largo ...]" } else { "" }
    );
    // Hasta 2 intentos: una llamada que se corta por tiempo o un JSON mal formado del verificador es un fallo del propio verificador, no del
    // trabajo de la tarea (visto en el eval del 09/10/2026: el código compilaba y el verificador murió a los 90 s). El worker espera el cierre
    // hasta 420 s (ver report_completion), así que dos intentos de ≤100 s caben de sobra.
    let r: Result<Value, String> = async {
        let mut ultimo = String::from("sin intentos");
        for intento in 0..2 {
            if intento > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
            let salida = match llm::call_open_code(
                st,
                "Eres Sigma, agente verificador de calidad de ArchiTechIA. Juzgás si un resultado responde de verdad lo que se pidió, nunca por longitud del texto.",
                &usuario,
                &format!("masd-verify-{tarea}"),
                2048,
                100,
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("[VERIFICADOR] intento {} falló ({e}) — {}", intento + 1, if intento == 0 { "reintentando" } else { "se rinde" });
                    ultimo = e;
                    continue;
                }
            };
            match (salida.find('{'), salida.rfind('}')) {
                (Some(i), Some(f)) if f >= i => match serde_json::from_str::<Value>(&salida[i..=f]) {
                    Ok(v) => return Ok(v),
                    Err(e) => ultimo = e.to_string(),
                },
                _ => ultimo = "No JSON in verifier response".to_string(),
            }
        }
        Err(ultimo)
    }
    .await;
    match r {
        Ok(v) => Veredicto { paso: v["passed"] == true, checklist: v["checklist"].as_array().cloned().unwrap_or_default() },
        Err(e) => {
            let razon = format!("No se pudo verificar automáticamente ({e}) — requiere revisión manual");
            Veredicto { paso: false, checklist: criterios.iter().map(|c| json!({ "criterion": c, "passed": false, "reason": razon })).collect() }
        }
    }
}

// ── Cierre ───────────────────────────────────────────────────────────────────────────────────
pub struct Cierre {
    pub tarea: String,
    pub ejecucion: String,
    pub estado_final: String,
    pub resumen: String,
    pub duracion_ms: i64,
    pub contexto_usado: Option<String>,
    pub tool_log: Vec<Value>,
    /// Uso de tokens de toda la tarea que reporta el worker: { prompt_tokens, completion_tokens, total_tokens, calls, model }.
    pub uso: Option<Value>,
}

/// Recibe el resultado del worker y cierra el ciclo: verificación, estado de la tarea y cierre de sprint.
pub async fn finalizar(st: &AppState, c: Cierre) -> R<Value> {
    let tarea = c.tarea.as_str();
    let exec_id = c.ejecucion.as_str();
    let t = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(x) FROM (SELECT bi.id, bi.title, bi.description, bi."sprintId", bi."taskCode", bi."solucionId", bi."prdRequisitoId", s."sprintCode"
             FROM "BacklogItem" bi LEFT JOIN "Sprint" s ON bi."sprintId" = s.id WHERE bi.id = $1) x"#,
        &[B::T(tarea.into())],
    )
    .await
    .map_err(err_db)?;
    let Some(t) = t else { return Err(format!("Task {tarea} not found")) };
    let raiz_tarea = repo::resolver_repo(st, so(&t, "solucionId").as_deref()).await?;
    let codigo = so(&t, "taskCode").filter(|x| !x.is_empty());
    // Actividad de fase (sin taskCode ni sprint): si recibió el repositorio en solo lectura, su worktree se llama act-<id>.
    let cod_actividad = repo::codigo_actividad(tarea);
    let es_actividad = codigo.is_none() && repo::worktree_tarea(&cod_actividad).exists();
    let wt_tarea = codigo.as_deref().map(repo::worktree_tarea).or_else(|| es_actividad.then(|| repo::worktree_tarea(&cod_actividad)));
    let uso_worktree = wt_tarea.as_ref().map(|p| p.exists()).unwrap_or(false);
    let raiz_chequeo: PathBuf = match (&wt_tarea, uso_worktree) {
        (Some(p), true) => p.clone(),
        _ => raiz_tarea.clone(),
    };

    exec(
        &st.pool,
        r#"UPDATE "TaskExecution" SET status=$2, "resultSummary"=$3, "finishedAt"=NOW(), "durationMs"=$4::int, "contextUsed"=$5 WHERE id=$1"#,
        &[B::T(exec_id.into()), B::T(c.estado_final.clone()), B::T(c.resumen.clone()), B::I(c.duracion_ms), B::T(cortar(c.contexto_usado.as_deref().unwrap_or(""), 4000))],
    )
    .await
    .map_err(err_db)?;

    let mut verificado = c.estado_final.clone();
    let mut checklist: Vec<Value> = vec![];
    if c.estado_final == "DONE" {
        // Las actividades de fase no escriben código: no se compila nada (el repositorio está en solo lectura).
        let chequeo = if es_actividad { ResultadoCodigo { corrio: false, paso: true, errores: vec![] } } else { chequeo_real(&c.tool_log, &raiz_chequeo).await };
        if chequeo.corrio {
            emitir_traza(
                st,
                tarea,
                Some(exec_id),
                if chequeo.paso { "check" } else { "fail" },
                &if chequeo.paso { "tsc --noEmit — 0 errores en los archivos tocados".to_string() } else { format!("tsc --noEmit — {} error(es) real(es)", chequeo.errores.len()) },
            )
            .await;
        }
        if chequeo.corrio && !chequeo.paso {
            verificado = "FAILED".into();
            checklist = vec![json!({ "criterion": "El código escrito compila (tsc --noEmit)", "passed": false, "reason": format!("Errores reales de TypeScript en los archivos que esta tarea escribió:\n{}", chequeo.errores.join("\n")) })];
        } else {
            let archivos: Vec<String> = archivos_tocados(&c.tool_log);
            // El parche real de la tarea (tope de 9.000 caracteres) para que el verificador juzgue el código y no solo el resumen.
            let cambios = match (&wt_tarea, uso_worktree) {
                (Some(wt), true) => {
                    let d = repo::diff_de_tarea(wt).await;
                    if d.chars().count() > 9000 { format!("{}\n[diff truncado: la tarea cambió más de lo que cabe acá]", cortar(&d, 9000)) } else { d }
                }
                _ => String::new(),
            };
            let criterio = criterio_prd(st, so(&t, "solucionId").as_deref(), so(&t, "prdRequisitoId").as_deref()).await;
            // Diseño técnico del hub: criterio para el verificador, aviso si el esquema de Prisma se aparta y registro de cambios de diseño declarados.
            let diseno = match so(&t, "solucionId") {
                Some(sid) => super::diseno::cargar(st, &sid).await,
                None => None,
            };
            if let Some(d) = &diseno {
                if archivos.iter().any(|a| a.ends_with("prisma/schema.prisma")) {
                    if let Ok(schema) = tokio::fs::read_to_string(raiz_chequeo.join("prisma/schema.prisma")).await {
                        for aviso in super::diseno::avisos_schema(d, &schema) {
                            emitir_traza(st, tarea, Some(exec_id), "info", &format!("diseño técnico — {aviso}")).await;
                        }
                    }
                }
            }
            if let Some(pos) = c.resumen.find(super::diseno::MARCA_CAMBIO) {
                let declarado: String = c.resumen[pos..].lines().next().unwrap_or("").chars().take(300).collect();
                emitir_traza(st, tarea, Some(exec_id), "info", &format!("el agente declaró un cambio de diseño — conviene actualizar el Diseño técnico del hub: {declarado}")).await;
            }
            let v = verificar(st, so(&t, "id").as_deref().unwrap_or(tarea), &sg(&t, "title"), so(&t, "description").as_deref(), criterio.map(|(_, crit)| vec![crit]).unwrap_or_default(), diseno.as_ref().map(|d| d.criterio()), &c.resumen, chequeo.corrio, &archivos, &cambios).await;
            // El revisor marca por su cuenta si el diff se aparta del diseño documentado (no depende de que el agente lo haya declarado).
            for x in &v.checklist {
                if let Some(r) = x["reason"].as_str().filter(|r| r.contains(super::diseno::MARCA_REVISOR)) {
                    emitir_traza(st, tarea, Some(exec_id), "info", &format!("el revisor detectó una desviación del diseño técnico — conviene actualizar el Diseño técnico del hub o revisar el cambio: {}", cortar(r, 300))).await;
                }
            }
            verificado = if v.paso { "DONE".into() } else { "FAILED".into() };
            checklist = v.checklist;
            if chequeo.corrio {
                checklist.insert(0, json!({ "criterion": "El código escrito compila (tsc --noEmit)", "passed": true, "reason": "Verificado con el compilador real" }));
            }
            let pasados = checklist.iter().filter(|x| x["passed"] == true).count();
            emitir_traza(st, tarea, Some(exec_id), if v.paso { "check" } else { "fail" }, &format!("verificador semántico — {} ({pasados}/{} criterios)", if v.paso { "PASSED" } else { "FAILED" }, checklist.len())).await;
        }
    }

    // Artefactos de la ejecución: checklist, registro de herramientas y, si el worker lo mandó, el uso de tokens (para medir cuánto cuesta cada tarea).
    let mut artefactos = json!({ "checklist": checklist, "toolLog": c.tool_log });
    if let Some(u) = c.uso.as_ref().filter(|u| u.is_object()) {
        artefactos["usage"] = u.clone();
        let n = |k: &str| u[k].as_i64().unwrap_or(0);
        emitir_traza(
            st,
            tarea,
            Some(exec_id),
            "info",
            &format!("uso de tokens — {} en total ({} entrada + {} salida) en {} llamada(s) al modelo{}", miles_es_ar(n("total_tokens").max(0) as usize), n("prompt_tokens"), n("completion_tokens"), n("calls"), u["model"].as_str().map(|m| format!(" ({m})")).unwrap_or_default()),
        )
        .await;
    }
    exec(&st.pool, r#"UPDATE "TaskExecution" SET artifacts=$2::jsonb WHERE id=$1"#, &[B::T(exec_id.into()), B::T(artefactos.to_string())]).await.map_err(err_db)?;

    let mut resultado_final = c.resumen.clone();
    if let (true, Some(codigo_sprint), Some(wt), Some(cod)) = (uso_worktree, so(&t, "sprintCode").filter(|x| !x.is_empty()), wt_tarea.as_ref(), codigo.as_deref()) {
        if verificado == "DONE" {
            let wt_sprint = repo::worktree_sprint(&codigo_sprint);
            match repo::commit_y_merge(cod, wt, &repo::rama_tarea(cod), &wt_sprint, &raiz_tarea).await {
                Ok(_) => {
                    emitir_traza(st, tarea, Some(exec_id), "info", "merge a la rama de integración del sprint — sin conflictos").await;
                    if repo::rama_existe(&repo::rama_parcial(cod), &raiz_tarea).await {
                        repo::borrar_rama(&repo::rama_parcial(cod), &raiz_tarea).await;
                    }
                }
                Err(ErrorMerge::Conflicto { rama, archivos }) => {
                    verificado = "BLOCKED".into();
                    let lista = if archivos.is_empty() { "(sin detalle)".to_string() } else { archivos.join(", ") };
                    emitir_traza(st, tarea, Some(exec_id), "fail", &format!("conflicto real de merge — archivos: {lista}")).await;
                    resultado_final = [
                        c.resumen.clone(),
                        String::new(),
                        "⚠️ CONFLICTO REAL DE MERGE — el código pasó verificación pero NO se integró a la rama del sprint.".into(),
                        format!("Archivos en conflicto: {lista}"),
                        format!("El trabajo quedó intacto en la rama {rama} — requiere resolución manual (probablemente otra tarea de este sprint tocó el mismo archivo)."),
                    ]
                    .join("\n");
                }
                Err(ErrorMerge::Otro(e)) => {
                    tracing::error!("[GIT] Error inesperado mergeando worktree de {cod}: {e}");
                    verificado = "BLOCKED".into();
                    resultado_final = [c.resumen.clone(), String::new(), format!("⚠️ Error inesperado integrando el código a la rama del sprint: {e} — requiere revisión manual.")].join("\n");
                }
            }
        } else {
            // La tarea no terminó: sus cambios no se pierden, quedan en una rama parcial desde la que continuará el siguiente intento.
            match repo::guardar_parcial(cod, wt, &raiz_tarea).await {
                Ok(Some((rama, n))) => {
                    let agoto = agoto_rondas(&c.resumen, c.uso.as_ref());
                    emitir_traza(st, tarea, Some(exec_id), "info", &format!("trabajo parcial conservado en la rama {rama} ({n} archivo(s)){}", if agoto { " — la tarea agotó sus rondas de herramientas" } else { "" })).await;
                    resultado_final = [
                        c.resumen.clone(),
                        String::new(),
                        format!("⚠️ TRABAJO PARCIAL CONSERVADO en la rama {rama} ({n} archivo(s) sin integrar). Al volver a ejecutar esta tarea el agente continúa desde ahí."),
                        if agoto { "La tarea agotó sus rondas de herramientas: es demasiado grande para un solo intento. Conviene dividirla en tareas más chicas (cada una con pocos archivos) o relanzarla para que continúe.".to_string() } else { String::new() },
                    ]
                    .join("\n");
                }
                Ok(None) => {
                    repo::descartar_worktree(wt, &raiz_tarea).await;
                    emitir_traza(st, tarea, Some(exec_id), "info", "worktree descartado — la tarea no dejó cambios").await;
                }
                Err(e) => {
                    tracing::error!("[GIT] No se pudo guardar el trabajo parcial de {cod}: {e}");
                    repo::descartar_worktree(wt, &raiz_tarea).await;
                    emitir_traza(st, tarea, Some(exec_id), "info", "worktree descartado — no se pudo guardar el trabajo parcial").await;
                }
            }
        }
    } else if let (true, Some(wt)) = (es_actividad && so(&t, "sprintCode").filter(|x| !x.is_empty()).is_none(), wt_tarea.as_ref()) {
        repo::descartar_worktree(wt, &raiz_tarea).await;
        emitir_traza(st, tarea, Some(exec_id), "info", "repositorio de solo lectura liberado").await;
    }

    emitir_traza(st, tarea, Some(exec_id), if verificado == "DONE" { "check" } else { "fail" }, &format!("estado final: {verificado}")).await;
    exec(&st.pool, r#"UPDATE "BacklogItem" SET status=$2, "fechaFin"=NOW(), resultado=$3 WHERE id=$1"#, &[B::T(tarea.into()), B::T(verificado.clone()), B::T(resultado_final.clone())]).await.map_err(err_db)?;

    if verificado == "DONE" {
        if let Some(sprint_id) = so(&t, "sprintId") {
            let decisiones = extraer_decisiones(&c.resumen);
            for d in &decisiones {
                let _ = exec(&st.pool, r#"INSERT INTO "SprintDecision" (id, "sprintId", "taskId", summary, "createdAt") VALUES ($1, $2, $3, $4, NOW())"#, &[B::T(new_id()), B::T(sprint_id.clone()), B::T(tarea.into()), B::T(d.clone())]).await;
            }
            if !decisiones.is_empty() {
                emitir_traza(st, tarea, Some(exec_id), "info", &format!("{} decisión(es) guardada(s) en la bitácora del sprint", decisiones.len())).await;
            }
        }
    }

    if verificado == "FAILED" || verificado == "BLOCKED" {
        let limite = agoto_rondas(&c.resumen, c.uso.as_ref());
        let titulo = sg(&t, "title");
        let codigo_o_id = codigo.clone().unwrap_or_else(|| tarea.to_string());
        let r = exec(
            &st.pool,
            r#"INSERT INTO "Notification" (id, "userId", type, title, message, link, "createdAt") VALUES (gen_random_uuid()::text, 'system', $1, $2, $3, '/backlog', NOW())"#,
            &[
                B::T(if limite { "warning" } else { "error" }.into()),
                B::T(if limite { format!("Agente sin terminar: {titulo}") } else { format!("Tarea {}: {titulo}", if verificado == "BLOCKED" { "bloqueada" } else { "fallida" }) }),
                B::T(if limite { format!("El agente de código llegó al límite de pasos de herramientas sin terminar ({codigo_o_id}). Puede necesitar más pasos o una tarea más chica.") } else { cortar(&resultado_final, 300) }),
            ],
        )
        .await;
        if let Err(e) = r {
            tracing::error!("[EXECUTOR] No se pudo crear la notificación de fallo: {e}");
        }
    }
    tracing::info!("[EXECUTOR] {} → {verificado} ({}s)", sg(&t, "title"), (c.duracion_ms as f64 / 1000.0).round() as i64);

    if let Some(sprint_id) = so(&t, "sprintId") {
        if let Err(e) = revisar_cierre_sprint(st, &sprint_id).await {
            tracing::error!("[SPRINT_MONITOR] Error: {e}");
        }
    }
    Ok(json!({ "verifiedStatus": verificado }))
}

// ── Cierre de sprint ─────────────────────────────────────────────────────────────────────────
async fn revisar_cierre_sprint(st: &AppState, sprint_id: &str) -> R<()> {
    let estados = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(status), '[]'::jsonb) FROM "BacklogItem" WHERE "sprintId" = $1"#, &[B::T(sprint_id.into())]).await.map_err(err_db)?;
    let estados = estados.as_array().cloned().unwrap_or_default();
    if estados.is_empty() {
        return Ok(());
    }
    // BLOCKED cuenta como terminal: un sprint con tareas saltadas en cascada también se cierra.
    if !estados.iter().all(|s| matches!(s.as_str(), Some("DONE" | "FAILED" | "CANCELLED" | "BLOCKED"))) {
        return Ok(());
    }
    resumir_sprint(st, sprint_id).await
}

async fn resumir_sprint(st: &AppState, sprint_id: &str) -> R<()> {
    let sprint = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', s.id, 'name', s.name, 'goal', s.goal, 'sprintCode', s."sprintCode", 'epicId', s."epicId", 'epicName', e.name, 'solucionId', sol.id, 'solucionCode', sol."solucionCode")
           FROM "Sprint" s JOIN "Epic" e ON s."epicId" = e.id JOIN "Solucion" sol ON e."solucionId" = sol.id WHERE s.id = $1"#,
        &[B::T(sprint_id.into())],
    )
    .await
    .map_err(err_db)?;
    let Some(sprint) = sprint else { return Err(format!("El sprint {sprint_id} no tiene épica/solución: no se puede resumir")) };
    let tareas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."createdAt"), '[]'::jsonb) FROM (
             SELECT bi.title, bi.status, bi.resultado, te."agentName", te."resultSummary", te."durationMs", bi."createdAt"
               FROM "BacklogItem" bi LEFT JOIN "TaskExecution" te ON te."backlogItemId" = bi.id AND te.status = bi.status WHERE bi."sprintId" = $1 ORDER BY bi."createdAt") x"#,
        &[B::T(sprint_id.into())],
    )
    .await
    .map_err(err_db)?;
    let tareas = tareas.as_array().cloned().unwrap_or_default();
    let cuenta = |e: &str| tareas.iter().filter(|t| t["status"] == e).count();
    let (hechas, falladas, bloqueadas) = (cuenta("DONE"), cuenta("FAILED"), cuenta("BLOCKED"));
    let lista = tareas
        .iter()
        .map(|t| format!("[{}] {}{}{}", sg(t, "status"), sg(t, "title"), so(t, "agentName").filter(|x| !x.is_empty()).map(|a| format!(" ({a})")).unwrap_or_default(), so(t, "resultado").filter(|x| !x.is_empty()).map(|r| format!(": {}", cortar(&r, 150))).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    let usuario = format!(
        "SPRINT: {}\nOBJETIVO: {}\nCOMPLETADAS: {hechas}/{n} | FALLIDAS: {falladas}/{n} | BLOQUEADAS (saltadas por una dependencia fallida): {bloqueadas}/{n}\n\nTAREAS:\n{lista}\n\nResponde ÚNICAMENTE con JSON:\n{{\n  \"summary\": \"Párrafo ejecutivo de 2-3 oraciones sobre qué se logró\",\n  \"achievements\": [\"logro 1\", \"logro 2\"],\n  \"blockers\": [\"bloqueo 1 si hubo\", \"...\"],\n  \"recommendation\": \"Qué hacer en el siguiente sprint\"\n}}\nSin markdown extra.",
        sg(&sprint, "name"),
        so(&sprint, "goal").filter(|x| !x.is_empty()).unwrap_or_else(|| "—".into()),
        n = tareas.len()
    );
    let ahora = || fetch_text_opt(&st.pool, r#"SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')"#, &[]);
    let cierre = ahora().await.map_err(err_db)?.unwrap_or_default();
    let mut metadata = json!({
        "tasksCompleted": hechas, "tasksFailed": falladas, "tasksBlocked": bloqueadas, "closedAt": cierre,
        "summary": format!("Sprint {}: {hechas}/{} tareas completadas.", sg(&sprint, "name"), tareas.len()),
        "achievements": [], "blockers": if falladas > 0 { json!([format!("{falladas} tarea(s) fallaron")]) } else { json!([]) },
        "recommendation": "Continuar con el siguiente sprint.",
    });
    if let Ok(salida) = llm::call_open_code(st, "Eres Orión, CEO del Council de ArchiTechIA. Generas Sprint Summaries ejecutivos.", &usuario, &format!("masd-sprintsummary-{sprint_id}"), 1024, 60).await {
        if let (Some(i), Some(f)) = (salida.find('{'), salida.rfind('}')) {
            if f > i {
                if let Ok(Value::Object(p)) = serde_json::from_str::<Value>(&salida[i..=f]) {
                    if let Some(m) = metadata.as_object_mut() {
                        for (k, v) in p {
                            m.insert(k, v);
                        }
                        m.insert("tasksCompleted".into(), json!(hechas));
                        m.insert("tasksFailed".into(), json!(falladas));
                        m.insert("tasksBlocked".into(), json!(bloqueadas));
                        m.insert("closedAt".into(), json!(ahora().await.map_err(err_db)?.unwrap_or_default()));
                    }
                }
            }
        }
    }
    exec(&st.pool, r#"UPDATE "Sprint" SET status = 'REVIEW_PENDING', metadata = $2::jsonb WHERE id = $1"#, &[B::T(sprint_id.into()), B::T(metadata.to_string())]).await.map_err(err_db)?;

    let lista_de = |k: &str| metadata[k].as_array().cloned().unwrap_or_default().iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>();
    let (logros, bloqueos) = (lista_de("achievements"), lista_de("blockers"));
    let texto = |k: &str| match &metadata[k] {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        otro => otro.to_string(),
    };
    let cuerpo = [
        "## Resumen".to_string(),
        texto("summary"),
        String::new(),
        "## Logros".into(),
        if logros.is_empty() { "(ninguno registrado)".to_string() } else { logros.iter().map(|a| format!("- {a}")).collect::<Vec<_>>().join("\n") },
        String::new(),
        "## Bloqueos".into(),
        if bloqueos.is_empty() { "(ninguno)".to_string() } else { bloqueos.iter().map(|b| format!("- {b}")).collect::<Vec<_>>().join("\n") },
        String::new(),
        "## Recomendación".into(),
        texto("recommendation"),
        String::new(),
        "## Epic relacionado".into(),
        format!("[[{}]]", sg(&sprint, "epicName")),
    ]
    .join("\n");
    let codigo_sprint = sg(&sprint, "sprintCode");
    if let Err(e) = escribir_nota(
        &format!("shared/decisions/sprints/{codigo_sprint}.md"),
        &[
            ("sprintCode", json!(codigo_sprint)),
            ("epicId", json!(sg(&sprint, "epicId"))),
            ("solucionCode", json!(sg(&sprint, "solucionCode"))),
            ("tasksCompleted", json!(hechas)),
            ("tasksFailed", json!(falladas)),
            ("closedAt", json!(ahora().await.map_err(err_db)?.unwrap_or_default())),
            ("tags", json!(["sprint-summary", sg(&sprint, "solucionCode")])),
        ],
        &format!("# {} ({codigo_sprint})\n\n{cuerpo}", sg(&sprint, "name")),
    )
    .await
    {
        tracing::error!("[SPRINT_MONITOR] No se pudo escribir la nota del vault: {e}");
    }
    tracing::info!("[SPRINT_MONITOR] Sprint {} → REVIEW_PENDING ({hechas} done, {falladas} failed)", sg(&sprint, "name"));

    // Si alguna tarea CODE corrió en su worktree, existe una rama de integración con commits reales: un solo PR hacia main.
    let wt_sprint = repo::worktree_sprint(&codigo_sprint);
    if wt_sprint.exists() {
        let r: R<Option<String>> = async {
            let raiz = repo::resolver_repo(st, so(&sprint, "solucionId").as_deref()).await?;
            repo::abrir_pr_sprint(
                st,
                &repo::rama_sprint(&codigo_sprint),
                &wt_sprint,
                &format!("[{codigo_sprint}] {}", sg(&sprint, "name")),
                &[
                    format!("**Sprint:** {} ({codigo_sprint})", sg(&sprint, "name")),
                    format!("**Épic:** {}", sg(&sprint, "epicName")),
                    format!("**Resultado:** {hechas}/{} tareas completadas, {falladas} fallidas.", tareas.len()),
                    String::new(),
                    "## Resumen".into(),
                    texto("summary"),
                    String::new(),
                    "## Tareas".into(),
                    lista.clone(),
                    String::new(),
                    "_PR abierto automáticamente por el Motor Agéntico SDD. Revisar y mergear manualmente — nunca se mergea solo._".into(),
                ]
                .join("\n"),
                &raiz,
            )
            .await
        }
        .await;
        match r {
            Ok(Some(url)) => tracing::info!("[SPRINT_MONITOR] PR del sprint {codigo_sprint}: {url}"),
            Ok(None) => {}
            Err(e) => tracing::error!("[SPRINT_MONITOR] No se pudo abrir el PR del sprint {codigo_sprint}: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod pruebas_rondas {
    use super::*;

    #[test]
    fn detecta_cuando_la_tarea_agoto_sus_rondas() {
        assert!(agoto_rondas("... se alcanzo el limite de pasos de herramientas ...", None));
        assert!(agoto_rondas("... se alcanzó el límite de pasos de herramientas ...", None));
        assert!(agoto_rondas("terminé", Some(&json!({ "calls": 21 }))));
        assert!(agoto_rondas("terminé", Some(&json!({ "calls": 20 }))));
        assert!(!agoto_rondas("terminé", Some(&json!({ "calls": 7 }))));
        assert!(!agoto_rondas("terminé", None));
    }

    #[test]
    fn lo_comercial_va_primero_en_la_cola() {
        assert_eq!(prioridad_por_tipo("PROJECT"), "HIGH");
        assert_eq!(prioridad_por_tipo("DEMO"), "HIGH");
        assert_eq!(prioridad_por_tipo("PARTNERSHIP"), "HIGH");
        assert_eq!(prioridad_por_tipo("PRODUCT"), "MEDIUM");
        assert_eq!(prioridad_por_tipo("INTERN"), "MEDIUM");
    }

    #[test]
    fn el_presupuesto_dice_las_rondas_y_como_avisar() {
        let p = presupuesto_de_rondas();
        assert!(p.contains("20 rondas") && p.contains("PARCIAL:") && p.contains("escribe cuanto antes"));
    }
}
