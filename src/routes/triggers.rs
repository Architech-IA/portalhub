//! Disparadores del consejo (`lib/council-trigger.ts`): al crear una solución o una épica, Orión propone
//! automáticamente un roadmap inicial como propuesta del consejo (canal `INTERNAL_TRIGGER`).
//!
//! La configuración (qué tipos disparan) vive en un archivo JSON, como en Next. Cambio respecto a Next:
//! la propuesta la genera OpenCode por HTTP en lugar de ejecutar el CLI `claude` (el servicio corre sin
//! privilegios y sin ese CLI).

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    llm,
    session::{Opcional, Session},
    state::AppState,
    util::{exec, fetch_json, fetch_text_opt, new_id, s, truthy, B},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/council/trigger/config", get(config_obtener).put(config_guardar))
}

fn ruta_config() -> String {
    std::env::var("COUNCIL_TRIGGER_CONFIG").unwrap_or_else(|_| "/opt/portalhub/council-trigger-config.json".to_string())
}

fn por_defecto() -> Value {
    json!({ "PRODUCT": true, "PROJECT": true, "INTERN": false, "PILOT": false, "epicTriggerEnabled": true })
}

async fn leer_config() -> Value {
    let mut cfg = por_defecto();
    if let Ok(raw) = tokio::fs::read_to_string(ruta_config()).await {
        if let (Ok(Value::Object(extra)), Some(base)) = (serde_json::from_str::<Value>(&raw), cfg.as_object_mut()) {
            for (k, v) in extra {
                base.insert(k, v);
            }
        }
    }
    cfg
}

async fn config_obtener(_s: Session) -> Json<Value> {
    Json(leer_config().await)
}

async fn config_guardar(_s: Session, Json(config): Json<Value>) -> ApiResult<Json<Value>> {
    let texto = serde_json::to_string_pretty(&config).map_err(|e| ApiError::internal(e.to_string()))?;
    tokio::fs::write(ruta_config(), texto).await.map_err(|e| {
        tracing::error!("[council/trigger/config] no se pudo escribir: {e}");
        ApiError::internal("Error interno")
    })?;
    Ok(Json(json!({ "ok": true })))
}

const ORION_SYSTEM: &str = r#"Eres Orión, agente estratégico de ArchiTechIA. Tu función en este canal es analizar nuevas soluciones y épicas recién creadas y proponer automáticamente un set inicial de épicas/sprints/tasks para que el consejo los valide antes de agregarlos al backlog.

Responde SIEMPRE con JSON EXACTO sin markdown ni explicaciones:
{
  "title": "título de la propuesta (max 80 chars)",
  "description": "descripción ejecutiva de 2-3 oraciones explicando el por qué y objetivo",
  "items": [
    {
      "type": "task" o "sprint" o "epic",
      "title": "título del item",
      "description": "qué implica este item",
      "areaSlug": "operations/sales/finance/marketing/people/delivery/dev/data/infra/security/qa",
      "priority": "LOW" o "MEDIUM" o "HIGH" o "CRITICAL"
    }
  ]
}"#;

/// Primer objeto `{...}` de la salida del modelo (equivale a `match(/\{[\s\S]*\}/)`: del primer `{` al último `}`).
fn extraer(salida: &str) -> Option<Value> {
    let ini = salida.find('{')?;
    let fin = salida.rfind('}')?;
    if fin < ini {
        return None;
    }
    serde_json::from_str(&salida[ini..=fin]).ok()
}

async fn llamar_orion(st: &AppState, usuario: &str, clave: &str) -> Option<Value> {
    let salida = llm::call_open_code(st, ORION_SYSTEM, usuario, &format!("trigger-{clave}"), 3500, 90).await.ok()?;
    extraer(&salida)
}

fn etiqueta_tipo(t: &str) -> String {
    match t {
        "PRODUCT" => "producto comercial para clientes externos".into(),
        "PROJECT" => "proyecto para un cliente específico".into(),
        "INTERN" => "herramienta o plataforma interna de ArchiTechIA".into(),
        "PILOT" => "piloto experimental o prueba de concepto".into(),
        otro => otro.to_string(),
    }
}

