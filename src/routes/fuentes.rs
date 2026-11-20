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
    routes::solucion_hub::{es_admin, nombre_de, parse_o_texto},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proyectos/{id}/fuentes", get(listar).post(crear))
        .route("/api/proyectos/{id}/fuentes/{fid}", get(ver))
        .route("/api/proyectos/{id}/fuentes/desde-reunion", post(desde_reunion))
        .route("/api/proyectos/{id}/reuniones-disponibles", get(reuniones_disponibles))
        .route("/api/proyectos/{id}/fuentes/{fid}/purgar", post(purgar))
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
    pub minuto: Option<String>,
}

/// ¿Parece una marca de tiempo (m:ss, mm:ss o h:mm:ss)?
pub(crate) fn es_minuto(s: &str) -> bool {
    let partes: Vec<&str> = s.split(':').collect();
    (partes.len() == 2 || partes.len() == 3) && partes.iter().all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())) && partes.last().map(|p| p.len() == 2).unwrap_or(false)
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
        let minuto = limpio(it, "minuto", 8).map(|m| m.trim_matches(|c| c == '[' || c == ']' || c == '(' || c == ')').to_string()).filter(|m| es_minuto(m));
        items.push(Item { tipo, texto, cita: limpio(it, "cita", 300), destino, responsable: limpio(it, "responsable", 80), minuto });
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
                'meta', f.meta, 'origen', f.origen, 'purgada', COALESCE((f.meta->>'purgada')::boolean, false),
                'vencida', CASE WHEN (f.meta->>'caduca') ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$' THEN (f.meta->>'caduca')::date < CURRENT_DATE ELSE false END,
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
    let mut meta = if b["meta"].is_object() { b["meta"].clone() } else { json!({}) };
    if tipo == "INVESTIGACION" {
        // Una investigación lleva su pregunta, su nivel de confianza y cuándo caduca (pasada esa fecha, queda para revisar).
        let pregunta = limpio(&b, "pregunta", 300).ok_or_else(|| ApiError::bad_request("La investigación necesita su pregunta: ¿qué se quiere saber?"))?;
        let confianza = b["confianza"].as_str().map(|c| c.to_uppercase()).filter(|c| ["ALTA", "MEDIA", "BAJA"].contains(&c.as_str())).unwrap_or_else(|| "MEDIA".into());
        let caduca = limpio(&b, "caduca", 10).filter(|c| c.len() == 10 && c.chars().enumerate().all(|(i, ch)| if i == 4 || i == 7 { ch == '-' } else { ch.is_ascii_digit() }));
        meta = json!({ "pregunta": pregunta, "confianza": confianza, "caduca": caduca });
    }
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
    let sistema = "Eres un analista que extrae lo accionable de reuniones, notas y documentos de un proyecto de software. Responde SOLO con un objeto JSON, sin explicaciones ni bloques de código: {\"resumen\":\"dos frases\",\"items\":[{\"tipo\":\"DECISION|ACCION|PREGUNTA|RIESGO|CONTRADICCION\",\"texto\":\"...\",\"cita\":\"frase literal corta del texto\",\"destinoArtefacto\":\"prd|disenoTecnico|planEjecucion|null\",\"responsable\":\"nombre o null\",\"minuto\":\"mm:ss o null\"}]}. Reglas: si el texto trae marcas de tiempo como [12:34] o (12:34), en «minuto» pon la más cercana a la frase citada (si no hay marcas, null); extrae SOLO lo que dice el texto, no inventes; DECISION es algo que se acordó y cambia el alcance, el diseño o el plan (indica a qué artefacto afecta); ACCION es algo que alguien debe hacer; PREGUNTA es algo que quedó sin responder; RIESGO es algo que podría salir mal; CONTRADICCION solo si lo dicho choca con el PRD resumido que recibes (explica con cuál requisito). Máximo 12 ítems, en español, sin duplicados.";
    let usuario = format!("PRD RESUMIDO:\n{}\nFUENTE ({tipo}) «{}»:\n{}", if ctx.is_empty() { "(sin PRD todavía)" } else { &ctx }, f["titulo"].as_str().unwrap_or(""), contenido.chars().take(MAX_PARA_MODELO).collect::<String>());
    let respuesta = crate::llm::call_open_code(&st, sistema, &usuario, &format!("fuente-{fid}"), 2500, 50).await.map_err(|e| ApiError::bad_request(format!("El modelo no respondió: {e}")))?;
    let Some((resumen, items)) = parsear_extraccion(&respuesta) else {
        return Err(ApiError::bad_request("No se pudo leer la respuesta del modelo. Vuelve a intentarlo."));
    };
    exec(&st.pool, r#"DELETE FROM "ActaItem" WHERE "fuenteId" = $1 AND estado = 'PENDIENTE'"#, &[B::T(fid.clone())]).await?;
    for it in &items {
        exec(
            &st.pool,
            r#"INSERT INTO "ActaItem" (id, "fuenteId", "solucionId", tipo, texto, cita, "destinoArtefacto", responsable, minuto) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
            &[B::T(new_id()), B::T(fid.clone()), B::T(id.clone()), B::T(it.tipo.clone()), B::T(it.texto.clone()), B::OT(it.cita.clone()), B::OT(it.destino.clone()), B::OT(it.responsable.clone()), B::OT(it.minuto.clone())],
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

// ═══════════════════════════ Reuniones del calendario como fuente ═══════════════════════════
/// Texto de una reunión del calendario: datos básicos, notas, lo registrado en su hub (objetivo, puntos, notas, decisiones) y sus acciones.
pub(crate) fn texto_de_reunion(m: &Value, acciones: &[Value]) -> String {
    let mut t = format!("# {}\n\n", m["title"].as_str().unwrap_or("Reunión"));
    let datos: Vec<String> = [("Fecha", "date"), ("Lugar", "location"), ("Asistentes", "attendees")].iter().filter_map(|(r, k)| m[*k].as_str().filter(|s| !s.trim().is_empty()).map(|s| format!("{r}: {}", s.trim()))).collect();
    if !datos.is_empty() {
        t.push_str(&format!("{}\n\n", datos.join(" · ")));
    }
    for (rotulo, k) in [("Descripción", "description"), ("Notas", "notes")] {
        if let Some(s) = m[k].as_str().filter(|s| !s.trim().is_empty()) {
            t.push_str(&format!("## {rotulo}\n\n{}\n\n", s.trim()));
        }
    }
    let mut hub = parse_o_texto(m["hub"].as_str().unwrap_or(""));
    if let Some(o) = hub.as_object_mut() {
        o.remove("archivos");
        // Las notas del hub viajan como un JSON dentro de un texto: se abre para leerlas.
        if let Some(n) = o.get("notas").and_then(|n| n.as_str()).map(parse_o_texto) {
            o.insert("notas".into(), n);
        }
        if !o.is_empty() {
            t.push_str("## Hub de la reunión\n\n");
            crate::motor::spec_export::cuerpo(&hub, 3, &mut t);
        }
    }
    if !acciones.is_empty() {
        t.push_str("## Acciones\n\n");
        for a in acciones {
            let extra: Vec<String> = [("responsable", "responsable"), ("fecha límite", "fechaLimite"), ("estado", "estado")].iter().filter_map(|(r, k)| a[*k].as_str().filter(|s| !s.is_empty()).map(|s| format!("{r}: {s}"))).collect();
            t.push_str(&format!("- {}{}\n", a["texto"].as_str().unwrap_or(""), if extra.is_empty() { String::new() } else { format!(" ({})", extra.join(", ")) }));
        }
    }
    t
}

/// ¿Puede esta persona leer esa reunión? (Administración, quien la creó o quien está entre los asistentes.)
fn puede_ver_reunion(sesion: &Session, m: &Value) -> bool {
    if es_admin(sesion) {
        return true;
    }
    m["userId"].as_str() == Some(sesion.id.as_str()) || (!sesion.email.is_empty() && m["attendees"].as_str().map(|a| a.to_lowercase().contains(&sesion.email.to_lowercase())).unwrap_or(false))
}

async fn reuniones_disponibles(State(st): State<AppState>, sesion: Session, Path(_id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let todas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.date DESC), '[]'::jsonb) FROM (SELECT id, title, date, type, "userId", attendees, (length(COALESCE(hub, '')) > 20 OR length(COALESCE(notes, '')) > 0 OR "actaFile" IS NOT NULL) AS con_contenido FROM "Meeting" ORDER BY date DESC LIMIT 60) m"#, &[]).await?;
    let lista: Vec<Value> = todas
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| puede_ver_reunion(&sesion, m))
        .take(30)
        .map(|m| json!({ "id": m["id"], "titulo": m["title"], "fecha": m["date"], "tipo": m["type"], "conContenido": m["con_contenido"] }))
        .collect();
    Ok(Json(json!({ "reuniones": lista })))
}

async fn desde_reunion(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let mid = b["meetingId"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| ApiError::bad_request("meetingId requerido"))?.to_string();
    let r = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('m', to_jsonb(mt), 'acciones', COALESCE((SELECT jsonb_agg(jsonb_build_object('texto', a.texto, 'responsable', a.responsable, 'fechaLimite', a."fechaLimite", 'estado', a.estado) ORDER BY a."createdAt") FROM "MeetingAction" a WHERE a."meetingId" = mt.id), '[]'::jsonb))
             FROM "Meeting" mt WHERE mt.id = $1"#,
        &[B::T(mid.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Reunión no encontrada"))?;
    let m = &r["m"];
    if !puede_ver_reunion(&sesion, m) {
        return Err(ApiError::forbidden("No participas en esa reunión"));
    }
    let acciones: Vec<Value> = r["acciones"].as_array().cloned().unwrap_or_default();
    let texto = texto_de_reunion(m, &acciones);
    if texto.trim().chars().count() < 60 || (m["hub"].is_null() && m["notes"].is_null() && acciones.is_empty()) {
        return Err(ApiError::bad_request("La reunión no tiene notas, decisiones ni acciones registradas todavía."));
    }
    if texto.chars().count() > MAX_CONTENIDO {
        return Err(ApiError::bad_request("La reunión es demasiado larga para importarla (más de 60.000 caracteres)."));
    }
    let origen = format!("reunion:{mid}");
    let ya = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('id', id, 'estado', estado) FROM "FuenteProyecto" WHERE "solucionId" = $1 AND origen = $2"#, &[B::T(id.clone()), B::T(origen.clone())]).await?;
    if let Some(y) = ya {
        if y["estado"] == "SIN_PROCESAR" {
            exec(&st.pool, r#"UPDATE "FuenteProyecto" SET contenido = $2, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(y["id"].as_str().unwrap_or("").into()), B::T(texto)]).await?;
        }
        return Ok(Json(json!({ "ok": true, "id": y["id"], "yaExistia": true })));
    }
    let fid = new_id();
    exec(
        &st.pool,
        r#"INSERT INTO "FuenteProyecto" (id, "solucionId", tipo, titulo, contenido, fecha, "faseClave", visibilidad, origen, meta, "creadoPor")
           VALUES ($1, $2, 'ACTA', $3, $4, NULLIF($5, '')::timestamp, $6, 'INTERNO', $7, $8, $9)"#,
        &[
            B::T(fid.clone()),
            B::T(id),
            B::T(format!("Reunión: {}", m["title"].as_str().unwrap_or("sin título")).chars().take(200).collect()),
            B::T(texto),
            B::T(m["date"].as_str().unwrap_or("").to_string()),
            B::OT(limpio(&b, "faseClave", 60)),
            B::T(origen),
            B::J(json!({ "reunionId": mid })),
            B::T(nombre_de(&sesion)),
        ],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "id": fid, "yaExistia": false })))
}

/// Borra el texto de una fuente (por ejemplo una transcripción que ya cumplió su plazo de retención) y conserva lo que se extrajo de ella.
async fn purgar(State(st): State<AppState>, sesion: Session, Path((id, fid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    if !es_admin(&sesion) {
        return Err(ApiError::forbidden("No autorizado"));
    }
    let n = exec(&st.pool, r#"UPDATE "FuenteProyecto" SET contenido = '', enlace = NULL, meta = meta || '{"purgada": true}'::jsonb, "updatedAt" = NOW() WHERE id = $1 AND "solucionId" = $2"#, &[B::T(fid), B::T(id)]).await?;
    if n == 0 {
        return Err(ApiError::not_found("Fuente no encontrada"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_marcas_de_tiempo() {
        assert!(es_minuto("12:34") && es_minuto("1:05") && es_minuto("01:02:03"));
        assert!(!es_minuto("12") && !es_minuto("12:3") && !es_minuto("abc:de") && !es_minuto("::") && !es_minuto("123:45"));
    }

    #[test]
    fn el_minuto_se_lee_aunque_venga_entre_corchetes() {
        let txt = "{\"resumen\":\"r\",\"items\":[{\"tipo\":\"ACCION\",\"texto\":\"Enviar la lista\",\"minuto\":\"[12:34]\"},{\"tipo\":\"RIESGO\",\"texto\":\"Retraso\",\"minuto\":\"pronto\"}]}";
        let (_, items) = parsear_extraccion(txt).unwrap();
        assert_eq!(items[0].minuto.as_deref(), Some("12:34"));
        assert_eq!(items[1].minuto, None);
    }

    #[test]
    fn arma_el_texto_de_una_reunion_con_hub_y_acciones() {
        let hub = json!({ "objetivo": "Acordar el alcance", "puntos": [{ "id": "p1", "texto": "Recordatorios" }], "notas": json!({ "tabs": [{ "titulo": "General", "contenido": "Se habló de WhatsApp" }] }).to_string(), "archivos": ["x.pdf"] });
        let m = json!({ "title": "Alcance", "date": "2026-11-18", "attendees": "laura@x.co", "hub": hub.to_string(), "notes": "Notas sueltas" });
        let t = texto_de_reunion(&m, &[json!({ "texto": "Enviar vacunas", "responsable": "Laura", "estado": "PENDIENTE" })]);
        assert!(t.starts_with("# Alcance") && t.contains("Asistentes: laura@x.co") && t.contains("Acordar el alcance"));
        assert!(t.contains("Se habló de WhatsApp"), "abre las notas del hub");
        assert!(t.contains("- Enviar vacunas (responsable: Laura, estado: PENDIENTE)"));
        assert!(!t.contains("x.pdf"), "no incluye los archivos adjuntos");
    }

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
