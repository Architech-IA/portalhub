//! Fuentes del proyecto y actas enrutadas (MASD-0026-0005).
//!
//! Una FUENTE es evidencia: audio (con su transcripción), nota, acta, documento, correo o investigación. No se edita: se
//! PROCESA. La IA extrae de ella ítems (decisión, acción, pregunta, riesgo, contradicción con el PRD) y cada ítem espera una
//! decisión humana. Aceptar una ACCIÓN crea una tarea en el backlog; aceptar una DECISIÓN crea una propuesta de cambio sobre el
//! artefacto afectado (el acta NO edita la spec); preguntas, riesgos y contradicciones aceptados quedan en el estado vivo.
//! El audio no se transcribe aquí: se pega o se carga su transcripción (o la envía el Documentador de Telegram).

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
        .route("/api/proyectos/{id}/fuentes", get(listar).post(crear))
        .route("/api/proyectos/{id}/fuentes/{fid}", get(ver))
        .route("/api/proyectos/{id}/fuentes/{fid}/procesar", post(procesar))
        .route("/api/proyectos/{id}/acta-items/{iid}/decidir", post(decidir))
}

const TIPOS_FUENTE: [&str; 6] = ["AUDIO", "NOTA", "ACTA", "DOCUMENTO", "CORREO", "INVESTIGACION"];
const TIPOS_ITEM: [&str; 5] = ["DECISION", "ACCION", "PREGUNTA", "RIESGO", "CONTRADICCION"];
const ARTEFACTOS_DESTINO: [&str; 3] = ["prd", "disenoTecnico", "planEjecucion"];
const MAX_CONTENIDO: usize = 60_000;
const MAX_PARA_MODELO: usize = 24_000;
const MAX_ITEMS: usize = 12;

#[derive(Debug, PartialEq)]
pub(crate) struct Item {
    pub tipo: String,
    pub texto: String,
    pub cita: Option<String>,
    pub destino: Option<String>,
    pub responsable: Option<String>,
}

fn limpio(v: &Value, k: &str, max: usize) -> Option<String> {
    v[k].as_str().map(str::trim).filter(|s| !s.is_empty() && *s != "null").map(|s| s.chars().take(max).collect())
}

/// Lee la extracción del modelo: un objeto `{ resumen, items: [...] }` (con o sin texto alrededor).
pub(crate) fn parsear_extraccion(texto: &str) -> Option<(String, Vec<Item>)> {
    let (a, b) = (texto.find('{')?, texto.rfind('}')?);
    if b <= a {
        return None;
    }
    let j: Value = serde_json::from_str(&texto[a..=b]).ok()?;
    let resumen = limpio(&j, "resumen", 600).unwrap_or_default();
    let mut items = vec![];
    for it in j["items"].as_array()? {
        let tipo = it["tipo"].as_str().unwrap_or("").trim().to_uppercase();
        let Some(texto) = limpio(it, "texto", 600) else { continue };
        if !TIPOS_ITEM.contains(&tipo.as_str()) {
            continue;
        }
        let destino = limpio(it, "destinoArtefacto", 40).filter(|d| ARTEFACTOS_DESTINO.contains(&d.as_str()));
        items.push(Item { tipo, texto, cita: limpio(it, "cita", 300), destino, responsable: limpio(it, "responsable", 80) });
        if items.len() >= MAX_ITEMS {
            break;
        }
    }
    Some((resumen, items))
}

fn exigir(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

async fn listar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir(&sesion)?;
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', f.id, 'tipo', f.tipo, 'titulo', f.titulo, 'fecha', f.fecha, 'faseClave', f."faseClave", 'visibilidad', f.visibilidad,
                'estado', f.estado, 'resumen', f.resumen, 'creadoPor', f."creadoPor", 'createdAt', f."createdAt",
                'items', (SELECT COUNT(*) FROM "ActaItem" a WHERE a."fuenteId" = f.id),
                'pendientes', (SELECT COUNT(*) FROM "ActaItem" a WHERE a."fuenteId" = f.id AND a.estado = 'PENDIENTE')) ORDER BY f."createdAt" DESC), '[]'::jsonb)
             FROM "FuenteProyecto" f WHERE f."solucionId" = $1"#,
        &[B::T(id)],
    )
    .await?;
    Ok(Json(json!({ "fuentes": v })))
}

