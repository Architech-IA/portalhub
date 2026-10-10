//! Hub de la solución: guardado seguro y funciones de proyecto comercial — MASD-0025.
//!
//! - Guardado por documento con control de edición simultánea (`base`), fusión de lo que mantiene el
//!   servidor (estado de cada requisito y su tarea), versiones del PRD, aprobación con quién y cuándo,
//!   y reapertura de un documento aprobado que se edita.
//! - Backlog desde el PRD en una sola transacción; requisitos que siguen a su tarea (hecha / verificada).
//! - Coherencia entre documentos, cruce con la propuesta comercial, control de cambios, economía,
//!   fotografías (línea base / as-built), enlace de solo lectura para el cliente, soluciones adicionales
//!   y diagramas (Mermaid) generados con IA.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use rand::Rng;
use serde_json::{json, Map, Value};

use crate::{
    error::{ApiError, ApiResult},
    llm,
    routes::gestion::SOLUCION_JSON,
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, new_id, parse_float, presente, s, s_o_nulo, truthy, uid, Upd, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/soluciones/{id}/backlog-desde-prd", post(backlog_desde_prd))
        .route("/api/soluciones/{id}/coherencia", get(coherencia))
        .route("/api/soluciones/{id}/cruce-propuesta", post(cruce_propuesta))
        .route("/api/soluciones/{id}/cambios", get(cambios_listar).post(cambio_crear))
        .route("/api/cambios/{id}", put(cambio_actualizar).delete(cambio_eliminar))
        .route("/api/soluciones/{id}/economia", get(economia))
        .route("/api/soluciones/{id}/snapshots", get(snapshots_listar).post(snapshot_crear))
        .route("/api/soluciones/{id}/snapshots/{sid}", get(snapshot_obtener))
        .route("/api/soluciones/{id}/enlace-cliente", post(enlace_crear).delete(enlace_revocar))
        .route("/api/soluciones/{id}/adicional", post(crear_adicional))
        .route("/api/soluciones/{id}/diagramas-generate", post(diagramas_generar))
        .route("/api/publico/proyecto/{token}", get(vista_cliente))
}

/// Documentos de la Solución que se guardan como un bloque de texto.
const DOCUMENTOS: [&str; 8] = ["arquitectura", "arquitecturaHtml", "planTrabajo", "cronograma", "prd", "disenoTecnico", "planEjecucion", "diagramas"];
/// Los tres que tienen ciclo de vida (BORRADOR → EN_REVISION → APROBADO).
const CON_APROBACION: [&str; 3] = ["prd", "disenoTecnico", "planEjecucion"];
const MAX_VERSIONES_PRD: i64 = 50;

fn por_defecto(k: &str) -> Option<&'static str> {
    match k {
        "arquitectura" | "cronograma" | "diagramas" => Some("[]"),
        "prd" | "disenoTecnico" | "planEjecucion" => Some("{}"),
        _ => None,
    }
}

fn es_admin(se: &Session) -> bool {
    se.is_admin() || se.is_service
}

async fn ahora(st: &AppState) -> String {
    fetch_text_opt(&st.pool, r#"SELECT to_char(NOW() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')"#, &[]).await.ok().flatten().unwrap_or_default()
}

fn uid_s(se: &Session) -> Option<String> {
    Some(se.id.clone()).filter(|x| !x.is_empty())
}

fn nombre_de(se: &Session) -> String {
    if !se.name.is_empty() {
        se.name.clone()
    } else if !se.email.is_empty() {
        se.email.clone()
    } else {
        "Sistema".into()
    }
}

// ═══════════════════════════ Comparación de documentos ═══════════════════════════
fn parse_o_texto(t: &str) -> Value {
    serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_string()))
}

/// Quita lo que el servidor mantiene por su cuenta (estado de cada requisito, su tarea, el sello de aprobación)
/// para que un cambio de esos campos no cuente como "otra persona editó el documento".
fn sin_volatiles(clave: &str, v: &mut Value) {
    if let Value::Object(o) = v {
        if CON_APROBACION.contains(&clave) {
            o.remove("aprobacion");
            o.remove("reabierto");
        }
        if clave == "prd" {
            if let Some(Value::Array(reqs)) = o.get_mut("requisitos") {
                for r in reqs {
                    if let Value::Object(ro) = r {
                        ro.remove("estado");
                        ro.remove("backlogItemId");
                    }
                }
            }
        }
    }
}

/// Forma canónica para comparar: nada, `""`, `{}` y `[]` son lo mismo.
pub fn huella(clave: &str, texto: &str) -> Value {
    let mut v = parse_o_texto(texto);
    sin_volatiles(clave, &mut v);
    match &v {
        Value::String(x) if x.trim().is_empty() => Value::Null,
        Value::Object(o) if o.is_empty() => Value::Null,
        Value::Array(a) if a.is_empty() => Value::Null,
        _ => v,
    }
}

fn indice_requisitos(v: &Value) -> HashMap<String, (Value, Value)> {
    let mut m = HashMap::new();
    if let Some(reqs) = v["requisitos"].as_array() {
        for r in reqs {
            if let Some(id) = r["id"].as_str() {
                m.insert(id.to_string(), (r["estado"].clone(), r["backlogItemId"].clone()));
            }
        }
    }
    m
}

/// Tres vías sobre lo que el servidor mantiene de cada requisito: si la persona no lo tocó (igual a la base
/// con la que abrió el hub), gana lo que hay hoy en el servidor. Así guardar el PRD no deshace el avance de las tareas.
pub fn fusionar_seguimiento(nuevo: &mut Value, base: &Value, actual: &Value) {
    let (ib, ia) = (indice_requisitos(base), indice_requisitos(actual));
    if let Some(reqs) = nuevo.get_mut("requisitos").and_then(|r| r.as_array_mut()) {
        for r in reqs {
            let Some(id) = r["id"].as_str().map(String::from) else { continue };
            let Some((ae, ab)) = ia.get(&id) else { continue };
            let (be, bb) = ib.get(&id).cloned().unwrap_or((Value::Null, Value::Null));
            if r["estado"] == be && !ae.is_null() {
                r["estado"] = ae.clone();
            }
            if r["backlogItemId"] == bb && !ab.is_null() {
                r["backlogItemId"] = ab.clone();
            }
        }
    }
}

// ═══════════════════════════ Guardado de la Solución ═══════════════════════════
fn es_unico_violado(e: &sqlx::Error) -> bool {
    e.as_database_error().and_then(|d| d.code().map(|c| c == "23505")).unwrap_or(false)
}