async fn insertar_propuesta(st: &AppState, extraida: &Value, titulo_defecto: String, metadata: Value) {
    let titulo = extraida["title"].as_str().filter(|t| !t.is_empty()).map(String::from).unwrap_or(titulo_defecto);
    let r = exec(
        &st.pool,
        r#"INSERT INTO "CouncilProposal" (id, title, description, status, "inputChannel", items, round, metadata, "createdAt", "updatedAt")
           VALUES ($1, $2, $3, 'PENDING', 'INTERNAL_TRIGGER', $4::jsonb, 1, $5::jsonb, NOW(), NOW())"#,
        &[B::T(new_id()), B::T(titulo), B::T(extraida["description"].as_str().unwrap_or("").to_string()), B::T(extraida.get("items").cloned().filter(|v| v.is_array()).unwrap_or_else(|| json!([])).to_string()), B::T(metadata.to_string())],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("[council-trigger] no se pudo guardar la propuesta: {e}");
    }
}

async fn disparar_solucion(st: AppState, id: String, nombre: String, descripcion: Option<String>, tipo: String) {
    let cfg = leer_config().await;
    if !truthy(&cfg, &tipo) {
        return;
    }
    let prompt = format!(
        "Se acaba de crear una nueva solución en ArchiTechIA.\n\nNombre: \"{nombre}\"\nTipo: {tipo} — {}\nDescripción: {}\n\nPropone un roadmap inicial para esta solución: épicas de alto nivel o sprints iniciales con las tareas más críticas para arrancar. El consejo validará antes de agregarlo al backlog.",
        etiqueta_tipo(&tipo),
        descripcion.as_deref().unwrap_or("(sin descripción)")
    );
    let Some(ext) = llamar_orion(&st, &prompt, &id).await else { return };
    insertar_propuesta(&st, &ext, format!("Propuesta inicial: {nombre}"), json!({ "trigger": "solution_created", "solucionId": id, "solucionNombre": nombre, "tipo": tipo })).await;
}

async fn disparar_epica(st: AppState, id: String, nombre: String, descripcion: Option<String>, solucion_id: Option<String>) {
    let cfg = leer_config().await;
    if !truthy(&cfg, "epicTriggerEnabled") {
        return;
    }
    let mut solucion_nombre: Option<String> = None;
    let mut otras: Vec<String> = vec![];
    if let Some(sid) = &solucion_id {
        solucion_nombre = fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Solucion" WHERE id = $1"#, &[B::T(sid.clone())]).await.ok().flatten();
        if let Ok(v) = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(name), '[]'::jsonb) FROM "Epic" WHERE "solucionId" = $1"#, &[B::T(sid.clone())]).await {
            otras = v.as_array().cloned().unwrap_or_default().iter().filter_map(|n| n.as_str().map(String::from)).filter(|n| *n != nombre).collect();
        }
    }
    let prompt = format!(
        "Se acaba de crear una nueva épica en ArchiTechIA.\n\nÉpica: \"{nombre}\"\nDescripción: {}\nSolución: {}{}\n\nPropone sprints que descompongan el trabajo de esta épica en iteraciones de 2 semanas. Cada sprint incluye tasks concretas con área responsable. Considera las otras épicas para evitar solapamientos.",
        descripcion.as_deref().unwrap_or("(sin descripción)"),
        solucion_nombre.as_deref().unwrap_or("no especificada"),
        if otras.is_empty() { String::new() } else { format!("\nOtras épicas en esta solución: {}", otras.join(", ")) }
    );
    let Some(ext) = llamar_orion(&st, &prompt, &id).await else { return };
    insertar_propuesta(&st, &ext, format!("Descomposición: {nombre}"), json!({ "trigger": "epic_created", "epicId": id, "epicNombre": nombre, "solucionId": solucion_id })).await;
}

