//! Pruebas de aceptación derivadas de los requisitos (MASD-0026-0004).
//!
//! La IA propone escenarios (dado, cuando, entonces) para cada requisito del PRD a partir de su criterio de aceptación; una
//! PERSONA los aprueba o descarta antes de construir (una prueba generada por IA puede validar un error). Los aprobados:
//! (1) entran en el contexto de la tarea que construye ese requisito, con la regla de nombrar cada prueba con su REQ-###;
//! (2) se exportan a `spec/pruebas/REQ-###.md` junto con la spec del repositorio.
//! El código REQ-### es el orden del requisito en el PRD (el mismo que usa la exportación de la spec).

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    routes::solucion_hub::{nombre_de, parse_o_texto},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proyectos/{id}/pruebas-aceptacion", get(listar))
        .route("/api/proyectos/{id}/pruebas-aceptacion/generar", post(generar))
        .route("/api/proyectos/{id}/pruebas-aceptacion/{pid}/decidir", post(decidir))
}

const MAX_REQUISITOS_POR_LLAMADA: usize = 2;

fn codigo(i: usize) -> String {
    format!("REQ-{:03}", i + 1)
}

fn texto_de(r: &Value, claves: &[&str]) -> String {
    claves.iter().find_map(|k| r.get(*k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())).unwrap_or("").to_string()
}

/// Requisitos del PRD con su código: (REQ-###, id, texto, criterio, prioridad).
pub(crate) fn requisitos_de(prd: &Value) -> Vec<(String, Option<String>, String, String, String)> {
    prd["requisitos"]
        .as_array()
        .map(|a| {
            a.iter()
                .enumerate()
                .map(|(i, r)| (codigo(i), r["id"].as_str().map(String::from), texto_de(r, &["titulo", "nombre", "texto", "descripcion"]), texto_de(r, &["criterioAceptacion", "criterio", "aceptacion"]), texto_de(r, &["prioridad"])))
                .collect()
        })
        .unwrap_or_default()
}

/// Lee la respuesta del modelo (un arreglo JSON, con o sin texto alrededor) y devuelve (REQ, título, escenarios en Markdown).
pub(crate) fn parsear_respuesta(texto: &str, permitidos: &[String]) -> Vec<(String, String, String)> {
    let (Some(a), Some(b)) = (texto.find('['), texto.rfind(']')) else { return vec![] };
    if b <= a {
        return vec![];
    }
    let Ok(items) = serde_json::from_str::<Vec<Value>>(&texto[a..=b]) else { return vec![] };
    let mut out = vec![];
    for it in items {
        let req = it["reqCodigo"].as_str().unwrap_or("").trim().to_string();
        if !permitidos.contains(&req) {
            continue;
        }
        let titulo = it["titulo"].as_str().unwrap_or("").trim().chars().take(200).collect::<String>();
        let escenarios: Vec<String> = it["escenarios"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let (d, c, en) = (e["dado"].as_str()?.trim(), e["cuando"].as_str()?.trim(), e["entonces"].as_str()?.trim());
                        if d.is_empty() || c.is_empty() || en.is_empty() {
                            return None;
                        }
                        Some(format!("- **Dado** {d}\n  **Cuando** {c}\n  **Entonces** {en}"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !titulo.is_empty() && !escenarios.is_empty() {
            out.push((req, titulo, escenarios.join("\n")));
        }
    }
    out
}

async fn listar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    let filas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p."reqCodigo", p."createdAt"), '[]'::jsonb) FROM "PruebaAceptacion" p WHERE p."solucionId" = $1"#, &[B::T(id.clone())]).await?;
    let prd = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(prd) FROM "Solucion" WHERE id = $1"#, &[B::T(id)]).await?.ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let reqs = requisitos_de(&parse_o_texto(prd.as_str().unwrap_or("")));
    let lista = filas.as_array().cloned().unwrap_or_default();
    let con_aprobada = reqs.iter().filter(|r| lista.iter().any(|p| p["reqCodigo"] == r.0.as_str() && p["estado"] == "APROBADA")).count();
    Ok(Json(json!({
        "pruebas": lista,
        "requisitos": reqs.iter().map(|r| json!({ "codigo": r.0, "texto": r.2, "criterio": r.3, "prioridad": r.4 })).collect::<Vec<_>>(),
        "cobertura": { "requisitos": reqs.len(), "conPruebaAprobada": con_aprobada },
    })))
}