/// PUT /api/soluciones/{id}. Solo cambia lo que viene en el pedido; un documento que trae su `base` se compara con lo
/// que hay hoy y, si otra persona o proceso lo cambió, responde 409 con el contenido vigente (salvo `forzar`).
pub async fn solucion_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let actual = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(so) FROM "Solucion" so WHERE so.id = $1"#, &[B::T(id.clone())])
        .await?
        .ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let forzar = truthy(&body, "forzar");
    let base = body.get("base").cloned().unwrap_or(Value::Null);
    let texto_actual = |k: &str| actual[k].as_str().unwrap_or("").to_string();

    // 1. Edición simultánea
    let mut conflictos: Vec<String> = vec![];
    let mut vigente = Map::new();
    for k in DOCUMENTOS {
        if !presente(&body, k) {
            continue;
        }
        if let Some(b) = base.get(k) {
            let cur = texto_actual(k);
            if huella(k, &cur) != huella(k, b.as_str().unwrap_or("")) {
                conflictos.push(k.to_string());
                vigente.insert(k.to_string(), Value::String(cur));
            }
        }
    }
    if !conflictos.is_empty() && !forzar {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Otra persona o un proceso cambió este documento mientras lo editabas. Revisa lo vigente antes de guardar.",
        )
        .con_extra(json!({ "conflictos": conflictos, "vigente": vigente })));
    }

    // 2. Valor nuevo de cada documento (con fusión, aprobación y reapertura)
    let marca = ahora(&st).await;
    let mut nuevos: Vec<(&str, Option<String>)> = vec![];
    for k in DOCUMENTOS {
        if !presente(&body, k) {
            continue;
        }
        let mut texto = match body.get(k) {
            None | Some(Value::Null) => por_defecto(k).map(String::from),
            Some(Value::String(x)) if x.is_empty() => por_defecto(k).map(String::from),
            Some(Value::String(x)) => Some(x.clone()),
            Some(_) => return Err(ApiError::bad_request(format!("«{k}» debe ser texto"))),
        };
        if CON_APROBACION.contains(&k) {
            let mut n: Value = parse_o_texto(texto.as_deref().unwrap_or("{}"));
            if !n.is_object() {
                return Err(ApiError::bad_request(format!("«{k}» no es un documento válido")));
            }
            let act = parse_o_texto(&texto_actual(k));
            if k == "prd" {
                let b = base.get("prd").and_then(|x| x.as_str()).map(parse_o_texto).unwrap_or(Value::Null);
                fusionar_seguimiento(&mut n, &b, &act);
            }
            let est_nuevo = n["estadoDocumento"].as_str().unwrap_or("BORRADOR").to_string();
            let est_viejo = act["estadoDocumento"].as_str().unwrap_or("BORRADOR").to_string();
            if est_nuevo == "APROBADO" {
                if est_viejo != "APROBADO" {
                    if !es_admin(&sesion) {
                        return Err(ApiError::forbidden("Solo un administrador puede aprobar un documento"));
                    }
                    n["aprobacion"] = json!({ "porId": sesion.id, "porNombre": nombre_de(&sesion), "en": marca });
                    if let Value::Object(o) = &mut n {
                        o.remove("reabierto");
                    }
                } else if huella(k, &n.to_string()) != huella(k, &act.to_string()) {
                    n["estadoDocumento"] = json!("EN_REVISION");
                    n["reabierto"] = json!({ "porNombre": nombre_de(&sesion), "en": marca, "motivo": "Se editó después de aprobarse: vuelve a revisión." });
                    if let Value::Object(o) = &mut n {
                        o.remove("aprobacion");
                    }
                } else if !act["aprobacion"].is_null() {
                    n["aprobacion"] = act["aprobacion"].clone();
                }
            } else if let Value::Object(o) = &mut n {
                o.remove("aprobacion");
            }
            texto = Some(n.to_string());
        }
        nuevos.push((k, texto));
    }

    // 3. Versión del PRD que se reemplaza
    if let Some((_, Some(nuevo_prd))) = nuevos.iter().find(|(k, _)| *k == "prd") {
        let viejo = texto_actual("prd");
        if !viejo.trim().is_empty() && viejo.trim() != "{}" && huella("prd", nuevo_prd) != huella("prd", &viejo) {
            let motivo = format!("Antes de la edición de {} desde el hub", nombre_de(&sesion));
            let _ = exec(
                &st.pool,
                r#"INSERT INTO "SolucionPrdVersion" (id, "solucionId", version, contenido, motivo, "usuarioId", "createdAt")
                   SELECT $1, $2, COALESCE(MAX(version), 0) + 1, $3, $4, $5, NOW() FROM "SolucionPrdVersion" WHERE "solucionId" = $2"#,
                &[B::T(new_id()), B::T(id.clone()), B::T(viejo), B::T(motivo), B::OT(uid_s(&sesion))],
            )
            .await;
            let _ = exec(
                &st.pool,
                r#"DELETE FROM "SolucionPrdVersion" WHERE "solucionId" = $1 AND version <= (SELECT COALESCE(MAX(version), 0) - $2 FROM "SolucionPrdVersion" WHERE "solucionId" = $1)"#,
                &[B::T(id.clone()), B::I(MAX_VERSIONES_PRD)],
            )
            .await;
        }
    }

    // 4. Escritura: solo lo que vino
    let mut up = Upd::new(&id);
    if let Some(v) = s_o_nulo(&body, "nombre") {
        up.set("nombre", B::T(v));
    }
    if presente(&body, "descripcion") {
        up.set("descripcion", B::OT(s_o_nulo(&body, "descripcion")));
    }
    if let Some(v) = s_o_nulo(&body, "tipo") {
        up.set("tipo", B::T(v));
    }
    if let Some(v) = s_o_nulo(&body, "estado") {
        up.set("estado", B::T(v));
    }
    if presente(&body, "valorEstimado") {
        up.set("valorEstimado", B::F(parse_float(body.get("valorEstimado"))));
    }
    if presente(&body, "leadId") {
        up.set("leadId", B::OT(s_o_nulo(&body, "leadId")));
    }
    if presente(&body, "repositorio") {
        up.set("repositorio", B::OT(s_o_nulo(&body, "repositorio")));
    }
    for (k, t) in nuevos {
        up.set(k, B::OT(t));
    }
    let sql = up.con("Solucion", &format!("SELECT {SOLUCION_JSON} FROM up so"));
    let solucion = match fetch_json_opt(&st.pool, &sql, &up.binds).await {
        Ok(Some(v)) => v,
        Err(e) if es_unico_violado(&e) => return Err(ApiError::new(StatusCode::CONFLICT, "Ese lead ya tiene otra Solución asociada.")),
        Err(e) => {
            tracing::error!("solucion_actualizar: {e}");
            return Err(ApiError::internal("Error al actualizar la solución"));
        }
        Ok(None) => return Err(ApiError::internal("Error al actualizar la solución")),
    };

    // 5. Línea base: cuando PRD y diseño quedan aprobados por primera vez
    let aprobado = |k: &str| solucion[k].as_str().map(|t| parse_o_texto(t)["estadoDocumento"] == "APROBADO").unwrap_or(false);
    if aprobado("prd") && aprobado("disenoTecnico") {
        let hay = fetch_text_opt(&st.pool, r#"SELECT id FROM "SolucionSnapshot" WHERE "solucionId" = $1 AND tipo = 'LINEA_BASE' LIMIT 1"#, &[B::T(id.clone())]).await.ok().flatten();
        if hay.is_none() {
            let _ = crear_snapshot(&st, &id, "Línea base (PRD y diseño aprobados)", "LINEA_BASE", &sesion).await;
        }
    }
    let nombre = s(&body, "nombre").unwrap_or_else(|| solucion["nombre"].as_str().unwrap_or_default().to_string());
    log_activity(&st.pool, "UPDATED", &format!("actualizó la solución {nombre}"), "solucion", &id, uid(&sesion), None).await;
    Ok(Json(solucion))
}

