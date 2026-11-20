//! Gateway de mensajería de los agentes (MASD-0028-0002/0003/0004).
//!
//! Un solo modelo para cualquier canal: persona (con una identidad por canal), hilo y mensaje. Los conectores (Telegram,
//! correo, portal) solo traducen hacia y desde `POST /api/gateway/entrada` y `/salida`; la memoria, el contexto y la
//! trazabilidad viven aquí y no dependen del canal.
//!
//! - Entrada: resuelve a la persona, comprueba con la matriz de comunicación que el agente puede hablar con ella, abre o
//!   continúa el hilo, guarda el mensaje (idempotente por id del canal) y devuelve el contexto en capas.
//! - Salida: guarda la respuesta del agente con su decisión y sus fuentes; según el nivel de autonomía puede quedar
//!   PENDIENTE de aprobación y el conector no la entrega hasta que una persona la apruebe.
//! - Memoria en capas: conversación, agente y proyecto (el estado vivo de `ProyectoMemoria` es la base del proyecto).

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    routes::solucion_hub::nombre_de,
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/gateway/entrada", post(entrada))
        .route("/api/gateway/salida", post(salida))
        .route("/api/gateway/mensajes/{id}/entregado", post(marcar_entregado))
        .route("/api/gateway/aprobaciones", get(aprobaciones))
        .route("/api/gateway/decisiones/{id}/decidir", post(decidir))
        .route("/api/gateway/hilos", get(listar_hilos))
        .route("/api/gateway/hilos/{id}", get(ver_hilo))
        .route("/api/gateway/memoria", get(leer_memoria).post(escribir_memoria))
        .route("/api/gateway/personas/vincular", post(vincular_persona))
        .route("/api/gateway/identidades", get(identidades))
        .route("/api/gateway/correo/{id}", post(asignar_correo))
        .route("/api/agentes/{slug}/contexto", get(contexto_ruta))
}

const CANALES: [&str; 3] = ["TELEGRAM", "CORREO", "PORTAL"];
const AMBITOS: [&str; 3] = ["CONVERSACION", "AGENTE", "PROYECTO"];
const PRESUPUESTO_CONTEXTO: usize = 16_000;

// ── Reglas puras (con pruebas) ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct Regla {
    pub tipo: String,
    pub destino: String,
    pub direccion: String,
    pub canales: Vec<String>,
    pub requiere_aprobacion: bool,
}

