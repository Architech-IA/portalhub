//! Orión: chat público (web, WhatsApp), historial de conversaciones y bitácora de acciones — MASD
//! PHUB-0001-0011. Paridad con `src/app/api/orion/{chat,messages}/route.ts`.
//!
//! `POST /api/orion/chat` es PÚBLICA en Next (`PUBLIC_PATHS` de proxy.ts: la llaman el bot de
//! WhatsApp y otros canales sin sesión); se conserva igual. GET/DELETE/PATCH usan la sesión si la
//! hay y, si no, el usuario "anonymous", igual que Next.

use std::{convert::Infallible, sync::LazyLock, time::Duration};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::StreamExt;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Opcional,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_json_raw, s, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/orion/chat", get(historial).post(chat).delete(cerrar_sesion).patch(reabrir_sesion))
        .route("/api/orion/messages", get(bitacora_listar).post(bitacora_crear))
}

const OPENCODE_URL: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const MAX_HISTORY: usize = 20;
const DEFAULT_MODEL: &str = "opencode-go/kimi-k3";

const DEFAULT_SYSTEM: &str = "Eres Orión, CEO y orquestador de ArchiTechIA. Coordinas, sintetizas y alineas. No tomas partido — buscas consenso, resumes posiciones y defines próximos pasos claros. Siempre respondés en el idioma del usuario.\n\nIMPORTANTE: Respondés SOLO en texto. No tenés acceso a herramientas, bash, ni bases de datos. Toda la información que necesitás para responder ya está en el contexto del sistema — no intentés ejecutar código ni consultas. Si el usuario pide leer un lead y el contexto ya está en el sistema, úsalo directamente.";

// ── Habilidad "contexto de lead" ─────────────────────────────────────────────────────────────
static RE_TRIGGERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(lee|leé|leer|revisa|revisar|analiza|analizar|contextualiza|contextualizar|valida|validar|resume|resumir|hub|lead)\b").expect("re"));
static RE_ENTRECOMILLADO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"["']([^"']{3,60})["']"#).expect("re"));
static RE_LEAD_DE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:lead|hub|empresa|cliente)\s+(?:de\s+|del\s+)?([A-ZÁÉÍÓÚÜÑ][A-Za-záéíóúüñ\s&\.]{2,50})").expect("re"));
static RE_VERBO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:lee|revisa|analiza|contextualiza|valida|resume)\s+(?:el\s+lead\s+(?:de\s+)?)?([A-ZÁÉÍÓÚÜÑ][A-Za-záéíóúüñ\s&\.]{2,50})").expect("re")
});

fn nombre_de_empresa(mensaje: &str) -> Option<String> {
    if let Some(c) = RE_ENTRECOMILLADO.captures(mensaje) {
        return Some(c[1].trim().to_string());
    }
    if let Some(c) = RE_LEAD_DE.captures(mensaje) {
        return Some(c[1].trim().to_string());
    }
    RE_VERBO.captures(mensaje).map(|c| c[1].trim().to_string())
}

fn quitar_html(html: &str) -> String {
    static R: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (r"(?i)<br\s*/?>", "\n"),
            (r"(?i)</p>", "\n"),
            (r"(?i)</li>", "\n"),
            (r"(?i)<li[^>]*>", "• "),
            (r"<[^>]+>", ""),
            (r"&nbsp;", " "),
            (r"&amp;", "&"),
            (r"&lt;", "<"),
            (r"&gt;", ">"),
            (r"\n{3,}", "\n\n"),
        ]
        .into_iter()
        .map(|(p, r)| (Regex::new(p).expect("re"), r))
        .collect()
    });
    let mut t = html.to_string();
    for (re, rep) in R.iter() {
        t = re.replace_all(&t, *rep).to_string();
    }
    t.trim().to_string()
}

fn contenido_hub(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        if let Some(tabs) = v.get("tabs").and_then(|t| t.as_array()) {
            return tabs
                .iter()
                .filter(|t| t["content"].as_str().map(|c| !c.is_empty() && c != "<p></p>").unwrap_or(false))
                .map(|t| format!("[{}]\n{}", t["name"].as_str().unwrap_or(""), quitar_html(t["content"].as_str().unwrap_or(""))))
                .collect::<Vec<_>>()
                .join("\n\n");
        }
    }
    quitar_html(raw)
}

