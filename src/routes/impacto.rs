//! Impacto de un cambio y regeneración asistida de artefactos (MASD-0026-0001 y 0006).
//!
//! - IMPACTO: de un artefacto (por ejemplo el PRD) lista lo que depende de él (según las entradas del catálogo, con su nivel),
//!   y si cambiaron los requisitos entre las dos últimas versiones del PRD, las tareas y pruebas de aceptación de esos requisitos.
//! - REGENERAR: para los artefactos que viven como documento de fase, la IA redacta una versión actualizada a partir de sus
//!   entradas y la deja como PROPUESTA del documento (nunca pisa el contenido): una persona la abre, la revisa y la acepta.

use std::collections::{HashMap, HashSet, VecDeque};

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    motor::spec_export::cuerpo,
    routes::{catalogo_artefactos, solucion_hub::{nombre_de, parse_o_texto}},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/soluciones/{id}/catalogo-artefactos/{clave}/impacto", get(impacto))
        .route("/api/soluciones/{id}/catalogo-artefactos/{clave}/regenerar", post(regenerar))
}

// ═══════════════════════════ Lógica pura (con pruebas) ═══════════════════════════
/// Tipos que dependen (directa o indirectamente) de `origen`, con su nivel (1 = directo).
pub(crate) fn dependientes(tipos: &[Value], origen: &str) -> Vec<(String, usize)> {
    let mut vistos: HashSet<String> = HashSet::from([origen.to_string()]);
    let mut cola: VecDeque<(String, usize)> = VecDeque::from([(origen.to_string(), 0)]);
    let mut out = vec![];
    while let Some((actual, nivel)) = cola.pop_front() {
        for t in tipos {
            let clave = t["clave"].as_str().unwrap_or("");
            let entradas: Vec<&str> = t["entradas"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
            if entradas.contains(&actual.as_str()) && !vistos.contains(clave) {
                vistos.insert(clave.to_string());
                out.push((clave.to_string(), nivel + 1));
                cola.push_back((clave.to_string(), nivel + 1));
            }
        }
    }
    out
}

fn texto_req(r: &Value, claves: &[&str]) -> String {
    claves.iter().find_map(|k| r.get(*k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())).unwrap_or("").to_string()
}

/// Requisitos (por `id`) que cambiaron entre dos versiones del PRD: (id, texto, motivo NUEVO | MODIFICADO | ELIMINADO).
pub(crate) fn requisitos_cambiados(anterior: &Value, actual: &Value) -> Vec<(String, String, &'static str)> {
    let mapa = |prd: &Value| -> HashMap<String, (String, String)> {
        prd["requisitos"]
            .as_array()
            .map(|a| a.iter().filter_map(|r| Some((r["id"].as_str()?.to_string(), (texto_req(r, &["titulo", "nombre", "texto", "descripcion"]), texto_req(r, &["criterioAceptacion", "criterio", "aceptacion"]))))).collect())
            .unwrap_or_default()
    };
    let (a, b) = (mapa(anterior), mapa(actual));
    let mut out = vec![];
    for (id, (texto, crit)) in &b {
        match a.get(id) {
            None => out.push((id.clone(), texto.clone(), "NUEVO")),
            Some((t0, c0)) if t0 != texto || c0 != crit => out.push((id.clone(), texto.clone(), "MODIFICADO")),
            _ => {}
        }
    }
    for (id, (texto, _)) in &a {
        if !b.contains_key(id) {
            out.push((id.clone(), texto.clone(), "ELIMINADO"));
        }
    }
    out.sort_by(|x, y| x.0.cmp(&y.0));
    out
}

fn sin_vallas(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix("```markdown").or_else(|| t.strip_prefix("```md")).or_else(|| t.strip_prefix("```")).unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t).trim().to_string()
}

