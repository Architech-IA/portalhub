//! Hub de cada lead — MASD PHUB-0001-0006.
//! Paridad con `src/app/api/leads/{hub-file,hub-phase,architecture,architecture/generate,diagram,
//! diagram/generate,diagram/chat}/route.ts`. (El chat de IA del lead vive en `aichat.rs`.)
//!
//! Corrección deliberada respecto de Next: al normalizar las posiciones del diagrama generado,
//! Next hacía `parseFloat(String(n.x)) || 5`, y como 0 es "falsy" en JavaScript, un nodo en la
//! columna 0 (el usuario/actor, justo lo que el prompt pide) terminaba movido a la columna 5.
//! Acá solo se usa el valor por defecto cuando el número no es válido, no cuando es 0.

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Map, Value};

use crate::{
    error::{ApiError, ApiResult},
    llm,
    session::Session,
    state::AppState,
    texto,
    util::{exec, fetch_json, fetch_json_opt, fetch_json_raw, fetch_text_opt, new_id, numero, parse_float, presente, s, s_no_vacio, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/leads/hub-file", post(archivo_subir).delete(archivo_borrar).get(archivo_obtener))
        .route("/api/leads/hub-phase", get(fases_listar).put(fase_guardar))
        .route("/api/leads/architecture", get(arquitectura_obtener).put(arquitectura_guardar))
        .route("/api/leads/architecture/generate", post(arquitectura_generar))
        .route("/api/leads/diagram", get(diagrama_obtener).put(diagrama_guardar))
        .route("/api/leads/diagram/generate", post(diagrama_generar))
        .route("/api/leads/diagram/chat", post(diagrama_chat))
}

const MAX_ARCHIVO: usize = 5 * 1024 * 1024;
/// Mismo orden que PHASES en leads/[id]/hub/page.tsx.
const ORDEN_FASES: [&str; 7] =
    ["NEW", "CONTACTED", "DIAGNOSIS", "DEMO_VALIDATION", "PROPOSAL_SENT", "NEGOTIATION", "RESULT"];
const FASE_DIAGRAMA: &str = "COMPONENT_DIAGRAM";

fn no_autenticado(s: &Session) -> ApiResult<()> {
    if s.is_service || s.id.is_empty() {
        Err(ApiError::new(StatusCode::UNAUTHORIZED, "No autenticado"))
    } else {
        Ok(())
    }
}

fn nombre_de(s: &Session) -> String {
    if !s.name.is_empty() {
        s.name.clone()
    } else if !s.email.is_empty() {
        s.email.clone()
    } else {
        "unknown".into()
    }
}

fn idx_fase(f: &str) -> Option<usize> {
    ORDEN_FASES.iter().position(|x| *x == f)
}

// ── Archivos adjuntos del hub ───────────────────────────────────────────────────────────────
const ARCHIVO_RESUMEN: &str = r#"jsonb_build_object('id', f.id, 'name', f.name, 'size', f.size, 'mimeType', f."mimeType",
  'uploadedBy', f."uploadedBy", 'createdAt', f."createdAt")"#;

async fn archivo_subir(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let (Some(hub_id), Some(nombre), Some(mime), Some(base64)) =
        (s_no_vacio(&body, "hubId"), s(&body, "name"), s(&body, "mimeType"), s(&body, "base64"))
    else {
        return Err(ApiError::bad_request("hubId, name, mimeType y base64 son requeridos"));
    };

    // Misma regla que hub-phase PUT: se puede adjuntar en la fase activa o en una pasada; solo
    // se bloquea una fase FUTURA a la que el pipeline todavía no llegó.
    let hub = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('leadId', "leadId", 'phase', phase) FROM "LeadHub" WHERE id = $1"#,
        &[B::T(hub_id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Fase no encontrada"))?;
    let lead_estado = fetch_text_opt(
        &st.pool,
        r#"SELECT status::text FROM "Lead" WHERE id = $1"#,
        &[B::T(hub["leadId"].as_str().unwrap_or_default().to_string())],
    )
    .await?;
    let fase = hub["phase"].as_str().unwrap_or_default();
    let actual = lead_estado.as_deref().and_then(idx_fase);
    let de_fase = idx_fase(fase);
    match (actual, de_fase) {
        (Some(a), Some(f)) if f <= a => {}
        _ => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "No se puede adjuntar en \"{fase}\" — el pipeline todavía no llegó hasta ahí (fase activa: \"{}\").",
                    lead_estado.as_deref().unwrap_or("undefined")
                ),
            ))
        }
    }

    let size = (base64.len() * 3 + 2) / 4; // Math.round(len * 3 / 4)
    if size > MAX_ARCHIVO {
        return Err(ApiError::bad_request("Archivo muy grande (máx 5MB)"));
    }

    let sql = format!(
        r#"WITH ins AS (
             INSERT INTO "LeadHubFile" (id, "hubId", name, size, "mimeType", base64, "uploadedBy", "createdAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW()) RETURNING *)
           SELECT {ARCHIVO_RESUMEN} FROM ins f"#
    );
    let f = fetch_json(
        &st.pool,
        &sql,
        &[B::T(new_id()), B::T(hub_id), B::T(nombre), B::I(size as i64), B::T(mime), B::T(base64), B::T(nombre_de(&sesion))],
    )
    .await?;
    Ok(Json(f))
}