pub(crate) fn reglas_de(v: &Value) -> Vec<Regla> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|r| Regla {
                    tipo: r["tipoDestino"].as_str().unwrap_or("").to_string(),
                    destino: r["destino"].as_str().unwrap_or("").to_string(),
                    direccion: r["direccion"].as_str().unwrap_or("AMBAS").to_string(),
                    canales: r["canales"].as_array().map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                    requiere_aprobacion: r["requiereAprobacion"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// ¿Puede esta persona hablar con el agente por este canal? Sin reglas hacia personas, solo el EQUIPO.
/// `Ok(Some(regla))` cuando una regla lo autoriza, `Ok(None)` cuando entra por el valor por defecto, `Err` si no puede.
pub(crate) fn regla_entrada<'a>(reglas: &'a [Regla], canal: &str, persona_id: &str, persona_tipo: &str) -> Result<Option<&'a Regla>, &'static str> {
    let hacia_personas: Vec<&Regla> = reglas.iter().filter(|r| r.tipo == "ROL" || r.tipo == "PERSONA").collect();
    if hacia_personas.is_empty() {
        return if persona_tipo == "EQUIPO" { Ok(None) } else { Err("Este agente solo atiende al equipo interno") };
    }
    hacia_personas
        .into_iter()
        .find(|r| (r.direccion == "ENTRADA" || r.direccion == "AMBAS") && (r.canales.is_empty() || r.canales.iter().any(|c| c == canal)) && ((r.tipo == "ROL" && r.destino == persona_tipo) || (r.tipo == "PERSONA" && r.destino == persona_id)))
        .map(Some)
        .ok_or("El agente no tiene permiso para hablar con esta persona por este canal")
}

/// Acciones de solo lectura: nunca cambian nada fuera del hilo.
pub(crate) fn es_lectura(accion: &str) -> bool {
    matches!(accion, "responder" | "consultar" | "consulta")
}

pub(crate) fn nivel_efectivo(autonomia: &str, por_accion: &Value, accion: &str) -> String {
    por_accion[accion].as_str().filter(|n| ["ASISTIDA", "SUPERVISADA", "AUTONOMA"].contains(n)).unwrap_or(autonomia).to_string()
}

/// ¿Sale ya (true) o queda pendiente de aprobación (false)?
///  - AUTONOMA: sale todo.
///  - SUPERVISADA: salen las lecturas; lo que cambia algo espera aprobación.
///  - ASISTIDA: nada sale solo, salvo responderle a una persona del equipo con una lectura.
///  - Una regla de comunicación con «requiere aprobación» fuerza la espera en cualquier nivel.
pub(crate) fn sale_directo(nivel: &str, accion: &str, destinatario_equipo: bool, regla_exige: bool) -> bool {
    if regla_exige {
        return false;
    }
    match nivel {
        "AUTONOMA" => true,
        "SUPERVISADA" => es_lectura(accion),
        _ => es_lectura(accion) && destinatario_equipo,
    }
}

pub(crate) fn recortar(texto: &str, max: usize) -> String {
    if texto.chars().count() <= max {
        texto.to_string()
    } else {
        format!("{}…", texto.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Elige, de más nuevo a más viejo, los mensajes que caben en el presupuesto y los devuelve en orden cronológico.
pub(crate) fn caben(mensajes: &[Value], presupuesto: usize) -> Vec<Value> {
    let mut usado = 0;
    let mut elegidos: Vec<Value> = vec![];
    for m in mensajes.iter().rev() {
        let largo = m["texto"].as_str().map(|t| t.chars().count()).unwrap_or(0).min(1500) + 40;
        if usado + largo > presupuesto {
            break;
        }
        usado += largo;
        let mut c = m.clone();
        if let Some(t) = m["texto"].as_str() {
            c["texto"] = json!(recortar(t, 1500));
        }
        elegidos.push(c);
    }
    elegidos.reverse();
    elegidos
}

// ── Personas ─────────────────────────────────────────────────────────────────────────────────────

fn exigir_servicio_o_usuario(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

/// Persona de una identidad; la crea si no existe. Las del portal son del EQUIPO y están verificadas; las de los demás
/// canales entran como EXTERNO sin verificar hasta que un administrador las vincule a un usuario.
async fn resolver_persona(st: &AppState, canal: &str, externo_id: &str, nombre: &str) -> Result<Value, ApiError> {
    let buscar = r#"SELECT jsonb_build_object('id', p.id, 'tipo', p.tipo, 'nombre', p.nombre, 'verificada', i.verificada)
                    FROM "GwIdentidad" i JOIN "GwPersona" p ON p.id = i."personaId" WHERE i.canal = $1 AND i."externoId" = $2"#;
    if let Some(p) = fetch_json_opt(&st.pool, buscar, &[B::T(canal.into()), B::T(externo_id.into())]).await? {
        return Ok(p);
    }
    let (tipo, user_id, verificada) = if canal == "PORTAL" { ("EQUIPO", Some(externo_id.to_string()), true) } else { ("EXTERNO", None, false) };
    let persona_id = if let Some(uid) = &user_id {
        match fetch_text_opt(&st.pool, r#"SELECT id FROM "GwPersona" WHERE "userId" = $1"#, &[B::T(uid.clone())]).await? {
            Some(id) => id,
            None => {
                let id = new_id();
                exec(&st.pool, r#"INSERT INTO "GwPersona" (id, nombre, tipo, "userId") VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING"#, &[B::T(id.clone()), B::T(nombre.into()), B::T(tipo.into()), B::T(uid.clone())]).await?;
                fetch_text_opt(&st.pool, r#"SELECT id FROM "GwPersona" WHERE "userId" = $1"#, &[B::T(uid.clone())]).await?.unwrap_or(id)
            }
        }
    } else {
        let id = new_id();
        exec(&st.pool, r#"INSERT INTO "GwPersona" (id, nombre, tipo) VALUES ($1, $2, $3)"#, &[B::T(id.clone()), B::T(nombre.into()), B::T(tipo.into())]).await?;
        id
    };
    exec(&st.pool, r#"INSERT INTO "GwIdentidad" (id, "personaId", canal, "externoId", verificada) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (canal, "externoId") DO NOTHING"#, &[B::T(new_id()), B::T(persona_id), B::T(canal.into()), B::T(externo_id.into()), B::Bo(verificada)]).await?;
    fetch_json_opt(&st.pool, buscar, &[B::T(canal.into()), B::T(externo_id.into())]).await?.ok_or_else(|| ApiError::internal("No se pudo resolver la persona"))
}

/// Un administrador une una identidad de canal (Telegram, correo) con un usuario del portal: la persona pasa a EQUIPO.
async fn vincular_persona(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let user_id = body["userId"].as_str().unwrap_or("").to_string();
    let canal = body["canal"].as_str().unwrap_or("").to_uppercase();
    let externo = body["externoId"].as_str().unwrap_or("").trim().to_string();
    if !CANALES.contains(&canal.as_str()) || canal == "PORTAL" || externo.is_empty() {
        return Err(ApiError::bad_request("Indica un canal (TELEGRAM o CORREO) y el id de la persona en ese canal"));
    }
    let nombre = fetch_text_opt(&st.pool, r#"SELECT name FROM "User" WHERE id = $1"#, &[B::T(user_id.clone())]).await?.ok_or_else(|| ApiError::bad_request("El usuario no existe"))?;
    let persona = resolver_persona(&st, "PORTAL", &user_id, &nombre).await?;
    let pid = persona["id"].as_str().unwrap_or("").to_string();
    exec(
        &st.pool,
        r#"INSERT INTO "GwIdentidad" (id, "personaId", canal, "externoId", verificada) VALUES ($1, $2, $3, $4, true)
           ON CONFLICT (canal, "externoId") DO UPDATE SET "personaId" = EXCLUDED."personaId", verificada = true"#,
        &[B::T(new_id()), B::T(pid.clone()), B::T(canal), B::T(externo)],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "personaId": pid })))
}

// ── Contexto en capas ────────────────────────────────────────────────────────────────────────────

async fn memoria_de(st: &AppState, ambito: &str, ref_id: &str, limite: i64) -> Result<Value, ApiError> {
    Ok(fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('clave', clave, 'contenido', contenido, 'en', "creadoEn") ORDER BY "creadoEn" DESC), '[]'::jsonb)
           FROM (SELECT * FROM "GwMemoria" WHERE ambito = $1 AND "refId" = $2 AND vigente ORDER BY "creadoEn" DESC LIMIT $3) x"#,
        &[B::T(ambito.into()), B::T(ref_id.into()), B::I(limite)],
    )
    .await?)
}

pub(crate) async fn contexto(st: &AppState, slug: &str, hilo_id: Option<&str>) -> Result<Value, ApiError> {
    let identidad = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('alma', f.alma, 'alcance', f.alcance, 'herramientas', f.herramientas, 'autonomia', f.autonomia, 'autonomiaPorAccion', f."autonomiaPorAccion", 'superior', f."superiorSlug")
           FROM "AgenteFicha" f WHERE f.slug = $1"#,
        &[B::T(slug.into())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("El agente no tiene ficha"))?;

    let mut solucion_id: Option<String> = None;
    let mut mensajes = vec![];
    let mut mem_conv = json!([]);
    if let Some(h) = hilo_id {
        solucion_id = fetch_text_opt(&st.pool, r#"SELECT "solucionId" FROM "GwHilo" WHERE id = $1"#, &[B::T(h.into())]).await?.filter(|s| !s.is_empty());
        let m = fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('direccion', m.direccion, 'autor', m."autorTipo", 'agente', m."agenteSlug", 'texto', m.texto, 'en', m."creadoEn") ORDER BY m."creadoEn"), '[]'::jsonb)
               FROM (SELECT * FROM "GwMensaje" WHERE "hiloId" = $1 AND estado <> 'RECHAZADO' ORDER BY "creadoEn" DESC LIMIT 40) m"#,
            &[B::T(h.into())],
        )
        .await?;
        mensajes = m.as_array().cloned().unwrap_or_default();
        mem_conv = memoria_de(st, "CONVERSACION", h, 10).await?;
    }
    let mem_agente = memoria_de(st, "AGENTE", slug, 10).await?;
    let (estado_proyecto, mem_proyecto) = match &solucion_id {
        Some(s) => (
            fetch_text_opt(&st.pool, r#"SELECT contenido FROM "ProyectoMemoria" WHERE "solucionId" = $1"#, &[B::T(s.clone())]).await?.map(|t| recortar(&t, 6000)),
            memoria_de(st, "PROYECTO", s, 10).await?,
        ),
        None => (None, json!([])),
    };
    // El presupuesto se reparte: lo fijo (alma, memorias, proyecto) primero; la conversación ocupa lo que queda.
    let fijo = identidad.to_string().chars().count() + mem_conv.to_string().chars().count() + mem_agente.to_string().chars().count() + mem_proyecto.to_string().chars().count() + estado_proyecto.as_ref().map(|t| t.chars().count()).unwrap_or(0);
    let libre = PRESUPUESTO_CONTEXTO.saturating_sub(fijo).max(2000);
    Ok(json!({
        "agente": identidad,
        "conversacion": caben(&mensajes, libre),
        "memoria": { "conversacion": mem_conv, "agente": mem_agente, "proyecto": mem_proyecto },
        "proyecto": solucion_id.map(|s| json!({ "solucionId": s, "estadoVivo": estado_proyecto })),
    }))
}

async fn contexto_ruta(State(st): State<AppState>, sesion: Session, Path(slug): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    Ok(Json(contexto(&st, &slug, q.get("hiloId").map(String::as_str)).await?))
}

// ── Entrada ──────────────────────────────────────────────────────────────────────────────────────

async fn entrada(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    // Una persona del portal solo habla como ella misma por el canal PORTAL; los conectores indican el canal y la identidad.
    let (canal, externo_id, nombre) = if sesion.is_service {
        (body["canal"].as_str().unwrap_or("").to_uppercase(), body["externoId"].as_str().unwrap_or("").trim().to_string(), body["nombre"].as_str().unwrap_or("Sin nombre").trim().to_string())
    } else {
        ("PORTAL".to_string(), sesion.id.clone(), nombre_de(&sesion))
    };
    if !CANALES.contains(&canal.as_str()) {
        return Err(ApiError::bad_request("Canal no válido (TELEGRAM, CORREO o PORTAL)"));
    }
    if externo_id.is_empty() {
        return Err(ApiError::bad_request("Falta la identidad de la persona en el canal (externoId)"));
    }
    let limpio = |k: &str| body[k].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let e = Entrada {
        canal,
        externo_id,
        nombre,
        slug: body["agente"].as_str().unwrap_or("").trim().to_string(),
        texto: body["texto"].as_str().unwrap_or("").trim().to_string(),
        canal_hilo: limpio("canalHiloId"),
        canal_msg: limpio("canalMensajeId"),
        solucion_id: limpio("solucionId"),
        asunto: limpio("asunto"),
        adjuntos: if body["adjuntos"].is_array() { body["adjuntos"].clone() } else { json!([]) },
        ya_autorizado: sesion.is_service && body["yaAutorizado"].as_bool() == Some(true),
    };
    Ok(Json(registrar_entrada(&st, e).await?))
}

/// Un mensaje que llega por cualquier canal.
pub(crate) struct Entrada {
    pub canal: String,
    pub externo_id: String,
    pub nombre: String,
    pub slug: String,
    pub texto: String,
    pub canal_hilo: Option<String>,
    pub canal_msg: Option<String>,
    pub solucion_id: Option<String>,
    pub asunto: Option<String>,
    pub adjuntos: Value,
    /// El conector ya comprobó a la persona con su propia lista (los bots de Telegram): solo se registra, sin pasar por la matriz.
    pub ya_autorizado: bool,
}

pub(crate) async fn registrar_entrada(st: &AppState, e: Entrada) -> Result<Value, ApiError> {
    let Entrada { canal, externo_id, nombre, slug, texto, canal_hilo, canal_msg, solucion_id, asunto, adjuntos, ya_autorizado } = e;
    if !CANALES.contains(&canal.as_str()) {
        return Err(ApiError::bad_request("Canal no válido (TELEGRAM, CORREO o PORTAL)"));
    }
    if externo_id.is_empty() {
        return Err(ApiError::bad_request("Falta la identidad de la persona en el canal (externoId)"));
    }
    if texto.is_empty() {
        return Err(ApiError::bad_request("El mensaje está vacío"));
    }
    let activo = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(a.status) FROM "Agent" a LEFT JOIN "AgenteConfig" c ON c.slug = a.slug WHERE a.slug = $1 AND COALESCE(c.activo, true)"#, &[B::T(slug.clone())]).await?;
    match activo {
        None => return Err(ApiError::bad_request("El agente no existe o está desactivado")),
        Some(s) if s.as_str() != Some("ACTIVE") => return Err(ApiError::bad_request("El agente no está activo")),
        _ => {}
    }
    let persona = resolver_persona(&st, &canal, &externo_id, &nombre).await?;
    let persona_id = persona["id"].as_str().unwrap_or("").to_string();
    let persona_tipo = persona["tipo"].as_str().unwrap_or("EXTERNO").to_string();

    let reglas = reglas_de(&fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(c)), '[]'::jsonb) FROM "AgenteComunicacion" c WHERE c."agenteSlug" = $1"#, &[B::T(slug.clone())]).await?);
    if !ya_autorizado {
        if let Err(motivo) = regla_entrada(&reglas, &canal, &persona_id, &persona_tipo) {
            tracing::warn!("[gateway] entrada rechazada: agente={slug} canal={canal} persona_tipo={persona_tipo}: {motivo}");
            return Err(ApiError::forbidden(motivo));
        }
    }

    // Hilo: continúa el del canal si ya existe; si no, abre uno.
    let mut hilo_id: Option<String> = None;
    if let Some(ch) = &canal_hilo {
        hilo_id = fetch_text_opt(&st.pool, r#"SELECT id FROM "GwHilo" WHERE canal = $1 AND "canalHiloId" = $2 AND "agenteSlug" = $3"#, &[B::T(canal.clone()), B::T(ch.clone()), B::T(slug.clone())]).await?;
    }
    let hilo_id = match hilo_id {
        Some(h) => h,
        None => {
            let h = new_id();
            let asunto = asunto.as_ref().map(|s| recortar(s, 200)).unwrap_or_else(|| recortar(&texto, 80));
            let creado = exec(
                &st.pool,
                r#"INSERT INTO "GwHilo" (id, canal, "personaId", "agenteSlug", "solucionId", asunto, "canalHiloId") VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
                &[B::T(h.clone()), B::T(canal.clone()), B::T(persona_id.clone()), B::T(slug.clone()), B::OT(solucion_id.clone()), B::T(asunto), B::OT(canal_hilo.clone())],
            )
            .await;
            match creado {
                Ok(_) => h,
                // Carrera: otro mensaje del mismo chat creó el hilo a la vez.
                Err(_) => fetch_text_opt(&st.pool, r#"SELECT id FROM "GwHilo" WHERE canal = $1 AND "canalHiloId" = $2 AND "agenteSlug" = $3"#, &[B::T(canal.clone()), B::OT(canal_hilo.clone()), B::T(slug.clone())]).await?.ok_or_else(|| ApiError::internal("No se pudo abrir el hilo"))?,
            }
        }
    };
    if let Some(s) = &solucion_id {
        exec(&st.pool, r#"UPDATE "GwHilo" SET "solucionId" = $2 WHERE id = $1 AND "solucionId" IS NULL"#, &[B::T(hilo_id.clone()), B::T(s.clone())]).await?;
    }

    let mensaje_id = new_id();
    let insertados = exec(
        &st.pool,
        r#"INSERT INTO "GwMensaje" (id, "hiloId", direccion, "autorTipo", "personaId", texto, adjuntos, "canalMensajeId") VALUES ($1, $2, 'ENTRADA', 'PERSONA', $3, $4, $5, $6)
           ON CONFLICT ("hiloId", "canalMensajeId") WHERE "canalMensajeId" IS NOT NULL DO NOTHING"#,
        &[B::T(mensaje_id.clone()), B::T(hilo_id.clone()), B::T(persona_id.clone()), B::T(texto), B::J(adjuntos), B::OT(canal_msg.clone())],
    )
    .await?;
    let duplicado = insertados == 0;
    let mensaje_id = if duplicado {
        fetch_text_opt(&st.pool, r#"SELECT id FROM "GwMensaje" WHERE "hiloId" = $1 AND "canalMensajeId" = $2"#, &[B::T(hilo_id.clone()), B::OT(canal_msg)]).await?.unwrap_or(mensaje_id)
    } else {
        exec(&st.pool, r#"UPDATE "GwHilo" SET "ultimoEn" = now() WHERE id = $1"#, &[B::T(hilo_id.clone())]).await?;
        mensaje_id
    };
    let ctx = contexto(st, &slug, Some(&hilo_id)).await?;
    Ok(json!({ "hiloId": hilo_id, "mensajeId": mensaje_id, "duplicado": duplicado, "persona": persona, "agente": slug, "contexto": ctx }))
}

// ── Salida y aprobación ──────────────────────────────────────────────────────────────────────────

async fn salida(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    // Los conectores que no guardan el id del hilo lo piden por canal + chat + agente.
    let mut hilo_id = body["hiloId"].as_str().unwrap_or("").to_string();
    if hilo_id.is_empty() {
        if let (Some(c), Some(ch), Some(ag)) = (body["canal"].as_str(), body["canalHiloId"].as_str(), body["agente"].as_str()) {
            hilo_id = fetch_text_opt(&st.pool, r#"SELECT id FROM "GwHilo" WHERE canal = $1 AND "canalHiloId" = $2 AND "agenteSlug" = $3"#, &[B::T(c.to_uppercase()), B::T(ch.into()), B::T(ag.into())]).await?.unwrap_or_default();
        }
    }
    let s = Salida {
        hilo_id,
        texto: body["texto"].as_str().unwrap_or("").trim().to_string(),
        accion: body["accion"].as_str().unwrap_or("responder").trim().to_string(),
        decision: body["decision"].as_str().map(String::from),
        fuentes: if body["fuentes"].is_array() { body["fuentes"].clone() } else { json!([]) },
        adjuntos: if body["adjuntos"].is_array() { body["adjuntos"].clone() } else { json!([]) },
        responde_a: body["respondeA"].as_str().map(String::from),
        entregado: sesion.is_service && body["entregado"].as_bool() == Some(true),
    };
    Ok(Json(registrar_salida(&st, s).await?))
}

/// Una respuesta o acción de un agente.
pub(crate) struct Salida {
    pub hilo_id: String,
    pub texto: String,
    pub accion: String,
    pub decision: Option<String>,
    pub fuentes: Value,
    pub adjuntos: Value,
    pub responde_a: Option<String>,
    /// El conector ya la entregó por su cuenta (bots, chat del portal): se registra con su decisión sin volver a retenerla.
    pub entregado: bool,
}

pub(crate) async fn registrar_salida(st: &AppState, s: Salida) -> Result<Value, ApiError> {
    let Salida { hilo_id, texto, accion, decision, fuentes, adjuntos, responde_a, entregado } = s;
    if hilo_id.is_empty() || texto.is_empty() {
        return Err(ApiError::bad_request("Faltan el hilo o el texto de la respuesta"));
    }
    let hilo = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('agente', h."agenteSlug", 'canal', h.canal, 'canalHiloId', h."canalHiloId", 'personaTipo', p.tipo, 'personaId', p.id)
           FROM "GwHilo" h LEFT JOIN "GwPersona" p ON p.id = h."personaId" WHERE h.id = $1"#,
        &[B::T(hilo_id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Hilo no encontrado"))?;
    let slug = hilo["agente"].as_str().unwrap_or("").to_string();
    let ficha = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(f) FROM "AgenteFicha" f WHERE f.slug = $1"#, &[B::T(slug.clone())]).await?.ok_or_else(|| ApiError::bad_request("El agente no tiene ficha"))?;
    let herramientas: Vec<String> = ficha["herramientas"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if accion != "responder" && !herramientas.contains(&accion) {
        return Err(ApiError::forbidden(format!("La acción «{accion}» no está entre las herramientas permitidas del agente")));
    }
    let nivel = nivel_efectivo(ficha["autonomia"].as_str().unwrap_or("ASISTIDA"), &ficha["autonomiaPorAccion"], &accion);
    let reglas = reglas_de(&fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(c)), '[]'::jsonb) FROM "AgenteComunicacion" c WHERE c."agenteSlug" = $1"#, &[B::T(slug.clone())]).await?);
    let persona_tipo = hilo["personaTipo"].as_str().unwrap_or("EXTERNO");
    let persona_id = hilo["personaId"].as_str().unwrap_or("");
    let canal = hilo["canal"].as_str().unwrap_or("");
    let exige = regla_entrada(&reglas, canal, persona_id, persona_tipo).ok().flatten().map(|r| r.requiere_aprobacion).unwrap_or(false);
    let directo = entregado || sale_directo(&nivel, &accion, persona_tipo == "EQUIPO", exige);

    let mensaje_id = new_id();
    let estado_msg = if entregado { "ENTREGADO" } else if directo { "ENTREGABLE" } else { "PENDIENTE_APROBACION" };
    exec(
        &st.pool,
        r#"INSERT INTO "GwMensaje" (id, "hiloId", direccion, "autorTipo", "agenteSlug", texto, adjuntos, "respondeA", estado) VALUES ($1, $2, 'SALIDA', 'AGENTE', $3, $4, $5, $6, $7)"#,
        &[B::T(mensaje_id.clone()), B::T(hilo_id.clone()), B::T(slug.clone()), B::T(texto), B::J(adjuntos), B::OT(responde_a), B::T(estado_msg.into())],
    )
    .await?;
    let decision_id = new_id();
    let decision = decision.map(|s| recortar(s.trim(), 2000)).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Respuesta del agente ({accion})"));
    exec(
        &st.pool,
        r#"INSERT INTO "GwDecision" (id, "mensajeId", "hiloId", "agenteSlug", accion, decision, fuentes, "autonomiaAplicada", estado) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
        &[B::T(decision_id.clone()), B::T(mensaje_id.clone()), B::T(hilo_id.clone()), B::T(slug), B::T(accion), B::T(decision), B::J(fuentes), B::T(nivel.clone()), B::T((if directo { "EJECUTADA" } else { "PENDIENTE_APROBACION" }).to_string())],
    )
    .await?;
    exec(&st.pool, r#"UPDATE "GwHilo" SET "ultimoEn" = now() WHERE id = $1"#, &[B::T(hilo_id)]).await?;
    Ok(json!({ "mensajeId": mensaje_id, "decisionId": decision_id, "entregar": directo && !entregado, "estado": estado_msg, "autonomia": nivel, "canal": canal, "canalHiloId": hilo["canalHiloId"] }))
}

async fn marcar_entregado(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    let n = exec(&st.pool, r#"UPDATE "GwMensaje" SET estado = 'ENTREGADO' WHERE id = $1 AND estado = 'ENTREGABLE'"#, &[B::T(id)]).await?;
    if n == 0 {
        return Err(ApiError::bad_request("El mensaje no existe o no estaba listo para entregar"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn aprobaciones(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('decisionId', d.id, 'agente', d."agenteSlug", 'accion', d.accion, 'decision', d.decision, 'fuentes', d.fuentes,
                  'autonomia', d."autonomiaAplicada", 'mensajeId', m.id, 'texto', m.texto, 'hiloId', d."hiloId", 'canal', h.canal, 'persona', p.nombre, 'en', d."creadoEn") ORDER BY d."creadoEn"), '[]'::jsonb)
           FROM "GwDecision" d JOIN "GwMensaje" m ON m.id = d."mensajeId" JOIN "GwHilo" h ON h.id = d."hiloId" LEFT JOIN "GwPersona" p ON p.id = h."personaId"
           WHERE d.estado = 'PENDIENTE_APROBACION'"#,
        &[],
    )
    .await?;
    Ok(Json(v))
}

async fn decidir(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let _ = sesion.require_user()?;
    let aprobar = match body["accion"].as_str() {
        Some("aprobar") => true,
        Some("rechazar") => false,
        _ => return Err(ApiError::bad_request("La acción debe ser «aprobar» o «rechazar»")),
    };
    let quien = nombre_de(&sesion);
    let d = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('mensajeId', "mensajeId", 'estado', estado) FROM "GwDecision" WHERE id = $1"#, &[B::T(id.clone())]).await?.ok_or_else(|| ApiError::not_found("Decisión no encontrada"))?;
    if d["estado"] != "PENDIENTE_APROBACION" {
        return Err(ApiError::bad_request("Esa decisión ya fue resuelta"));
    }
    let mensaje_id = d["mensajeId"].as_str().unwrap_or("").to_string();
    exec(&st.pool, r#"UPDATE "GwDecision" SET estado = $2, "decididoPor" = $3, "decididoEn" = now() WHERE id = $1"#, &[B::T(id), B::T((if aprobar { "EJECUTADA" } else { "RECHAZADA" }).to_string()), B::T(quien)]).await?;
    exec(&st.pool, r#"UPDATE "GwMensaje" SET estado = $2 WHERE id = $1"#, &[B::T(mensaje_id.clone()), B::T((if aprobar { "ENTREGABLE" } else { "RECHAZADO" }).to_string())]).await?;
    let envio = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('mensajeId', m.id, 'texto', m.texto, 'canal', h.canal, 'canalHiloId', h."canalHiloId", 'agente', h."agenteSlug")
           FROM "GwMensaje" m JOIN "GwHilo" h ON h.id = m."hiloId" WHERE m.id = $1"#,
        &[B::T(mensaje_id)],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "entregar": aprobar, "envio": envio })))
}

// ── Consulta (trazabilidad) ──────────────────────────────────────────────────────────────────────

async fn listar_hilos(State(st): State<AppState>, sesion: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    let limite = q.get("limite").and_then(|l| l.parse::<i64>().ok()).unwrap_or(50).clamp(1, 200);
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', h.id, 'canal', h.canal, 'agente', h."agenteSlug", 'persona', p.nombre, 'personaTipo', p.tipo, 'asunto', h.asunto,
                  'estado', h.estado, 'ultimoEn', h."ultimoEn", 'mensajes', (SELECT count(*) FROM "GwMensaje" m WHERE m."hiloId" = h.id),
                  'pendientes', (SELECT count(*) FROM "GwDecision" d WHERE d."hiloId" = h.id AND d.estado = 'PENDIENTE_APROBACION')) ORDER BY h."ultimoEn" DESC), '[]'::jsonb)
           FROM (SELECT * FROM "GwHilo" WHERE ($1 = '' OR "agenteSlug" = $1) AND ($2 = '' OR canal = $2) ORDER BY "ultimoEn" DESC LIMIT $3) h
           LEFT JOIN "GwPersona" p ON p.id = h."personaId""#,
        &[B::T(q.get("agente").cloned().unwrap_or_default()), B::T(q.get("canal").map(|c| c.to_uppercase()).unwrap_or_default()), B::I(limite)],
    )
    .await?;
    Ok(Json(v))
}

/// El hilo completo con cada mensaje, su agente, su decisión y las fuentes en que se apoyó.
async fn ver_hilo(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    let hilo = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', h.id, 'canal', h.canal, 'agente', h."agenteSlug", 'persona', p.nombre, 'personaTipo', p.tipo, 'asunto', h.asunto, 'estado', h.estado, 'solucionId', h."solucionId", 'creadoEn', h."creadoEn")
           FROM "GwHilo" h LEFT JOIN "GwPersona" p ON p.id = h."personaId" WHERE h.id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Hilo no encontrado"))?;
    let mensajes = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', m.id, 'direccion', m.direccion, 'autor', m."autorTipo", 'agente', m."agenteSlug", 'persona', p.nombre, 'texto', m.texto, 'adjuntos', m.adjuntos, 'estado', m.estado, 'en', m."creadoEn",
                  'decision', (SELECT to_jsonb(d) FROM "GwDecision" d WHERE d."mensajeId" = m.id LIMIT 1)) ORDER BY m."creadoEn"), '[]'::jsonb)
           FROM "GwMensaje" m LEFT JOIN "GwPersona" p ON p.id = m."personaId" WHERE m."hiloId" = $1"#,
        &[B::T(id)],
    )
    .await?;
    Ok(Json(json!({ "hilo": hilo, "mensajes": mensajes })))
}

// ── Memoria ──────────────────────────────────────────────────────────────────────────────────────

async fn leer_memoria(State(st): State<AppState>, sesion: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    let ambito = q.get("ambito").map(|a| a.to_uppercase()).unwrap_or_default();
    let ref_id = q.get("refId").cloned().unwrap_or_default();
    if !AMBITOS.contains(&ambito.as_str()) || ref_id.is_empty() {
        return Err(ApiError::bad_request("Indica el ámbito (CONVERSACION, AGENTE o PROYECTO) y la referencia"));
    }
    Ok(Json(memoria_de(&st, &ambito, &ref_id, 50).await?))
}

/// Escribe una entrada de memoria. Si ya hay una vigente con la misma clave, la reemplaza (la vieja queda como no vigente).
async fn escribir_memoria(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    exigir_servicio_o_usuario(&sesion)?;
    let ambito = body["ambito"].as_str().unwrap_or("").to_uppercase();
    let ref_id = body["refId"].as_str().unwrap_or("").trim().to_string();
    let contenido = body["contenido"].as_str().unwrap_or("").trim().to_string();
    let clave = body["clave"].as_str().unwrap_or("").trim().to_string();
    if !AMBITOS.contains(&ambito.as_str()) || ref_id.is_empty() || contenido.is_empty() {
        return Err(ApiError::bad_request("Faltan el ámbito, la referencia o el contenido de la memoria"));
    }
    if !clave.is_empty() {
        exec(&st.pool, r#"UPDATE "GwMemoria" SET vigente = false WHERE ambito = $1 AND "refId" = $2 AND clave = $3 AND vigente"#, &[B::T(ambito.clone()), B::T(ref_id.clone()), B::T(clave.clone())]).await?;
    }
    let id = new_id();
    exec(
        &st.pool,
        r#"INSERT INTO "GwMemoria" (id, ambito, "refId", clave, contenido, "origenMensajeId") VALUES ($1, $2, $3, $4, $5, $6)"#,
        &[B::T(id.clone()), B::T(ambito), B::T(ref_id), B::T(clave), B::T(recortar(&contenido, 4000)), B::OT(body["origenMensajeId"].as_str().map(String::from))],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

// ── Identidades, correo y prompt ─────────────────────────────────────────────────────────────────

/// Personas del gateway con sus identidades por canal y, si ya están vinculadas, su usuario del portal; más los usuarios
/// disponibles para vincular. Sirve a la pantalla de identidades (un administrador une un Telegram con un usuario).
async fn identidades(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let personas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('personaId', p.id, 'nombre', p.nombre, 'tipo', p.tipo, 'userId', p."userId", 'usuario', u.name,
              'identidades', (SELECT COALESCE(jsonb_agg(jsonb_build_object('canal', i.canal, 'externoId', i."externoId", 'verificada', i.verificada) ORDER BY i.canal), '[]'::jsonb) FROM "GwIdentidad" i WHERE i."personaId" = p.id),
              'hilos', (SELECT count(*) FROM "GwHilo" h WHERE h."personaId" = p.id)) ORDER BY p."creadoEn" DESC), '[]'::jsonb)
           FROM "GwPersona" p LEFT JOIN "User" u ON u.id = p."userId""#,
        &[],
    )
    .await?;
    let usuarios = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'nombre', name, 'rol', role) ORDER BY name), '[]'::jsonb) FROM "User""#, &[]).await?;
    Ok(Json(json!({ "personas": personas, "usuarios": usuarios })))
}