async fn crear(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let tipo = b["tipo"].as_str().unwrap_or("").to_uppercase();
    if !TIPOS_FUENTE.contains(&tipo.as_str()) {
        return Err(ApiError::bad_request("tipo: AUDIO, NOTA, ACTA, DOCUMENTO, CORREO o INVESTIGACION"));
    }
    let titulo = b["titulo"].as_str().map(str::trim).unwrap_or("");
    if titulo.is_empty() {
        return Err(ApiError::bad_request("El título es obligatorio"));
    }
    let contenido = b["contenido"].as_str().unwrap_or("").trim().to_string();
    if contenido.chars().count() > MAX_CONTENIDO {
        return Err(ApiError::bad_request("El contenido no puede superar los 60.000 caracteres"));
    }
    let enlace = limpio(&b, "enlace", 500);
    if contenido.is_empty() && enlace.is_none() {
        return Err(ApiError::bad_request("Pega el texto (o la transcripción) o indica un enlace"));
    }
    let visibilidad = if b["visibilidad"] == "COMPARTIBLE" { "COMPARTIBLE" } else { "INTERNO" };
    let existe = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    if existe.is_none() {
        return Err(ApiError::not_found("No encontrado"));
    }
    let fid = new_id();
    let meta = if b["meta"].is_object() { b["meta"].clone() } else { json!({}) };
    exec(
        &st.pool,
        r#"INSERT INTO "FuenteProyecto" (id, "solucionId", tipo, titulo, contenido, fecha, "faseClave", visibilidad, origen, enlace, meta, "creadoPor")
           VALUES ($1, $2, $3, $4, $5, NULLIF($6, '')::timestamp, $7, $8, 'portal', $9, $10, $11)"#,
        &[
            B::T(fid.clone()),
            B::T(id),
            B::T(tipo),
            B::T(titulo.chars().take(200).collect()),
            B::T(contenido),
            B::T(b["fecha"].as_str().unwrap_or("").trim().to_string()),
            B::OT(limpio(&b, "faseClave", 60)),
            B::T(visibilidad.into()),
            B::OT(enlace),
            B::J(meta),
            B::T(nombre_de(&sesion)),
        ],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "id": fid })))
}

async fn ver(State(st): State<AppState>, sesion: Session, Path((id, fid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    exigir(&sesion)?;
    let f = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(f) FROM "FuenteProyecto" f WHERE f.id = $1 AND f."solucionId" = $2"#, &[B::T(fid.clone()), B::T(id)]).await?.ok_or_else(|| ApiError::not_found("Fuente no encontrada"))?;
    let items = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a."createdAt", a.id), '[]'::jsonb) FROM "ActaItem" a WHERE a."fuenteId" = $1"#, &[B::T(fid)]).await?;
    Ok(Json(json!({ "fuente": f, "items": items })))
}