async fn archivo_borrar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let id = s_no_vacio(&body, "id").ok_or_else(|| ApiError::bad_request("id requerido"))?;
    let n = exec(&st.pool, r#"DELETE FROM "LeadHubFile" WHERE id = $1"#, &[B::T(id)]).await?;
    if n == 0 {
        return Err(ApiError::not_found("No encontrado"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn archivo_obtener(
    State(st): State<AppState>,
    sesion: Session,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let id = q.get("id").filter(|x| !x.is_empty()).ok_or_else(|| ApiError::bad_request("id requerido"))?;
    fetch_json_opt(&st.pool, r#"SELECT to_jsonb(f) FROM "LeadHubFile" f WHERE f.id = $1"#, &[B::T(id.clone())])
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No encontrado"))
}

// ── Fases del hub ───────────────────────────────────────────────────────────────────────────
const FASE_CON_ARCHIVOS: &str = r#"to_jsonb(h) || jsonb_build_object('files', COALESCE((
  SELECT jsonb_agg(jsonb_build_object('id', f.id, 'name', f.name, 'size', f.size, 'mimeType', f."mimeType",
         'uploadedBy', f."uploadedBy", 'createdAt', f."createdAt") ORDER BY f.ctid)
  FROM "LeadHubFile" f WHERE f."hubId" = h.id), '[]'::jsonb))"#;

async fn fases_listar(
    State(st): State<AppState>,
    sesion: Session,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let lead_id = q.get("leadId").filter(|x| !x.is_empty()).ok_or_else(|| ApiError::bad_request("leadId requerido"))?;
    let sql = format!(
        r#"SELECT COALESCE(jsonb_agg({FASE_CON_ARCHIVOS} ORDER BY h."createdAt" ASC), '[]'::jsonb)
           FROM "LeadHub" h WHERE h."leadId" = $1"#
    );
    Ok(Json(fetch_json(&st.pool, &sql, &[B::T(lead_id.clone())]).await?))
}

async fn fase_guardar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    no_autenticado(&sesion)?;
    let (Some(lead_id), Some(fase)) = (s_no_vacio(&body, "leadId"), s_no_vacio(&body, "phase")) else {
        return Err(ApiError::bad_request("leadId y phase son requeridos"));
    };

    // Defensa en profundidad: una fase futura (a la que el pipeline todavía no llegó) no se
    // puede escribir; una pasada sí. (El "contenido vacío" se valida solo en el cliente a
    // propósito: adjuntar un archivo crea la fase con contenido todavía vacío.)
    let estado = fetch_text_opt(&st.pool, r#"SELECT status::text FROM "Lead" WHERE id = $1"#, &[B::T(lead_id.clone())])
        .await?
        .ok_or_else(|| ApiError::not_found("Lead no encontrado"))?;
    match (idx_fase(&estado), idx_fase(&fase)) {
        (Some(a), Some(f)) if f <= a => {}
        _ => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("No se puede guardar contenido en \"{fase}\" — el pipeline todavía no llegó hasta ahí (fase activa: \"{estado}\")."),
            ))
        }
    }

    let sql = format!(
        r#"WITH up AS (
             INSERT INTO "LeadHub" (id, "leadId", phase, content, "updatedBy", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
             ON CONFLICT ("leadId", phase) DO UPDATE
               SET content = CASE WHEN $6::bool THEN EXCLUDED.content ELSE "LeadHub".content END,
                   "updatedBy" = EXCLUDED."updatedBy", "updatedAt" = NOW()
             RETURNING *)
           SELECT {FASE_CON_ARCHIVOS} FROM up h"#
    );
    let hub = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::T(lead_id),
            B::T(fase),
            B::OT(s(&body, "content")),
            B::T(nombre_de(&sesion)),
            B::Bo(presente(&body, "content")),
        ],
    )
    .await?;
    Ok(Json(hub))
}

// ── Arquitectura del lead ───────────────────────────────────────────────────────────────────
fn lead_id_query(q: &HashMap<String, String>) -> ApiResult<String> {
    q.get("leadId").filter(|x| !x.is_empty()).cloned().ok_or_else(|| ApiError::bad_request("leadId requerido"))
}

async fn arquitectura_obtener(
    State(st): State<AppState>,
    _s: Session,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let lead_id = lead_id_query(&q)?;
    let fila = fetch_json_raw(
        &st.pool,
        r#"SELECT jsonb_build_object('data', data) FROM "LeadArchitecture" WHERE "leadId" = $1"#,
        &[B::T(lead_id)],
    )
    .await?;
    Ok(Json(fila.unwrap_or_else(|| json!({ "data": null }))))
}

async fn arquitectura_guardar(State(st): State<AppState>, _s: Session, body: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let lead_id = s_no_vacio(&body, "leadId").ok_or_else(|| ApiError::bad_request("leadId requerido"))?;
    let data = match body.get("data") {
        None | Some(Value::Null) => json!({}),
        Some(d) => d.clone(),
    };
    let r = fetch_json_raw(
        &st.pool,
        r#"WITH up AS (
             INSERT INTO "LeadArchitecture" (id, "leadId", data, "createdAt", "updatedAt")
             VALUES ($1, $2, $3::jsonb, NOW(), NOW())
             ON CONFLICT ("leadId") DO UPDATE SET data = EXCLUDED.data, "updatedAt" = NOW()
             RETURNING id)
           SELECT jsonb_build_object('ok', true, 'id', id) FROM up"#,
        &[B::T(new_id()), B::T(lead_id), B::J(data)],
    )
    .await?;
    r.map(Json).ok_or_else(|| ApiError::internal("Error al guardar"))
}