/// DELETE /api/soluciones/{id}: solo administradores; si la solución tiene backlog o sprints pide confirmar (`?forzar=1`).
pub async fn solucion_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    if !es_admin(&sesion) {
        return Err(ApiError::forbidden("Solo un administrador puede eliminar una solución"));
    }
    let nombre = fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await?.ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let cuentas = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'backlog', (SELECT COUNT(*) FROM "BacklogItem" WHERE "solucionId" = $1),
             'sprints', (SELECT COUNT(*) FROM "Sprint" WHERE "solucionId" = $1),
             'epicas', (SELECT COUNT(*) FROM "Epic" WHERE "solucionId" = $1),
             'riesgos', (SELECT COUNT(*) FROM "Riesgo" WHERE "solucionId" = $1),
             'hitos', (SELECT COUNT(*) FROM "Hito" WHERE "solucionId" = $1))"#,
        &[B::T(id.clone())],
    )
    .await?;
    let forzar = q.get("forzar").map(|v| v == "1" || v == "true").unwrap_or(false);
    let con_trabajo = cuentas["backlog"].as_i64().unwrap_or(0) + cuentas["sprints"].as_i64().unwrap_or(0) > 0;
    if con_trabajo && !forzar {
        return Err(ApiError::new(StatusCode::CONFLICT, "La solución tiene backlog o sprints: al borrarla quedan sin proyecto. Confirma para continuar.").con_extra(json!({ "cuentas": cuentas })));
    }
    match exec(&st.pool, r#"DELETE FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(n) if n > 0 => {}
        _ => return Err(ApiError::internal("Error al eliminar la solución")),
    }
    log_activity(&st.pool, "UPDATED", &format!("eliminó la solución {nombre}"), "solucion", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════ Backlog desde el PRD ═══════════════════════════
/// Crea una tarea por cada requisito que aún no tiene una, y guarda el vínculo en el PRD, todo en una transacción
/// (un doble clic o un corte a la mitad no duplican ni dejan tareas sin vínculo).
async fn backlog_desde_prd(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let mut tx = st.pool.begin().await?;
    let fila = sqlx::query_as::<_, (Option<String>, Option<String>)>(r#"SELECT prd, "disenoTecnico" FROM "Solucion" WHERE id = $1 FOR UPDATE"#)
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((prd_txt, dis_txt)) = fila else { return Err(ApiError::not_found("No encontrado")) };
    let mut prd: Value = prd_txt.as_deref().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    let dis: Value = dis_txt.as_deref().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    if prd["estadoDocumento"] != "APROBADO" || dis["estadoDocumento"] != "APROBADO" {
        return Err(ApiError::bad_request("El PRD y el Diseño Técnico deben estar en estado «Aprobado» antes de generar backlog."));
    }
    let (mut creadas, mut omitidas) = (0, 0);
    {
        let Some(reqs) = prd.get_mut("requisitos").and_then(|r| r.as_array_mut()).filter(|r| !r.is_empty()) else {
            return Err(ApiError::bad_request("El PRD no tiene requisitos."));
        };
        for r in reqs.iter_mut() {
            if r["backlogItemId"].as_str().map(|x| !x.is_empty()).unwrap_or(false) {
                omitidas += 1;
                continue;
            }
            let texto = crate::routes::soluciones_ia::strip_html(r["texto"].as_str().unwrap_or(""));
            let criterio = crate::routes::soluciones_ia::strip_html(r["criterioAceptacion"].as_str().unwrap_or(""));
            let compacto = texto.split_whitespace().collect::<Vec<_>>().join(" ");
            let titulo: String = if compacto.is_empty() { "Requisito sin título".into() } else { compacto.chars().take(120).collect() };
            let tipo = if r["tipo"] == "historia" { "Historia de usuario" } else { "Caso de uso" };
            let descripcion = format!("{tipo}: {texto}\n\nCriterio de aceptación: {criterio}");
            let prioridad = match r["prioridad"].as_str() {
                Some("MUST") => "HIGH",
                Some("SHOULD") => "MEDIUM",
                _ => "LOW",
            };
            let nuevo = new_id();
            sqlx::query(
                r#"INSERT INTO "BacklogItem" (id, title, description, type, priority, status, "solucionId", "prdRequisitoId", "createdAt", "updatedAt")
                   VALUES ($1, $2, $3, 'TASK', $4, 'BACKLOG', $5, $6, NOW(), NOW())"#,
            )
            .bind(&nuevo)
            .bind(&titulo)
            .bind(&descripcion)
            .bind(prioridad)
            .bind(&id)
            .bind(r["id"].as_str().unwrap_or_default())
            .execute(&mut *tx)
            .await?;
            r["backlogItemId"] = json!(nuevo);
            creadas += 1;
        }
    }
    sqlx::query(r#"UPDATE "Solucion" SET prd = $2, "updatedAt" = NOW() WHERE id = $1"#).bind(&id).bind(prd.to_string()).execute(&mut *tx).await?;
    tx.commit().await?;
    log_activity(&st.pool, "UPDATED", &format!("generó {creadas} tarea(s) de backlog desde el PRD"), "solucion", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "creadas": creadas, "omitidas": omitidas })))
}

/// El requisito sigue a su tarea: hecha = IMPLEMENTADO; hecha y verificada por el Motor (`verificada`) = VERIFICADO; si la tarea
/// vuelve a fallar o a la cola, el requisito vuelve a APROBADO. Nunca falla hacia afuera.
pub async fn sincronizar_requisito(st: &AppState, item_id: &str, verificada: bool) {
    if let Err(e) = sincronizar_requisito_int(st, item_id, verificada).await {
        tracing::warn!("sincronizar_requisito: {e}");
    }
}

async fn sincronizar_requisito_int(st: &AppState, item_id: &str, verificada: bool) -> Result<(), sqlx::Error> {
    let Some(it) = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('sol', b."solucionId", 'req', b."prdRequisitoId", 'status', b.status) FROM "BacklogItem" b WHERE b.id = $1"#,
        &[B::T(item_id.into())],
    )
    .await?
    else {
        return Ok(());
    };
    let (Some(sol), Some(req)) = (it["sol"].as_str(), it["req"].as_str()) else { return Ok(()) };
    for _ in 0..3 {
        let Some(txt) = fetch_text_opt(&st.pool, r#"SELECT prd FROM "Solucion" WHERE id = $1"#, &[B::T(sol.into())]).await? else { return Ok(()) };
        let mut prd: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
        let Some(r) = prd.get_mut("requisitos").and_then(|r| r.as_array_mut()).and_then(|a| a.iter_mut().find(|r| r["id"] == req)) else { return Ok(()) };
        let actual = r["estado"].as_str().unwrap_or("PROPUESTO").to_string();
        let ya_hecho = actual == "IMPLEMENTADO" || actual == "VERIFICADO";
        let nuevo = match it["status"].as_str().unwrap_or("") {
            "DONE" if verificada => "VERIFICADO",
            "DONE" => "IMPLEMENTADO",
            "FAILED" | "BACKLOG" | "BLOCKED" | "CANCELLED" if ya_hecho => "APROBADO",
            _ => return Ok(()),
        };
        if nuevo == actual {
            return Ok(());
        }
        r["estado"] = json!(nuevo);
        let n = exec(&st.pool, r#"UPDATE "Solucion" SET prd = $2, "updatedAt" = NOW() WHERE id = $1 AND prd = $3"#, &[B::T(sol.into()), B::T(prd.to_string()), B::T(txt)]).await?;
        if n > 0 {
            return Ok(());
        }
    }
    Ok(())
}

// ═══════════════════════════ Coherencia entre documentos ═══════════════════════════
fn hallazgo(nivel: &str, area: &str, mensaje: impl Into<String>, tab: &str) -> Value {
    json!({ "nivel": nivel, "area": area, "mensaje": mensaje.into(), "tab": tab })
}

fn lista<'a>(v: &'a Value, k: &str) -> Vec<&'a Value> {
    v[k].as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

fn vacio_txt(v: &Value, k: &str) -> bool {
    v[k].as_str().map(|t| crate::routes::soluciones_ia::strip_html(t).trim().is_empty()).unwrap_or(true)
}