/// `n.toLocaleString('es-CO')`: miles con punto, decimales con coma.
fn miles_es(n: f64) -> String {
    let negativo = n < 0.0;
    let n = n.abs();
    let entero = n.trunc() as u64;
    let mut dig = entero.to_string();
    let mut out = String::new();
    while dig.len() > 3 {
        let resto = dig.split_off(dig.len() - 3);
        out = format!(".{resto}{out}");
    }
    out = format!("{dig}{out}");
    let frac = ((n - n.trunc()) * 1000.0).round() / 1000.0;
    if frac > 0.0 {
        let f = format!("{frac:.3}").trim_start_matches("0.").trim_end_matches('0').to_string();
        if !f.is_empty() {
            out = format!("{out},{f}");
        }
    }
    if negativo {
        format!("-{out}")
    } else {
        out
    }
}

fn fecha_corta_es(iso: &str) -> String {
    const MESES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];
    let (y, m, d) = (iso.get(0..4), iso.get(5..7).and_then(|x| x.parse::<usize>().ok()), iso.get(8..10));
    match (y, m, d) {
        (Some(y), Some(m), Some(d)) if (1..=12).contains(&m) => format!("{d} {} {y}", MESES[m - 1]),
        _ => iso.to_string(),
    }
}

async fn contexto_de_lead(st: &AppState, empresa: &str) -> Result<Option<String>, sqlx::Error> {
    let palabras: Vec<String> = empresa.split_whitespace().filter(|w| w.chars().count() > 3).map(String::from).collect();
    let terminos: Vec<String> = if palabras.is_empty() { vec![empresa.to_string()] } else { palabras.clone() };
    let candidatos = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', x.id, 'companyName', x."companyName")), '[]'::jsonb)
           FROM (SELECT id, "companyName" FROM "Lead" l
                 WHERE EXISTS (SELECT 1 FROM jsonb_array_elements_text($1::jsonb) t(w) WHERE position(lower(t.w) in lower(l."companyName")) > 0) LIMIT 5) x"#,
        &[B::J(json!(terminos))],
    )
    .await?;
    let mut puntuados: Vec<(String, usize)> = candidatos
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|l| {
            let nombre = l["companyName"].as_str().unwrap_or("").to_lowercase();
            (l["id"].as_str().unwrap_or("").to_string(), palabras.iter().filter(|w| nombre.contains(&w.to_lowercase())).count())
        })
        .collect();
    puntuados.sort_by(|a, b| b.1.cmp(&a.1));
    let Some((lead_id, _)) = puntuados.first().cloned() else { return Ok(None) };

    let Some(d) = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'lead', to_jsonb(l),
             'propuestas', COALESCE((SELECT jsonb_agg(to_jsonb(p) ORDER BY p."createdAt" DESC) FROM (SELECT * FROM "Proposal" WHERE "leadId" = l.id ORDER BY "createdAt" DESC LIMIT 1) p), '[]'::jsonb),
             'actividades', COALESCE((SELECT jsonb_agg(jsonb_build_object('type', a.type, 'description', a.description, 'date', a.date, 'createdAt', a."createdAt",
                                'user', (SELECT u.name FROM "User" u WHERE u.id = a."userId"),
                                'meeting', (SELECT jsonb_build_object('title', m.title, 'type', m.type, 'status', m.status, 'date', m.date, 'location', m.location, 'link', m.link) FROM "Meeting" m WHERE m.id = a."meetingId"))
                                ORDER BY a."createdAt" DESC)
                FROM (SELECT * FROM "Activity" WHERE "leadId" = l.id AND type::text IN ('CALL', 'EMAIL', 'MEETING', 'WHATSAPP') ORDER BY "createdAt" DESC LIMIT 15) a), '[]'::jsonb),
             'solucion', (SELECT jsonb_build_object('nombre', so.nombre, 'items', COALESCE((SELECT jsonb_agg(jsonb_build_object('title', b.title, 'status', b.status,
                                'sprint', (SELECT sp.name FROM "Sprint" sp WHERE sp.id = b."sprintId")) ORDER BY b."createdAt" DESC)
                                FROM (SELECT * FROM "BacklogItem" WHERE "solucionId" = so.id ORDER BY "createdAt" DESC LIMIT 20) b), '[]'::jsonb))
                          FROM "Solucion" so WHERE so."leadId" = l.id),
             'fases', COALESCE((SELECT jsonb_agg(jsonb_build_object('phase', h.phase, 'content', h.content,
                                'archivos', COALESCE((SELECT jsonb_agg(f.name) FROM "LeadHubFile" f WHERE f."hubId" = h.id), '[]'::jsonb)))
                        FROM "LeadHub" h WHERE h."leadId" = l.id), '[]'::jsonb))
           FROM "Lead" l WHERE l.id = $1"#,
        &[B::T(lead_id)],
    )
    .await?
    else {
        return Ok(None);
    };
    let lead = &d["lead"];
    let g = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    let estado = |k: &str| -> String {
        match k {
            "NEW" => "Nuevo", "CONTACTED" => "Contactado", "DIAGNOSIS" => "Diagnóstico", "DEMO_VALIDATION" => "Demo",
            "PROPOSAL_SENT" => "Propuesta enviada", "NEGOTIATION" => "Negociación", "RESULT" => "Resultado",
            otro => otro,
        }
        .to_string()
    };
    let mut l: Vec<String> = vec![];
    l.push(format!("━━━ CONTEXTO DEL LEAD: {} ━━━", g(lead, "companyName").to_uppercase()));
    l.push(format!("Empresa: {}", g(lead, "companyName")));
    let tel = lead["phone"].as_str().filter(|p| !p.is_empty()).map(|p| format!(" | {p}")).unwrap_or_default();
    l.push(format!("Contacto: {} | {}{}", g(lead, "contactName"), g(lead, "email"), tel));
    l.push(format!("Estado: {}", estado(&g(lead, "status"))));
    l.push(format!("Valor estimado: ${}", miles_es(lead["estimatedValue"].as_f64().unwrap_or(0.0))));
    l.push(format!("Origen: {}", g(lead, "source")));
    if let Some(a) = lead["scope"].as_str().filter(|x| !x.is_empty()) {
        l.push(format!("Alcance: {a}"));
    }
    if let Some(n) = lead["notes"].as_str().filter(|x| !x.is_empty()) {
        l.push(format!("Notas generales: {n}"));
    }
    l.push(String::new());

    let fases_orden = [
        ("NEW", "Identificación"), ("CONTACTED", "Contacto"), ("DIAGNOSIS", "Diagnóstico"), ("DEMO_VALIDATION", "Demo"),
        ("PROPOSAL_SENT", "Propuesta"), ("NEGOTIATION", "Negociación"), ("WON", "Resultado"),
    ];
    let fases = d["fases"].as_array().cloned().unwrap_or_default();
    l.push("📋 FASES DEL HUB:".to_string());
    for (clave, etiqueta) in fases_orden {
        match fases.iter().find(|h| h["phase"].as_str() == Some(clave)).filter(|h| h["content"].as_str().map(|c| !c.is_empty()).unwrap_or(false)) {
            None => l.push(format!("  [{etiqueta}] Sin contenido")),
            Some(h) => {
                l.push(format!("  [{etiqueta}]:"));
                for linea in contenido_hub(h["content"].as_str().unwrap_or("")).split('\n').filter(|x| !x.is_empty()) {
                    l.push(format!("    {linea}"));
                }
                let archivos: Vec<&str> = h["archivos"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
                if !archivos.is_empty() {
                    l.push(format!("    Archivos adjuntos: {}", archivos.join(", ")));
                }
            }
        }
    }
    l.push(String::new());

    let acts = d["actividades"].as_array().cloned().unwrap_or_default();
    if !acts.is_empty() {
        l.push("🗣️ INTERACCIONES (más recientes):".to_string());
        for a in &acts {
            let fecha = fecha_corta_es(a["date"].as_str().filter(|x| !x.is_empty()).unwrap_or_else(|| a["createdAt"].as_str().unwrap_or("")));
            let quien = a["user"].as_str().unwrap_or("Sistema");
            if a["meeting"].is_object() {
                let m = &a["meeting"];
                let est = match m["status"].as_str().unwrap_or("") {
                    "SCHEDULED" => "Programada", "COMPLETED" => "Completada", "CANCELLED" => "Cancelada", otro => otro,
                };
                l.push(format!("  • REUNIÓN VINCULADA | {fecha} | {quien}"));
                l.push(format!("    Título: {} | {est}", g(m, "title")));
                if let Some(lugar) = m["location"].as_str().filter(|x| !x.is_empty()) {
                    l.push(format!("    Lugar: {lugar}"));
                }
            } else {
                let tipo = match a["type"].as_str().unwrap_or("") {
                    "CALL" => "Llamada", "EMAIL" => "Email", "MEETING" => "Reunión", "WHATSAPP" => "WhatsApp", otro => otro,
                };
                l.push(format!("  • {tipo} | {fecha} | {quien}: {}", g(a, "description")));
            }
        }
        l.push(String::new());
    }
    if let Some(p) = d["propuestas"].as_array().and_then(|a| a.first()) {
        let est = match p["status"].as_str().unwrap_or("") {
            "DRAFT" => "Borrador", "SENT" => "Enviada", "ACCEPTED" => "Aceptada", "REJECTED" => "Rechazada", otro => otro,
        };
        l.push("📄 PROPUESTA:".to_string());
        l.push(format!("  Título: {}", g(p, "title")));
        l.push(format!("  Estado: {est} | Monto: ${}", miles_es(p["amount"].as_f64().unwrap_or(0.0))));
        if let Some(desc) = p["description"].as_str().filter(|x| !x.is_empty()) {
            l.push(format!("  Descripción: {}", desc.chars().take(300).collect::<String>()));
        }
        l.push(String::new());
    }
    if let Some(sol) = d["solucion"].as_object() {
        let items = sol.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
        if !items.is_empty() {
            l.push(format!("🗂️ SOLUCIÓN ASOCIADA: {}", sol.get("nombre").and_then(|n| n.as_str()).unwrap_or("")));
            l.push(format!("  Backlog items ({}):", items.len()));
            for it in items.iter().take(15) {
                let est = match it["status"].as_str().unwrap_or("") {
                    "TODO" => "Por hacer", "IN_PROGRESS" => "En progreso", "DONE" => "Hecho", "BACKLOG" => "Backlog", otro => otro,
                };
                let sprint = it["sprint"].as_str().map(|s| format!(" ({s})")).unwrap_or_default();
                l.push(format!("    - [{est}] {}{sprint}", g(it, "title")));
            }
            l.push(String::new());
        }
    }
    l.push("━━━ FIN CONTEXTO DEL LEAD ━━━".to_string());
    Ok(Some(l.join("\n")))
}