// ── Diagrama del lead (se guarda como texto JSON en LeadHub, fase COMPONENT_DIAGRAM) ───────
async fn diagrama_obtener(
    State(st): State<AppState>,
    _s: Session,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let lead_id = lead_id_query(&q)?;
    let contenido = fetch_text_opt(
        &st.pool,
        r#"SELECT content FROM "LeadHub" WHERE "leadId" = $1 AND phase = $2"#,
        &[B::T(lead_id), B::T(FASE_DIAGRAMA.into())],
    )
    .await?;
    let data = contenido
        .filter(|c| !c.is_empty())
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .unwrap_or(Value::Null);
    Ok(Json(json!({ "data": data })))
}

async fn diagrama_guardar(State(st): State<AppState>, _s: Session, body: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let lead_id = s_no_vacio(&body, "leadId").ok_or_else(|| ApiError::bad_request("leadId requerido"))?;
    let data = match body.get("data") {
        None | Some(Value::Null) => json!({}),
        Some(d) => d.clone(),
    };
    exec(
        &st.pool,
        r#"INSERT INTO "LeadHub" (id, "leadId", phase, content, "createdAt", "updatedAt")
           VALUES ($1, $2, $3, $4, NOW(), NOW())
           ON CONFLICT ("leadId", phase) DO UPDATE SET content = EXCLUDED.content, "updatedAt" = NOW()"#,
        &[B::T(new_id()), B::T(lead_id), B::T(FASE_DIAGRAMA.into()), B::T(data.to_string())],
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ── Contexto compartido por las rutas de generación ─────────────────────────────────────────
struct ContextoLead {
    empresa: String,
    texto: String,
}

/// Arma el contexto del lead (empresa, solución, alcance, notas, fases) como lo hacen
/// architecture/generate y diagram/generate. `excluir_diagrama` saca la fase del diagrama.
async fn contexto_generacion(
    st: &AppState,
    lead_id: &str,
    excluir_diagrama: bool,
    max_fase: usize,
) -> ApiResult<ContextoLead> {
    let lead = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('companyName', "companyName", 'scope', scope, 'solucionAsociada', "solucionAsociada",
             'notes', notes, 'tipo', tipo) FROM "Lead" WHERE id = $1"#,
        &[B::T(lead_id.to_string())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Lead no encontrado"))?;

    let fases = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('phase', phase, 'content', content) ORDER BY "createdAt" ASC), '[]'::jsonb)
           FROM "LeadHub" WHERE "leadId" = $1"#,
        &[B::T(lead_id.to_string())],
    )
    .await?;

    let notas_fases = fases
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|p| {
                    p["content"].as_str().map(|c| !c.is_empty()).unwrap_or(false)
                        && !(excluir_diagrama && p["phase"].as_str() == Some(FASE_DIAGRAMA))
                })
                .map(|p| {
                    let crudo = p["content"].as_str().unwrap_or("");
                    let t = match serde_json::from_str::<Value>(crudo).ok().and_then(|v| v.get("tabs").cloned()) {
                        Some(Value::Array(tabs)) => tabs
                            .iter()
                            .map(|t| {
                                format!(
                                    "[{}] {}",
                                    t["name"].as_str().unwrap_or(""),
                                    texto::strip_html(t["content"].as_str().unwrap_or(""))
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" | "),
                        _ => texto::strip_html(crudo),
                    };
                    format!("{}: {}", p["phase"].as_str().unwrap_or(""), texto::truncar(&t, max_fase))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let sh = |k: &str, max: usize| texto::truncar(&texto::strip_html(lead[k].as_str().unwrap_or("")), max);
    let empresa = lead["companyName"].as_str().unwrap_or("").to_string();
    let mut lineas = vec![format!("Empresa: {empresa}")];
    if let Some(x) = lead["solucionAsociada"].as_str().filter(|x| !x.is_empty()) {
        lineas.push(format!("Solución: {x}"));
    }
    if let Some(x) = lead["tipo"].as_str().filter(|x| !x.is_empty()) {
        lineas.push(format!("Tipo: {x}"));
    }
    if lead["scope"].as_str().map(|x| !x.is_empty()).unwrap_or(false) {
        lineas.push(format!("Alcance: {}", sh("scope", 400)));
    }
    if lead["notes"].as_str().map(|x| !x.is_empty()).unwrap_or(false) {
        lineas.push(format!("Notas generales: {}", sh("notes", 400)));
    }
    if !notas_fases.is_empty() {
        lineas.push(format!("\nNotas de fases:\n{notas_fases}"));
    }
    Ok(ContextoLead { empresa, texto: lineas.join("\n") })
}

fn quitar_cercas(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```JSON")).or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.trim_start();
    let t = t.strip_suffix("```").unwrap_or(t);
    t.trim().to_string()
}

fn error_modelo(msg: String) -> ApiError {
    tracing::error!("[hub] {}", msg.chars().take(400).collect::<String>());
    ApiError::internal(msg)
}

fn objetos(v: &Value, k: &str) -> Vec<Map<String, Value>> {
    v.get(k)
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
        .unwrap_or_default()
}

fn aristas_validas(edges: Vec<Map<String, Value>>, ids: &HashSet<String>) -> Vec<Map<String, Value>> {
    edges
        .into_iter()
        .filter(|e| {
            e.get("from").and_then(|x| x.as_str()).map(|x| ids.contains(x)).unwrap_or(false)
                && e.get("to").and_then(|x| x.as_str()).map(|x| ids.contains(x)).unwrap_or(false)
        })
        .collect()
}

fn ids_de(nodes: &[Map<String, Value>]) -> HashSet<String> {
    nodes.iter().filter_map(|n| n.get("id").and_then(|i| i.as_str()).map(|i| i.to_string())).collect()
}

// ── Generación de arquitectura isométrica (IA) ─────────────────────────────────────────────
const SYSTEM_ARQUITECTURA: &str = r#"Sos un arquitecto de software experto en sistemas empresariales latinoamericanos.
Dado el contexto de un proyecto de software, generás un mapa de arquitectura isométrico en JSON.

REGLAS CRÍTICAS DEL GRID 9×9 (gridX y gridY de 0 a 8):
- MÁXIMO 7 NODOS — elegí solo los componentes clave del sistema, no todos los detalles
- NINGÚN nodo puede compartir posición (gridX, gridY) — posiciones únicas obligatorio
- Separación mínima: dos nodos no pueden estar en posiciones adyacentes (diferencia < 2 en ambos ejes)
- Los labels deben ser CORTOS: máximo 18 caracteres

LAYOUT OBLIGATORIO — respetar estas zonas:
  • Usuario / cliente externo → gridX: 0, gridY: 0
  • Frontend / app / portal  → gridX: 1, gridY: 3
  • API Gateway / BFF        → gridX: 3, gridY: 3
  • Servidor lógica negocio  → gridX: 5, gridY: 2  o  gridX: 6, gridY: 3
  • Base de datos principal  → gridX: 6, gridY: 6
  • Cola / broker / eventos  → gridX: 4, gridY: 7
  • Servicio externo         → gridX: 8, gridY: 1  o  gridX: 8, gridY: 4
  • Cache / memoria          → gridX: 7, gridY: 5
  (Usá esas posiciones exactas o cercanas, nunca agrupes varios nodos en la misma zona)

TIPOS DE NODOS: server | database | api | frontend | queue | cache | external | user
TIPOS DE CONEXIÓN: data (flujo de datos) | control (configuración/orquestación) | event (eventos asíncronos)

FORMATO DE SALIDA — devolvé ÚNICAMENTE el JSON, sin explicaciones ni markdown:
{
  "title": "...",
  "description": "...",
  "nodes": [
    { "id": "n1", "type": "user", "label": "...", "description": "...", "payload": "...", "gridX": 0, "gridY": 0 }
  ],
  "edges": [
    { "id": "e1", "from": "n1", "to": "n2", "label": "...", "type": "data" }
  ]
}"#;

async fn arquitectura_generar(State(st): State<AppState>, _s: Session, body: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let lead_id = s_no_vacio(&body, "leadId").ok_or_else(|| ApiError::bad_request("leadId requerido"))?;
    let ctx = contexto_generacion(&st, &lead_id, false, 600).await?;
    let prompt = format!(
        "Contexto del proyecto:\n{}\n\nGenerá el mapa de arquitectura del sistema que se necesita construir para este cliente.",
        ctx.texto
    );

    let salida = llm::call_open_code(&st, SYSTEM_ARQUITECTURA, &prompt, &format!("lead-arch-{lead_id}"), 4096, 150)
        .await
        .map_err(error_modelo)?;
    let mut data: Value = serde_json::from_str(&quitar_cercas(&salida)).map_err(|e| error_modelo(e.to_string()))?;

    let (mut nodes, mut edges) = (objetos(&data, "nodes"), objetos(&data, "edges"));
    if !data.get("nodes").map(|n| n.is_array()).unwrap_or(false) || !data.get("edges").map(|n| n.is_array()).unwrap_or(false) {
        return Err(error_modelo("JSON inválido: falta nodes o edges".into()));
    }

    if nodes.len() > 9 {
        nodes.truncate(9);
        edges = aristas_validas(edges, &ids_de(&nodes));
    }

    // Deduplica posiciones: si dos nodos comparten (gridX, gridY), el segundo se corre a la
    // celda libre más cercana (búsqueda en anillos).
    let mut usadas: HashSet<(i64, i64)> = HashSet::new();
    for (i, n) in nodes.iter_mut().enumerate() {
        if n.get("id").map(|x| x.is_null()).unwrap_or(true) {
            n.insert("id".into(), json!(format!("n{i}")));
        }
        let gx = (numero(n.get("gridX")) as i64).clamp(0, 8);
        let gy = (numero(n.get("gridY")) as i64).clamp(0, 8);
        'busqueda: for d in 0..=8i64 {
            for dx in -d..=d {
                for dy in -d..=d {
                    if dx.abs() != d && dy.abs() != d {
                        continue;
                    }
                    let (nx, ny) = (gx + dx, gy + dy);
                    if !(0..=8).contains(&nx) || !(0..=8).contains(&ny) {
                        continue;
                    }
                    if usadas.insert((nx, ny)) {
                        n.insert("gridX".into(), json!(nx));
                        n.insert("gridY".into(), json!(ny));
                        break 'busqueda;
                    }
                }
            }
        }
    }
    for (i, e) in edges.iter_mut().enumerate() {
        if e.get("id").map(|x| x.is_null()).unwrap_or(true) {
            e.insert("id".into(), json!(format!("e{i}")));
        }
    }
    data["nodes"] = Value::Array(nodes.into_iter().map(Value::Object).collect());
    data["edges"] = Value::Array(edges.into_iter().map(Value::Object).collect());
    Ok(Json(json!({ "data": data })))
}

// ── Generación del diagrama de componentes (IA) ────────────────────────────────────────────
const SYSTEM_DIAGRAMA: &str = r#"Sos un arquitecto de software experto en sistemas empresariales latinoamericanos.
Dado el contexto de un proyecto, generás un diagrama de ARQUITECTURA DE COMPONENTES en JSON.

El diagrama muestra los componentes tecnicos del sistema y como se conectan.
NO es un flujograma. NO muestra pasos ni flujos de proceso.
Es un diagrama de arquitectura como los de Azure, AWS o draw.io.

POSICIONAMIENTO — cuadricula HORIZONTAL con coordenadas ENTERAS x (0-11) e y (0-5):
  El eje X representa las capas de izquierda a derecha:
    x=0-1:  Usuarios / clientes / navegadores / actores externos
    x=2-3:  Frontend — apps web, portales, dashboards, apps moviles
    x=4-5:  API / Gateway / BFF / autenticacion
    x=6-7:  Backend — servicios, microservicios, logica de negocio
    x=8-9:  Datos — bases de datos, cache, colas, almacenamiento
    x=10-11: Servicios externos / cloud / integraciones de terceros

  El eje Y distribuye verticalmente los componentes dentro de cada columna.
  Ejemplo: si hay 2 servicios backend, uno va en x=6,y=1 y otro en x=6,y=3.
  Centrar los nodos verticalmente: con 1 nodo en una columna usá y=2, con 2 usá y=1 e y=3.

REGLAS:
- Minimo 5 nodos, maximo 12 nodos — solo los componentes clave del stack
- x debe ser ENTERO de 0 a 11, y debe ser ENTERO de 0 a 5 — sin decimales
- Dos nodos no pueden compartir la misma celda (x,y) — separalos siempre
- label: nombre corto del componente, maximo 20 caracteres
- description: tecnologia o rol brevísimo, maximo 30 caracteres (opcional)
- type: uno de estos valores exactos según el rol del componente:
    user | frontend | api | backend | database | queue | external
- Las conexiones representan que dos componentes se comunican o dependen entre si
- NO pongas labels en las conexiones

FORMATO DE SALIDA — devolvé UNICAMENTE el JSON, sin explicaciones ni markdown:
{
  "title": "...",
  "description": "...",
  "nodes": [
    { "id": "n1", "label": "Usuario", "description": "Navegador web", "type": "user", "x": 0, "y": 2 },
    { "id": "n2", "label": "Portal Web", "description": "React / Next.js", "x": 2, "y": 2 },
    { "id": "n3", "label": "API Gateway", "description": "Express / REST", "x": 4, "y": 2 },
    { "id": "n4", "label": "Servicio Core", "description": "Node.js", "x": 6, "y": 1 },
    { "id": "n5", "label": "Notificaciones", "description": "Worker", "x": 6, "y": 3 },
    { "id": "n6", "label": "PostgreSQL", "description": "Base de datos", "x": 8, "y": 2 },
    { "id": "n7", "label": "WhatsApp API", "description": "Integración", "x": 10, "y": 2 }
  ],
  "edges": [
    { "id": "e1", "from": "n1", "to": "n2" },
    { "id": "e2", "from": "n2", "to": "n3" },
    { "id": "e3", "from": "n3", "to": "n4" },
    { "id": "e4", "from": "n3", "to": "n5" }
  ]
}"#;

/// `parseFloat(String(v)) || def`, pero respetando el 0 (ver nota del módulo).
fn coordenada(v: Option<&Value>, def: f64) -> f64 {
    let texto = match v {
        Some(Value::String(t)) => Some(t.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    match texto {
        Some(t) => {
            let n = parse_float(Some(&Value::String(t.clone())));
            if n == 0.0 && !t.trim_start().starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+' || c == '.') {
                def
            } else {
                n
            }
        }
        None => def,
    }
}

async fn diagrama_generar(State(st): State<AppState>, _s: Session, body: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let lead_id = s_no_vacio(&body, "leadId").ok_or_else(|| ApiError::bad_request("leadId requerido"))?;
    let ctx = contexto_generacion(&st, &lead_id, true, 600).await?;
    let prompt = format!(
        "Contexto del proyecto:\n{}\n\nGenerá el diagrama de arquitectura de componentes del sistema a construir para este cliente. Usá posicionamiento 2D libre para que se vea como una arquitectura real, no como un flujograma.",
        ctx.texto
    );

    let salida = llm::call_open_code(&st, SYSTEM_DIAGRAMA, &prompt, &format!("lead-diagram-{lead_id}"), 4096, 150)
        .await
        .map_err(error_modelo)?;
    let mut data: Value = serde_json::from_str(&quitar_cercas(&salida)).map_err(|e| error_modelo(e.to_string()))?;
    if !data.get("nodes").map(|n| n.is_array()).unwrap_or(false) || !data.get("edges").map(|n| n.is_array()).unwrap_or(false) {
        return Err(error_modelo("JSON inválido: falta nodes o edges".into()));
    }
    let (mut nodes, mut edges) = (objetos(&data, "nodes"), objetos(&data, "edges"));

    if nodes.len() > 12 {
        nodes.truncate(12);
        edges = aristas_validas(edges, &ids_de(&nodes));
    }

    // Normaliza posiciones a la grilla entera y resuelve colisiones.
    let mut usadas: HashSet<(i64, i64)> = HashSet::new();
    for (i, n) in nodes.iter_mut().enumerate() {
        if n.get("id").map(|x| x.is_null()).unwrap_or(true) {
            n.insert("id".into(), json!(format!("n{i}")));
        }
        let mut x = (coordenada(n.get("x"), 5.0).round() as i64).clamp(0, 11);
        let mut y = (coordenada(n.get("y"), ((i % 3) * 2) as f64).round() as i64).clamp(0, 5);
        let mut intentos = 0;
        while usadas.contains(&(x, y)) && intentos < 30 {
            y += 1;
            if y > 5 {
                y = 0;
                x = (x + 1).min(11);
            }
            intentos += 1;
        }
        usadas.insert((x, y));
        n.insert("x".into(), json!(x));
        n.insert("y".into(), json!(y));
    }
    // Las conexiones se reducen a id/from/to (sin etiquetas).
    let edges: Vec<Value> = edges
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let id = e.get("id").filter(|x| !x.is_null()).cloned().unwrap_or_else(|| json!(format!("e{i}")));
            json!({ "id": id, "from": e.get("from"), "to": e.get("to") })
        })
        .collect();
    data["nodes"] = Value::Array(nodes.into_iter().map(Value::Object).collect());
    data["edges"] = Value::Array(edges);
    Ok(Json(json!({ "data": data })))
}

// ── Chat para debatir el diagrama (IA) ─────────────────────────────────────────────────────
const TIPOS_NODO: [&str; 7] = ["user", "frontend", "api", "backend", "database", "queue", "external"];
const MAX_NODOS: usize = 12;
const COLS: i64 = 12;
const ROWS: i64 = 6;

const SYSTEM_CHAT: &str = r#"Sos un arquitecto de software experto en sistemas empresariales latinoamericanos. Estás debatiendo con el usuario un DIAGRAMA DE ARQUITECTURA DE COMPONENTES (no un flujograma) de un cliente.

Recibís el diagrama actual (componentes y conexiones), el contexto del cliente y el historial de la conversación. Tu trabajo: conversar de forma concreta y útil — cuestionar decisiones, señalar riesgos (puntos únicos de falla, seguridad, escalabilidad, acoplamiento, lo que falta o sobra), y proponer mejoras.

REGLAS DE LA CONVERSACIÓN:
- Respondé corto: 2 a 5 oraciones, sin listas largas. Una idea principal por respuesta.
- Basate en el diagrama y el contexto reales; no inventes componentes ni requisitos que no se desprendan de ellos.
- Solo incluí una "propuesta" cuando el usuario pide un cambio, o cuando hay una mejora clara y concreta. Si solo estás opinando o preguntando, "propuesta" es null.
- Si el usuario tiene un componente seleccionado, centrate en ese.

CÓMO PROPONER CAMBIOS (campo "propuesta"):
- Componentes existentes se referencian por su "id" exacto.
- Componentes nuevos: usá un id temporal propio ("nuevo1", "nuevo2"…) y usá ese mismo id en las conexiones nuevas.
- Cuadrícula: x entero 0-11, y entero 0-5. Columnas: x=0-1 usuarios/actores, 2-3 frontend, 4-5 API/gateway, 6-7 backend, 8-9 datos/colas, 10-11 servicios externos. No pongas dos nodos en la misma celda.
- label: máximo 20 caracteres. description: máximo 30 caracteres, opcional. type exacto: user | frontend | api | backend | database | queue | external.
- El diagrama completo no puede pasar de 12 componentes.
- Las conexiones no llevan etiqueta.
- Cada cambio de "propuesta" debe ser mínimo y justificado: no rehagas el diagrama entero.

FORMATO DE SALIDA — devolvé ÚNICAMENTE un objeto JSON, sin markdown ni texto alrededor:
{
  "mensaje": "tu respuesta al usuario",
  "propuesta": null
}
o, cuando proponés cambios:
{
  "mensaje": "tu respuesta al usuario",
  "propuesta": {
    "descripcion": "resumen de una línea del cambio",
    "agregarNodos": [ { "id": "nuevo1", "label": "Cola de mensajes", "description": "RabbitMQ", "type": "queue", "x": 8, "y": 4 } ],
    "modificarNodos": [ { "id": "<id existente>", "label": "...", "description": "...", "type": "...", "x": 6, "y": 2 } ],
    "quitarNodos": [ "<id existente>" ],
    "agregarConexiones": [ { "from": "<id>", "to": "nuevo1" } ],
    "quitarConexiones": [ { "from": "<id>", "to": "<id>" } ]
  }
}
(Los arrays de "propuesta" que no uses van vacíos: [].)"#;

pub fn extraer_json(text: &str) -> Option<Value> {
    let t = quitar_cercas(text);
    if let Ok(v) = serde_json::from_str::<Value>(&t) {
        return Some(v);
    }
    let chars: Vec<char> = t.chars().collect();
    let ini = chars.iter().position(|c| *c == '{')?;
    let mut prof = 0i32;
    for i in ini..chars.len() {
        match chars[i] {
            '{' => prof += 1,
            '}' => {
                prof -= 1;
                if prof == 0 {
                    let trozo: String = chars[ini..=i].iter().collect();
                    return serde_json::from_str(&trozo).ok();
                }
            }
            _ => {}
        }
    }
    None
}

fn entero(v: Option<&Value>, min: i64, max: i64, def: i64) -> i64 {
    let n = match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(t)) => t.trim().parse::<f64>().ok(),
        _ => None,
    };
    match n {
        Some(x) if x.is_finite() => (x.round() as i64).clamp(min, max),
        _ => def,
    }
}