async fn coherencia(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let d = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('sol', to_jsonb(so),
             'hitos', COALESCE((SELECT jsonb_agg(to_jsonb(h)) FROM "Hito" h WHERE h."solucionId" = so.id), '[]'::jsonb),
             'riesgos', COALESCE((SELECT jsonb_agg(to_jsonb(r)) FROM "Riesgo" r WHERE r."solucionId" = so.id), '[]'::jsonb),
             'cambios', COALESCE((SELECT jsonb_agg(to_jsonb(c)) FROM "SolicitudCambio" c WHERE c."solucionId" = so.id), '[]'::jsonb),
             'propuestas', COALESCE((SELECT jsonb_agg(jsonb_build_object('title', p.title, 'amount', p.amount, 'status', p.status::text)) FROM "Proposal" p WHERE p."leadId" = so."leadId"), '[]'::jsonb),
             'tareas', (SELECT jsonb_build_object('total', COUNT(*), 'conRequisito', COUNT(*) FILTER (WHERE b."prdRequisitoId" IS NOT NULL)) FROM "BacklogItem" b WHERE b."solucionId" = so.id))
           FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let sol = &d["sol"];
    let doc = |k: &str| sol[k].as_str().map(parse_o_texto).unwrap_or(Value::Null);
    let (prd, dis, plan) = (doc("prd"), doc("disenoTecnico"), doc("planEjecucion"));
    let arq = doc("arquitectura");
    let crono = doc("cronograma");
    let mut h: Vec<Value> = vec![];
    let tipo = sol["tipo"].as_str().unwrap_or("");

    // PRD
    let reqs = lista(&prd, "requisitos");
    if reqs.is_empty() {
        h.push(hallazgo("CRITICO", "PRD", "El PRD no tiene requisitos funcionales.", "prd"));
    } else {
        if !reqs.iter().any(|r| r["prioridad"] == "MUST") {
            h.push(hallazgo("ALERTA", "PRD", "Ningún requisito es MUST: ¿qué es indispensable para el cliente?", "prd"));
        }
        let sin_criterio = reqs.iter().filter(|r| vacio_txt(r, "criterioAceptacion")).count();
        if sin_criterio > 0 {
            h.push(hallazgo("ALERTA", "PRD", format!("{sin_criterio} requisito(s) sin criterio de aceptación: no se pueden verificar."), "prd"));
        }
        if prd["estadoDocumento"] == "APROBADO" {
            let sin_tarea = reqs.iter().filter(|r| r["backlogItemId"].as_str().map(|x| x.is_empty()).unwrap_or(true)).count();
            if sin_tarea > 0 {
                h.push(hallazgo("ALERTA", "Backlog", format!("{sin_tarea} requisito(s) del PRD aprobado todavía no tienen tarea en el backlog."), "prd"));
            }
        }
    }
    if lista(&prd, "dentroDeAlcance").is_empty() && tipo != "DEMO" {
        h.push(hallazgo("ALERTA", "PRD", "No hay ítems «dentro de alcance»: el alcance vendido no está definido.", "prd"));
    }
    if tipo != "DEMO" && lista(&prd, "requisitosNoFuncionales").iter().all(|r| r["categoria"] != "seguridad") {
        h.push(hallazgo("INFO", "PRD", "No hay requisitos no funcionales de seguridad.", "prd"));
    }

    // Diseño
    let ents = lista(&dis, "entidades");
    if !reqs.is_empty() && ents.is_empty() {
        h.push(hallazgo("ALERTA", "Diseño", "Hay requisitos pero el modelo de datos no tiene entidades.", "diseno"));
    }
    let sin_attr = ents.iter().filter(|e| vacio_txt(e, "atributos")).count();
    if sin_attr > 0 {
        h.push(hallazgo("ALERTA", "Diseño", format!("{sin_attr} entidad(es) sin atributos."), "diseno"));
    }
    if lista(&dis, "stack").is_empty() && !reqs.is_empty() {
        h.push(hallazgo("ALERTA", "Diseño", "No se definió el stack tecnológico.", "diseno"));
    }
    let menciona_integracion = lista(&prd, "dependencias").iter().chain(lista(&prd, "dentroDeAlcance").iter()).any(|x| {
        let t = x["texto"].as_str().unwrap_or("").to_lowercase();
        ["api", "integra", "whatsapp", "erp", "pasarela", "pago", "correo", "sap", "webhook"].iter().any(|p| t.contains(p))
    });
    if menciona_integracion && lista(&dis, "integraciones").is_empty() {
        h.push(hallazgo("ALERTA", "Diseño", "El PRD menciona integraciones o dependencias externas, pero el diseño no documenta ninguna integración.", "diseno"));
    }
    if (prd["estadoDocumento"] == "APROBADO" || dis["estadoDocumento"] == "APROBADO") && arq["nodes"].as_array().map(|n| n.is_empty()).unwrap_or(true) {
        h.push(hallazgo("ALERTA", "Arquitectura", "PRD o diseño aprobado, pero el diagrama de arquitectura está vacío.", "arquitectura"));
    }
    if dis["estadoDocumento"] == "APROBADO" && prd["estadoDocumento"] != "APROBADO" {
        h.push(hallazgo("ALERTA", "Diseño", "El diseño está aprobado pero el PRD no: el cómo se aprobó antes que el qué.", "diseno"));
    }

    // Plan de ejecución
    if plan["estadoDocumento"] == "APROBADO" {
        if vacio_txt(&plan, "qa") {
            h.push(hallazgo("ALERTA", "Plan de ejecución", "Plan aprobado sin estrategia de QA.", "plan-ejec"));
        }
        if lista(&plan, "raci").is_empty() {
            h.push(hallazgo("ALERTA", "Plan de ejecución", "Plan aprobado sin matriz de responsables (RACI).", "plan-ejec"));
        }
    }

    // Cronograma, hitos y dinero
    let hoy = fetch_text_opt(&st.pool, r#"SELECT to_char(NOW(), 'YYYY-MM-DD')"#, &[]).await?.unwrap_or_default();
    let fases = crono.as_array().cloned().unwrap_or_default();
    let sin_fechas = fases.iter().filter(|f| f["fechaInicio"].as_str().unwrap_or("").is_empty() || f["fechaFin"].as_str().unwrap_or("").is_empty()).count();
    if sin_fechas > 0 {
        h.push(hallazgo("INFO", "Cronograma", format!("{sin_fechas} fase(s) del cronograma sin fechas."), "cronograma"));
    }
    let atrasadas = fases.iter().filter(|f| f["fechaFin"].as_str().map(|x| !x.is_empty() && x < hoy.as_str()).unwrap_or(false) && f["estado"] != "COMPLETADA" && f["estado"] != "COMPLETADO").count();
    if atrasadas > 0 {
        h.push(hallazgo("ALERTA", "Cronograma", format!("{atrasadas} fase(s) pasaron su fecha de fin sin completarse."), "cronograma"));
    }
    let hitos = lista(&d, "hitos");
    let ultimo_hito = hitos.iter().filter_map(|x| x["fechaComprometida"].as_str().map(|f| f.chars().take(10).collect::<String>())).max();
    let ultima_fase = fases.iter().filter_map(|f| f["fechaFin"].as_str().filter(|x| !x.is_empty()).map(String::from)).max();
    if let (Some(uh), Some(uf)) = (&ultimo_hito, &ultima_fase) {
        if uf > uh {
            h.push(hallazgo("ALERTA", "Cronograma", format!("El cronograma termina el {uf}, después del último hito comprometido con el cliente ({uh})."), "cronograma"));
        }
    }
    let atrasados = hitos.iter().filter(|x| x["estado"] == "PENDIENTE" && x["fechaComprometida"].as_str().map(|f| f.chars().take(10).collect::<String>() < hoy).unwrap_or(false)).count();
    if atrasados > 0 {
        h.push(hallazgo("CRITICO", "Cumplimiento", format!("{atrasados} hito(s) vencieron y siguen pendientes."), "cumplimiento"));
    }
    let sin_aceptar = hitos.iter().filter(|x| x["estado"] == "CUMPLIDO" && x["aceptadoEn"].is_null()).count();
    if sin_aceptar > 0 {
        h.push(hallazgo("ALERTA", "Cumplimiento", format!("{sin_aceptar} hito(s) cumplido(s) sin aceptación registrada del cliente."), "cumplimiento"));
    }
    let valor = sol["valorEstimado"].as_f64().unwrap_or(0.0);
    let suma_montos: f64 = hitos.iter().map(|x| x["monto"].as_f64().unwrap_or(0.0)).sum();
    if valor > 0.0 && !hitos.is_empty() && (suma_montos - valor).abs() > 1.0 {
        h.push(hallazgo("ALERTA", "Cumplimiento", format!("Los hitos suman {suma_montos:.0} y el valor del proyecto es {valor:.0}: el calendario de pagos no cuadra."), "cumplimiento"));
    }
    if valor > 0.0 && hitos.is_empty() {
        h.push(hallazgo("INFO", "Cumplimiento", "No hay hitos ni calendario de pagos para el valor del proyecto.", "cumplimiento"));
    }
    let aceptada: f64 = lista(&d, "propuestas").iter().filter(|p| p["status"] == "ACCEPTED").map(|p| p["amount"].as_f64().unwrap_or(0.0)).sum();
    let cambios_aprobados: f64 = lista(&d, "cambios").iter().filter(|c| c["estado"] == "APROBADA" || c["estado"] == "IMPLEMENTADA").map(|c| c["impactoCosto"].as_f64().unwrap_or(0.0)).sum();
    if aceptada > 0.0 && (valor - (aceptada + cambios_aprobados)).abs() > 1.0 {
        h.push(hallazgo(
            "ALERTA",
            "Propuesta",
            format!("La propuesta aceptada es {aceptada:.0} (+{cambios_aprobados:.0} en cambios aprobados) y el valor de la solución es {valor:.0}."),
            "cumplimiento",
        ));
    }
    let abiertos = lista(&d, "riesgos").iter().filter(|r| r["estado"] == "ABIERTO" && (r["severidad"] == "CRITICA" || r["severidad"] == "ALTA") && vacio_txt(r, "mitigacion")).count();
    if abiertos > 0 {
        h.push(hallazgo("ALERTA", "Riesgos", format!("{abiertos} riesgo(s) alto(s) o crítico(s) abiertos sin mitigación."), "riesgos"));
    }
    let pend = lista(&d, "cambios").iter().filter(|c| c["estado"] == "SOLICITADA" || c["estado"] == "EN_EVALUACION").count();
    if pend > 0 {
        h.push(hallazgo("INFO", "Cambios", format!("{pend} solicitud(es) de cambio esperan decisión."), "cambios"));
    }
    let criticos = h.iter().filter(|x| x["nivel"] == "CRITICO").count();
    let alertas = h.iter().filter(|x| x["nivel"] == "ALERTA").count();
    Ok(Json(json!({ "hallazgos": h, "resumen": { "criticos": criticos, "alertas": alertas, "total": h.len() } })))
}

/// Compara con IA lo que prometió la propuesta comercial con lo que quedó en el PRD.
async fn cruce_propuesta(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let d = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('prd', so.prd,
             'propuestas', COALESCE((SELECT jsonb_agg(jsonb_build_object('title', p.title, 'description', p.description, 'amount', p.amount, 'status', p.status::text) ORDER BY (p.status::text = 'ACCEPTED') DESC, p."createdAt" DESC) FROM "Proposal" p WHERE p."leadId" = so."leadId"), '[]'::jsonb))
           FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let props = d["propuestas"].as_array().cloned().unwrap_or_default();
    if props.is_empty() {
        return Err(ApiError::bad_request("Esta solución no tiene propuestas comerciales asociadas a su lead."));
    }
    let prd = d["prd"].as_str().map(parse_o_texto).unwrap_or(Value::Null);
    let reqs: Vec<String> = lista(&prd, "requisitos").iter().enumerate().map(|(i, r)| format!("R{}: [{}] {}", i + 1, r["prioridad"].as_str().unwrap_or("?"), crate::routes::soluciones_ia::strip_html(r["texto"].as_str().unwrap_or("")))).collect();
    if reqs.is_empty() {
        return Err(ApiError::bad_request("El PRD no tiene requisitos para comparar."));
    }
    let alcance: Vec<String> = lista(&prd, "dentroDeAlcance").iter().map(|x| format!("- {}", x["texto"].as_str().unwrap_or(""))).collect();
    let props_txt = props
        .iter()
        .map(|p| format!("[{}] {} — {}\n{}", p["status"].as_str().unwrap_or(""), p["title"].as_str().unwrap_or(""), p["amount"], crate::routes::soluciones_ia::strip_html(p["description"].as_str().unwrap_or(""))))
        .collect::<Vec<_>>()
        .join("\n\n");
    let system = r#"Eres un auditor de alcance de proyectos de software. Comparas lo que la propuesta comercial PROMETIÓ al cliente con lo que quedó en los requisitos del PRD.
Devuelve SOLO un objeto JSON (sin markdown) con esta forma:
{ "faltantes": [ { "promesa": "string (lo prometido en la propuesta)", "motivo": "string (por qué ningún requisito lo cubre)" } ],
  "extras": [ { "requisito": "R3", "motivo": "string (qué no estaba prometido y podría ser alcance no cobrado)" } ],
  "cubiertos": number,
  "resumen": "string de 2-3 líneas" }
Solo cuenta como faltante algo concreto que la propuesta ofrece y que ningún requisito cubre; no inventes promesas. Prefiere pocos hallazgos y precisos."#;
    let usuario = format!("PROPUESTAS COMERCIALES:\n{}\n\nALCANCE DEFINIDO EN EL PRD:\n{}\n\nREQUISITOS DEL PRD:\n{}", crate::routes::soluciones_ia::truncar(&props_txt, 9000), alcance.join("\n"), reqs.join("\n"));
    let salida = llm::call_open_code(&st, system, &usuario, &format!("cruce-{id}"), 4096, 170)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))?;
    match crate::routes::soluciones_ia::extraer_objeto(&salida) {
        Some(v) if v.is_object() => Ok(Json(v)),
        _ => Err(ApiError::new(StatusCode::BAD_GATEWAY, "No se pudo interpretar la respuesta del modelo.")),
    }
}