async fn procesar(State(st): State<AppState>, sesion: Session, Path((id, fid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let f = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(f) FROM "FuenteProyecto" f WHERE f.id = $1 AND f."solucionId" = $2"#, &[B::T(fid.clone()), B::T(id.clone())]).await?.ok_or_else(|| ApiError::not_found("Fuente no encontrada"))?;
    let contenido = f["contenido"].as_str().unwrap_or("").trim().to_string();
    if contenido.is_empty() {
        return Err(ApiError::bad_request("La fuente no tiene texto para procesar (falta la transcripción)"));
    }
    // Resumen del PRD para detectar contradicciones.
    let prd_txt = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(prd) FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await.ok().flatten();
    let prd = parse_o_texto(prd_txt.as_ref().and_then(|v| v.as_str()).unwrap_or(""));
    let mut ctx = String::new();
    for k in ["resumenEjecutivo", "problema"] {
        if let Some(t) = prd[k].as_str().filter(|t| !t.trim().is_empty()) {
            ctx.push_str(&format!("{k}: {}\n", t.chars().take(500).collect::<String>()));
        }
    }
    for (i, r) in prd["requisitos"].as_array().cloned().unwrap_or_default().iter().take(20).enumerate() {
        let t = ["titulo", "nombre", "texto", "descripcion"].iter().find_map(|k| r[*k].as_str()).unwrap_or("");
        ctx.push_str(&format!("REQ-{:03}: {}\n", i + 1, t.chars().take(120).collect::<String>()));
    }
    let tipo = f["tipo"].as_str().unwrap_or("NOTA");
    let sistema = "Eres un analista que extrae lo accionable de reuniones, notas y documentos de un proyecto de software. Responde SOLO con un objeto JSON, sin explicaciones ni bloques de código: {\"resumen\":\"dos frases\",\"items\":[{\"tipo\":\"DECISION|ACCION|PREGUNTA|RIESGO|CONTRADICCION\",\"texto\":\"...\",\"cita\":\"frase literal corta del texto\",\"destinoArtefacto\":\"prd|disenoTecnico|planEjecucion|null\",\"responsable\":\"nombre o null\"}]}. Reglas: extrae SOLO lo que dice el texto, no inventes; DECISION es algo que se acordó y cambia el alcance, el diseño o el plan (indica a qué artefacto afecta); ACCION es algo que alguien debe hacer; PREGUNTA es algo que quedó sin responder; RIESGO es algo que podría salir mal; CONTRADICCION solo si lo dicho choca con el PRD resumido que recibes (explica con cuál requisito). Máximo 12 ítems, en español, sin duplicados.";
    let usuario = format!("PRD RESUMIDO:\n{}\nFUENTE ({tipo}) «{}»:\n{}", if ctx.is_empty() { "(sin PRD todavía)" } else { &ctx }, f["titulo"].as_str().unwrap_or(""), contenido.chars().take(MAX_PARA_MODELO).collect::<String>());
    let respuesta = crate::llm::call_open_code(&st, sistema, &usuario, &format!("fuente-{fid}"), 2500, 50).await.map_err(|e| ApiError::bad_request(format!("El modelo no respondió: {e}")))?;
    let Some((resumen, items)) = parsear_extraccion(&respuesta) else {
        return Err(ApiError::bad_request("No se pudo leer la respuesta del modelo. Vuelve a intentarlo."));
    };
    exec(&st.pool, r#"DELETE FROM "ActaItem" WHERE "fuenteId" = $1 AND estado = 'PENDIENTE'"#, &[B::T(fid.clone())]).await?;
    for it in &items {
        exec(
            &st.pool,
            r#"INSERT INTO "ActaItem" (id, "fuenteId", "solucionId", tipo, texto, cita, "destinoArtefacto", responsable) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
            &[B::T(new_id()), B::T(fid.clone()), B::T(id.clone()), B::T(it.tipo.clone()), B::T(it.texto.clone()), B::OT(it.cita.clone()), B::OT(it.destino.clone()), B::OT(it.responsable.clone())],
        )
        .await?;
    }
    exec(&st.pool, r#"UPDATE "FuenteProyecto" SET estado = 'PROCESADA', resumen = $2, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(fid), B::T(resumen.clone())]).await?;
    let _ = crate::routes::estado_vivo::actualizar(&st, &id, "fuente procesada").await;
    Ok(Json(json!({ "ok": true, "resumen": resumen, "items": items.len() })))
}

async fn decidir(State(st): State<AppState>, sesion: Session, Path((id, iid)): Path<(String, String)>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let aceptar = match b["accion"].as_str() {
        Some("aceptar") => true,
        Some("descartar") => false,
        _ => return Err(ApiError::bad_request("accion: aceptar o descartar")),
    };
    let it = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('item', to_jsonb(a), 'fuente', f.titulo) FROM "ActaItem" a JOIN "FuenteProyecto" f ON f.id = a."fuenteId" WHERE a.id = $1 AND a."solucionId" = $2"#,
        &[B::T(iid.clone()), B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Ítem no encontrado"))?;
    let item = &it["item"];
    if item["estado"] != "PENDIENTE" {
        return Err(ApiError::new(axum::http::StatusCode::CONFLICT, "Este ítem ya fue decidido"));
    }
    let quien = nombre_de(&sesion);
    let texto = item["texto"].as_str().unwrap_or("").to_string();
    let fuente = it["fuente"].as_str().unwrap_or("").to_string();
    let cita = item["cita"].as_str().map(|c| format!(" Cita: «{c}»")).unwrap_or_default();
    let (estado, resultado): (&str, Option<String>) = if !aceptar {
        ("DESCARTADO", None)
    } else {
        match item["tipo"].as_str().unwrap_or("") {
            "ACCION" => {
                let tid = new_id();
                let desc = format!("Acción surgida de «{fuente}».{}{}", item["responsable"].as_str().map(|r| format!(" Responsable: {r}.")).unwrap_or_default(), cita);
                exec(
                    &st.pool,
                    r#"INSERT INTO "BacklogItem" (id, title, description, type, priority, status, "solucionId", "createdAt", "updatedAt") VALUES ($1, $2, $3, 'TASK', 'MEDIUM', 'BACKLOG', $4, NOW(), NOW())"#,
                    &[B::T(tid), B::T(texto.chars().take(200).collect()), B::T(desc), B::T(id.clone())],
                )
                .await?;
                ("ACEPTADO", Some("Tarea creada en el backlog del proyecto".to_string()))
            }
            "DECISION" => {
                let clave = item["destinoArtefacto"].as_str().filter(|c| ARTEFACTOS_DESTINO.contains(c)).unwrap_or("disenoTecnico").to_string();
                let ops = if clave == "disenoTecnico" { json!([{ "op": "agregar", "ruta": "decisiones", "valor": { "decision": texto.chars().take(400).collect::<String>(), "alternativas": "", "justificacion": format!("Acordada en «{fuente}»") } }]) } else { json!([]) };
                exec(
                    &st.pool,
                    r#"INSERT INTO "ArtefactoPropuesta" (id, "solucionId", clave, titulo, detalle, ops, origen, "creadoPor", "createdAt") VALUES ($1, $2, $3, $4, $5, $6, 'ACTA', $7, NOW())"#,
                    &[B::T(new_id()), B::T(id.clone()), B::T(clave.clone()), B::T(format!("Decisión de «{fuente}»").chars().take(200).collect()), B::T(format!("{texto}{cita}")), B::J(ops), B::T(quien.clone())],
                )
                .await?;
                ("ACEPTADO", Some(format!("Propuesta de cambio creada sobre «{clave}»: revísala en Artefactos › Propuestas")))
            }
            _ => ("ACEPTADO", Some("Registrado en el estado vivo del proyecto".to_string())),
        }
    };
    exec(
        &st.pool,
        r#"UPDATE "ActaItem" SET estado = $2, resultado = $3, "decididoPor" = $4, "decididoEn" = NOW() WHERE id = $1"#,
        &[B::T(iid), B::T(estado.into()), B::OT(resultado.clone()), B::T(quien)],
    )
    .await?;
    let _ = crate::routes::estado_vivo::actualizar(&st, &id, "ítem de acta decidido").await;
    Ok(Json(json!({ "ok": true, "estado": estado, "resultado": resultado })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrae_items_validos_y_descarta_lo_demas() {
        let txt = "Claro:\n{\"resumen\":\"Se acordó WhatsApp.\",\"items\":[\
            {\"tipo\":\"decision\",\"texto\":\"Recordatorios solo por WhatsApp\",\"cita\":\"solo WhatsApp\",\"destinoArtefacto\":\"prd\",\"responsable\":null},\
            {\"tipo\":\"ACCION\",\"texto\":\"Enviar lista de vacunas\",\"responsable\":\"Laura\",\"destinoArtefacto\":\"otro\"},\
            {\"tipo\":\"CHISME\",\"texto\":\"no vale\"},\
            {\"tipo\":\"RIESGO\",\"texto\":\"\"}]}";
        let (r, items) = parsear_extraccion(txt).unwrap();
        assert_eq!(r, "Se acordó WhatsApp.");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].tipo, "DECISION");
        assert_eq!(items[0].destino.as_deref(), Some("prd"));
        assert_eq!(items[0].responsable, None, "«null» no es un responsable");
        assert_eq!(items[1].destino, None, "un destino desconocido se ignora");
        assert_eq!(items[1].responsable.as_deref(), Some("Laura"));
    }

    #[test]
    fn respuesta_sin_json_no_produce_items() {
        assert!(parsear_extraccion("no hay nada").is_none());
        assert!(parsear_extraccion("{\"resumen\":\"x\"}").is_none(), "sin la lista de ítems no es válida");
    }

    #[test]
    fn limita_la_cantidad_de_items() {
        let uno = "{\"tipo\":\"PREGUNTA\",\"texto\":\"¿algo?\"}";
        let txt = format!("{{\"resumen\":\"r\",\"items\":[{}]}}", vec![uno; 30].join(","));
        assert_eq!(parsear_extraccion(&txt).unwrap().1.len(), MAX_ITEMS);
    }
}
