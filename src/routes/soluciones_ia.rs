//! "Generar con IA" de una Solución: borrador de PRD, entrevista por sección (PRD / Diseño técnico /
//! Plan de ejecución) y diagrama de arquitectura. Puerto de `src/app/api/soluciones/[id]/{prd-generate,
//! prd-seccion-chat,arquitectura-generate}/route.ts`. Ninguna guarda nada: devuelven la propuesta para que
//! la persona la revise.

use std::sync::LazyLock;

use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use rand::Rng;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::{
    error::{ApiError, ApiResult},
    llm,
    routes::proyectos::num_js,
    session::Opcional,
    state::AppState,
    util::{fetch_json, fetch_json_opt, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/soluciones/{id}/prd-generate", post(prd_generar))
        .route("/api/soluciones/{id}/prd-seccion-chat", post(prd_seccion_chat))
        .route("/api/soluciones/{id}/arquitectura-generate", post(arquitectura_generar))
}

fn autenticado(o: &Opcional) -> Result<(), ApiError> {
    if o.0.is_some() {
        Ok(())
    } else {
        Err(ApiError::unauthorized_msg("No autenticado"))
    }
}

fn bad_gateway(msg: &str) -> ApiError {
    ApiError::new(axum::http::StatusCode::BAD_GATEWAY, msg)
}

// ── Utilidades de texto ──────────────────────────────────────────────────────────────────────
fn re(p: &str) -> Regex {
    Regex::new(p).expect("regex")
}

static R_BR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<br\s*/?>"));
static R_P: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</p>"));
static R_LI_FIN: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</li>"));
static R_LI: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<li[^>]*>"));
static R_TAG: LazyLock<Regex> = LazyLock::new(|| re(r"<[^>]+>"));
static R_SALTOS3: LazyLock<Regex> = LazyLock::new(|| re(r"\n{3,}"));
static R_BLOQUE_FIN: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</(p|h[1-6]|div|li)>"));
static R_ESP: LazyLock<Regex> = LazyLock::new(|| re(r"[ \t]+"));
static R_SALTOS2: LazyLock<Regex> = LazyLock::new(|| re(r"\n{2,}"));

fn entidades(t: String) -> String {
    t.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
}

/// `stripHtml` de prd-generate / prd-seccion-chat.
pub(crate) fn strip_html(html: &str) -> String {
    let t = R_BR.replace_all(html, "\n").to_string();
    let t = R_P.replace_all(&t, "\n").to_string();
    let t = R_LI_FIN.replace_all(&t, "\n").to_string();
    let t = R_LI.replace_all(&t, "• ").to_string();
    let t = R_TAG.replace_all(&t, "").to_string();
    let t = entidades(t);
    R_SALTOS3.replace_all(&t, "\n\n").trim().to_string()
}

fn parse_hub_content(raw: Option<&str>) -> String {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else { return String::new() };
    if let Ok(p) = serde_json::from_str::<Value>(raw) {
        if let Some(tabs) = p.get("tabs").and_then(|t| t.as_array()) {
            return tabs
                .iter()
                .filter(|t| t["content"].as_str().map(|c| !c.is_empty() && c != "<p></p>").unwrap_or(false))
                .map(|t| format!("[{}]\n{}", t["name"].as_str().unwrap_or("undefined"), strip_html(t["content"].as_str().unwrap_or(""))))
                .collect::<Vec<_>>()
                .join("\n\n");
        }
    }
    strip_html(raw)
}

pub(crate) fn truncar(texto: &str, max: usize) -> String {
    if texto.chars().count() <= max {
        texto.to_string()
    } else {
        format!("{}\n[...contenido truncado por longitud...]", texto.chars().take(max).collect::<String>())
    }
}

/// `extractJsonObject`: el texto como JSON, o el primer objeto balanceado.
pub(crate) fn extraer_objeto(texto: &str) -> Option<Value> {
    static INI: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^```(?:json)?\s*"));
    static FIN: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)```\s*$"));
    let t = texto.trim();
    let t = INI.replace(t, "").to_string();
    let t = FIN.replace(&t, "").to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&t) {
        return Some(v);
    }
    let ini = t.find('{')?;
    let mut d = 0i32;
    for (i, c) in t[ini..].char_indices() {
        match c {
            '{' => d += 1,
            '}' => {
                d -= 1;
                if d == 0 {
                    return serde_json::from_str(&t[ini..ini + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

fn vacio(v: &Value) -> bool {
    v.as_str().map(|s| s.trim().is_empty()).unwrap_or(true)
}

fn txt(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().filter(|x| !x.is_empty()).map(String::from)
}

// ── Contexto compartido por el PRD ───────────────────────────────────────────────────────────
struct BaseContexto {
    sol: Value,
    fases: String,
    riesgos: String,
    hitos: String,
    lead: Option<Value>,
    hub: String,
}

async fn base_contexto(st: &AppState, id: &str) -> Result<Option<BaseContexto>, sqlx::Error> {
    let Some(d) = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'sol', to_jsonb(so),
             'riesgos', COALESCE((SELECT jsonb_agg(to_jsonb(r)) FROM "Riesgo" r WHERE r."solucionId" = so.id), '[]'::jsonb),
             'hitos', COALESCE((SELECT jsonb_agg(to_jsonb(h)) FROM "Hito" h WHERE h."solucionId" = so.id), '[]'::jsonb),
             'lead', (SELECT to_jsonb(l) FROM "Lead" l WHERE l.id = so."leadId"),
             'hub', COALESCE((SELECT jsonb_agg(h.content ORDER BY h.phase ASC) FROM "LeadHub" h WHERE h."leadId" = so."leadId"), '[]'::jsonb))
           FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id.into())],
    )
    .await?
    else {
        return Ok(None);
    };
    let sol = d["sol"].clone();
    let mut fases = String::new();
    if let Some(f) = sol["cronograma"].as_str().and_then(|c| serde_json::from_str::<Value>(c).ok()).and_then(|v| v.as_array().cloned()) {
        let cad = |x: &Value, k: &str, def: &str| x[k].as_str().filter(|s| !s.is_empty()).unwrap_or(def).to_string();
        fases = f.iter().map(|x| format!("- {} ({}): {} → {}", cad(x, "fase", "Sin nombre"), cad(x, "estado", "PENDIENTE"), cad(x, "fechaInicio", "?"), cad(x, "fechaFin", "?"))).collect::<Vec<_>>().join("\n");
    }
    let riesgos = d["riesgos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| format!("- [{}/{}] {}{}", r["severidad"].as_str().unwrap_or(""), r["probabilidad"].as_str().unwrap_or(""), r["titulo"].as_str().unwrap_or(""), txt(r, "descripcion").map(|x| format!(": {x}")).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    let hitos = d["hitos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|h| format!("- {} ({}){}", h["titulo"].as_str().unwrap_or(""), h["estado"].as_str().unwrap_or(""), h["fechaComprometida"].as_str().map(|f| format!(" — comprometido: {}", f.chars().take(10).collect::<String>())).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    let hub = d["hub"].as_array().cloned().unwrap_or_default().iter().map(|c| parse_hub_content(c.as_str())).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n");
    let lead = Some(d["lead"].clone()).filter(|l| !l.is_null());
    Ok(Some(BaseContexto { sol, fases, riesgos, hitos, lead, hub }))
}

// ═══════════════════════════════ PRD: BORRADOR ═══════════════════════════════
fn secciones_por_tipo(tipo: &str) -> Vec<&'static str> {
    match tipo {
        "DEMO" => vec!["resumenEjecutivo", "problema", "objetivoGeneral", "objetivosEspecificos", "dentroDeAlcance", "fueraDeAlcance", "requisitos"],
        "INTERN" => vec!["resumenEjecutivo", "problema", "objetivoGeneral", "objetivosEspecificos", "dentroDeAlcance", "fueraDeAlcance", "personas", "requisitos", "supuestos", "preguntasAbiertas"],
        _ => vec!["resumenEjecutivo", "problema", "objetivoGeneral", "objetivosEspecificos", "dentroDeAlcance", "fueraDeAlcance", "personas", "requisitos", "requisitosNoFuncionales", "metricas", "riesgos", "dependencias", "supuestos", "preguntasAbiertas"],
    }
}

async fn prd_generar(State(st): State<AppState>, o: Opcional, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(b) = base_contexto(&st, &id).await? else { return Err(ApiError::not_found("No encontrado")) };
    let sol = &b.sol;
    let secciones = secciones_por_tipo(sol["tipo"].as_str().unwrap_or(""));
    let g = |k: &str| txt(sol, k);
    // Lo que la propuesta comercial le prometió al cliente es el alcance vendido: tiene que entrar al PRD.
    let propuestas_txt = match sol["leadId"].as_str() {
        Some(l) => {
            let v = fetch_json(
                &st.pool,
                r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('title', p.title, 'description', p.description, 'amount', p.amount, 'status', p.status::text)
                        ORDER BY (p.status::text = 'ACCEPTED') DESC, p."createdAt" DESC), '[]'::jsonb) FROM "Proposal" p WHERE p."leadId" = $1"#,
                &[B::T(l.to_string())],
            )
            .await?;
            v.as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|p| format!("[{}] {} — {}\n{}", p["status"].as_str().unwrap_or(""), p["title"].as_str().unwrap_or(""), p["amount"], strip_html(p["description"].as_str().unwrap_or(""))))
                .collect::<Vec<_>>()
                .join("\n\n")
        }
        None => String::new(),
    };
    // Si ya hay diseño técnico, el PRD no puede contradecirlo.
    let diseno_txt = {
        let d: Value = sol["disenoTecnico"].as_str().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
        let nombres = |k: &str, c: &str| d[k].as_array().map(|a| a.iter().filter_map(|x| x[c].as_str()).filter(|x| !x.is_empty()).take(30).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        let (e, stk, i) = (nombres("entidades", "nombre"), nombres("stack", "tecnologia"), nombres("integraciones", "sistema"));
        if e.is_empty() && stk.is_empty() && i.is_empty() { String::new() } else { format!("Entidades: {e}\nStack: {stk}\nIntegraciones: {i}") }
    };
    let contexto = [
        Some(format!("Nombre: {}", sol["nombre"].as_str().unwrap_or(""))),
        g("descripcion").map(|d| format!("Descripción: {d}")),
        Some(format!("Tipo de Solución: {}", sol["tipo"].as_str().unwrap_or(""))),
        g("planTrabajo").map(|p| format!("Plan de trabajo:\n{}", truncar(&p, 4000))),
        Some(b.fases.clone()).filter(|f| !f.is_empty()).map(|f| format!("Cronograma (fases ya definidas):\n{f}")),
        Some(b.riesgos.clone()).filter(|f| !f.is_empty()).map(|f| format!("Riesgos ya identificados:\n{f}")),
        Some(b.hitos.clone()).filter(|f| !f.is_empty()).map(|f| format!("Hitos de cumplimiento:\n{f}")),
        b.lead.as_ref().map(|l| format!("Cliente/Lead asociado: {} (contacto: {})", l["companyName"].as_str().unwrap_or(""), l["contactName"].as_str().unwrap_or(""))),
        Some(propuestas_txt.clone()).filter(|f| !f.is_empty()).map(|f| format!("Propuestas comerciales (lo prometido al cliente; la ACCEPTED es el alcance vendido):\n{}", truncar(&f, 6000))),
        Some(b.hub.clone()).filter(|f| !f.is_empty()).map(|f| format!("Notas del proceso de preventa (Lead Hub):\n{}", truncar(&f, 12000))),
        Some(diseno_txt.clone()).filter(|f| !f.is_empty()).map(|f| format!("Diseño técnico ya definido (el PRD no debe contradecirlo):\n{f}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n\n");
    let tiene_contexto = g("descripcion").map(|d| !d.trim().is_empty()).unwrap_or(false)
        || g("planTrabajo").map(|d| !d.trim().is_empty()).unwrap_or(false)
        || !b.fases.is_empty()
        || !b.riesgos.is_empty()
        || !b.hitos.is_empty()
        || !b.hub.is_empty()
        || !propuestas_txt.is_empty();
    let con_requisitos = secciones.contains(&"requisitos");
    let secciones_base: Vec<&str> = secciones.iter().copied().filter(|x| *x != "requisitos").collect();

    let system = format!(
        r#"Sos un analista de producto que redacta borradores de PRD (Product Requirements Document) para ArchiTechIA, una consultora de IA. El PRD debe ser un documento robusto y desglosado — cada sección es una lista de ítems concretos, nunca un párrafo genérico de relleno.
Con el contexto que te den, generá un borrador en español, conciso y concreto.
Devolvé SOLO un objeto JSON (sin markdown, sin texto alrededor) con esta forma exacta:
{{
  "resumenEjecutivo": "string (2-3 líneas)",
  "problema": "string",
  "objetivoGeneral": "string (una frase)",
  "objetivosEspecificos": [ {{ "texto": "string" }} ],
  "dentroDeAlcance": [ {{ "texto": "string" }} ],
  "fueraDeAlcance": [ {{ "texto": "string" }} ],
  "personas": [ {{ "rol": "string", "necesidad": "string" }} ],
  "requisitos": [ {{ "tipo": "historia" | "caso_uso", "texto": "string", "criterioAceptacion": "string", "prioridad": "MUST" | "SHOULD" | "COULD" | "WONT" }} ],
  "requisitosNoFuncionales": [ {{ "categoria": "performance" | "seguridad" | "compatibilidad" | "escalabilidad" | "otro", "texto": "string" }} ],
  "metricas": [ {{ "nombre": "string", "meta": "string", "comoSeMide": "string" }} ],
  "riesgos": [ {{ "texto": "string" }} ],
  "dependencias": [ {{ "texto": "string" }} ],
  "supuestos": [ {{ "texto": "string" }} ],
  "preguntasAbiertas": [ {{ "texto": "string" }} ]
}}
Incluí SOLO estas claves (array vacío [] o string vacío "" para las que no apliquen): {}.
Este es un documento REAL para uso profesional, no un resumen — priorizá profundidad y volumen de contenido sobre brevedad en TODAS las secciones:
- "resumenEjecutivo": 4-6 líneas, no 2.
- "problema": 2-3 párrafos completos (separados por \n\n), con contexto, evidencia o señales concretas, y consecuencia de no resolverlo — no una sola oración genérica.
- "objetivoGeneral": una frase, pero específica y verificable.
- "objetivosEspecificos": 4-6 ítems, cada uno medible.
- "dentroDeAlcance" y "fueraDeAlcance": 4-6 ítems cada uno.
- "personas": 3-4 personas distintas.
- Los requisitos funcionales NO los redactes aquí: se generan aparte (omite la clave "requisitos").
- "requisitosNoFuncionales": 4-6 ítems, al menos uno de cada categoría relevante (performance, seguridad, compatibilidad, escalabilidad).
- "metricas": 3-5 KPIs, cada uno con meta numérica concreta cuando sea posible (porcentaje, tiempo, cantidad) y cómo se mide en términos operativos reales.
- "riesgos" y "dependencias": 3-5 ítems cada uno.
- "supuestos" y "preguntasAbiertas": 3-5 ítems cada uno.
No dejes una sección corta o vacía por pereza si el contexto da para llenarla — es preferible un documento largo y específico a uno breve y genérico.
Si el contexto es escaso, hacé tu mejor inferencia razonable a partir del nombre y tipo de Solución, pero no inventes detalles muy específicos (nombres de personas, cifras exactas) que no estén en el contexto.
Si hay una propuesta comercial aceptada, el "dentroDeAlcance" tiene que reflejar fielmente lo que ella promete, y lo que la propuesta no ofrece va en "fueraDeAlcance"."#,
        secciones_base.join(", ")
    );
    let usuario = format!("Generá el borrador de PRD con este contexto:\n\n{}", if contexto.is_empty() { "(sin contexto adicional — solo el nombre y tipo de Solución)" } else { &contexto });
    let system_req = r#"Eres un analista de producto de ArchiTechIA, una consultora de IA. Redactas los REQUISITOS FUNCIONALES de un PRD a partir del contexto del proyecto.
Devuelve SOLO un objeto JSON (sin markdown ni texto alrededor): { "requisitos": [ { "tipo": "historia" | "caso_uso", "texto": "string", "criterioAceptacion": "string", "prioridad": "MUST" | "SHOULD" | "COULD" | "WONT", "fuente": "propuesta" | "preventa" | "inferido" } ] }
- "fuente": "propuesta" si lo promete la propuesta comercial; "preventa" si sale de las notas del cliente; "inferido" si lo deduces tú porque el sistema lo necesita aunque nadie lo pidió expresamente. No presentes como pedido del cliente lo que inferiste.
- Cantidad: entre 12 y 25 requisitos según el tamaño del alcance (un sistema de gestión mediano suele necesitar entre 18 y 25). Mejor muchos requisitos precisos que pocos genéricos.
- Cada criterio de aceptación es concreto y verificable (nada como "funciona bien"). Las prioridades están repartidas de verdad: como referencia un tercio MUST, un tercio SHOULD y el resto entre COULD y WONT.
- COBERTURA OBLIGATORIA, cuando el contexto lo justifique: cada ítem del alcance vendido; cada rol o persona; el ciclo de vida completo del dato central (crear, consultar, editar, dar de baja); permisos y roles; validaciones y manejo de errores; cada integración externa y qué pasa si no responde; reportes o visibilidad para quien administra; notificaciones; configuración básica.
- No inventes funcionalidades que el contexto contradiga ni cifras que no estén en él."#;
    let usuario_req = format!("Redacta los requisitos funcionales de este proyecto con este contexto:\n\n{}", if contexto.is_empty() { "(sin contexto adicional — solo el nombre y tipo de Solución)" } else { &contexto });
    let sesion_req = format!("prd-req-{id}");
    let sesion_prd = format!("prd-{id}");
    let (borrador, requisitos) = tokio::join!(llamar_json(&st, &system, &usuario, &sesion_prd, 8192), async {
        if con_requisitos {
            Some(llamar_json(&st, system_req, &usuario_req, &sesion_req, 7168).await)
        } else {
            None
        }
    });
    let mut prd = borrador?;
    let mut avisos: Vec<String> = vec![];
    match requisitos {
        Some(Ok(v)) => match v.get("requisitos").filter(|r| r.is_array()).cloned() {
            Some(r) => prd["requisitos"] = r,
            None => avisos.push("La IA no devolvió requisitos: genera esa sección aparte.".into()),
        },
        Some(Err(_)) => avisos.push("No se pudieron generar los requisitos funcionales: genera esa sección aparte.".into()),
        None => {}
    }
    Ok(Json(json!({ "prd": prd, "tieneContexto": tiene_contexto, "avisos": avisos })))
}

/// Una llamada al modelo que debe devolver un objeto JSON; si no es JSON válido reintenta una vez (con menos tiempo,
/// para no pasar los 300 s del proxy).
async fn llamar_json(st: &AppState, system: &str, usuario: &str, sesion: &str, max_tokens: u32) -> Result<Value, ApiError> {
    for intento in 0..2 {
        let u = if intento == 0 { usuario.to_string() } else { format!("{usuario}\n\nIMPORTANTE: tu respuesta anterior no era un JSON válido. Devuelve SOLO un objeto JSON válido, sin texto antes ni después.") };
        let salida = match llm::call_open_code(st, system, &u, sesion, max_tokens, if intento == 0 { 140 } else { 100 }).await {
            Ok(x) => x,
            Err(e) => {
                tracing::error!("prd-generate: {e}");
                if intento == 1 {
                    return Err(bad_gateway("El modelo no respondió correctamente."));
                }
                continue;
            }
        };
        if let Some(v) = extraer_objeto(&salida).filter(|v| v.is_object()) {
            return Ok(v);
        }
    }
    Err(bad_gateway("No se pudo interpretar la respuesta del modelo."))
}

// ═══════════════════════════════ PRD: ENTREVISTA POR SECCIÓN ═══════════════════════════════
fn seccion_info(k: &str) -> Option<(&'static str, String)> {
    let requisitos = r#"un array de ENTRE 8 Y 15 objetos (nunca menos de 8, ni siquiera en el primer intento — usá más de 12 si hace falta para cubrir todo, ver abajo) {"tipo": "historia" | "caso_uso", "texto": string, "criterioAceptacion": string (concreto y verificable, nunca algo vago como "funciona bien"), "prioridad": "MUST" | "SHOULD" | "COULD" | "WONT"}.

COBERTURA OBLIGATORIA — no es solo el flujo principal feliz, tienen que quedar representados, cuando el contexto lo justifique:
- Cada ítem de "Dentro de alcance" ya definido en el PRD (si te lo pasaron en el contexto) — ninguno puede quedar sin al menos un requisito.
- Cada rol/persona ya definido en el PRD — al menos un requisito pensado para su necesidad específica.
- El ciclo de vida completo del dato o proceso central: creación, consulta/listado, edición y baja/cancelación (no solo "crear").
- Permisos y control de acceso (quién puede hacer qué), si la Solución distingue roles.
- Validaciones y manejo de errores: qué pasa si un dato es inválido, si falla una integración externa, si hay datos duplicados o inconsistentes.
- Integraciones externas mencionadas en el contexto (APIs, otros sistemas) y qué pasa si no responden.
- Reportes, métricas o visibilidad para quien administra o supervisa (si aplica al tipo de Solución).
- Notificaciones o alertas relevantes al flujo (si aplica).
- Configuración/administración básica que el negocio necesitaría (si aplica).

No inventes secciones de alcance que no existan — cubrí SOLO lo que el contexto sugiere que aplica a esta Solución específica, pero cubrilo de verdad en vez de limitarte al caso feliz de un solo flujo.

Las prioridades tienen que estar REALMENTE repartidas entre las 4 opciones: JAMÁS pongas "MUST" en todos los ítems — como referencia, algo como un tercio MUST, un tercio SHOULD, y el resto entre COULD y WONT es una distribución realista."#;
    let (label, schema): (&'static str, &str) = match k {
        "resumen" => ("Resumen ejecutivo", "un string de 4-6 líneas"),
        "problema" => ("Problema / contexto", r#"un string de 2-3 párrafos completos, separados por "\n\n""#),
        "objetivoGeneral" => ("Objetivo general", "un string de una sola frase, específica y verificable"),
        "objetivosEspecificos" => ("Objetivos específicos", "un array de 4-6 strings, cada uno un objetivo medible"),
        "dentroDeAlcance" => ("Dentro de alcance", "un array de 4-6 strings: qué SÍ entra en esta versión"),
        "fueraDeAlcance" => ("Fuera de alcance", "un array de 4-6 strings: qué explícitamente NO entra"),
        "personas" => ("Usuarios / personas", r#"un array de 3-4 objetos {"rol": string, "necesidad": string}"#),
        "requisitos" => ("Requisitos funcionales", requisitos),
        "rnf" => ("Requisitos no funcionales", r#"un array de 4-6 objetos {"categoria": "performance" | "seguridad" | "compatibilidad" | "escalabilidad" | "otro", "texto": string}"#),
        "metricas" => ("Métricas de éxito (KPIs)", r#"un array de 3-5 objetos {"nombre": string, "meta": string (con valor numérico concreto cuando aplique), "comoSeMide": string}"#),
        "riesgos" => ("Riesgos", "un array de 3-5 strings"),
        "dependencias" => ("Dependencias", "un array de 3-5 strings"),
        "supuestos" => ("Supuestos", "un array de 3-5 strings"),
        "preguntasAbiertas" => ("Preguntas abiertas", "un array de 3-5 strings"),
        "dt_arquitectura" => ("Arquitectura general", "un string de 2-4 párrafos: componentes principales, cómo se comunican entre sí y por qué se estructuran así (sin repetir el PRD: esto es el CÓMO, no el qué)"),
        "dt_modelo" => ("Modelo de datos (entidades clave)", r#"un array de 4-10 objetos {"nombre": string, "atributos": string (lista separada por comas de los atributos principales), "relaciones": string (con qué otras entidades se relaciona y cardinalidad)}, derivados de los requisitos funcionales del PRD — cada entidad tiene que existir porque algún requisito la necesita"#),
        "dt_stack" => ("Stack tecnológico", r#"un array de 4-8 objetos {"capa": string (ej. Frontend, Backend, Base de datos, IA, Infraestructura, Observabilidad), "tecnologia": string, "justificacion": string (razón concreta ligada a un requisito o restricción del contexto, no marketing)}"#),
        "dt_integraciones" => ("Integraciones externas", r#"un array de 1-8 objetos {"sistema": string, "proposito": string, "detalle": string (protocolo, autenticación, formato de datos), "siFalla": string (comportamiento definido si el sistema externo no responde o devuelve error)} — solo las que el PRD/contexto realmente implican, no inventes integraciones"#),
        "dt_decisiones" => ("Decisiones técnicas clave", r#"un array de 3-8 objetos {"decision": string, "alternativas": string (qué otras opciones reales se evaluaron), "justificacion": string (por qué esta y no las otras, con el trade-off asumido)}"#),
        "dt_seguridad" => ("Consideraciones de seguridad", "un string de 2-4 párrafos: autenticación y autorización, manejo de datos sensibles/personales, cifrado, cumplimiento normativo aplicable y amenazas relevantes para ESTA solución"),
        "dt_escalabilidad" => ("Escalabilidad y rendimiento", "un string de 2-4 párrafos: carga esperada, cuellos de botella previstos, cómo crece el sistema, y qué se monitorea"),
        "pe_qa" => ("Plan de pruebas / QA", "un string de 3-5 párrafos: niveles de prueba (unitarias, integración, extremo a extremo), qué se automatiza y qué es manual, cómo se verifican los criterios de aceptación y los requisitos no funcionales del PRD, cómo es el UAT con el cliente (quién, cuándo) y el criterio de salida para considerar algo listo para producción"),
        "pe_ambientes" => ("Ambientes y despliegue", r#"un array de 2-4 objetos {"ambiente": string (ej. Desarrollo, Staging, Producción), "proposito": string, "despliega": string (quién y cómo despliega en ese ambiente), "promocion": string (condición concreta para promover al siguiente ambiente)}"#),
        "pe_release" => ("Estrategia de release y rollback", "un string de 2-3 párrafos: cómo se libera a producción (ventanas, por etapas o feature flags), qué se monitorea al liberar, y cuándo y cómo se revierte (criterios de rollback)"),
        "pe_raci" => ("Equipo y roles (matriz RACI)", r#"un array de 6-12 objetos {"actividad": string, "responsable": string, "aprueba": string, "consultado": string, "informado": string} — usá ROLES (ej. Líder técnico, Product owner, Cliente sponsor, Agente ejecutor), no inventes nombres propios de personas; cubrí desde aprobar el PRD y el diseño hasta despliegue, QA y cierre"#),
        "pe_cambios" => ("Gestión de cambios", "un string de 2-3 párrafos: cómo se solicita, evalúa (impacto en alcance, costo y plazo), aprueba y registra un cambio de alcance; quién decide; y cómo se refleja en el PRD, el diseño técnico y el cronograma"),
        "pe_comunicacion" => ("Comunicación con el cliente", r#"un array de 3-6 objetos {"que": string (qué se comunica), "audiencia": string, "frecuencia": string, "canal": string, "responsable": string}"#),
        _ => return None,
    };
    Some((label, schema.to_string()))
}

#[derive(PartialEq, Clone, Copy)]
enum Tipo {
    Texto,
    Lista,
    Personas,
    Requisitos,
    Rnf,
    Metricas,
    Item,
    DtTabla,
}

fn tipo_seccion(k: &str) -> Option<Tipo> {
    Some(match k {
        "resumen" | "problema" | "objetivoGeneral" | "dt_arquitectura" | "dt_seguridad" | "dt_escalabilidad" | "pe_qa" | "pe_release" | "pe_cambios" => Tipo::Texto,
        "objetivosEspecificos" | "dentroDeAlcance" | "fueraDeAlcance" | "riesgos" | "dependencias" | "supuestos" | "preguntasAbiertas" => Tipo::Lista,
        "personas" => Tipo::Personas,
        "requisitos" => Tipo::Requisitos,
        "rnf" => Tipo::Rnf,
        "metricas" => Tipo::Metricas,
        "requisito_item" => Tipo::Item,
        "dt_modelo" | "dt_stack" | "dt_integraciones" | "dt_decisiones" | "pe_ambientes" | "pe_raci" | "pe_comunicacion" => Tipo::DtTabla,
        _ => return None,
    })
}

fn dt_requeridos(k: &str) -> &'static [&'static str] {
    match k {
        "dt_modelo" => &["nombre", "atributos"],
        "dt_stack" => &["capa", "tecnologia", "justificacion"],
        "dt_integraciones" => &["sistema", "proposito", "siFalla"],
        "dt_decisiones" => &["decision", "alternativas", "justificacion"],
        "pe_ambientes" => &["ambiente", "proposito", "promocion"],
        "pe_raci" => &["actividad", "responsable", "aprueba"],
        "pe_comunicacion" => &["que", "audiencia", "frecuencia", "responsable"],
        _ => &[],
    }
}

/// `None` si el valor tiene la forma esperada; si no, el problema (se muestra a la persona).
fn validar_valor(k: &str, valor: &Value, laxo: bool) -> Option<String> {
    let texto_ok = |v: &Value| v.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false);
    match tipo_seccion(k) {
        Some(Tipo::Texto) | Some(Tipo::Item) => {
            if !texto_ok(valor) {
                return Some(if tipo_seccion(k) == Some(Tipo::Item) { "no devolvió el contenido revisado para este campo del requisito.".into() } else { "no devolvió texto para esta sección.".into() });
            }
            None
        }
        Some(Tipo::Lista) => {
            let a = valor.as_array().filter(|a| !a.is_empty());
            let Some(a) = a else { return Some("no devolvió ninguna lista de ítems.".into()) };
            if a.iter().any(|v| !texto_ok(v)) {
                return Some("devolvió algún ítem vacío en la lista.".into());
            }
            None
        }
        Some(Tipo::Personas) => {
            let a = valor.as_array().filter(|a| !a.is_empty());
            let Some(a) = a else { return Some("no devolvió ninguna persona.".into()) };
            if a.iter().any(|p| !texto_ok(&p["rol"]) || !texto_ok(&p["necesidad"])) {
                return Some("devolvió alguna persona con rol o necesidad vacíos.".into());
            }
            None
        }
        Some(Tipo::Requisitos) => {
            let Some(a) = valor.as_array() else { return Some("no devolvió una lista de requisitos.".into()) };
            if !laxo && a.len() < 8 {
                return Some(format!("devolvió solo {} requisito(s); se esperaban al menos 8.", a.len()));
            }
            if laxo && a.is_empty() {
                return Some("no devolvió ningún requisito.".into());
            }
            for r in a {
                if !texto_ok(&r["texto"]) || !texto_ok(&r["criterioAceptacion"]) {
                    return Some("devolvió algún requisito sin texto o sin criterio de aceptación.".into());
                }
                if !matches!(r["prioridad"].as_str(), Some("MUST" | "SHOULD" | "COULD" | "WONT")) {
                    return Some("devolvió algún requisito con una prioridad inválida.".into());
                }
            }
            let usadas: std::collections::HashSet<String> = a.iter().map(|r| r["prioridad"].to_string()).collect();
            if !laxo && usadas.len() < 2 {
                return Some("devolvió todos los requisitos con la misma prioridad (sin variedad MoSCoW).".into());
            }
            None
        }
        Some(Tipo::Rnf) => {
            let a = valor.as_array().filter(|a| !a.is_empty());
            let Some(a) = a else { return Some("no devolvió requisitos no funcionales.".into()) };
            if a.iter().any(|r| !texto_ok(&r["texto"])) {
                return Some("devolvió algún requisito no funcional sin texto.".into());
            }
            None
        }
        Some(Tipo::Metricas) => {
            let a = valor.as_array().filter(|a| !a.is_empty());
            let Some(a) = a else { return Some("no devolvió métricas.".into()) };
            if a.iter().any(|m| !texto_ok(&m["nombre"])) {
                return Some("devolvió alguna métrica sin nombre.".into());
            }
            None
        }
        Some(Tipo::DtTabla) => {
            let a = valor.as_array().filter(|a| !a.is_empty());
            let Some(a) = a else { return Some("no devolvió ningún ítem para esta sección.".into()) };
            for it in a {
                if let Some(f) = dt_requeridos(k).iter().find(|c| !texto_ok(&it[**c])) {
                    return Some(format!("devolvió algún ítem sin el campo \"{f}\"."));
                }
            }
            None
        }
        None => None,
    }
}

/// `asStringArray` de Next: cada elemento como `String(x)`, sin vacíos.
fn como_cadena_js(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Array(a) => a.iter().map(como_cadena_js).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        otro => otro.to_string(),
    }
}

fn lista_cadenas(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(como_cadena_js).filter(|s| !s.trim().is_empty()).collect()).unwrap_or_default()
}

fn bullets(titulo: &str, l: &[String]) -> Option<String> {
    if l.is_empty() {
        None
    } else {
        Some(format!("{titulo}:\n{}", l.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n")))
    }
}

fn js_json(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

async fn prd_seccion_chat(State(st): State<AppState>, o: Opcional, Path(id): Path<String>, cuerpo: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let seccion_key = match &body["seccionKey"] {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        otro => otro.to_string(),
    };
    let es_debate_item = seccion_key == "requisito_item";
    let es_mejora = body["modo"] == "mejorar" && !es_debate_item;
    let campo_criterio = body["campo"] == "criterioAceptacion";
    let doc_label = if seccion_key.starts_with("pe_") {
        "un Plan de Ejecución (cómo se organiza el equipo para ejecutar: pruebas, despliegue, roles, cambios y comunicación; complementa al PRD y al Diseño Técnico)"
    } else if seccion_key.starts_with("dt_") {
        "un Documento de Diseño Técnico (el CÓMO se construye la solución; complementa al PRD, que define el qué)"
    } else {
        "un PRD (Product Requirements Document)"
    };
    let campo_label = if campo_criterio { "el criterio de aceptación" } else { "la historia de usuario / caso de uso" };
    let info: (String, String) = if es_debate_item {
        (format!("Requisito funcional (debate de {campo_label})"), format!("un string: {campo_label} revisado"))
    } else {
        match seccion_info(&seccion_key) {
            Some((l, s)) => (l.to_string(), s),
            None => return Err(ApiError::bad_request("Sección desconocida.")),
        }
    };
    let historial_entrada: Vec<Value> = body["historial"]
        .as_array()
        .map(|a| a.iter().filter(|m| matches!(m["role"].as_str(), Some("user" | "assistant")) && m["content"].is_string()).cloned().collect())
        .unwrap_or_default();
    let historial: Vec<Value> = historial_entrada[historial_entrada.len().saturating_sub(20)..].iter().map(|m| json!({ "role": m["role"], "content": m["content"] })).collect();

    let Some(b) = base_contexto(&st, &id).await? else { return Err(ApiError::not_found("No encontrado")) };
    let sol = &b.sol;
    let cp = &body["contextoPrd"];
    let alcance = lista_cadenas(&cp["dentroDeAlcance"]);
    let objetivos = lista_cadenas(&cp["objetivosEspecificos"]);
    let personas = lista_cadenas(&cp["personas"]);
    let requisitos = lista_cadenas(&cp["requisitos"]);
    let rnf = lista_cadenas(&cp["requisitosNoFuncionales"]);
    let stack = lista_cadenas(&cp["stack"]);
    let integraciones = lista_cadenas(&cp["integraciones"]);
    let es_diseno = seccion_key.starts_with("dt_");
    let es_plan = seccion_key.starts_with("pe_");
    let es_req = seccion_key == "requisitos";
    let g = |k: &str| txt(sol, k);
    let contexto = [
        Some(format!("Nombre: {}", sol["nombre"].as_str().unwrap_or(""))),
        g("descripcion").map(|d| format!("Descripción: {d}")),
        Some(format!("Tipo de Solución: {}", sol["tipo"].as_str().unwrap_or(""))),
        g("planTrabajo").map(|p| format!("Plan de trabajo:\n{}", truncar(&p, 3000))),
        Some(b.fases.clone()).filter(|f| !f.is_empty()).map(|f| format!("Cronograma (fases ya definidas):\n{f}")),
        Some(b.riesgos.clone()).filter(|f| !f.is_empty()).map(|f| format!("Riesgos ya identificados:\n{f}")),
        Some(b.hitos.clone()).filter(|f| !f.is_empty()).map(|f| format!("Hitos de cumplimiento:\n{f}")),
        b.lead.as_ref().map(|l| format!("Cliente/Lead asociado: {} (contacto: {})", l["companyName"].as_str().unwrap_or(""), l["contactName"].as_str().unwrap_or(""))),
        Some(b.hub.clone()).filter(|f| !f.is_empty()).map(|f| format!("Notas del proceso de preventa (Lead Hub):\n{}", truncar(&f, 4000))),
        if es_req { bullets("Dentro de alcance ya definido en este PRD (CADA UNO de estos ítems tiene que quedar cubierto por al menos un requisito)", &alcance) } else { None },
        if es_req { bullets("Objetivos específicos ya definidos en este PRD", &objetivos) } else { None },
        if es_req { bullets("Personas/usuarios ya definidos en este PRD (cada rol relevante tiene que tener al menos un requisito pensado para su necesidad)", &personas) } else { None },
        if es_diseno || es_plan { bullets("Requisitos funcionales del PRD (el diseño técnico tiene que poder sostener CADA UNO; no inventes funcionalidad que no esté acá)", &requisitos) } else { None },
        if es_diseno || es_plan { bullets("Requisitos no funcionales del PRD (restricciones que el diseño tiene que cumplir)", &rnf) } else { None },
        if es_diseno || es_plan { bullets("Dentro de alcance del PRD", &alcance) } else { None },
        if es_plan { bullets("Stack tecnológico definido en el Diseño Técnico", &stack) } else { None },
        if es_plan { bullets("Integraciones externas definidas en el Diseño Técnico (el plan de pruebas y despliegue tiene que contemplarlas)", &integraciones) } else { None },
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n\n");
    let ctx = if contexto.is_empty() { "(sin contexto adicional — solo el nombre y tipo de Solución)".to_string() } else { contexto };
    let valor_actual = js_json(if body["valorActual"].is_null() { &Value::Null } else { &body["valorActual"] });

    let system = if es_debate_item {
        let texto_actual = if !body["contextoRequisito"]["texto"].is_null() { body["contextoRequisito"]["texto"].clone() } else if !campo_criterio { body["valorActual"].clone() } else { Value::Null };
        let criterio_actual = if !body["contextoRequisito"]["criterioAceptacion"].is_null() { body["contextoRequisito"]["criterioAceptacion"].clone() } else if campo_criterio { body["valorActual"].clone() } else { Value::Null };
        format!(
            r#"Sos un analista de producto senior que DEBATE y refina, junto con quien lo escribió, {campo_label} de UN requisito funcional que YA EXISTE en un PRD (Product Requirements Document) de ArchiTechIA — esto no es una entrevista para generar algo desde cero, el requisito ya está escrito y tu rol es cuestionarlo, señalar huecos concretos, y proponer mejoras puntuales.

IMPORTANTE: tu trabajo es EXCLUSIVAMENTE sobre {campo_label}. El otro campo del requisito NO es tuyo para modificar — se muestra abajo solo como contexto, para que tu propuesta sea coherente con el resto del requisito.

Historia de usuario / caso de uso (texto) actual:
{}

Criterio de aceptación actual:
{}

Contexto conocido de la Solución:
{ctx}

Reglas del debate:
1. Cada turno tuyo es UNA sola idea, siempre sobre {campo_label}: o señalás una debilidad concreta (ambigüedad, caso no cubierto{}) y proponés una mejora puntual, o hacés una pregunta puntual si necesitás más info del usuario para refinarlo. Nunca una lista de varias objeciones juntas, y nunca propongas cambios al otro campo.
2. Generá el contenido final revisado (solo {campo_label}) cuando el usuario esté de acuerdo con una mejora, pida aplicar los cambios, o diga que ya está bien como está (en ese último caso, devolvé el mismo contenido, sin inventar cambios).
3. Nunca hagas más de 3 intervenciones de debate antes de ofrecer una versión final (aunque el usuario no esté de acuerdo con nada, en la 3ra ofrecé tu mejor version igual).
4. Cada vez que uses tipo "pregunta" (tu turno de debate, sea objeción o pregunta), proponé también EXACTAMENTE 5 respuestas/posturas posibles para que el usuario elija con un clic, ademas de poder escribir la propia.
5. Devolvé SIEMPRE y SOLO un objeto JSON (sin markdown, sin texto alrededor), con una de estas dos formas EXACTAS:
   - Para seguir debatiendo: {{"tipo": "pregunta", "mensaje": "string", "opciones": ["string", "string", "string", "string", "string"]}}
   - Para la versión final: {{"tipo": "contenido", "valor": "string"}}  (SOLO {campo_label}, un string plano, no un objeto)"#,
            js_json(&texto_actual),
            js_json(&criterio_actual),
            if campo_criterio { ", algo poco medible/verificable" } else { "" }
        )
    } else if es_mejora {
        let forma = if es_req { r#"un array de objetos {"tipo": "historia" | "caso_uso", "texto": string, "criterioAceptacion": string, "prioridad": "MUST" | "SHOULD" | "COULD" | "WONT"}"#.to_string() } else { info.1.clone() };
        format!(
            r#"Sos un editor senior que MEJORA una sola sección de {doc_label} para ArchiTechIA, siguiendo lo que la persona te diga.

Sección: "{}"
Forma del valor (respetá la estructura; IGNORÁ los rangos de cantidad de ítems o de extensión que aparezcan acá, porque la cantidad la define el pedido de la persona): {forma}

Contexto conocido de la Solución:
{ctx}

Contenido ACTUAL de la sección (es lo que hay que corregir):
{valor_actual}

La persona te va a decir qué está mal: que no se entiende, que algo se puede omitir, que falta detalle, que el tono no sirve, que es muy largo, etc.

Reglas:
1. Aplicá EXACTAMENTE lo que pide sobre el contenido actual. Conservá todo lo que no critica: no reescribas la sección entera si solo pidió tocar una parte.
2. No inventes datos, cifras ni nombres que no estén en el contenido actual o en el contexto. Si para aplicar el pedido hace falta un dato que no tenés, hacé UNA pregunta concreta en vez de inventarlo.
3. Si el pedido es claro, devolvé directamente el contenido corregido (sin preguntar). Preguntá solo si el pedido es ambiguo o necesitás un dato que falta, y nunca más de una pregunta seguida.
4. Si pide omitir algo, sacalo de verdad. Si pide más detalle, agregalo solo con lo que se desprende del contexto.
5. El campo "cambios" describe en 1 o 2 frases cortas qué modificaste, para que la persona lo verifique.
6. Devolvé SIEMPRE y SOLO un objeto JSON (sin markdown, sin texto alrededor), con una de estas dos formas EXACTAS:
   - Contenido corregido: {{"tipo": "contenido", "valor": <el valor con la misma forma indicada arriba>, "cambios": "string"}}
   - Solo si hace falta aclarar: {{"tipo": "pregunta", "mensaje": "string", "opciones": ["string", "string", "string", "string", "string"]}}  (exactamente 5 opciones)"#,
            info.0
        )
    } else {
        format!(
            r#"Sos un analista de producto experto que ayuda a completar UNA sola sección de {doc_label} para ArchiTechIA, una consultora de IA, mediante una breve entrevista conversacional — no generás de una sola vez sin preguntar si falta información clave y específica que nadie más podría inferir.

Sección a trabajar: "{}"
El valor final que vas a generar debe ser: {}

Contexto conocido de la Solución:
{ctx}

Contenido que ya existe en esta sección (puede estar vacío — si no está vacío, tu trabajo es completarlo o mejorarlo, no ignorarlo ni repetir lo mismo):
{valor_actual}

Reglas de la entrevista:
1. OBLIGATORIO: en el primer turno (cuando todavía no hay ninguna respuesta del usuario en la conversación) SIEMPRE tenés que hacer una pregunta — nunca generes el contenido final en el primer turno, ni siquiera si el contexto ya te parece suficiente. El objetivo de esta función es que haya una entrevista real, no un atajo directo a generar.
2. Cada pregunta debe ser UNA sola, concreta y breve (nunca una lista de preguntas en el mismo turno), y tiene que buscar un dato específico que el contexto NO responde y que cambiaría el contenido (no preguntes algo ya respondido en el contexto o antes en esta conversación).
3. A partir de la segunda respuesta del usuario en adelante, y hasta un máximo de 3 preguntas en total, podés generar el contenido final si ya tenés lo suficiente. Generá también si el usuario pide generar ya, dice que no sabe / no tiene esa información, o ya le hiciste 3 preguntas — haciendo tu mejor inferencia razonable para lo que falte (sin inventar cifras o nombres muy específicos que no estén en el contexto).
4. Cada vez que preguntes, proponé también EXACTAMENTE 5 respuestas posibles distintas entre sí, concretas y directamente utilizables tal cual (no genéricas como "otra opción"), para que la persona pueda elegir una con un clic en vez de escribir. El usuario siempre puede además escribir su propia respuesta libremente, así que las 5 opciones son sugerencias, no las únicas respuestas válidas.
5. Devolvé SIEMPRE y SOLO un objeto JSON (sin markdown, sin texto alrededor), con una de estas dos formas EXACTAS:
   - Para preguntar: {{"tipo": "pregunta", "mensaje": "string", "opciones": ["string", "string", "string", "string", "string"]}}  (exactamente 5 opciones)
   - Para el contenido final: {{"tipo": "contenido", "valor": <el valor con la forma indicada arriba>}}"#,
            info.0, info.1
        )
    };

    let mensajes: Vec<Value> = if historial.is_empty() {
        vec![json!({ "role": "user", "content": if es_debate_item { format!("Empezá el debate: dame tu primera observación u objeción sobre {campo_label} de este requisito.") } else { "Empezá la entrevista: hacé tu primera pregunta.".to_string() } })]
    } else {
        historial
    };
    let item_id = body["itemId"].as_str().filter(|x| !x.is_empty());
    let sesion = format!(
        "prd-seccion-{id}-{seccion_key}{}{}{}",
        item_id.map(|i| format!("-{i}")).unwrap_or_default(),
        if es_debate_item { if campo_criterio { "-criterioAceptacion" } else { "-texto" } } else { "" },
        if es_mejora { "-mejorar" } else { "" }
    );
    let max_tokens = if es_req { 7168 } else { 4096 };
    let salida = match llm::call_open_code_messages(&st, &system, &mensajes, &sesion, max_tokens, 170).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("prd-seccion-chat: {e}");
            return Err(bad_gateway("El modelo no respondió correctamente."));
        }
    };
    let parsed = extraer_objeto(&salida).filter(|p| matches!(p["tipo"].as_str(), Some("pregunta" | "contenido")));
    let Some(parsed) = parsed else { return Err(bad_gateway("No se pudo interpretar la respuesta del modelo.")) };
    if parsed["tipo"] == "contenido" {
        if let Some(problema) = validar_valor(&seccion_key, &parsed["valor"], es_mejora) {
            tracing::error!("prd-seccion-chat: contenido invalido {seccion_key} {problema}");
            return Err(bad_gateway(&format!("La IA generó contenido incompleto ({problema}). Probá de nuevo.")));
        }
    }
    Ok(Json(parsed))
}

// ═══════════════════════════════ ARQUITECTURA ═══════════════════════════════
const TIPOS_NODO: [&str; 8] = ["frontend", "backend", "database", "api", "ia", "queue", "cache", "externo"];
const PASO_X: i64 = 180;
const PASO_Y: i64 = 100;

const SYSTEM_ARQ: &str = r#"Eres un arquitecto de software experto en sistemas empresariales. Recibes TODO el contexto disponible de un proyecto (PRD, diseño técnico, plan de ejecución, notas comerciales, backlog, riesgos, etc.) y produces el diagrama de ARQUITECTURA DE COMPONENTES del sistema a construir.

No es un flujograma de procesos: es un diagrama de componentes técnicos y sus conexiones, como los de AWS/Azure.

Reglas:
- Fundamenta cada componente en el contexto. Si el Diseño Técnico ya define stack, entidades o integraciones, respétalos. Si hay un diagrama actual, consérvalo y complétalo/corrígelo en vez de rehacerlo de cero.
- No inventes tecnologías que el contexto contradiga. Cuando el contexto no dice la tecnología, propón la más razonable y deja claro el rol en "description".
- Incluye las integraciones externas, canales (por ejemplo WhatsApp, correo), proveedores de IA, colas, cachés y almacenamiento que el contexto mencione o exija.
- Entre 6 y 24 componentes (un sistema grande necesita más de 16: sepáralos por capa y por integración). Cada uno con:
  - "label": nombre corto, máximo 22 caracteres
  - "description": tecnología o rol, máximo 40 caracteres
  - "type": exactamente uno de: frontend | backend | database | api | ia | queue | cache | externo
  - "x" entero 0-9 e "y" entero 0-5 (celda de la cuadrícula; no repitas celda).
- Columnas (x) de izquierda a derecha: 0-1 interfaces y canales de usuario; 2-3 API / gateway / autenticación; 4-5 backend y lógica de negocio (incluye IA/LLM); 6-7 datos, caché y colas; 8-9 servicios externos e integraciones de terceros. Reparte verticalmente (y) y centra: un solo nodo en una columna va en y=2; dos van en y=1 e y=3.
- "edges": conexiones { "from", "to" } entre ids de nodos. Sin etiquetas. No repitas un par en ambos sentidos.
- "resumen": 2 a 4 frases explicando las decisiones y qué partes del contexto las motivaron.
- "supuestos": lista de cosas que asumiste por falta de información (puede ir vacía).

Devuelve ÚNICAMENTE un JSON válido, sin markdown ni comentarios:
{ "resumen": "...", "supuestos": ["..."], "nodes": [ { "id": "n1", "label": "...", "description": "...", "type": "frontend", "x": 0, "y": 2 } ], "edges": [ { "from": "n1", "to": "n2" } ] }"#;

const RUIDO: [&str; 5] = ["id", "backlogItemId", "itemId", "createdAt", "updatedAt"];

fn limpiar_html(t: &str) -> String {
    let x = R_BLOQUE_FIN.replace_all(t, "\n").to_string();
    let x = R_BR.replace_all(&x, "\n").to_string();
    let x = R_TAG.replace_all(&x, "").to_string();
    let x = entidades(x);
    let x = R_ESP.replace_all(&x, " ").to_string();
    R_SALTOS2.replace_all(&x, "\n").trim().to_string()
}

fn a_texto(v: &Value, depth: usize) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(t) => limpiar_html(t),
        Value::Number(_) | Value::Bool(_) => v.to_string(),
        Value::Array(a) => {
            let partes: Vec<String> = a.iter().map(|x| a_texto(x, depth + 1)).filter(|t| !t.is_empty()).map(|t| if depth > 0 { t } else { format!("- {t}") }).collect();
            partes.join(if depth > 0 { " | " } else { "\n" })
        }
        Value::Object(o) => {
            let partes: Vec<String> = o
                .iter()
                .filter(|(k, _)| !RUIDO.contains(&k.as_str()))
                .map(|(k, x)| {
                    let t = a_texto(x, depth + 1);
                    if t.is_empty() {
                        String::new()
                    } else {
                        format!("{k}: {t}")
                    }
                })
                .filter(|t| !t.is_empty())
                .collect();
            partes.join(if depth > 0 { "; " } else { "\n" })
        }
    }
}

fn corta_puntos(t: String, max: usize) -> String {
    if t.chars().count() > max {
        format!("{}…", t.chars().take(max).collect::<String>())
    } else {
        t
    }
}

fn json_a_texto(raw: Option<&str>, max: usize) -> String {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else { return String::new() };
    let t = match serde_json::from_str::<Value>(raw) {
        Ok(v) => a_texto(&v, 0),
        Err(_) => limpiar_html(raw),
    };
    corta_puntos(t, max)
}

fn notas_fase(content: Option<&str>) -> String {
    let Some(c) = content.filter(|c| !c.is_empty()) else { return String::new() };
    if let Ok(p) = serde_json::from_str::<Value>(c) {
        if let Some(tabs) = p.get("tabs").and_then(|t| t.as_array()) {
            return tabs
                .iter()
                .map(|t| format!("[{}] {}", t["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("Nota"), limpiar_html(t["content"].as_str().unwrap_or(""))))
                .filter(|t| t.chars().count() > 8)
                .collect::<Vec<_>>()
                .join(" | ");
        }
    }
    limpiar_html(c)
}

fn seccion(titulo: &str, cuerpo: &str, fuentes: &mut Vec<String>, max: usize) -> String {
    let t = cuerpo.trim();
    if t.is_empty() {
        return String::new();
    }
    fuentes.push(titulo.to_string());
    format!("## {titulo}\n{}\n", if t.chars().count() > max { format!("{}…", t.chars().take(max).collect::<String>()) } else { t.to_string() })
}

fn primeros(t: &str, n: usize) -> String {
    t.chars().take(n).collect()
}

async fn arquitectura_generar(State(st): State<AppState>, o: Opcional, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    autenticado(&o)?;
    let Some(d) = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(so) || jsonb_build_object(
             'riesgos', COALESCE((SELECT jsonb_agg(to_jsonb(r)) FROM "Riesgo" r WHERE r."solucionId" = so.id), '[]'::jsonb),
             'hitos', COALESCE((SELECT jsonb_agg(to_jsonb(h)) FROM "Hito" h WHERE h."solucionId" = so.id), '[]'::jsonb),
             'backlogItems', COALESCE((SELECT jsonb_agg(jsonb_build_object('title', b.title, 'status', b.status, 'type', b.type, 'priority', b.priority) ORDER BY b."createdAt" ASC)
                                       FROM (SELECT * FROM "BacklogItem" WHERE "solucionId" = so.id ORDER BY "createdAt" ASC LIMIT 80) b), '[]'::jsonb),
             'epics', COALESCE((SELECT jsonb_agg(jsonb_build_object('name', e.name, 'description', e.description)) FROM "Epic" e WHERE e."solucionId" = so.id), '[]'::jsonb),
             'sprints', COALESCE((SELECT jsonb_agg(jsonb_build_object('name', s.name, 'goal', s.goal, 'status', s.status)) FROM "Sprint" s WHERE s."solucionId" = so.id), '[]'::jsonb))
           FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    else {
        return Err(ApiError::not_found("Solución no encontrada"));
    };
    let sol = &d;
    let g = |k: &str| txt(sol, k);
    let arr = |k: &str| sol[k].as_array().cloned().unwrap_or_default();
    let mut fuentes: Vec<String> = vec![];
    let mut partes: Vec<String> = vec![];

    let ficha = [
        Some(format!("Nombre: {}", sol["nombre"].as_str().unwrap_or(""))),
        Some(format!("Tipo: {}", sol["tipo"].as_str().unwrap_or(""))),
        Some(format!("Estado: {}", sol["estado"].as_str().unwrap_or(""))),
        g("empresa").map(|e| format!("Empresa: {e}")),
        g("repositorio").map(|e| format!("Repositorio: {e}")),
        g("descripcion").map(|e| format!("Descripción: {}", limpiar_html(&e))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    partes.push(seccion("Solución", &ficha, &mut fuentes, 2000));
    partes.push(seccion("PRD", &json_a_texto(sol["prd"].as_str(), 9000), &mut fuentes, 9000));
    partes.push(seccion("Diseño técnico", &json_a_texto(sol["disenoTecnico"].as_str(), 6000), &mut fuentes, 6000));
    partes.push(seccion("Plan de ejecución", &json_a_texto(sol["planEjecucion"].as_str(), 5000), &mut fuentes, 5000));
    partes.push(seccion("Plan de trabajo", &limpiar_html(sol["planTrabajo"].as_str().unwrap_or("")), &mut fuentes, 4000));
    partes.push(seccion("Cronograma", &json_a_texto(sol["cronograma"].as_str(), 2000), &mut fuentes, 2000));
    let riesgos = arr("riesgos").iter().map(|r| format!("- {} ({}){}", r["titulo"].as_str().unwrap_or(""), r["severidad"].as_str().unwrap_or(""), txt(r, "descripcion").map(|x| format!(": {}", limpiar_html(&x))).unwrap_or_default())).collect::<Vec<_>>().join("\n");
    partes.push(seccion("Riesgos", &riesgos, &mut fuentes, 2500));
    let hitos = arr("hitos").iter().map(|h| format!("- {}{}", h["titulo"].as_str().unwrap_or(""), txt(h, "descripcion").map(|x| format!(": {}", limpiar_html(&x))).unwrap_or_default())).collect::<Vec<_>>().join("\n");
    partes.push(seccion("Hitos", &hitos, &mut fuentes, 1500));
    let epicas_sprints = arr("epics")
        .iter()
        .map(|e| format!("Épica: {}{}", e["name"].as_str().unwrap_or(""), txt(e, "description").map(|x| format!(" - {}", primeros(&limpiar_html(&x), 200))).unwrap_or_default()))
        .chain(arr("sprints").iter().map(|s| format!("Sprint: {}{}", s["name"].as_str().unwrap_or(""), txt(s, "goal").map(|x| format!(" - {}", primeros(&limpiar_html(&x), 200))).unwrap_or_default())))
        .collect::<Vec<_>>()
        .join("\n");
    partes.push(seccion("Épicas y sprints", &epicas_sprints, &mut fuentes, 3000));
    let backlog = arr("backlogItems").iter().map(|b| format!("- [{}/{}] {}", b["type"].as_str().unwrap_or(""), b["status"].as_str().unwrap_or(""), b["title"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n");
    partes.push(seccion("Backlog (tareas)", &backlog, &mut fuentes, 5000));

    // Diagrama actual del lienzo (para conservarlo y completarlo)
    let mut actual = String::new();
    if let Some(a) = sol["arquitectura"].as_str().and_then(|x| serde_json::from_str::<Value>(x).ok()) {
        let nodes = if a.is_array() { a.as_array().cloned().unwrap_or_default() } else { a["nodes"].as_array().cloned().unwrap_or_default() };
        let conns = a["connections"].as_array().cloned().unwrap_or_default();
        if !nodes.is_empty() {
            let nombre: std::collections::HashMap<String, String> = nodes.iter().map(|n| (n["id"].as_str().unwrap_or("").to_string(), n["label"].as_str().unwrap_or("").to_string())).collect();
            let conexiones = conns
                .iter()
                .map(|c| format!("{} -> {}", nombre.get(c["from"].as_str().unwrap_or("")).map(String::as_str).unwrap_or("?"), nombre.get(c["to"].as_str().unwrap_or("")).map(String::as_str).unwrap_or("?")))
                .collect::<Vec<_>>()
                .join("; ");
            actual = format!(
                "Componentes: {}\nConexiones: {}",
                nodes.iter().map(|n| format!("{} ({})", n["label"].as_str().unwrap_or("undefined"), n["type"].as_str().unwrap_or("undefined"))).collect::<Vec<_>>().join(", "),
                if conexiones.is_empty() { "ninguna".to_string() } else { conexiones }
            );
        }
    }
    partes.push(seccion("Diagrama actual del lienzo de arquitectura", &actual, &mut fuentes, 3000));

    // Lead asociado: datos comerciales, notas de fases, interacciones, propuestas, diagrama del lead, archivos
    if let Some(lead_id) = g("leadId") {
        let l = fetch_json_opt(
            &st.pool,
            r#"SELECT to_jsonb(l) || jsonb_build_object(
                 'cliente', (SELECT jsonb_build_object('nombre', c.nombre) FROM "Cliente" c WHERE c.id = l."clienteId"),
                 'proposals', COALESCE((SELECT jsonb_agg(jsonb_build_object('title', p.title, 'description', p.description, 'amount', p.amount, 'status', p.status::text,
                      'tasks', COALESCE((SELECT jsonb_agg(jsonb_build_object('title', t.title, 'completed', t.completed)) FROM "ProposalTask" t WHERE t."proposalId" = p.id), '[]'::jsonb)))
                    FROM "Proposal" p WHERE p."leadId" = l.id), '[]'::jsonb),
                 'activities', COALESCE((SELECT jsonb_agg(jsonb_build_object('type', a.type::text, 'description', a.description) ORDER BY a."createdAt" DESC)
                    FROM (SELECT * FROM "Activity" WHERE "leadId" = l.id ORDER BY "createdAt" DESC LIMIT 25) a), '[]'::jsonb),
                 'fases', COALESCE((SELECT jsonb_agg(jsonb_build_object('phase', h.phase, 'content', h.content,
                      'files', COALESCE((SELECT jsonb_agg(jsonb_build_object('name', f.name)) FROM "LeadHubFile" f WHERE f."hubId" = h.id), '[]'::jsonb)) ORDER BY h."createdAt" ASC)
                    FROM "LeadHub" h WHERE h."leadId" = l.id), '[]'::jsonb))
               FROM "Lead" l WHERE l.id = $1"#,
            &[B::T(lead_id)],
        )
        .await?;
        if let Some(lead) = l {
            let lg = |k: &str| txt(&lead, k);
            let ficha = [
                Some(format!("Empresa: {}", lead["companyName"].as_str().unwrap_or(""))),
                lead["cliente"]["nombre"].as_str().filter(|x| !x.is_empty()).map(|x| format!("Cliente: {x}")),
                Some(format!("Contacto: {}", lead["contactName"].as_str().unwrap_or(""))),
                Some(format!("Fuente: {}", lead["source"].as_str().unwrap_or(""))),
                Some(format!("Valor estimado: {}", num_js(lead["estimatedValue"].as_f64().unwrap_or(0.0)))),
                lg("tipo").map(|x| format!("Tipo: {x}")),
                lg("solucionAsociada").map(|x| format!("Solución asociada: {x}")),
                lg("scope").map(|x| format!("Alcance: {}", limpiar_html(&x))),
                lg("notes").map(|x| format!("Notas: {}", limpiar_html(&x))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
            partes.push(seccion("Lead / cliente", &ficha, &mut fuentes, 3500));

            let fases = lead["fases"].as_array().cloned().unwrap_or_default();
            let notas = fases
                .iter()
                .filter(|f| f["phase"] != "COMPONENT_DIAGRAM")
                .map(|f| format!("{}: {}", f["phase"].as_str().unwrap_or(""), notas_fase(f["content"].as_str())))
                .filter(|t| t.chars().count() > 12)
                .collect::<Vec<_>>()
                .join("\n");
            partes.push(seccion("Notas de las fases del lead", &notas, &mut fuentes, 6000));
            let archivos = fases
                .iter()
                .flat_map(|f| f["files"].as_array().cloned().unwrap_or_default().into_iter().map(move |x| format!("{}: {}", f["phase"].as_str().unwrap_or(""), x["name"].as_str().unwrap_or(""))))
                .collect::<Vec<_>>()
                .join("\n");
            partes.push(seccion("Archivos adjuntos del lead (solo nombres)", &archivos, &mut fuentes, 1200));

            if let Some(dg) = fases.iter().find(|f| f["phase"] == "COMPONENT_DIAGRAM").and_then(|f| f["content"].as_str()).filter(|c| !c.is_empty()) {
                if let Ok(dd) = serde_json::from_str::<Value>(dg) {
                    let nodes = dd["nodes"].as_array().cloned().unwrap_or_default();
                    if !nodes.is_empty() {
                        let nom: std::collections::HashMap<String, String> = nodes.iter().map(|n| (n["id"].as_str().unwrap_or("").to_string(), n["label"].as_str().unwrap_or("").to_string())).collect();
                        let aristas = dd["edges"].as_array().cloned().unwrap_or_default().iter().map(|e| format!("{} -> {}", nom.get(e["from"].as_str().unwrap_or("")).map(String::as_str).unwrap_or("?"), nom.get(e["to"].as_str().unwrap_or("")).map(String::as_str).unwrap_or("?"))).collect::<Vec<_>>().join("; ");
                        let comp = nodes.iter().map(|n| format!("{}{}", n["label"].as_str().unwrap_or("undefined"), txt(n, "description").map(|d| format!(" [{d}]")).unwrap_or_default())).collect::<Vec<_>>().join(", ");
                        partes.push(seccion("Diagrama de componentes del lead (borrador comercial)", &format!("Componentes: {comp}\nConexiones: {aristas}"), &mut fuentes, 2500));
                    }
                }
            }

            let inter = lead["activities"].as_array().cloned().unwrap_or_default().iter().map(|a| format!("- [{}] {}", a["type"].as_str().unwrap_or(""), limpiar_html(a["description"].as_str().unwrap_or("")))).collect::<Vec<_>>().join("\n");
            partes.push(seccion("Interacciones con el cliente", &inter, &mut fuentes, 4000));
            let props = lead["proposals"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|p| {
                    let tareas = p["tasks"].as_array().cloned().unwrap_or_default();
                    format!(
                        "- {} ({}, {}): {}{}",
                        p["title"].as_str().unwrap_or(""),
                        p["status"].as_str().unwrap_or(""),
                        num_js(p["amount"].as_f64().unwrap_or(0.0)),
                        limpiar_html(p["description"].as_str().unwrap_or("")),
                        if tareas.is_empty() { String::new() } else { format!(" | Tareas: {}", tareas.iter().map(|t| t["title"].as_str().unwrap_or("")).collect::<Vec<_>>().join(", ")) }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            partes.push(seccion("Propuestas comerciales", &props, &mut fuentes, 5000));

            // Reuniones cuyo título menciona a la empresa (no hay vínculo directo reunión-lead todavía)
            let reuniones = fetch_json(
                &st.pool,
                r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x.date DESC), '[]'::jsonb) FROM
                     (SELECT title, description, notes, hub, date FROM "Meeting" WHERE position(lower($1) in lower(title)) > 0 ORDER BY date DESC LIMIT 6) x"#,
                &[B::T(lead["companyName"].as_str().unwrap_or("").to_string())],
            )
            .await?;
            let reus = reuniones
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|m| {
                    let mut hub_txt = String::new();
                    if let Some(h) = serde_json::from_str::<Value>(m["hub"].as_str().filter(|x| !x.is_empty()).unwrap_or("{}")).ok().filter(|h| h.is_object()) {
                        let unir = |k: &str| h[k].as_array().cloned().unwrap_or_default().iter().filter_map(|p| p["texto"].as_str().filter(|x| !x.is_empty()).map(String::from)).collect::<Vec<_>>().join("; ");
                        hub_txt = [unir("puntos"), notas_fase(h["notas"].as_str()), unir("decisiones")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" | ");
                    }
                    format!(
                        "- {}: {}",
                        m["title"].as_str().unwrap_or(""),
                        [txt(m, "description").map(|x| limpiar_html(&x)).unwrap_or_default(), txt(m, "notes").map(|x| limpiar_html(&x)).unwrap_or_default(), hub_txt].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" | ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            partes.push(seccion("Reuniones relacionadas (por nombre de la empresa)", &reus, &mut fuentes, 5000));
        }
    }

    let contexto: String = partes.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n").chars().take(32000).collect();
    if fuentes.len() <= 1 {
        return Err(ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, "Hay muy poco contexto para generar la arquitectura. Completa el PRD, el Diseño Técnico o las notas del lead primero."));
    }

    let error_amable = |msg: &str| -> String {
        let bajo = msg.to_lowercase();
        if bajo.contains("timeout") || bajo.contains("aborted") {
            "El modelo tardó demasiado en responder. Vuelve a intentarlo en un momento.".into()
        } else if Regex::new(r"\b50[0-9]\b").map(|r| r.is_match(msg)).unwrap_or(false) {
            "El proveedor de IA no está disponible ahora mismo (error temporal). Vuelve a intentarlo en un momento.".into()
        } else if msg.contains("JSON") {
            "El modelo devolvió un formato inválido. Intenta de nuevo.".into()
        } else {
            msg.to_string()
        }
    };
    let salida = llm::call_open_code(&st, SYSTEM_ARQ, &format!("Contexto completo del proyecto:\n\n{contexto}\n\nGenera el diagrama de arquitectura de componentes."), &format!("arq-{id}"), 3000, 150).await;
    let salida = match salida {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("[soluciones/arquitectura-generate] {}", primeros(&e, 400));
            return Err(bad_gateway(&error_amable(&e)));
        }
    };
    static FENCE_INI: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^```(?:json)?\s*"));
    static FENCE_FIN: LazyLock<Regex> = LazyLock::new(|| re(r"\s*```\s*$"));
    let limpio = FENCE_INI.replace(salida.trim(), "").to_string();
    let limpio = FENCE_FIN.replace(&limpio, "").trim().to_string();
    let candidato = match (limpio.find('{'), limpio.rfind('}')) {
        (Some(i), Some(f)) if f > i => limpio[i..=f].to_string(),
        _ => limpio.clone(),
    };
    let data: Value = match serde_json::from_str(&candidato) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("[soluciones/arquitectura-generate] {}", primeros(&e.to_string(), 400));
            return Err(bad_gateway("El modelo devolvió un formato inválido. Intenta de nuevo."));
        }
    };
    let Some(nodos_in) = data["nodes"].as_array().filter(|n| !n.is_empty()) else {
        return Err(bad_gateway("La respuesta no trae componentes"));
    };

    let mut usadas: std::collections::HashSet<(i64, i64)> = Default::default();
    let mut ids: std::collections::HashSet<String> = Default::default();
    let mut nodes: Vec<Value> = vec![];
    for (i, n) in nodos_in.iter().take(24).enumerate() {
        let mut nid = n["id"].as_str().filter(|x| !x.is_empty() && !ids.contains(*x)).map(String::from).unwrap_or_else(|| format!("n{}", i + 1));
        while ids.contains(&nid) {
            nid.push('_');
        }
        ids.insert(nid.clone());
        let num = |k: &str| n[k].as_f64().or_else(|| n[k].as_str().and_then(|x| x.trim().parse().ok())).filter(|v| v.is_finite()).map(|v| v.round() as i64).unwrap_or(0);
        let (mut gx, mut gy) = (num("x").clamp(0, 9), num("y").clamp(0, 5));
        let mut t = 0;
        while usadas.contains(&(gx, gy)) && t < 60 {
            gy += 1;
            if gy > 5 {
                gy = 0;
                gx = (gx + 1).min(9);
            }
            t += 1;
        }
        usadas.insert((gx, gy));
        let label = primeros(&match &n["label"] {
            Value::Null => "Componente".to_string(),
            Value::String(s) => s.clone(),
            otro => otro.to_string(),
        }, 22);
        let tipo = n["type"].as_str().filter(|t| TIPOS_NODO.contains(t)).unwrap_or("externo");
        nodes.push(json!({ "id": nid, "label": label, "type": tipo, "x": 16 + gx * PASO_X, "y": 16 + gy * PASO_Y }));
    }
    let mut pares: std::collections::HashSet<String> = Default::default();
    let mut connections: Vec<Value> = vec![];
    let mut rng = rand::thread_rng();
    for e in data["edges"].as_array().cloned().unwrap_or_default() {
        let (Some(de), Some(a)) = (e["from"].as_str(), e["to"].as_str()) else { continue };
        if !ids.contains(de) || !ids.contains(a) || de == a {
            continue;
        }
        let mut par = [de, a];
        par.sort();
        if !pares.insert(par.join("|")) {
            continue;
        }
        const ALF: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let azar: String = (0..4).map(|_| ALF[rng.gen_range(0..ALF.len())] as char).collect();
        connections.push(json!({ "id": format!("c{}{}", connections.len() + 1, azar), "from": de, "to": a }));
    }
    let supuestos: Vec<Value> = data["supuestos"].as_array().map(|a| a.iter().filter(|s| s.is_string()).take(8).cloned().collect()).unwrap_or_default();
    let _ = Map::<String, Value>::new();
    Ok(Json(json!({
        "nodes": nodes, "connections": connections,
        "resumen": data["resumen"].as_str().unwrap_or(""),
        "supuestos": supuestos, "fuentes": fuentes,
    })))
}