// ═══════════════════════════ Control de cambios ═══════════════════════════
const ESTADOS_CAMBIO: [&str; 5] = ["SOLICITADA", "EN_EVALUACION", "APROBADA", "RECHAZADA", "IMPLEMENTADA"];

async fn cambios_listar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    Ok(Json(fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c."createdAt" DESC), '[]'::jsonb) FROM "SolicitudCambio" c WHERE c."solucionId" = $1"#, &[B::T(id)]).await?))
}

async fn cambio_crear(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let Some(titulo) = s(&body, "titulo").map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else {
        return Err(ApiError::bad_request("titulo requerido"));
    };
    let fila = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "SolicitudCambio" (id, "solucionId", titulo, descripcion, "impactoAlcance", "impactoCosto", "impactoDias", estado, "solicitadoPor", "solicitadoPorId", "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7::float8::int, 'SOLICITADA', $8, $9, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::T(id.clone()),
            B::T(titulo.clone()),
            B::OT(s_o_nulo(&body, "descripcion")),
            B::OT(s_o_nulo(&body, "impactoAlcance")),
            B::F(parse_float(body.get("impactoCosto"))),
            B::F(parse_float(body.get("impactoDias"))),
            B::T(nombre_de(&sesion)),
            B::OT(uid_s(&sesion)),
        ],
    )
    .await?;
    log_activity(&st.pool, "CREATED", &format!("registró la solicitud de cambio {titulo}"), "solucion", &id, uid(&sesion), None).await;
    Ok(Json(fila))
}