async fn generar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let prd_txt = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(prd) FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await?.ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let reqs = requisitos_de(&parse_o_texto(prd_txt.as_str().unwrap_or("")));
    if reqs.is_empty() {
        return Err(ApiError::bad_request("El PRD todavía no tiene requisitos"));
    }
    let existentes = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(DISTINCT "reqCodigo"), '[]'::jsonb) FROM "PruebaAceptacion" WHERE "solucionId" = $1 AND estado IN ('APROBADA', 'PROPUESTA')"#, &[B::T(id.clone())]).await?;
    let con_aprobada: Vec<String> = existentes.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let pedido = body["reqCodigo"].as_str().map(str::to_string);
    let candidatos: Vec<_> = reqs
        .iter()
        .filter(|r| match &pedido {
            Some(p) => &r.0 == p,
            None => !con_aprobada.contains(&r.0),
        })
        .collect();
    let restantes_tras_lote = candidatos.len().saturating_sub(MAX_REQUISITOS_POR_LLAMADA);
    let elegidos: Vec<_> = candidatos.into_iter().take(MAX_REQUISITOS_POR_LLAMADA).collect();
    if elegidos.is_empty() {
        return Ok(Json(json!({ "generadas": 0, "mensaje": "Todos los requisitos ya tienen pruebas aprobadas (o el código pedido no existe)." })));
    }
    let permitidos: Vec<String> = elegidos.iter().map(|r| r.0.clone()).collect();
    let usuario = elegidos
        .iter()
        .map(|r| format!("{} · prioridad {} · {}\n   Criterio de aceptación: {}", r.0, if r.4.is_empty() { "—" } else { &r.4 }, r.2, if r.3.is_empty() { "(no definido: propón uno razonable y dilo en el título)" } else { &r.3 }))
        .collect::<Vec<_>>()
        .join("\n");
    let sistema = "Eres un ingeniero de QA. Para cada requisito, escribe pruebas de aceptación como escenarios dado/cuando/entonces, concretos y verificables, con datos de ejemplo reales (no genéricos). Incluye siempre al menos un escenario de error o de límite, y uno de permisos si el requisito toca datos de personas. Responde SOLO con un arreglo JSON, sin explicaciones ni bloques de código: [{\"reqCodigo\":\"REQ-001\",\"titulo\":\"...\",\"escenarios\":[{\"dado\":\"...\",\"cuando\":\"...\",\"entonces\":\"...\"}]}]. Entre 2 y 4 escenarios por requisito. Usa exactamente los códigos REQ recibidos.";
    let respuesta = crate::llm::call_open_code(&st, sistema, &usuario, &format!("pruebas-{id}"), 7000, 56).await.map_err(|e| ApiError::bad_request(format!("El modelo no respondió: {e}")))?;
    let nuevas = parsear_respuesta(&respuesta, &permitidos);
    if nuevas.is_empty() {
        return Err(ApiError::bad_request("El modelo no devolvió pruebas utilizables. Vuelve a intentarlo."));
    }
    let quien = nombre_de(&sesion);
    for cod in &permitidos {
        exec(&st.pool, r#"DELETE FROM "PruebaAceptacion" WHERE "solucionId" = $1 AND "reqCodigo" = $2 AND estado = 'PROPUESTA'"#, &[B::T(id.clone()), B::T(cod.clone())]).await?;
    }
    for (req, titulo, esc) in &nuevas {
        let req_id = reqs.iter().find(|r| &r.0 == req).and_then(|r| r.1.clone());
        exec(
            &st.pool,
            r#"INSERT INTO "PruebaAceptacion" (id, "solucionId", "reqCodigo", "reqId", titulo, escenarios, origen, "creadoPor") VALUES ($1, $2, $3, $4, $5, $6, 'IA', $7)"#,
            &[B::T(new_id()), B::T(id.clone()), B::T(req.clone()), B::OT(req_id), B::T(titulo.clone()), B::T(esc.clone()), B::T(quien.clone())],
        )
        .await?;
    }
    Ok(Json(json!({ "generadas": nuevas.len(), "requisitos": permitidos, "restantes": restantes_tras_lote })))
}