// ── Persistencia de la conversación ──────────────────────────────────────────────────────────
async fn cargar_historial(st: &AppState, canal_tipo: &str, canal_id: &str) -> Vec<Value> {
    fetch_json_raw(
        &st.pool,
        r#"SELECT messages FROM "AgentConversation" WHERE "agentSlug" = 'orion' AND "channelType" = $1 AND "channelId" = $2"#,
        &[B::T(canal_tipo.into()), B::T(canal_id.into())],
    )
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default()
}

async fn guardar_historial(st: &AppState, canal_tipo: &str, canal_id: &str, mensajes: &[Value]) {
    let desde = mensajes.len().saturating_sub(MAX_HISTORY);
    let recortado = Value::Array(mensajes[desde..].to_vec());
    let r = exec(
        &st.pool,
        r#"INSERT INTO "AgentConversation" (id, "agentSlug", "channelType", "channelId", messages)
           VALUES ($1, 'orion', $2, $3, $4::jsonb)
           ON CONFLICT ("agentSlug", "channelType", "channelId") DO UPDATE SET messages = $4::jsonb, "updatedAt" = NOW()"#,
        &[B::T(crate::util::new_id()), B::T(canal_tipo.into()), B::T(canal_id.into()), B::T(recortado.to_string())],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("[orion] no se pudo guardar el historial: {e}");
    }
}