async fn cambio_actualizar(State(st): State<AppState>, sesion: Session, Path(cid): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let previo = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(c) FROM "SolicitudCambio" c WHERE c.id = $1"#, &[B::T(cid.clone())]).await?.ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let est_prev = previo["estado"].as_str().unwrap_or("SOLICITADA").to_string();
    let mut up = Upd::new(&cid);
    if let Some(t) = s_o_nulo(&body, "titulo") {
        up.set("titulo", B::T(t));
    }
    for k in ["descripcion", "impactoAlcance", "decisionNota"] {
        if presente(&body, k) {
            up.set(k, B::OT(s_o_nulo(&body, k)));
        }
    }
    if presente(&body, "impactoCosto") {
        up.set("impactoCosto", B::F(parse_float(body.get("impactoCosto"))));
    }
    if presente(&body, "impactoDias") {
        up.set_expr("impactoDias", B::F(parse_float(body.get("impactoDias"))), "{n}::float8::int");
    }
    let mut aplicar_valor: Option<f64> = None;
    let costo = previo["impactoCosto"].as_f64().unwrap_or(0.0);
    let aplicado = previo["aplicado"].as_bool().unwrap_or(false);
    if let Some(nuevo) = s_o_nulo(&body, "estado") {
        if !ESTADOS_CAMBIO.contains(&nuevo.as_str()) {
            return Err(ApiError::bad_request("Estado inválido"));
        }
        if nuevo != est_prev {
            if matches!(nuevo.as_str(), "APROBADA" | "RECHAZADA" | "IMPLEMENTADA") && !es_admin(&sesion) {
                return Err(ApiError::forbidden("Solo un administrador decide una solicitud de cambio"));
            }
            up.set("estado", B::T(nuevo.clone()));
            if matches!(nuevo.as_str(), "APROBADA" | "RECHAZADA") {
                up.set("decididoPor", B::T(nombre_de(&sesion)));
                up.sets.push(r#""decididoEn" = NOW()"#.to_string());
            }
            let costo_final = if presente(&body, "impactoCosto") { parse_float(body.get("impactoCosto")) } else { costo };
            if matches!(nuevo.as_str(), "APROBADA" | "IMPLEMENTADA") && !aplicado && costo_final != 0.0 {
                aplicar_valor = Some(costo_final);
                up.set("aplicado", B::Bo(true));
            } else if matches!(nuevo.as_str(), "RECHAZADA" | "SOLICITADA" | "EN_EVALUACION") && aplicado {
                aplicar_valor = Some(-costo);
                up.set("aplicado", B::Bo(false));
            }
        }
    }
    let fila = fetch_json_opt(&st.pool, &up.sql("SolicitudCambio"), &up.binds).await?.ok_or_else(|| ApiError::internal("Error al actualizar el cambio"))?;
    if let (Some(delta), Some(sol)) = (aplicar_valor, previo["solucionId"].as_str()) {
        exec(&st.pool, r#"UPDATE "Solucion" SET "valorEstimado" = "valorEstimado" + $2, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(sol.into()), B::F(delta)]).await?;
    }
    Ok(Json(fila))
}

async fn cambio_eliminar(State(st): State<AppState>, sesion: Session, Path(cid): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let previo = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(c) FROM "SolicitudCambio" c WHERE c.id = $1"#, &[B::T(cid.clone())]).await?.ok_or_else(|| ApiError::not_found("No encontrado"))?;
    if previo["aplicado"].as_bool().unwrap_or(false) {
        return Err(ApiError::bad_request("El cambio ya se sumó al valor del proyecto: devuélvelo a «Rechazada» antes de borrarlo."));
    }
    exec(&st.pool, r#"DELETE FROM "SolicitudCambio" WHERE id = $1"#, &[B::T(cid)]).await?;
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════ Economía ═══════════════════════════
async fn economia(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let v = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'valorEstimado', so."valorEstimado",
             'cambiosAprobados', COALESCE((SELECT SUM(c."impactoCosto") FROM "SolicitudCambio" c WHERE c."solucionId" = so.id AND c.aplicado), 0),
             'diasCambios', COALESCE((SELECT SUM(c."impactoDias") FROM "SolicitudCambio" c WHERE c."solucionId" = so.id AND c.estado IN ('APROBADA','IMPLEMENTADA')), 0),
             'hitos', (SELECT jsonb_build_object('total', COUNT(*), 'monto', COALESCE(SUM(h.monto), 0),
                          'facturado', COALESCE(SUM(h.monto) FILTER (WHERE h."estadoPago" IN ('FACTURADO','PAGADO')), 0),
                          'pagado', COALESCE(SUM(h.monto) FILTER (WHERE h."estadoPago" = 'PAGADO'), 0),
                          'cumplidos', COUNT(*) FILTER (WHERE h.estado = 'CUMPLIDO'),
                          'aceptados', COUNT(*) FILTER (WHERE h."aceptadoEn" IS NOT NULL))
                       FROM "Hito" h WHERE h."solucionId" = so.id),
             'tareas', (SELECT jsonb_build_object('total', COUNT(*), 'hechas', COUNT(*) FILTER (WHERE b.status = 'DONE'),
                          'enCurso', COUNT(*) FILTER (WHERE b.status = 'IN_PROGRESS'), 'fallidas', COUNT(*) FILTER (WHERE b.status = 'FAILED'))
                       FROM "BacklogItem" b WHERE b."solucionId" = so.id),
             'tokens', (SELECT jsonb_build_object('total', COALESCE(SUM((e.artifacts->'usage'->>'total_tokens')::bigint), 0),
                          'entrada', COALESCE(SUM((e.artifacts->'usage'->>'prompt_tokens')::bigint), 0),
                          'salida', COALESCE(SUM((e.artifacts->'usage'->>'completion_tokens')::bigint), 0),
                          'ejecuciones', COUNT(*))
                       FROM "TaskExecution" e JOIN "BacklogItem" b ON b.id = e."backlogItemId" WHERE b."solucionId" = so.id))
           FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id)],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrado"))?;
    Ok(Json(v))
}

// ═══════════════════════════ Fotografías (línea base, as-built) ═══════════════════════════
async fn crear_snapshot(st: &AppState, id: &str, etiqueta: &str, tipo: &str, se: &Session) -> Result<Value, sqlx::Error> {
    let contenido = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'nombre', so.nombre, 'tipo', so.tipo, 'estado', so.estado, 'valorEstimado', so."valorEstimado",
             'repositorio', so.repositorio, 'deployUrl', so."deployUrl",
             'prd', so.prd, 'disenoTecnico', so."disenoTecnico", 'planEjecucion', so."planEjecucion", 'arquitectura', so.arquitectura,
             'cronograma', so.cronograma, 'planTrabajo', so."planTrabajo", 'diagramas', so.diagramas,
             'hitos', COALESCE((SELECT jsonb_agg(to_jsonb(h)) FROM "Hito" h WHERE h."solucionId" = so.id), '[]'::jsonb),
             'riesgos', COALESCE((SELECT jsonb_agg(to_jsonb(r)) FROM "Riesgo" r WHERE r."solucionId" = so.id), '[]'::jsonb),
             'cambios', COALESCE((SELECT jsonb_agg(to_jsonb(c)) FROM "SolicitudCambio" c WHERE c."solucionId" = so.id), '[]'::jsonb))
           FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id.into())],
    )
    .await?
    .unwrap_or(Value::Null);
    let sid = new_id();
    exec(
        &st.pool,
        r#"INSERT INTO "SolucionSnapshot" (id, "solucionId", etiqueta, tipo, contenido, "creadoPorId", "creadoPorNombre", "createdAt") VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())"#,
        &[B::T(sid.clone()), B::T(id.into()), B::T(etiqueta.into()), B::T(tipo.into()), B::J(contenido), B::OT(uid_s(se)), B::T(nombre_de(se))],
    )
    .await?;
    Ok(json!({ "id": sid, "etiqueta": etiqueta, "tipo": tipo }))
}