// ── Creación de épicas y soluciones (disparan la propuesta) ───────────────────────────────────
pub async fn epic_crear(State(st): State<AppState>, o: Opcional, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if o.0.is_none() {
        return Err(ApiError::unauthorized_msg("No autenticado"));
    }
    let nombre = s(&body, "name");
    let Some(nombre) = nombre else {
        // `prisma.epic.create` sin `name` falla en Next → 500.
        return Err(ApiError::internal("Error interno"));
    };
    let desc = s(&body, "description").filter(|d| !d.is_empty());
    let id_opt = |k: &str| s(&body, k).filter(|x| !x.is_empty());
    let (roadmap, solucion) = (id_opt("roadmapId"), id_opt("solucionId"));
    let inicio = crate::util::fecha_cuerpo(&body, "startDate");
    let fin = crate::util::fecha_cuerpo(&body, "endDate");
    let id = new_id();
    let sql = format!(
        r#"WITH e AS (INSERT INTO "Epic" (id, name, description, priority, color, "startDate", "endDate", "roadmapId", "solucionId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, {}, {}, $8, $9, NOW(), NOW()) RETURNING *)
           SELECT to_jsonb(e) || jsonb_build_object(
             'roadmap', (SELECT jsonb_build_object('id', r.id, 'name', r.name, 'quarter', r.quarter) FROM "Roadmap" r WHERE r.id = e."roadmapId"),
             'sprints', '[]'::jsonb) FROM e"#,
        crate::util::ts_js_opt(6),
        crate::util::ts_js_opt(7)
    );
    let epic = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(id.clone()),
            B::T(nombre.clone()),
            B::OT(desc.clone()),
            B::T(s(&body, "priority").filter(|p| !p.is_empty()).unwrap_or_else(|| "MEDIUM".into())),
            B::T(s(&body, "color").filter(|p| !p.is_empty()).unwrap_or_else(|| "#1D9375".into())),
            B::OT(inicio),
            B::OT(fin),
            B::OT(roadmap),
            B::OT(solucion.clone()),
        ],
    )
    .await?;
    tokio::spawn(disparar_epica(st.clone(), id, nombre, desc, solucion));
    Ok(Json(epic))
}

pub async fn solucion_crear(State(st): State<AppState>, o: Opcional, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let usuario = o.0.as_ref().map(|se| se.id.clone());
    let Some(nombre) = s(&body, "nombre") else { return Err(ApiError::internal("Error interno")) };
    let Some(tipo) = s(&body, "tipo") else { return Err(ApiError::internal("Error interno")) };
    let codigo = match s(&body, "solucionCode").filter(|c| !c.is_empty()) {
        Some(c) => c.to_uppercase(),
        None => crate::routes::council::codigo_solucion_unico(&st, &crate::routes::council::generar_codigo_solucion(&nombre)).await?,
    };
    let texto = |k: &str| s(&body, k).filter(|x| !x.is_empty());
    let valor = crate::util::parse_float(body.get("valorEstimado"));
    let id = new_id();
    let sql = format!(
        r#"WITH so AS (INSERT INTO "Solucion" (id, nombre, descripcion, tipo, estado, "valorEstimado", empresa, "leadId", repositorio, arquitectura, "planTrabajo", cronograma, "solucionCode", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW(), NOW()) RETURNING *)
           SELECT {} FROM so"#,
        crate::routes::gestion::SOLUCION_JSON
    );
    let solucion = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(id.clone()),
            B::T(nombre.clone()),
            B::OT(texto("descripcion")),
            B::T(tipo.clone()),
            B::T(texto("estado").unwrap_or_else(|| "ACTIVO".into())),
            B::F(valor),
            B::OT(texto("empresa")),
            B::OT(texto("leadId")),
            B::OT(texto("repositorio")),
            B::T(texto("arquitectura").unwrap_or_else(|| "[]".into())),
            B::OT(texto("planTrabajo")),
            B::T(texto("cronograma").unwrap_or_else(|| "[]".into())),
            B::T(codigo),
        ],
    )
    .await?;
    crate::util::log_activity(&st.pool, "CREATED", &format!("creó la solución {nombre}"), "solucion", &id, usuario.as_deref(), None).await;
    tokio::spawn(disparar_solucion(st.clone(), id, nombre, texto("descripcion"), tipo));
    Ok(Json(solucion))
}

#[allow(dead_code)]
fn _no_usado() -> axum::routing::MethodRouter<AppState> {
    post(|| async {})
}