// ── Chat ─────────────────────────────────────────────────────────────────────────────────────
fn error_json(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

async fn llamar_claude(model: &str, system: &str, historial: &[Value]) -> Result<String, String> {
    let previos: Vec<String> = historial[..historial.len().saturating_sub(1)]
        .iter()
        .rev()
        .take(10)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|m| format!("{}: {}", if m["role"] == "user" { "Usuario" } else { "Orion" }, m["content"].as_str().unwrap_or("")))
        .collect();
    let ultimo = historial.last().and_then(|m| m["content"].as_str()).unwrap_or("");
    let completo = if previos.is_empty() { ultimo.to_string() } else { format!("{}\nUsuario: {ultimo}", previos.join("\n")) };
    let salida = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("claude").args(["--model", model, "--system-prompt", system, "--tools", "", "-p", &completo]).output(),
    )
    .await
    .map_err(|_| "claude: tiempo agotado".to_string())?
    .map_err(|e| e.to_string())?;
    let out = String::from_utf8_lossy(&salida.stdout).trim().to_string();
    if !salida.status.success() && out.is_empty() {
        return Err(String::from_utf8_lossy(&salida.stderr).to_string());
    }
    Ok(out)
}

/// Lógica del chat sin HTTP: la usan la ruta y el bot de WhatsApp. Devuelve `(respuesta, empresa del lead)`.
pub async fn responder(st: &AppState, mensaje: &str, canal_tipo: &str, canal_id: &str) -> Result<(String, Option<String>), String> {
    let (base, modelo) = {
        let a = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('p', "systemPrompt", 'm', "llmModel") FROM "Agent" WHERE slug = 'orion'"#, &[])
            .await
            .map_err(|e| e.to_string())?;
        (
            a.as_ref().and_then(|x| x["p"].as_str()).map(String::from).unwrap_or_else(|| DEFAULT_SYSTEM.to_string()),
            a.as_ref().and_then(|x| x["m"].as_str()).map(String::from).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        )
    };
    let mut system = base.clone();
    let mut nota = None;
    if RE_TRIGGERS.is_match(mensaje) {
        if let Some(empresa) = nombre_de_empresa(mensaje) {
            match contexto_de_lead(st, &empresa).await {
                Ok(Some(ctx)) => {
                    system = format!("{base}\n\nEl usuario te ha pedido que analices un lead. A continuación está toda la información disponible del lead en el sistema. Úsala para responder con precisión y profundidad.\n\n{ctx}");
                    nota = Some(empresa);
                }
                Ok(None) => {}
                Err(e) => tracing::error!("[Orion] Lead context fetch error: {e}"),
            }
        }
    }
    let mut historial = cargar_historial(st, canal_tipo, canal_id).await;
    historial.push(json!({ "role": "user", "content": mensaje.trim() }));

    let respuesta = if modelo.starts_with("opencode-go/") || modelo.starts_with("opencode/") {
        let id_modelo = modelo.rsplit('/').next().unwrap_or("").to_string();
        let mut todos = vec![json!({ "role": "system", "content": system })];
        todos.extend(historial.iter().cloned());
        let r = st
            .http
            .post(OPENCODE_URL)
            .bearer_auth(st.cfg.opencode_api_key.clone().unwrap_or_default())
            .header("x-opencode-session", format!("orion-{canal_tipo}-{canal_id}"))
            .timeout(Duration::from_secs(60))
            .json(&json!({ "model": id_modelo, "messages": todos, "max_tokens": 2048 }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(r.text().await.unwrap_or_default());
        }
        let d: Value = r.json().await.map_err(|e| e.to_string())?;
        d.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("").trim().to_string()
    } else if modelo.starts_with("claude") {
        llamar_claude(&modelo, &system, &historial).await?
    } else {
        return Err(format!("Modelo no soportado: {modelo}"));
    };
    if !respuesta.is_empty() {
        historial.push(json!({ "role": "assistant", "content": respuesta }));
        guardar_historial(st, canal_tipo, canal_id, &historial).await;
    }
    Ok((respuesta, nota))
}

async fn chat(State(st): State<AppState>, cuerpo: Option<Json<Value>>) -> Response {
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let mensaje = s(&body, "message").unwrap_or_default();
    if mensaje.trim().is_empty() {
        return error_json(StatusCode::BAD_REQUEST, "message requerido");
    }
    let canal_tipo = s(&body, "channelType").unwrap_or_else(|| "portal".into());
    let canal_id = s(&body, "channelId").unwrap_or_else(|| "anonymous".into());
    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // Modo streaming con OpenCode: se reenvía el flujo y, al terminar, se guarda la conversación.
    let (modelo, base) = {
        let a = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('p', "systemPrompt", 'm', "llmModel") FROM "Agent" WHERE slug = 'orion'"#, &[]).await.ok().flatten();
        (
            a.as_ref().and_then(|x| x["m"].as_str()).map(String::from).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            a.as_ref().and_then(|x| x["p"].as_str()).map(String::from).unwrap_or_else(|| DEFAULT_SYSTEM.to_string()),
        )
    };
    let es_opencode = modelo.starts_with("opencode-go/") || modelo.starts_with("opencode/");
    let es_claude = modelo.starts_with("claude");

    if stream && es_opencode {
        let mut system = base.clone();
        if RE_TRIGGERS.is_match(&mensaje) {
            if let Some(empresa) = nombre_de_empresa(&mensaje) {
                if let Ok(Some(ctx)) = contexto_de_lead(&st, &empresa).await {
                    system = format!("{base}\n\nEl usuario te ha pedido que analices un lead. A continuación está toda la información disponible del lead en el sistema. Úsala para responder con precisión y profundidad.\n\n{ctx}");
                }
            }
        }
        let mut historial = cargar_historial(&st, &canal_tipo, &canal_id).await;
        historial.push(json!({ "role": "user", "content": mensaje.trim() }));
        let mut todos = vec![json!({ "role": "system", "content": system })];
        todos.extend(historial.iter().cloned());
        let id_modelo = modelo.rsplit('/').next().unwrap_or("").to_string();
        let up = match st
            .http
            .post(OPENCODE_URL)
            .bearer_auth(st.cfg.opencode_api_key.clone().unwrap_or_default())
            .header("x-opencode-session", format!("orion-{canal_tipo}-{canal_id}"))
            .timeout(Duration::from_secs(60))
            .json(&json!({ "model": id_modelo, "messages": todos, "max_tokens": 2048, "stream": true }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return error_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        if !up.status().is_success() {
            let code = StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            return error_json(code, up.text().await.unwrap_or_default());
        }
        let (tx, rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(32);
        let st2 = st.clone();
        tokio::spawn(async move {
            let mut completa = String::new();
            let mut flujo = up.bytes_stream();
            while let Some(Ok(trozo)) = flujo.next().await {
                for linea in String::from_utf8_lossy(&trozo).split('\n') {
                    if let Some(dato) = linea.strip_prefix("data: ") {
                        if dato == "[DONE]" {
                            continue;
                        }
                        if let Some(d) = serde_json::from_str::<Value>(dato).ok().and_then(|v| v.pointer("/choices/0/delta/content").and_then(|c| c.as_str()).map(String::from)) {
                            completa.push_str(&d);
                        }
                    }
                }
                if tx.send(trozo).await.is_err() {
                    break;
                }
            }
            drop(tx);
            if !completa.is_empty() {
                historial.push(json!({ "role": "assistant", "content": completa }));
                guardar_historial(&st2, &canal_tipo, &canal_id, &historial).await;
            }
        });
        let cuerpo = Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|b| (Ok::<_, Infallible>(b), rx)) }));
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(cuerpo)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    match responder(&st, &mensaje, &canal_tipo, &canal_id).await {
        Ok((respuesta, nota)) => {
            if stream && es_claude {
                let trozo = format!("data: {}\n\ndata: [DONE]\n\n", json!({ "choices": [{ "delta": { "content": respuesta } }] }));
                return Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .header(header::CONNECTION, "keep-alive")
                    .body(Body::from(trozo))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            let mut out = json!({ "reply": respuesta });
            if let Some(n) = nota {
                out["leadContextLoaded"] = json!(n);
            }
            Json(out).into_response()
        }
        Err(e) => {
            tracing::error!("[Orion] LLM error {}", e.chars().take(300).collect::<String>());
            let status = if e.starts_with("Modelo no soportado") { StatusCode::BAD_REQUEST } else { StatusCode::INTERNAL_SERVER_ERROR };
            error_json(status, e)
        }
    }
}

// ── Historial de sesiones (GET / DELETE / PATCH) ─────────────────────────────────────────────
fn usuario_de(sesion: &Opcional) -> String {
    sesion.0.as_ref().map(|s| s.id.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| "anonymous".into())
}

async fn conversacion(st: &AppState, usuario: &str) -> Result<Option<Value>, sqlx::Error> {
    fetch_json_raw(
        &st.pool,
        r#"SELECT jsonb_build_object('messages', messages, 'sessions', sessions,
                  'createdAt', to_char(c."createdAt", 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'))
           FROM "AgentConversation" c WHERE "agentSlug" = 'orion' AND "channelType" = 'hub' AND "channelId" = $1 LIMIT 1"#,
        &[B::T(usuario.into())],
    )
    .await
}

async fn historial(State(st): State<AppState>, sesion: Opcional) -> ApiResult<Json<Value>> {
    let conv = conversacion(&st, &usuario_de(&sesion)).await?;
    Ok(Json(json!({
        "sessions": conv.as_ref().map(|c| c["sessions"].clone()).filter(|v| v.is_array()).unwrap_or_else(|| json!([])),
        "messages": conv.as_ref().map(|c| c["messages"].clone()).filter(|v| v.is_array()).unwrap_or_else(|| json!([])),
    })))
}

fn ahora_iso() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let (ms, seg) = (d.subsec_millis(), d.as_secs() as i64);
    let dias = seg.div_euclid(86400);
    let resto = seg.rem_euclid(86400);
    // días desde 1970 → fecha civil (Howard Hinnant)
    let z = dias + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dia = doy - (153 * mp + 2) / 5 + 1;
    let mes = if mp < 10 { mp + 3 } else { mp - 9 };
    let anio = yoe + era * 400 + if mes <= 2 { 1 } else { 0 };
    format!("{anio:04}-{mes:02}-{dia:02}T{:02}:{:02}:{:02}.{ms:03}Z", resto / 3600, (resto % 3600) / 60, resto % 60)
}

async fn guardar_conversacion(st: &AppState, usuario: &str, mensajes: &Value, sesiones: &Value) -> Result<(), sqlx::Error> {
    exec(
        &st.pool,
        r#"INSERT INTO "AgentConversation" (id, "agentSlug", "channelType", "channelId", messages, sessions)
           VALUES ($1, 'orion', 'hub', $2, $3::jsonb, $4::jsonb)
           ON CONFLICT ("agentSlug", "channelType", "channelId") DO UPDATE SET messages = $3::jsonb, sessions = $4::jsonb, "updatedAt" = NOW()"#,
        &[B::T(crate::util::new_id()), B::T(usuario.into()), B::T(mensajes.to_string()), B::T(sesiones.to_string())],
    )
    .await?;
    Ok(())
}

async fn cerrar_sesion(State(st): State<AppState>, sesion: Opcional) -> ApiResult<Json<Value>> {
    let usuario = usuario_de(&sesion);
    let conv = conversacion(&st, &usuario).await?;
    let actuales = conv.as_ref().and_then(|c| c["messages"].as_array().cloned()).unwrap_or_default();
    let existentes = conv.as_ref().and_then(|c| c["sessions"].as_array().cloned()).unwrap_or_default();
    if actuales.is_empty() {
        return Ok(Json(json!({ "ok": true, "sessions": existentes })));
    }
    let primero = actuales.iter().find(|m| m["role"] == "user").and_then(|m| m["content"].as_str()).unwrap_or("");
    let ahora = ahora_iso();
    let nueva = json!({
        "id": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0).to_string(),
        "startedAt": conv.as_ref().and_then(|c| c["createdAt"].as_str()).unwrap_or(&ahora),
        "endedAt": ahora,
        "preview": primero.chars().take(80).collect::<String>(),
        "messages": actuales,
    });
    let mut sesiones = existentes;
    sesiones.push(nueva);
    let desde = sesiones.len().saturating_sub(20);
    let sesiones = Value::Array(sesiones[desde..].to_vec());
    guardar_conversacion(&st, &usuario, &json!([]), &sesiones).await?;
    Ok(Json(json!({ "ok": true, "sessions": sesiones })))
}

async fn reabrir_sesion(State(st): State<AppState>, sesion: Opcional, cuerpo: Option<Json<Value>>) -> Response {
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let Some(sid) = s(&body, "sessionId").filter(|x| !x.is_empty()) else {
        return error_json(StatusCode::BAD_REQUEST, "sessionId requerido");
    };
    let usuario = usuario_de(&sesion);
    let conv = match conversacion(&st, &usuario).await {
        Ok(Some(c)) => c,
        Ok(None) => return error_json(StatusCode::NOT_FOUND, "No hay conversaciones para este usuario"),
        Err(e) => return ApiError::from(e).into_response(),
    };
    let sesiones = conv["sessions"].as_array().cloned().unwrap_or_default();
    let Some(objetivo) = sesiones.iter().find(|x| x["id"].as_str() == Some(sid.as_str())).cloned() else {
        return error_json(StatusCode::NOT_FOUND, "Sesión no encontrada");
    };
    let mut nuevas: Vec<Value> = sesiones.into_iter().filter(|x| x["id"].as_str() != Some(sid.as_str())).collect();
    let actuales = conv["messages"].as_array().cloned().unwrap_or_default();
    if !actuales.is_empty() {
        let primero = actuales.iter().find(|m| m["role"] == "user").and_then(|m| m["content"].as_str()).unwrap_or("");
        nuevas.push(json!({
            "id": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0).to_string(),
            "startedAt": conv["createdAt"], "endedAt": ahora_iso(),
            "preview": primero.chars().take(80).collect::<String>(), "messages": actuales,
        }));
        let desde = nuevas.len().saturating_sub(20);
        nuevas = nuevas[desde..].to_vec();
    }
    let sesiones_v = Value::Array(nuevas);
    if let Err(e) = guardar_conversacion(&st, &usuario, &objetivo["messages"], &sesiones_v).await {
        return ApiError::from(e).into_response();
    }
    Json(json!({ "ok": true, "messages": objetivo["messages"], "sessions": sesiones_v })).into_response()
}

