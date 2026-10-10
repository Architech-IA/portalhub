//! El lead y el cliente de una Solución como contexto de las tareas del Motor — MASD-0024.
//!
//! Las actividades de preventa (ficha del cliente, briefing, acta, estimación, propuesta) son tareas de la Solución sin sprint:
//! antes el agente recibía «sin vínculo a solución» y ni el nombre de la empresa, y con razón se negaba a inventar. Ahora recibe
//! los datos del lead, la fase del proyecto, las notas de cada etapa del Lead Hub y las propuestas.

use serde_json::Value;

use crate::{
    routes::aichat::{texto_fase, FASES_LEAD},
    state::AppState,
    util::{fetch_json_opt, B},
};

const MAX_LEAD: usize = 7000;
const MAX_ETAPA: usize = 1800;

fn cortar(t: &str, n: usize) -> String {
    if t.chars().count() <= n { t.to_string() } else { format!("{}…", t.chars().take(n).collect::<String>()) }
}

fn etiqueta_estado(status: &str) -> &str {
    FASES_LEAD.iter().find(|f| f.0 == status).map(|f| f.1).unwrap_or(status)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("").trim()
}

/// Da forma de texto a lo que sabemos del lead. `None` si no hay lead.
pub fn formatear(v: &Value) -> Option<String> {
    if v.is_null() || s(v, "empresa").is_empty() {
        return None;
    }
    let mut l: Vec<String> = vec!["=== LEAD / CLIENTE DE LA SOLUCIÓN (datos reales del portal: no inventes lo que no esté acá) ===".into()];
    let contacto: Vec<&str> = [s(v, "contacto"), s(v, "email"), s(v, "telefono")].into_iter().filter(|x| !x.is_empty()).collect();
    l.push(format!("Empresa: {}", s(v, "empresa")));
    if !contacto.is_empty() {
        l.push(format!("Contacto: {}", contacto.join(" · ")));
    }
    let resultado = match v["resultado"].as_str() { Some("WON") => " (ganado)", Some("LOST") => " (perdido)", _ => "" };
    l.push(format!("Estado comercial: {}{resultado} · Origen: {}", etiqueta_estado(s(v, "estado")), if s(v, "origen").is_empty() { "—" } else { s(v, "origen") }));
    if let Some(valor) = v["valor"].as_f64().filter(|x| *x > 0.0) {
        l.push(format!("Valor estimado: {valor:.0}"));
    }
    if !s(v, "fase").is_empty() {
        l.push(format!("Fase actual del proyecto en el motor: {}", s(v, "fase")));
    }
    if !s(v, "alcance").is_empty() {
        l.push(format!("Alcance pedido por el cliente: {}", cortar(s(v, "alcance"), 1500)));
    }
    if !s(v, "notas").is_empty() {
        l.push(format!("Notas del lead: {}", cortar(s(v, "notas"), 1200)));
    }
    let mut etapas: Vec<String> = vec![];
    for (clave, nombre, _) in FASES_LEAD.iter() {
        if let Some(h) = v["hub"].as_array().and_then(|a| a.iter().find(|h| h["phase"] == *clave)) {
            let t = texto_fase(h["content"].as_str().unwrap_or(""));
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                etapas.push(format!("[{nombre}] {}", cortar(&t, MAX_ETAPA)));
            }
        }
    }
    if !etapas.is_empty() {
        l.push("NOTAS POR ETAPA (Lead Hub):".into());
        l.extend(etapas);
    }
    if let Some(ps) = v["propuestas"].as_array().filter(|a| !a.is_empty()) {
        l.push("PROPUESTAS:".into());
        for p in ps.iter().take(5) {
            let monto = p["amount"].as_f64().filter(|x| *x > 0.0).map(|x| format!(", {x:.0}")).unwrap_or_default();
            l.push(format!("- {} ({}{monto}){}", s(p, "title"), s(p, "status"), if s(p, "description").is_empty() { String::new() } else { format!(": {}", cortar(s(p, "description"), 400)) }));
        }
    }
    let t = l.join("\n");
    Some(if t.chars().count() > MAX_LEAD { format!("{}\n[... datos del lead truncados por tamaño ...]", t.chars().take(MAX_LEAD).collect::<String>()) } else { t })
}