async fn snapshots_listar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', x.id, 'etiqueta', x.etiqueta, 'tipo', x.tipo, 'creadoPorNombre', x."creadoPorNombre", 'createdAt', x."createdAt") ORDER BY x."createdAt" DESC), '[]'::jsonb)
               FROM "SolucionSnapshot" x WHERE x."solucionId" = $1"#,
            &[B::T(id)],
        )
        .await?,
    ))
}

async fn snapshot_crear(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let tipo = s_o_nulo(&body, "tipo").unwrap_or_else(|| "MANUAL".into());
    if !["LINEA_BASE", "AS_BUILT", "MANUAL"].contains(&tipo.as_str()) {
        return Err(ApiError::bad_request("Tipo inválido"));
    }
    if tipo == "AS_BUILT" && !es_admin(&sesion) {
        return Err(ApiError::forbidden("Solo un administrador congela el as-built"));
    }
    let etiqueta = s_o_nulo(&body, "etiqueta").unwrap_or_else(|| match tipo.as_str() {
        "LINEA_BASE" => "Línea base".into(),
        "AS_BUILT" => "As-built (entrega)".into(),
        _ => "Fotografía manual".into(),
    });
    Ok(Json(crear_snapshot(&st, &id, &etiqueta, &tipo, &sesion).await?))
}

async fn snapshot_obtener(State(st): State<AppState>, sesion: Session, Path((id, sid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    fetch_json_opt(&st.pool, r#"SELECT to_jsonb(x) FROM "SolucionSnapshot" x WHERE x.id = $1 AND x."solucionId" = $2"#, &[B::T(sid), B::T(id)])
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No encontrado"))
}

// ═══════════════════════════ Enlace para el cliente (solo lectura) ═══════════════════════════
fn token_nuevo() -> String {
    let mut rng = rand::thread_rng();
    (0..32).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect()
}

async fn enlace_crear(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    let token = token_nuevo();
    let n = exec(&st.pool, r#"UPDATE "Solucion" SET "tokenCliente" = $2, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(id.clone()), B::T(token.clone())]).await?;
    if n == 0 {
        return Err(ApiError::not_found("No encontrado"));
    }
    log_activity(&st.pool, "UPDATED", "generó el enlace de seguimiento para el cliente", "solucion", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "token": token, "ruta": format!("/cliente/{token}") })))
}

async fn enlace_revocar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_admin()?;
    exec(&st.pool, r#"UPDATE "Solucion" SET "tokenCliente" = NULL, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(id.clone())]).await?;
    log_activity(&st.pool, "UPDATED", "revocó el enlace de seguimiento del cliente", "solucion", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "ok": true })))
}

/// Sin sesión: lo que el cliente puede ver con su enlace. Nada interno (ni dinero, ni código, ni riesgos).
async fn vista_cliente(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    if token.len() != 32 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::not_found("Enlace no válido"));
    }
    let v = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'nombre', so.nombre, 'empresa', so.empresa, 'estado', so.estado, 'descripcion', so.descripcion,
             'actualizado', so."updatedAt",
             'fase', (SELECT jsonb_build_object('clave', pf."faseActual", 'estado', pf.estado,
                        'nombre', (SELECT f->>'nombre' FROM jsonb_array_elements(pf.definicion->'fases') f WHERE f->>'clave' = pf."faseActual" LIMIT 1))
                      FROM "ProyectoFase" pf WHERE pf."solucionId" = so.id),
             'avance', (SELECT jsonb_build_object('total', COUNT(*), 'hechas', COUNT(*) FILTER (WHERE b.status = 'DONE')) FROM "BacklogItem" b WHERE b."solucionId" = so.id),
             'hitos', COALESCE((SELECT jsonb_agg(jsonb_build_object('titulo', h.titulo, 'descripcion', h.descripcion, 'fechaComprometida', h."fechaComprometida",
                          'fechaReal', h."fechaReal", 'estado', h.estado, 'aceptadoEn', h."aceptadoEn") ORDER BY h."fechaComprometida" ASC) FROM "Hito" h WHERE h."solucionId" = so.id), '[]'::jsonb),
             'cambios', COALESCE((SELECT jsonb_agg(jsonb_build_object('titulo', c.titulo, 'estado', c.estado, 'impactoDias', c."impactoDias") ORDER BY c."createdAt" DESC)
                          FROM "SolicitudCambio" c WHERE c."solucionId" = so.id AND c.estado IN ('APROBADA','IMPLEMENTADA')), '[]'::jsonb))
           FROM "Solucion" so WHERE so."tokenCliente" = $1"#,
        &[B::T(token)],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Enlace no válido"))?;
    Ok(Json(v))
}

// ═══════════════════════════ Solución adicional (fase 2, mantenimiento) ═══════════════════════════
async fn crear_adicional(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let Some(nombre) = s(&body, "nombre").map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else {
        return Err(ApiError::bad_request("nombre requerido"));
    };
    let padre = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', so.id, 'empresa', COALESCE(so.empresa, (SELECT l."companyName" FROM "Lead" l WHERE l.id = so."leadId")), 'tipo', so.tipo, 'parentId', so."parentId") FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Solución de origen no encontrada"))?;
    // El adicional cuelga del proyecto original, no de otro adicional.
    let raiz = padre["parentId"].as_str().map(String::from).unwrap_or(id.clone());
    let tipo = s_o_nulo(&body, "tipo").filter(|t| ["PROJECT", "DEMO", "PARTNERSHIP"].contains(&t.as_str())).unwrap_or_else(|| "PROJECT".into());
    let codigo = crate::routes::council::codigo_solucion_unico(&st, &crate::routes::council::generar_codigo_solucion(&nombre)).await?;
    let nueva = fetch_json(
        &st.pool,
        &format!(
            r#"WITH ins AS (INSERT INTO "Solucion" (id, nombre, descripcion, tipo, estado, "valorEstimado", empresa, "parentId", "solucionCode", "createdAt", "updatedAt")
                 VALUES ($1, $2, $3, $4, 'ACTIVO', $5, $6, $7, $8, NOW(), NOW()) RETURNING *)
               SELECT {SOLUCION_JSON} FROM ins so"#
        ),
        &[
            B::T(new_id()),
            B::T(nombre.clone()),
            B::OT(s_o_nulo(&body, "descripcion")),
            B::T(tipo),
            B::F(parse_float(body.get("valorEstimado"))),
            B::OT(padre["empresa"].as_str().map(String::from)),
            B::T(raiz),
            B::T(codigo),
        ],
    )
    .await?;
    log_activity(&st.pool, "CREATED", &format!("creó la solución adicional {nombre}"), "solucion", nueva["id"].as_str().unwrap_or_default(), uid(&sesion), None).await;
    Ok(Json(nueva))
}