fn exigir_lectura(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

// ═══════════════════════════ Impacto ═══════════════════════════
async fn impacto(State(st): State<AppState>, sesion: Session, Path((id, clave)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion)?;
    let (filas, _) = catalogo_artefactos::filas(&st, &id).await?;
    if !filas.iter().any(|t| t["clave"] == clave.as_str()) {
        return Err(ApiError::not_found("Tipo de artefacto desconocido"));
    }
    let deps: Vec<Value> = dependientes(&filas, &clave)
        .into_iter()
        .filter_map(|(c, nivel)| {
            let f = filas.iter().find(|t| t["clave"] == c.as_str())?;
            Some(json!({ "clave": c, "nombre": f["nombre"], "nivel": nivel, "existe": f["instancia"]["existe"], "estado": f["instancia"]["estado"], "desactualizado": f["desactualizado"] }))
        })
        .collect();

    // Requisitos que cambiaron entre las dos últimas versiones del PRD (solo si el artefacto es el PRD o los requisitos).
    let (mut cambiados, mut tareas, mut pruebas) = (vec![], Value::Array(vec![]), Value::Array(vec![]));
    if clave == "prd" || clave == "requisitos" {
        let v = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(contenido ORDER BY version DESC), '[]'::jsonb) FROM (SELECT contenido, version FROM "SolucionPrdVersion" WHERE "solucionId" = $1 ORDER BY version DESC LIMIT 2) x"#, &[B::T(id.clone())]).await?;
        let versiones: Vec<Value> = v.as_array().cloned().unwrap_or_default();
        if versiones.len() == 2 {
            let (actual, anterior) = (parse_o_texto(versiones[0].as_str().unwrap_or("")), parse_o_texto(versiones[1].as_str().unwrap_or("")));
            cambiados = requisitos_cambiados(&anterior, &actual);
        }
        if !cambiados.is_empty() {
            let ids = serde_json::to_string(&cambiados.iter().map(|c| c.0.clone()).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into());
            tareas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('taskCode', "taskCode", 'title', title, 'status', status)), '[]'::jsonb) FROM "BacklogItem" WHERE "solucionId" = $1 AND "prdRequisitoId" IN (SELECT jsonb_array_elements_text($2::text::jsonb))"#, &[B::T(id.clone()), B::T(ids.clone())]).await?;
            pruebas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('reqCodigo', "reqCodigo", 'titulo', titulo, 'estado', estado)), '[]'::jsonb) FROM "PruebaAceptacion" WHERE "solucionId" = $1 AND "reqId" IN (SELECT jsonb_array_elements_text($2::text::jsonb))"#, &[B::T(id.clone()), B::T(ids)]).await?;
        }
    }
    let n_t = tareas.as_array().map(|a| a.len()).unwrap_or(0);
    let n_p = pruebas.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(Json(json!({
        "clave": clave,
        "dependientes": deps,
        "requisitosCambiados": cambiados.iter().map(|c| json!({ "id": c.0, "texto": c.1, "motivo": c.2 })).collect::<Vec<_>>(),
        "tareas": tareas,
        "pruebas": pruebas,
        "resumen": format!("{} artefacto(s) dependen de este; {} requisito(s) cambiaron entre las dos últimas versiones del PRD, con {n_t} tarea(s) y {n_p} prueba(s) de aceptación a revisar.", deps.len(), cambiados.len()),
    })))
}