// ── Bitácora de acciones de Orión ────────────────────────────────────────────────────────────
async fn bitacora_listar(State(st): State<AppState>, _s: crate::session::Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let area = q.get("areaId").filter(|x| !x.is_empty()).cloned();
    let sql = r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."createdAt" DESC), '[]'::jsonb) FROM
        (SELECT id, message, "actionType", "backlogItemId", "backlogItemTitle", "backlogItemCode", metadata, "createdAt" FROM "OrionLog"
         WHERE ($1::text IS NULL OR ("actionType" = 'DISPATCHED' AND metadata->>'toAreaId' = $1))
         ORDER BY "createdAt" DESC LIMIT CASE WHEN $1::text IS NULL THEN 60 ELSE 40 END) x"#;
    Ok(Json(fetch_json(&st.pool, sql, &[B::OT(area)]).await?))
}

async fn bitacora_crear(State(st): State<AppState>, headers: axum::http::HeaderMap, cuerpo: Option<Json<Value>>) -> Response {
    let key = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    if key != st.cfg.internal_api_key.as_deref() {
        return error_json(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    let body = cuerpo.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    let Some(msg) = s(&body, "message").filter(|m| !m.is_empty()) else {
        return error_json(StatusCode::BAD_REQUEST, "message required");
    };
    let meta = match body.get("metadata") {
        Some(Value::Null) | None => None,
        Some(m) => Some(m.to_string()),
    };
    let r = exec(
        &st.pool,
        r#"INSERT INTO "OrionLog" (message, "actionType", "backlogItemId", "backlogItemTitle", "backlogItemCode", metadata)
           VALUES ($1, COALESCE($2, 'INFO'), $3, $4, $5, $6::jsonb)"#,
        &[B::T(msg), B::OT(s(&body, "actionType")), B::OT(s(&body, "backlogItemId")), B::OT(s(&body, "backlogItemTitle")), B::OT(s(&body, "backlogItemCode")), B::OT(meta)],
    )
    .await;
    match r {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}