// ═══════════════════════════ Diagramas (Mermaid) ═══════════════════════════
fn recorte(t: &str, n: usize) -> String {
    t.chars().take(n).collect()
}

async fn diagramas_generar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let tipo = s_o_nulo(&body, "tipo").unwrap_or_else(|| "er".into());
    let (instruccion, inicios): (&str, &[&str]) = match tipo.as_str() {
        "er" => ("un diagrama entidad-relación en sintaxis Mermaid `erDiagram`: una entidad por cada entidad del modelo de datos, con sus atributos principales (tipo y nombre) y las relaciones con su cardinalidad", &["erDiagram"]),
        "secuencia" => ("un diagrama de secuencia en sintaxis Mermaid `sequenceDiagram` del flujo principal que describe la instrucción (actores, componentes del sistema e integraciones externas, con los mensajes en orden)", &["sequenceDiagram"]),
        "c4" => ("un diagrama de contexto/contenedores en sintaxis Mermaid `flowchart TB` (estilo C4): personas, el sistema, sus contenedores principales y los sistemas externos, con subgraph para los límites", &["flowchart", "graph"]),
        "flujo" => ("un diagrama de flujo en sintaxis Mermaid `flowchart TD` del proceso que describe la instrucción, con decisiones y caminos de error", &["flowchart", "graph"]),
        _ => return Err(ApiError::bad_request("Tipo de diagrama inválido (er, secuencia, c4 o flujo)")),
    };
    let d = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('nombre', nombre, 'prd', prd, 'diseno', "disenoTecnico", 'arq', arquitectura) FROM "Solucion" WHERE id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let prd = d["prd"].as_str().map(parse_o_texto).unwrap_or(Value::Null);
    let dis = d["diseno"].as_str().map(parse_o_texto).unwrap_or(Value::Null);
    let arq = d["arq"].as_str().map(parse_o_texto).unwrap_or(Value::Null);
    let mut ctx: Vec<String> = vec![format!("Solución: {}", d["nombre"].as_str().unwrap_or(""))];
    let reqs: Vec<String> = lista(&prd, "requisitos").iter().take(30).map(|r| format!("- [{}] {}", r["prioridad"].as_str().unwrap_or("?"), recorte(&crate::routes::soluciones_ia::strip_html(r["texto"].as_str().unwrap_or("")), 220))).collect();
    if !reqs.is_empty() {
        ctx.push(format!("REQUISITOS:\n{}", reqs.join("\n")));
    }
    let ents: Vec<String> = lista(&dis, "entidades").iter().map(|e| format!("- {}: {} | relaciones: {}", e["nombre"].as_str().unwrap_or(""), recorte(e["atributos"].as_str().unwrap_or(""), 240), recorte(e["relaciones"].as_str().unwrap_or(""), 160))).collect();
    if !ents.is_empty() {
        ctx.push(format!("MODELO DE DATOS:\n{}", ents.join("\n")));
    }
    let integ: Vec<String> = lista(&dis, "integraciones").iter().map(|e| format!("- {}: {}", e["sistema"].as_str().unwrap_or(""), recorte(e["proposito"].as_str().unwrap_or(""), 120))).collect();
    if !integ.is_empty() {
        ctx.push(format!("INTEGRACIONES:\n{}", integ.join("\n")));
    }
    let stack: Vec<String> = lista(&dis, "stack").iter().map(|e| format!("- {}: {}", e["capa"].as_str().unwrap_or(""), e["tecnologia"].as_str().unwrap_or(""))).collect();
    if !stack.is_empty() {
        ctx.push(format!("STACK:\n{}", stack.join("\n")));
    }
    let nodos: Vec<String> = lista(&arq, "nodes").iter().map(|n| format!("{} ({})", n["label"].as_str().unwrap_or(""), n["type"].as_str().unwrap_or(""))).collect();
    if !nodos.is_empty() {
        ctx.push(format!("COMPONENTES DE LA ARQUITECTURA: {}", nodos.join(", ")));
    }
    let extra = s_o_nulo(&body, "instruccion").map(|x| format!("\n\nINSTRUCCIÓN DE LA PERSONA: {x}")).unwrap_or_default();
    let system = format!("Eres un arquitecto de software. Genera {instruccion}. Devuelve SOLO el código Mermaid, sin bloque de markdown y sin texto alrededor. Usa únicamente nombres que existan en el contexto; no inventes entidades ni sistemas. Usa identificadores simples sin espacios ni tildes y etiquetas entre comillas cuando lleven espacios.");
    let usuario = format!("{}{extra}", ctx.join("\n\n"));
    let salida = llm::call_open_code(&st, &system, &usuario, &format!("diagrama-{id}"), 4096, 170).await.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))?;
    let mut codigo = salida.trim().to_string();
    if let Some(i) = codigo.find("```") {
        let resto = &codigo[i + 3..];
        let resto = resto.strip_prefix("mermaid").unwrap_or(resto);
        codigo = resto.split("```").next().unwrap_or("").trim().to_string();
    }
    if !inicios.iter().any(|p| codigo.starts_with(p)) {
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "El modelo no devolvió un diagrama Mermaid válido. Vuelve a intentarlo."));
    }
    Ok(Json(json!({ "tipo": tipo, "codigo": codigo })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huella_ignora_lo_que_mantiene_el_servidor() {
        let a = r#"{"requisitos":[{"id":"r1","texto":"x","estado":"PROPUESTO","backlogItemId":null}],"aprobacion":{"porNombre":"A"}}"#;
        let b = r#"{"requisitos":[{"id":"r1","texto":"x","estado":"VERIFICADO","backlogItemId":"t1"}]}"#;
        assert_eq!(huella("prd", a), huella("prd", b));
        let c = r#"{"requisitos":[{"id":"r1","texto":"y","estado":"VERIFICADO"}]}"#;
        assert_ne!(huella("prd", a), huella("prd", c));
    }

    #[test]
    fn vacios_son_lo_mismo() {
        assert_eq!(huella("prd", ""), huella("prd", "{}"));
        assert_eq!(huella("cronograma", "[]"), huella("cronograma", ""));
        assert_ne!(huella("planTrabajo", "hola"), huella("planTrabajo", ""));
    }

    #[test]
    fn fusion_conserva_el_avance_del_servidor_si_la_persona_no_lo_toco() {
        let base: Value = serde_json::from_str(r#"{"requisitos":[{"id":"r1","estado":"APROBADO","backlogItemId":"t1"},{"id":"r2","estado":"APROBADO"}]}"#).unwrap();
        let actual: Value = serde_json::from_str(r#"{"requisitos":[{"id":"r1","estado":"VERIFICADO","backlogItemId":"t1"},{"id":"r2","estado":"IMPLEMENTADO"}]}"#).unwrap();
        // la persona cambió r2 a mano (COULD → estado MANUAL) y no tocó r1
        let mut nuevo: Value = serde_json::from_str(r#"{"requisitos":[{"id":"r1","estado":"APROBADO","backlogItemId":"t1"},{"id":"r2","estado":"PROPUESTO"},{"id":"r3","estado":"PROPUESTO"}]}"#).unwrap();
        fusionar_seguimiento(&mut nuevo, &base, &actual);
        assert_eq!(nuevo["requisitos"][0]["estado"], "VERIFICADO");
        assert_eq!(nuevo["requisitos"][1]["estado"], "PROPUESTO");
        assert_eq!(nuevo["requisitos"][2]["estado"], "PROPUESTO");
    }

    #[test]
    fn el_token_del_cliente_tiene_32_hex() {
        let t = token_nuevo();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