/// Un administrador entrega un correo del buzón a un agente: abre un hilo de canal CORREO (identidad = remitente, hilo = conversación).
async fn asignar_correo(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let m = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('asunto', subject, 'remitente', sender, 'nombre', "senderName", 'texto', "bodyPreview", 'conversacion', "conversationId") FROM "ExternalMessage" WHERE id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Correo no encontrado"))?;
    let remitente = m["remitente"].as_str().unwrap_or("").trim().to_lowercase();
    if remitente.is_empty() {
        return Err(ApiError::bad_request("El correo no tiene remitente"));
    }
    let asunto = m["asunto"].as_str().unwrap_or("").trim().to_string();
    let cuerpo = m["texto"].as_str().unwrap_or("").trim().to_string();
    let texto = if asunto.is_empty() { cuerpo } else { format!("{asunto}\n\n{cuerpo}") };
    let e = Entrada {
        canal: "CORREO".into(),
        externo_id: remitente.clone(),
        nombre: m["nombre"].as_str().filter(|s| !s.is_empty()).unwrap_or(&remitente).to_string(),
        slug: body["agente"].as_str().unwrap_or("").trim().to_string(),
        texto: texto.trim().to_string(),
        canal_hilo: m["conversacion"].as_str().filter(|s| !s.is_empty()).map(String::from).or_else(|| Some(id.clone())),
        canal_msg: Some(id),
        solucion_id: body["solucionId"].as_str().filter(|s| !s.is_empty()).map(String::from),
        asunto: Some(asunto).filter(|s| !s.is_empty()),
        adjuntos: json!([]),
        ya_autorizado: true, // lo decide el administrador que lo asigna
    };
    Ok(Json(registrar_entrada(&st, e).await?))
}