/// Bloque de contexto con el lead de la Solución. Nunca falla: sin lead (o con error) el Motor sigue como antes.
pub async fn cargar(st: &AppState, solucion: &str) -> Option<String> {
    let v = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'empresa', l."companyName", 'contacto', l."contactName", 'email', l.email, 'telefono', l.phone, 'estado', l.status::text, 'resultado', l.outcome,
             'valor', l."estimatedValue", 'alcance', l.scope, 'notas', l.notes, 'origen', l.source, 'fase', pf."faseActual",
             'hub', (SELECT jsonb_agg(jsonb_build_object('phase', h.phase, 'content', h.content)) FROM "LeadHub" h WHERE h."leadId" = l.id),
             'propuestas', (SELECT jsonb_agg(jsonb_build_object('title', p.title, 'status', p.status::text, 'amount', p.amount, 'description', p.description) ORDER BY p."updatedAt" DESC)
                              FROM "Proposal" p WHERE p."leadId" = l.id))
             FROM "Solucion" s JOIN "Lead" l ON l.id = s."leadId" LEFT JOIN "ProyectoFase" pf ON pf."solucionId" = s.id WHERE s.id = $1"#,
        &[B::T(solucion.to_string())],
    )
    .await
    .ok()
    .flatten()?;
    formatear(&v)
}

/// (código, nombre, descripción) de la Solución, para las tareas que no pasan por un sprint.
pub async fn solucion_basica(st: &AppState, solucion: &str) -> Option<(String, String, String)> {
    let v = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('c', "solucionCode", 'n', nombre, 'd', descripcion) FROM "Solucion" WHERE id = $1"#, &[B::T(solucion.to_string())])
        .await
        .ok()
        .flatten()?;
    Some((s(&v, "c").to_string(), s(&v, "n").to_string(), s(&v, "d").to_string()))
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use serde_json::json;

    #[test]
    fn da_forma_al_lead_con_notas_y_propuestas() {
        let v = json!({
            "empresa": "Acme SAS", "contacto": "Ana", "email": "ana@acme.co", "telefono": null, "estado": "DIAGNOSIS", "resultado": null, "valor": 5000000.0,
            "alcance": "Un CRM", "notas": "Cliente referido", "origen": "Referido", "fase": "diagnostico",
            "hub": [{ "phase": "DIAGNOSIS", "content": r#"{"tabs":[{"name":"Nota","content":"<p>Usan <b>Excel</b></p>"}]}"# }, { "phase": "NEW", "content": "" }],
            "propuestas": [{ "title": "Propuesta v1", "status": "SENT", "amount": 5000000.0, "description": "Alcance base" }],
        });
        let t = formatear(&v).unwrap();
        assert!(t.contains("Empresa: Acme SAS") && t.contains("Contacto: Ana · ana@acme.co"));
        assert!(t.contains("Estado comercial: Diagnóstico · Origen: Referido"));
        assert!(t.contains("Fase actual del proyecto en el motor: diagnostico"));
        assert!(t.contains("[Diagnóstico] [Nota] Usan Excel"), "la nota de la etapa se vuelve texto: {t}");
        assert!(!t.contains("[Identificación]"), "las etapas vacías no aparecen");
        assert!(t.contains("- Propuesta v1 (SENT, 5000000): Alcance base"));
    }

    #[test]
    fn sin_lead_no_hay_bloque() {
        assert!(formatear(&Value::Null).is_none());
        assert!(formatear(&json!({ "empresa": "" })).is_none());
    }

    #[test]
    fn el_bloque_no_pasa_del_tope() {
        let v = json!({ "empresa": "X", "alcance": "a".repeat(5000), "notas": "n".repeat(5000), "estado": "NEW", "hub": [{ "phase": "NEW", "content": "h".repeat(9000) }, { "phase": "CONTACTED", "content": "c".repeat(9000) }, { "phase": "DIAGNOSIS", "content": "d".repeat(9000) }, { "phase": "DEMO_VALIDATION", "content": "d".repeat(9000) }] });
        let t = formatear(&v).unwrap();
        assert!(t.chars().count() < MAX_LEAD + 80);
    }
}