fn txt(v: Option<&Value>, max: usize) -> String {
    match v {
        Some(Value::String(t)) => texto::truncar(t.trim(), max),
        _ => String::new(),
    }
}

fn misma_arista(a: (&str, &str), b: (&str, &str)) -> bool {
    (a.0 == b.0 && a.1 == b.1) || (a.0 == b.1 && a.1 == b.0)
}

/// Normaliza y valida la propuesta del modelo contra el diagrama real: ids que existan, celdas
/// libres, sin duplicados, tope de nodos. `None` si no queda ningún cambio válido.
fn normalizar_propuesta(raw: &Value, nodos: &[Value], aristas: &[Value]) -> Value {
    let Some(p) = raw.as_object() else { return Value::Null };
    let arr = |k: &str| -> Vec<Value> {
        p.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter(|x| x.is_object()).cloned().collect()).unwrap_or_default()
    };
    let existentes: HashSet<String> =
        nodos.iter().filter_map(|n| n["id"].as_str().map(|s| s.to_string())).collect();

    let quitar_nodos: Vec<String> = p
        .get("quitarNodos")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).filter(|id| existentes.contains(*id)).map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let quitar: HashSet<&str> = quitar_nodos.iter().map(|s| s.as_str()).collect();

    let modificar: Vec<Value> = arr("modificarNodos")
        .into_iter()
        .filter(|m| m["id"].as_str().map(|id| existentes.contains(id) && !quitar.contains(id)).unwrap_or(false))
        .map(|m| {
            let mut o = Map::new();
            o.insert("id".into(), m["id"].clone());
            let l = txt(m.get("label"), 20);
            if !l.is_empty() {
                o.insert("label".into(), json!(l));
            }
            if m.get("description").is_some() {
                o.insert("description".into(), json!(txt(m.get("description"), 30)));
            }
            if let Some(t) = m["type"].as_str().filter(|t| TIPOS_NODO.contains(t)) {
                o.insert("type".into(), json!(t));
            }
            if m.get("x").is_some() && m.get("y").is_some() {
                o.insert("x".into(), json!(entero(m.get("x"), 0, COLS - 1, 0)));
                o.insert("y".into(), json!(entero(m.get("y"), 0, ROWS - 1, 0)));
            }
            Value::Object(o)
        })
        .filter(|o| o.as_object().map(|m| m.len() > 1).unwrap_or(false))
        .collect();

    // celdas ocupadas tras quitar nodos
    let mut ocupadas: HashSet<(i64, i64)> = nodos
        .iter()
        .filter(|n| !n["id"].as_str().map(|i| quitar.contains(i)).unwrap_or(false))
        .map(|n| (n["x"].as_f64().unwrap_or(0.0).round() as i64, n["y"].as_f64().unwrap_or(0.0).round() as i64))
        .collect();
    let mut libre = |x: i64, y: i64| -> (i64, i64) {
        let (mut cx, mut cy, mut t) = (x, y, 0);
        while ocupadas.contains(&(cx, cy)) && t < 80 {
            cy += 1;
            if cy >= ROWS {
                cy = 0;
                cx = (COLS - 1).min(cx + 1);
            }
            t += 1;
        }
        ocupadas.insert((cx, cy));
        (cx, cy)
    };

    let cupo = MAX_NODOS.saturating_sub(nodos.len().saturating_sub(quitar_nodos.len()));
    let ahora = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let mut id_temporal: HashMap<String, String> = HashMap::new();
    let mut agregar: Vec<Value> = Vec::new();
    for (i, n) in arr("agregarNodos").into_iter().take(cupo).enumerate() {
        let real = format!("n{ahora}-{i}");
        if let Some(t) = n["id"].as_str() {
            id_temporal.insert(t.to_string(), real.clone());
        }
        let (x, y) = libre(entero(n.get("x"), 0, COLS - 1, 5), entero(n.get("y"), 0, ROWS - 1, 2));
        let label = txt(n.get("label"), 20);
        let desc = txt(n.get("description"), 30);
        let mut o = Map::new();
        o.insert("id".into(), json!(real));
        o.insert("label".into(), json!(if label.is_empty() { "Nuevo componente".to_string() } else { label }));
        if !desc.is_empty() {
            o.insert("description".into(), json!(desc));
        }
        if let Some(t) = n["type"].as_str().filter(|t| TIPOS_NODO.contains(t)) {
            o.insert("type".into(), json!(t));
        }
        o.insert("x".into(), json!(x));
        o.insert("y".into(), json!(y));
        agregar.push(Value::Object(o));
    }

    let resolver = |id: &Value| -> Option<String> {
        let id = id.as_str()?;
        if let Some(r) = id_temporal.get(id) {
            return Some(r.clone());
        }
        (existentes.contains(id) && !quitar.contains(id)).then(|| id.to_string())
    };

    let mut agregar_con: Vec<(String, String)> = Vec::new();
    for c in arr("agregarConexiones") {
        let (Some(from), Some(to)) = (resolver(&c["from"]), resolver(&c["to"])) else { continue };
        if from == to {
            continue;
        }
        let ya_en_diagrama = aristas
            .iter()
            .any(|e| misma_arista((e["from"].as_str().unwrap_or(""), e["to"].as_str().unwrap_or("")), (&from, &to)));
        let ya_en_nuevas = agregar_con.iter().any(|(a, b)| misma_arista((a, b), (&from, &to)));
        if ya_en_diagrama || ya_en_nuevas {
            continue;
        }
        agregar_con.push((from, to));
    }

    let quitar_con: Vec<Value> = arr("quitarConexiones")
        .into_iter()
        .filter_map(|c| Some((c["from"].as_str()?.to_string(), c["to"].as_str()?.to_string())))
        .filter(|(f, t)| {
            aristas.iter().any(|e| misma_arista((e["from"].as_str().unwrap_or(""), e["to"].as_str().unwrap_or("")), (f, t)))
        })
        .map(|(f, t)| json!({ "from": f, "to": t }))
        .collect();

    if agregar.is_empty() && modificar.is_empty() && quitar_nodos.is_empty() && agregar_con.is_empty() && quitar_con.is_empty() {
        return Value::Null;
    }
    let desc = txt(p.get("descripcion"), 160);
    json!({
        "descripcion": if desc.is_empty() { "Cambios propuestos al diagrama".to_string() } else { desc },
        "agregarNodos": agregar,
        "modificarNodos": modificar,
        "quitarNodos": quitar_nodos,
        "agregarConexiones": agregar_con.into_iter().map(|(f, t)| json!({ "from": f, "to": t })).collect::<Vec<_>>(),
        "quitarConexiones": quitar_con,
    })
}