/// Texto para sumar al prompt del agente: su alma, su alcance, su memoria y el estado vivo del proyecto del hilo.
pub(crate) fn bloque_prompt(ctx: &Value) -> String {
    let a = &ctx["agente"];
    let alma = &a["alma"];
    let lista = |v: &Value| v.as_array().map(|x| x.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join("; ")).unwrap_or_default();
    let mut t = String::new();
    let mut linea = |etiqueta: &str, valor: &str| {
        if !valor.trim().is_empty() {
            t.push_str(&format!("{etiqueta}: {}\n", valor.trim()));
        }
    };
    linea("Tu misión", alma["mision"].as_str().unwrap_or(""));
    linea("Tu forma de ser", alma["tono"].as_str().unwrap_or(""));
    linea("Tus valores", &lista(&alma["valores"]));
    linea("Límites que no debes cruzar", &lista(&alma["limites"]));
    linea("Eres responsable de", a["alcance"].as_str().unwrap_or(""));
    for (titulo, v) in [("Lo que recuerdas de tu trabajo", &ctx["memoria"]["agente"]), ("Lo que recuerdas de esta conversación", &ctx["memoria"]["conversacion"])] {
        if let Some(items) = v.as_array().filter(|i| !i.is_empty()) {
            t.push_str(&format!("{titulo}:\n"));
            for m in items {
                t.push_str(&format!("- {}\n", m["contenido"].as_str().unwrap_or("")));
            }
        }
    }
    if let Some(e) = ctx["proyecto"]["estadoVivo"].as_str().filter(|s| !s.trim().is_empty()) {
        t.push_str(&format!("Estado actual del proyecto de esta conversación:\n{e}\n"));
    }
    t
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn regla(tipo: &str, destino: &str, dir: &str, canales: &[&str], exige: bool) -> Regla {
        Regla { tipo: tipo.into(), destino: destino.into(), direccion: dir.into(), canales: canales.iter().map(|c| c.to_string()).collect(), requiere_aprobacion: exige }
    }

    #[test]
    fn sin_reglas_solo_entra_el_equipo() {
        assert!(regla_entrada(&[], "TELEGRAM", "p1", "EQUIPO").is_ok());
        assert!(regla_entrada(&[], "TELEGRAM", "p1", "CLIENTE").is_err());
        assert!(regla_entrada(&[], "CORREO", "p1", "EXTERNO").is_err());
    }

    #[test]
    fn reglas_solo_entre_agentes_no_cierran_la_puerta_al_equipo() {
        let r = [regla("AGENTE", "presales", "AMBAS", &[], false)];
        assert!(regla_entrada(&r, "TELEGRAM", "p1", "EQUIPO").is_ok());
        assert!(regla_entrada(&r, "TELEGRAM", "p1", "CLIENTE").is_err());
    }

    #[test]
    fn una_regla_de_rol_abre_la_puerta_por_los_canales_indicados() {
        let r = [regla("ROL", "CLIENTE", "AMBAS", &["CORREO"], true)];
        let ok = regla_entrada(&r, "CORREO", "p9", "CLIENTE").unwrap().unwrap();
        assert!(ok.requiere_aprobacion);
        assert!(regla_entrada(&r, "TELEGRAM", "p9", "CLIENTE").is_err());
        // Con una regla hacia clientes, el equipo ya no entra por el valor por defecto: hay que declararlo.
        assert!(regla_entrada(&r, "CORREO", "p1", "EQUIPO").is_err());
    }

    #[test]
    fn una_regla_de_persona_vale_solo_para_esa_persona() {
        let r = [regla("PERSONA", "p1", "ENTRADA", &[], false)];
        assert!(regla_entrada(&r, "PORTAL", "p1", "EXTERNO").is_ok());
        assert!(regla_entrada(&r, "PORTAL", "p2", "EXTERNO").is_err());
        let solo_salida = [regla("PERSONA", "p1", "SALIDA", &[], false)];
        assert!(regla_entrada(&solo_salida, "PORTAL", "p1", "EXTERNO").is_err());
    }

    #[test]
    fn la_autonomia_decide_si_sale_o_espera() {
        // AUTONOMA: todo sale; SUPERVISADA: solo lecturas; ASISTIDA: solo lectura al equipo.
        assert!(sale_directo("AUTONOMA", "crear", false, false));
        assert!(sale_directo("SUPERVISADA", "responder", false, false));
        assert!(!sale_directo("SUPERVISADA", "crear", true, false));
        assert!(sale_directo("ASISTIDA", "consulta", true, false));
        assert!(!sale_directo("ASISTIDA", "consulta", false, false));
        assert!(!sale_directo("ASISTIDA", "crear", true, false));
        // La regla que exige aprobación gana a cualquier nivel.
        assert!(!sale_directo("AUTONOMA", "responder", true, true));
    }

    #[test]
    fn el_nivel_por_accion_anula_el_general() {
        let p = json!({ "crear": "AUTONOMA", "mover": "LIBRE" });
        assert_eq!(nivel_efectivo("ASISTIDA", &p, "crear"), "AUTONOMA");
        assert_eq!(nivel_efectivo("ASISTIDA", &p, "mover"), "ASISTIDA"); // nivel inválido: se ignora
        assert_eq!(nivel_efectivo("SUPERVISADA", &p, "borrar"), "SUPERVISADA");
    }

    #[test]
    fn el_contexto_conserva_lo_mas_reciente_y_el_orden() {
        let ms: Vec<Value> = (0..10).map(|i| json!({ "texto": format!("mensaje {i} {}", "x".repeat(200)) })).collect();
        let c = caben(&ms, 700);
        assert!(c.len() < 10 && !c.is_empty());
        assert!(c.last().unwrap()["texto"].as_str().unwrap().starts_with("mensaje 9"));
        let primero: usize = c[0]["texto"].as_str().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
        let ultimo: usize = c.last().unwrap()["texto"].as_str().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
        assert!(primero < ultimo);
    }

    #[test]
    fn el_prompt_lleva_el_alma_la_memoria_y_el_proyecto() {
        let ctx = json!({
            "agente": { "alma": { "mision": "Llevar la agenda", "tono": "cercano", "valores": ["claridad", "puntualidad"], "limites": [] }, "alcance": "Reuniones del equipo" },
            "memoria": { "agente": [{ "contenido": "Las reuniones duran 30 min" }], "conversacion": [] },
            "proyecto": { "estadoVivo": "Fase: diseño" }
        });
        let p = bloque_prompt(&ctx);
        assert!(p.contains("Tu misión: Llevar la agenda"));
        assert!(p.contains("claridad; puntualidad"));
        assert!(p.contains("- Las reuniones duran 30 min"));
        assert!(p.contains("Fase: diseño"));
        assert!(!p.contains("Límites")); // vacío: no se escribe la línea
        assert!(!p.contains("Lo que recuerdas de esta conversación")); // sin memoria de conversación
    }

    #[test]
    fn recorta_sin_romper_caracteres() {
        assert_eq!(recortar("corto", 10), "corto");
        let r = recortar("áéíóúáéíóú", 5);
        assert_eq!(r.chars().count(), 5);
        assert!(r.ends_with('…'));
    }
}