async fn decidir(State(st): State<AppState>, sesion: Session, Path((id, pid)): Path<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let estado = match body["accion"].as_str() {
        Some("aprobar") => "APROBADA",
        Some("descartar") => "DESCARTADA",
        _ => return Err(ApiError::bad_request("accion: aprobar o descartar")),
    };
    let n = exec(
        &st.pool,
        r#"UPDATE "PruebaAceptacion" SET estado = $3, "decididoPor" = $4, "decididoEn" = NOW(), "updatedAt" = NOW() WHERE id = $1 AND "solucionId" = $2"#,
        &[B::T(pid), B::T(id), B::T(estado.into()), B::T(nombre_de(&sesion))],
    )
    .await?;
    if n == 0 {
        return Err(ApiError::not_found("Prueba no encontrada"));
    }
    Ok(Json(json!({ "ok": true, "estado": estado })))
}

/// Bloque de contexto para la tarea que construye un requisito: sus pruebas de aceptación APROBADAS.
pub async fn bloque_para_requisito(st: &AppState, solucion_id: &str, req_id: &str) -> Option<String> {
    let filas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('req', "reqCodigo", 'titulo', titulo, 'escenarios', escenarios) ORDER BY "createdAt"), '[]'::jsonb)
             FROM "PruebaAceptacion" WHERE "solucionId" = $1 AND "reqId" = $2 AND estado = 'APROBADA'"#,
        &[B::T(solucion_id.into()), B::T(req_id.into())],
    )
    .await
    .ok()?;
    let lista = filas.as_array()?;
    if lista.is_empty() {
        return None;
    }
    let req = lista[0]["req"].as_str().unwrap_or("REQ").to_string();
    let mut m = format!("=== PRUEBAS DE ACEPTACIÓN APROBADAS ({req}) ===\n");
    for p in lista {
        m.push_str(&format!("{}\n{}\n", p["titulo"].as_str().unwrap_or(""), p["escenarios"].as_str().unwrap_or("")));
    }
    m.push_str(&format!("REGLA: escribe pruebas automáticas que cubran estos escenarios y nombra cada prueba incluyendo «{req}» (por ejemplo `{req} · mensaje de error con clave incorrecta`). La tarea no está hecha si falta alguno."));
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codigos_siguen_el_orden_del_prd() {
        let prd = json!({ "requisitos": [{ "id": "a", "texto": "Registrar", "criterioAceptacion": "Se guarda" }, { "id": "b", "titulo": "Mapa", "prioridad": "MUST" }] });
        let r = requisitos_de(&prd);
        assert_eq!(r[0].0, "REQ-001");
        assert_eq!(r[1].0, "REQ-002");
        assert_eq!(r[0].3, "Se guarda");
        assert_eq!(r[1].2, "Mapa");
        assert_eq!(r[1].4, "MUST");
    }

    #[test]
    fn parsea_la_respuesta_aunque_traiga_texto_alrededor() {
        let txt = "Aquí tienes:\n[{\"reqCodigo\":\"REQ-001\",\"titulo\":\"Alta\",\"escenarios\":[{\"dado\":\"un cliente nuevo\",\"cuando\":\"se guarda\",\"entonces\":\"aparece en la lista\"}]},{\"reqCodigo\":\"REQ-009\",\"titulo\":\"Ajeno\",\"escenarios\":[{\"dado\":\"a\",\"cuando\":\"b\",\"entonces\":\"c\"}]}]\nListo.";
        let r = parsear_respuesta(txt, &["REQ-001".to_string()]);
        assert_eq!(r.len(), 1, "descarta los códigos que no se pidieron");
        assert_eq!(r[0].0, "REQ-001");
        assert!(r[0].2.contains("**Dado** un cliente nuevo") && r[0].2.contains("**Entonces** aparece en la lista"));
    }

    #[test]
    fn ignora_escenarios_incompletos_y_basura() {
        let txt = "[{\"reqCodigo\":\"REQ-001\",\"titulo\":\"X\",\"escenarios\":[{\"dado\":\"a\",\"cuando\":\"\",\"entonces\":\"c\"}]}]";
        assert!(parsear_respuesta(txt, &["REQ-001".to_string()]).is_empty());
        assert!(parsear_respuesta("sin json", &["REQ-001".to_string()]).is_empty());
    }
}