async fn diagrama_chat(State(st): State<AppState>, _s: Session, body: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let lead_id = s_no_vacio(&body, "leadId").ok_or_else(|| ApiError::bad_request("leadId requerido"))?;
    let mensaje = s(&body, "mensaje").map(|m| m.trim().to_string()).unwrap_or_default();
    if mensaje.is_empty() {
        return Err(ApiError::bad_request("mensaje requerido"));
    }

    let diagrama = body.get("diagram").cloned().unwrap_or_else(|| json!({}));
    let nodos: Vec<Value> = diagrama["nodes"]
        .as_array()
        .map(|a| a.iter().filter(|n| n["id"].is_string()).cloned().collect())
        .unwrap_or_default();
    let aristas: Vec<Value> = diagrama["edges"]
        .as_array()
        .map(|a| a.iter().filter(|e| e["from"].is_string() && e["to"].is_string()).cloned().collect())
        .unwrap_or_default();

    let ctx = contexto_generacion(&st, &lead_id, true, 500).await?;
    let por_id: HashMap<String, &Value> =
        nodos.iter().filter_map(|n| n["id"].as_str().map(|i| (i.to_string(), n))).collect();
    let etiqueta = |id: &str| -> String {
        por_id.get(id).and_then(|n| n["label"].as_str()).map(|l| l.to_string()).unwrap_or_else(|| id.to_string())
    };

    let mut diag_txt = vec![
        format!("Título: {}", diagrama["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(sin título)")),
        format!("Componentes ({}):", nodos.len()),
    ];
    for n in &nodos {
        diag_txt.push(format!(
            "- id={} | {} | tipo={} | {} | celda({},{})",
            n["id"].as_str().unwrap_or(""),
            n["label"].as_str().unwrap_or(""),
            n["type"].as_str().unwrap_or("sin tipo"),
            n["description"].as_str().unwrap_or(""),
            n["x"].as_f64().unwrap_or(0.0).round() as i64,
            n["y"].as_f64().unwrap_or(0.0).round() as i64,
        ));
    }
    diag_txt.push(format!("Conexiones ({}):", aristas.len()));
    for e in &aristas {
        let (f, t) = (e["from"].as_str().unwrap_or(""), e["to"].as_str().unwrap_or(""));
        diag_txt.push(format!("- {} ({f}) — {} ({t})", etiqueta(f), etiqueta(t)));
    }
    let sel = body
        .get("seleccionado")
        .and_then(|x| x.as_str())
        .filter(|id| por_id.contains_key(*id))
        .map(|id| format!("\nComponente seleccionado por el usuario: {} (id={id})", etiqueta(id)))
        .unwrap_or_default();

    // Historial en un solo mensaje de usuario (callOpenCode es de un turno).
    let previo = body
        .get("historial")
        .and_then(|h| h.as_array())
        .map(|h| {
            let validos: Vec<&Value> = h
                .iter()
                .filter(|m| matches!(m["role"].as_str(), Some("user") | Some("assistant")) && m["content"].is_string())
                .collect();
            let desde = validos.len().saturating_sub(10);
            validos[desde..]
                .iter()
                .map(|m| {
                    format!(
                        "{}: {}",
                        if m["role"].as_str() == Some("user") { "Usuario" } else { "Vos" },
                        texto::truncar(m["content"].as_str().unwrap_or(""), 1200)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let user = format!(
        "CONTEXTO DEL CLIENTE:\n{}\n\nDIAGRAMA ACTUAL:\n{}{}\n\n{}MENSAJE ACTUAL DEL USUARIO:\n{}",
        ctx.texto,
        diag_txt.join("\n"),
        sel,
        if previo.is_empty() { String::new() } else { format!("CONVERSACIÓN PREVIA:\n{previo}\n\n") },
        mensaje
    );

    let salida = llm::call_open_code(&st, SYSTEM_CHAT, &user, &format!("lead-diagram-chat-{lead_id}"), 3000, 150)
        .await
        .map_err(error_modelo)?;
    let j = extraer_json(&salida);
    // Si el modelo no devolvió JSON, se muestra como texto plano sin propuesta.
    let msg = match &j {
        Some(v) if v["mensaje"].as_str().map(|m| !m.trim().is_empty()).unwrap_or(false) => {
            v["mensaje"].as_str().unwrap_or("").trim().to_string()
        }
        Some(_) => String::new(),
        None => salida.trim().to_string(),
    };
    if msg.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "La IA no devolvió una respuesta utilizable. Probá de nuevo."));
    }
    let propuesta = j.as_ref().map(|v| normalizar_propuesta(&v["propuesta"], &nodos, &aristas)).unwrap_or(Value::Null);
    Ok(Json(json!({ "mensaje": msg, "propuesta": propuesta })))
}