// ═══════════════════════════ Regenerar ═══════════════════════════
fn recorte(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

async fn regenerar(State(st): State<AppState>, sesion: Session, Path((id, clave)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let tipos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb) FROM "ArtefactoTipo" t WHERE t.activo"#, &[]).await?;
    let tipos: Vec<Value> = tipos.as_array().cloned().unwrap_or_default();
    let tipo = tipos.iter().find(|t| t["clave"] == clave.as_str()).ok_or_else(|| ApiError::not_found("Tipo de artefacto desconocido"))?;
    if tipo["fuente"]["tipo"] != "documento_fase" {
        return Err(ApiError::bad_request("Este tipo no se regenera aquí: vive en otro lugar del hub (PRD, diseño, plan). Edítalo allí o pídele una propuesta a la IA en su pestaña."));
    }
    let fase = tipo["faseDuena"].as_str().ok_or_else(|| ApiError::bad_request("Este tipo no tiene fase dueña definida"))?.to_string();
    let entregable = tipo["entregableClaves"].as_array().and_then(|a| a.first()).and_then(|x| x.as_str()).ok_or_else(|| ApiError::bad_request("Este tipo no tiene un entregable de fase asociado"))?.to_string();
    let nombre_tipo = tipo["nombre"].as_str().unwrap_or("Documento").to_string();

    let sol = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(so) FROM "Solucion" so WHERE so.id = $1"#, &[B::T(id.clone())]).await?.ok_or_else(|| ApiError::not_found("No encontrado"))?;
    // Entradas: lo que debe reflejar el documento.
    let mut entradas_txt = String::new();
    for e in tipo["entradas"].as_array().cloned().unwrap_or_default() {
        let Some(ek) = e.as_str() else { continue };
        let Some(et) = tipos.iter().find(|t| t["clave"] == ek) else { continue };
        let texto = match et["fuente"]["tipo"].as_str().unwrap_or("") {
            "solucion_columna" => {
                let col = et["fuente"]["columna"].as_str().unwrap_or("");
                let mut s = String::new();
                cuerpo(&parse_o_texto(sol[col].as_str().unwrap_or("")), 3, &mut s);
                s
            }
            "prd_requisitos" => {
                let prd = parse_o_texto(sol["prd"].as_str().unwrap_or(""));
                prd["requisitos"].as_array().map(|a| a.iter().enumerate().map(|(i, r)| format!("REQ-{:03}: {} — criterio: {}", i + 1, texto_req(r, &["titulo", "nombre", "texto", "descripcion"]), texto_req(r, &["criterioAceptacion", "criterio", "aceptacion"]))).collect::<Vec<_>>().join("\n")).unwrap_or_default()
            }
            "documento_fase" => {
                let claves = serde_json::to_string(&et["entregableClaves"]).unwrap_or_else(|_| "[]".into());
                fetch_text_opt(&st.pool, r#"SELECT contenido FROM "ProyectoDocumento" WHERE "solucionId" = $1 AND "entregableClave" IN (SELECT jsonb_array_elements_text($2::text::jsonb)) AND btrim(COALESCE(contenido, '')) <> '' ORDER BY "updatedAt" DESC LIMIT 1"#, &[B::T(id.clone()), B::T(claves)]).await?.unwrap_or_default()
            }
            "memoria" => fetch_text_opt(&st.pool, r#"SELECT contenido FROM "ProyectoMemoria" WHERE "solucionId" = $1"#, &[B::T(id.clone())]).await?.unwrap_or_default(),
            _ => String::new(),
        };
        if !texto.trim().is_empty() {
            entradas_txt.push_str(&format!("=== {} ===\n{}\n\n", et["nombre"].as_str().unwrap_or(ek), recorte(texto.trim(), 3500)));
        }
    }
    if entradas_txt.is_empty() {
        return Err(ApiError::bad_request("Las entradas de este documento todavía no tienen contenido: no hay de dónde regenerarlo."));
    }
    let actual_doc = fetch_text_opt(&st.pool, r#"SELECT contenido FROM "ProyectoDocumento" WHERE "solucionId" = $1 AND "faseClave" = $2 AND "entregableClave" = $3"#, &[B::T(id.clone()), B::T(fase.clone()), B::T(entregable.clone())]).await?.unwrap_or_default();

    let sistema = "Eres un redactor técnico de proyectos de software. Actualiza el documento indicado para que refleje el contenido vigente de sus fuentes. Si hay una versión actual, conserva su estructura, su tono y todo lo que siga siendo cierto; cambia solo lo que quedó desactualizado y marca con «(NUEVO)» lo que añadas. Si no hay versión actual, créalo con títulos claros. No inventes datos que no estén en las fuentes: si falta información, deja «Por definir». Responde SOLO con el documento en Markdown, sin explicaciones.";
    let usuario = format!("DOCUMENTO A ACTUALIZAR: {nombre_tipo}\n\nVERSIÓN ACTUAL:\n{}\n\nFUENTES VIGENTES:\n{entradas_txt}", if actual_doc.trim().is_empty() { "(no existe todavía)".to_string() } else { recorte(actual_doc.trim(), 5000) });
    let respuesta = crate::llm::call_open_code(&st, sistema, &usuario, &format!("regenerar-{id}-{clave}"), 3500, 50).await.map_err(|e| ApiError::bad_request(format!("El modelo no respondió: {e}")))?;
    let md = sin_vallas(&respuesta);
    if md.chars().count() < 80 {
        return Err(ApiError::bad_request("El modelo no devolvió un documento utilizable. Vuelve a intentarlo."));
    }
    exec(
        &st.pool,
        r#"INSERT INTO "ProyectoDocumento" (id, "solucionId", "faseClave", "entregableClave", titulo, contenido, version, propuesta, "propuestaPor", "propuestaEn")
           VALUES ($1, $2, $3, $4, $5, '', 1, $6, $7, NOW())
           ON CONFLICT ("solucionId", "faseClave", "entregableClave") DO UPDATE SET propuesta = EXCLUDED.propuesta, "propuestaPor" = EXCLUDED."propuestaPor", "propuestaEn" = NOW()"#,
        &[B::T(crate::util::new_id()), B::T(id), B::T(fase.clone()), B::T(entregable.clone()), B::T(nombre_tipo.chars().take(200).collect()), B::T(md.clone()), B::T(format!("IA (a pedido de {})", nombre_de(&sesion)))],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "fase": fase, "entregable": entregable, "caracteres": md.chars().count() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tipos() -> Vec<Value> {
        vec![
            json!({ "clave": "prd", "entradas": [] }),
            json!({ "clave": "requisitos", "entradas": ["prd"] }),
            json!({ "clave": "diseno", "entradas": ["prd", "requisitos"] }),
            json!({ "clave": "manual", "entradas": ["requisitos", "diseno"] }),
            json!({ "clave": "suelto", "entradas": [] }),
        ]
    }

    #[test]
    fn los_dependientes_incluyen_los_indirectos_con_su_nivel() {
        let d = dependientes(&tipos(), "prd");
        let nivel = |c: &str| d.iter().find(|x| x.0 == c).map(|x| x.1);
        assert_eq!(nivel("requisitos"), Some(1));
        assert_eq!(nivel("diseno"), Some(1));
        assert_eq!(nivel("manual"), Some(2));
        assert_eq!(nivel("suelto"), None);
        assert_eq!(nivel("prd"), None, "no se incluye a sí mismo");
    }

    #[test]
    fn detecta_requisitos_nuevos_modificados_y_eliminados() {
        let antes = json!({ "requisitos": [{ "id": "a", "texto": "Registrar", "criterioAceptacion": "Se guarda" }, { "id": "b", "texto": "Mapa" }, { "id": "c", "texto": "Viejo" }] });
        let ahora = json!({ "requisitos": [{ "id": "a", "texto": "Registrar", "criterioAceptacion": "Se guarda y avisa" }, { "id": "b", "texto": "Mapa" }, { "id": "d", "texto": "Nuevo" }] });
        let c = requisitos_cambiados(&antes, &ahora);
        let m = |id: &str| c.iter().find(|x| x.0 == id).map(|x| x.2);
        assert_eq!(m("a"), Some("MODIFICADO"));
        assert_eq!(m("b"), None);
        assert_eq!(m("c"), Some("ELIMINADO"));
        assert_eq!(m("d"), Some("NUEVO"));
    }

    #[test]
    fn quita_las_vallas_de_codigo() {
        assert_eq!(sin_vallas("```markdown\n# Título\ntexto\n```"), "# Título\ntexto");
        assert_eq!(sin_vallas("# Sin vallas"), "# Sin vallas");
    }
}
