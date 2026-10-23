//! Espacio de trabajo unificado (MASD-0024-0013): un solo modelo de edición para todas las piezas de la solución.
//!
//! Cada pieza ("artefacto": prd, disenoTecnico, planEjecucion, arquitectura, cronograma, diagramas, planTrabajo) tiene estado
//! (borrador / en revisión / aprobado), versiones, comentarios y propuestas. Las propuestas son lo que el motor (o una persona)
//! quiere cambiar: no se aplican hasta que alguien las acepta. El contenido sigue viviendo en las columnas de `Solucion`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    routes::misc::{civil_desde_dias, dias_desde_civil},
    routes::solucion_hub::{ahora, es_admin, huella, nombre_de, parse_o_texto, uid_s},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, new_id, s, s_o_nulo, uid, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/soluciones/{id}/artefactos", get(listar))
        .route("/api/soluciones/{id}/artefactos/{clave}/estado", put(cambiar_estado))
        .route("/api/soluciones/{id}/artefactos/{clave}/versiones", get(versiones_listar))
        .route("/api/soluciones/{id}/artefactos/{clave}/versiones/{version}", get(version_obtener))
        .route("/api/soluciones/{id}/artefactos/{clave}/comentarios", get(comentarios_listar).post(comentario_crear))
        .route("/api/comentarios-artefacto/{cid}", put(comentario_actualizar))
        .route("/api/soluciones/{id}/propuestas", get(propuestas_listar).post(propuesta_crear))
        .route("/api/propuestas-artefacto/{pid}/decidir", post(propuesta_decidir))
}

pub const CLAVES: [&str; 7] = ["prd", "disenoTecnico", "planEjecucion", "arquitectura", "cronograma", "diagramas", "planTrabajo"];
/// Los que guardan su estado dentro del documento (JSON con `estadoDocumento`).
const CON_ESTADO_PROPIO: [&str; 3] = ["prd", "disenoTecnico", "planEjecucion"];
const ESTADOS: [&str; 3] = ["BORRADOR", "EN_REVISION", "APROBADO"];
const MAX_VERSIONES: i64 = 50;
const TIPOS_NODO: [&str; 8] = ["frontend", "backend", "database", "api", "ia", "queue", "cache", "externo"];

fn clave_valida(c: &str) -> Result<(), ApiError> {
    if CLAVES.contains(&c) { Ok(()) } else { Err(ApiError::bad_request("Artefacto desconocido")) }
}

async fn texto_de(st: &AppState, id: &str, clave: &str) -> Result<Option<String>, ApiError> {
    clave_valida(clave)?;
    // `clave` sale de CLAVES: no es entrada libre.
    let sql = format!(r#"SELECT "{clave}" FROM "Solucion" WHERE id = $1"#);
    let existe = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Solucion" WHERE id = $1"#, &[B::T(id.into())]).await?;
    if existe.is_none() {
        return Err(ApiError::not_found("No encontrado"));
    }
    Ok(fetch_text_opt(&st.pool, &sql, &[B::T(id.into())]).await?)
}

// ═══════════════════════════ Listado (el árbol del espacio) ═══════════════════════════
async fn listar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let sol = fetch_json_opt(
        &st.pool,
        r#"SELECT to_jsonb(so) FROM "Solucion" so WHERE so.id = $1"#,
        &[B::T(id.clone())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let estados = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(e)), '[]'::jsonb) FROM "ArtefactoEstado" e WHERE e."solucionId" = $1"#, &[B::T(id.clone())]).await?;
    let cuentas = |sql: &'static str| {
        let st = st.clone();
        let id = id.clone();
        async move { fetch_json(&st.pool, sql, &[B::T(id)]).await }
    };
    let comentarios = cuentas(r#"SELECT COALESCE(jsonb_object_agg(clave, n), '{}'::jsonb) FROM (SELECT clave, COUNT(*) n FROM "ArtefactoComentario" WHERE "solucionId" = $1 AND NOT resuelto GROUP BY clave) x"#).await?;
    let propuestas = cuentas(r#"SELECT COALESCE(jsonb_object_agg(clave, n), '{}'::jsonb) FROM (SELECT clave, COUNT(*) n FROM "ArtefactoPropuesta" WHERE "solucionId" = $1 AND estado = 'PENDIENTE' GROUP BY clave) x"#).await?;
    let versiones = cuentas(r#"SELECT COALESCE(jsonb_object_agg(clave, n), '{}'::jsonb) FROM (SELECT clave, COUNT(*) n FROM "ArtefactoVersion" WHERE "solucionId" = $1 GROUP BY clave UNION ALL SELECT 'prd', COUNT(*) FROM "SolucionPrdVersion" WHERE "solucionId" = $1) x"#).await?;
    let mut out: Vec<Value> = vec![];
    for k in CLAVES {
        let txt = sol[k].as_str().unwrap_or("");
        let doc = parse_o_texto(txt);
        let (estado, aprobacion, reabierto) = if CON_ESTADO_PROPIO.contains(&k) {
            (doc["estadoDocumento"].as_str().unwrap_or("BORRADOR").to_string(), doc["aprobacion"].clone(), doc["reabierto"].clone())
        } else {
            let fila = estados.as_array().and_then(|a| a.iter().find(|e| e["clave"] == k));
            (
                fila.and_then(|f| f["estado"].as_str()).unwrap_or("BORRADOR").to_string(),
                fila.filter(|f| f["estado"] == "APROBADO").map(|f| json!({ "porNombre": f["aprobadoPor"], "en": f["aprobadoEn"] })).unwrap_or(Value::Null),
                fila.map(|f| f["reabiertoEn"].clone()).filter(|r| !r.is_null()).map(|r| json!({ "en": r })).unwrap_or(Value::Null),
            )
        };
        out.push(json!({
            "clave": k,
            "estado": estado,
            "aprobacion": aprobacion,
            "reabierto": reabierto,
            "conContenido": !matches!(huella(k, txt), Value::Null),
            "versiones": versiones[k].as_i64().unwrap_or(0),
            "comentarios": comentarios[k].as_i64().unwrap_or(0),
            "propuestas": propuestas[k].as_i64().unwrap_or(0),
        }));
    }
    Ok(Json(Value::Array(out)))
}

// ═══════════════════════════ Estado (aprobar, pasar a revisión) ═══════════════════════════
/// Reescribe una columna solo si nadie la cambió desde que se leyó (tres intentos).
async fn actualizar_columna<F>(st: &AppState, id: &str, clave: &str, mut f: F) -> Result<(String, String), ApiError>
where
    F: FnMut(&str) -> Result<String, ApiError>,
{
    for _ in 0..3 {
        let viejo = texto_de(st, id, clave).await?.unwrap_or_default();
        let nuevo = f(&viejo)?;
        let sql = format!(r#"UPDATE "Solucion" SET "{clave}" = $2, "updatedAt" = NOW() WHERE id = $1 AND COALESCE("{clave}", '') = $3"#);
        if exec(&st.pool, &sql, &[B::T(id.into()), B::T(nuevo.clone()), B::T(viejo.clone())]).await? > 0 {
            return Ok((viejo, nuevo));
        }
    }
    Err(ApiError::new(StatusCode::CONFLICT, "El documento cambió mientras se actualizaba: vuelve a intentarlo."))
}

pub(crate) async fn guardar_version(st: &AppState, id: &str, clave: &str, contenido: &str, motivo: &str, quien: &str) {
    if clave == "prd" {
        let _ = exec(
            &st.pool,
            r#"INSERT INTO "SolucionPrdVersion" (id, "solucionId", version, contenido, motivo, "usuarioId", "createdAt")
               SELECT $1, $2, COALESCE(MAX(version), 0) + 1, $3, $4, NULL, NOW() FROM "SolucionPrdVersion" WHERE "solucionId" = $2"#,
            &[B::T(new_id()), B::T(id.into()), B::T(contenido.into()), B::T(motivo.into())],
        )
        .await;
        return;
    }
    let _ = exec(
        &st.pool,
        r#"INSERT INTO "ArtefactoVersion" (id, "solucionId", clave, version, contenido, motivo, "usuarioNombre", "createdAt")
           SELECT $1, $2, $3, COALESCE(MAX(version), 0) + 1, $4, $5, $6, NOW() FROM "ArtefactoVersion" WHERE "solucionId" = $2 AND clave = $3"#,
        &[B::T(new_id()), B::T(id.into()), B::T(clave.into()), B::T(contenido.into()), B::T(motivo.into()), B::T(quien.into())],
    )
    .await;
    let _ = exec(
        &st.pool,
        r#"DELETE FROM "ArtefactoVersion" WHERE "solucionId" = $1 AND clave = $2 AND version <= (SELECT COALESCE(MAX(version), 0) - $3 FROM "ArtefactoVersion" WHERE "solucionId" = $1 AND clave = $2)"#,
        &[B::T(id.into()), B::T(clave.into()), B::I(MAX_VERSIONES)],
    )
    .await;
}

/// Lo que pasa cuando cambia el contenido de un artefacto aprobado: vuelve a revisión.
pub async fn reabrir_si_aprobado(st: &AppState, id: &str, clave: &str, quien: &str) {
    if CON_ESTADO_PROPIO.contains(&clave) {
        return; // estos lo hacen dentro de su propio JSON (solucion_actualizar y aplicar_ops)
    }
    let _ = exec(
        &st.pool,
        r#"UPDATE "ArtefactoEstado" SET estado = 'EN_REVISION', "aprobadoPor" = NULL, "aprobadoEn" = NULL, "reabiertoEn" = NOW(), "updatedAt" = NOW()
           WHERE "solucionId" = $1 AND clave = $2 AND estado = 'APROBADO'"#,
        &[B::T(id.into()), B::T(clave.into())],
    )
    .await;
    let _ = quien;
}

async fn cambiar_estado(State(st): State<AppState>, sesion: Session, Path((id, clave)): Path<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    clave_valida(&clave)?;
    let Some(estado) = s_o_nulo(&body, "estado").filter(|e| ESTADOS.contains(&e.as_str())) else {
        return Err(ApiError::bad_request("Estado inválido"));
    };
    if estado == "APROBADO" && !es_admin(&sesion) {
        return Err(ApiError::forbidden("Solo un administrador puede aprobar"));
    }
    let marca = ahora(&st).await;
    if CON_ESTADO_PROPIO.contains(&clave.as_str()) {
        let (nombre, sid) = (nombre_de(&sesion), sesion.id.clone());
        actualizar_columna(&st, &id, &clave, |viejo| {
            let mut d = parse_o_texto(viejo);
            if !d.is_object() {
                d = json!({});
            }
            d["estadoDocumento"] = json!(estado);
            if let Value::Object(o) = &mut d {
                o.remove("reabierto");
                o.remove("aprobacion");
            }
            if estado == "APROBADO" {
                d["aprobacion"] = json!({ "porId": sid, "porNombre": nombre, "en": marca });
            }
            Ok(d.to_string())
        })
        .await?;
    } else {
        exec(
            &st.pool,
            r#"INSERT INTO "ArtefactoEstado" ("solucionId", clave, estado, "aprobadoPor", "aprobadoEn", "updatedAt")
               VALUES ($1, $2, $3, CASE WHEN $3 = 'APROBADO' THEN $4 END, CASE WHEN $3 = 'APROBADO' THEN NOW() END, NOW())
               ON CONFLICT ("solucionId", clave) DO UPDATE SET estado = EXCLUDED.estado, "aprobadoPor" = EXCLUDED."aprobadoPor",
                 "aprobadoEn" = EXCLUDED."aprobadoEn", "reabiertoEn" = NULL, "updatedAt" = NOW()"#,
            &[B::T(id.clone()), B::T(clave.clone()), B::T(estado.clone()), B::T(nombre_de(&sesion))],
        )
        .await?;
    }
    // La línea base se crea sola cuando PRD y diseño quedan aprobados.
    if estado == "APROBADO" && (clave == "prd" || clave == "disenoTecnico") {
        let aprobado = |txt: Option<String>| txt.map(|t| parse_o_texto(&t)["estadoDocumento"] == "APROBADO").unwrap_or(false);
        let (p, d) = (texto_de(&st, &id, "prd").await?, texto_de(&st, &id, "disenoTecnico").await?);
        if aprobado(p) && aprobado(d) {
            let hay = fetch_text_opt(&st.pool, r#"SELECT id FROM "SolucionSnapshot" WHERE "solucionId" = $1 AND tipo = 'LINEA_BASE' LIMIT 1"#, &[B::T(id.clone())]).await?;
            if hay.is_none() {
                let _ = crate::routes::solucion_hub::crear_snapshot(&st, &id, "Línea base (PRD y diseño aprobados)", "LINEA_BASE", &sesion).await;
            }
        }
    }
    log_activity(&st.pool, "UPDATED", &format!("puso «{clave}» en estado {estado}"), "solucion", &id, uid(&sesion), None).await;
    Ok(Json(json!({ "ok": true, "estado": estado })))
}

// ═══════════════════════════ Versiones ═══════════════════════════
async fn versiones_listar(State(st): State<AppState>, sesion: Session, Path((id, clave)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    clave_valida(&clave)?;
    let sql = if clave == "prd" {
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY v.version DESC), '[]'::jsonb) FROM (SELECT version, motivo, NULL::text AS "usuarioNombre", length(contenido) AS chars, "createdAt" FROM "SolucionPrdVersion" WHERE "solucionId" = $1 AND $2::text = 'prd') v"#
    } else {
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY v.version DESC), '[]'::jsonb) FROM (SELECT version, motivo, "usuarioNombre", length(contenido) AS chars, "createdAt" FROM "ArtefactoVersion" WHERE "solucionId" = $1 AND clave = $2) v"#
    };
    Ok(Json(fetch_json(&st.pool, sql, &[B::T(id), B::T(clave)]).await?))
}

async fn version_obtener(State(st): State<AppState>, sesion: Session, Path((id, clave, version)): Path<(String, String, i32)>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    clave_valida(&clave)?;
    let sql = if clave == "prd" {
        r#"SELECT to_jsonb(v) FROM (SELECT version, motivo, contenido, "createdAt" FROM "SolucionPrdVersion" WHERE "solucionId" = $1 AND version = $3 AND $2::text = 'prd') v"#
    } else {
        r#"SELECT to_jsonb(v) FROM (SELECT version, motivo, contenido, "createdAt" FROM "ArtefactoVersion" WHERE "solucionId" = $1 AND clave = $2 AND version = $3) v"#
    };
    fetch_json_opt(&st.pool, sql, &[B::T(id), B::T(clave), B::I(version as i64)]).await?.map(Json).ok_or_else(|| ApiError::not_found("Esa versión no existe"))
}

// ═══════════════════════════ Comentarios ═══════════════════════════
async fn comentarios_listar(State(st): State<AppState>, sesion: Session, Path((id, clave)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    clave_valida(&clave)?;
    Ok(Json(
        fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c."createdAt" ASC), '[]'::jsonb) FROM "ArtefactoComentario" c WHERE c."solucionId" = $1 AND c.clave = $2"#, &[B::T(id), B::T(clave)]).await?,
    ))
}

async fn comentario_crear(State(st): State<AppState>, sesion: Session, Path((id, clave)): Path<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    clave_valida(&clave)?;
    let Some(texto) = s(&body, "texto").map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else {
        return Err(ApiError::bad_request("texto requerido"));
    };
    if texto.chars().count() > 4000 {
        return Err(ApiError::bad_request("El comentario es demasiado largo"));
    }
    let fila = fetch_json_opt(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "ArtefactoComentario" (id, "solucionId", clave, "autorId", "autorNombre", texto, "createdAt")
             SELECT $1, so.id, $3, $4, $5, $6, NOW() FROM "Solucion" so WHERE so.id = $2 RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[B::T(new_id()), B::T(id), B::T(clave), B::OT(uid_s(&sesion)), B::T(nombre_de(&sesion)), B::T(texto)],
    )
    .await?;
    fila.map(Json).ok_or_else(|| ApiError::not_found("No encontrado"))
}

async fn comentario_actualizar(State(st): State<AppState>, sesion: Session, Path(cid): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let Some(r) = body.get("resuelto").and_then(|v| v.as_bool()) else { return Err(ApiError::bad_request("resuelto requerido")) };
    fetch_json_opt(&st.pool, r#"WITH up AS (UPDATE "ArtefactoComentario" SET resuelto = $2 WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#, &[B::T(cid), B::Bo(r)])
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No encontrado"))
}

// ═══════════════════════════ Propuestas ═══════════════════════════
fn texto_corto(v: &Value, k: &str, max: usize) -> String {
    v[k].as_str().unwrap_or("").chars().take(max).collect()
}

/// Posición libre para un componente propuesto: debajo del más bajo, en la primera columna libre.
fn posicion_libre(nodos: &[Value]) -> (i64, i64) {
    let max_y = nodos.iter().filter_map(|n| n["y"].as_f64()).fold(f64::MIN, f64::max);
    if nodos.is_empty() || max_y == f64::MIN {
        return (16, 16);
    }
    (16, max_y as i64 + 88)
}

/// Aplica las operaciones de una propuesta al texto del artefacto. Pura: no toca la base de datos.
pub fn aplicar_ops(clave: &str, actual: &str, ops: &Value) -> Result<String, ApiError> {
    let lista = ops.as_array().filter(|o| !o.is_empty()).ok_or_else(|| ApiError::bad_request("La propuesta no trae operaciones"))?;
    match clave {
        "arquitectura" => {
            let base = parse_o_texto(actual);
            let (mut nodos, mut conex) = match &base {
                Value::Array(a) => (a.clone(), vec![]),
                Value::Object(_) => (base["nodes"].as_array().cloned().unwrap_or_default(), base["connections"].as_array().cloned().unwrap_or_default()),
                _ => (vec![], vec![]),
            };
            for op in lista {
                if op["op"] != "agregar" {
                    return Err(ApiError::bad_request("En la arquitectura solo se puede agregar"));
                }
                let mut v = op["valor"].clone();
                if !v.is_object() {
                    return Err(ApiError::bad_request("Cada valor debe ser un objeto"));
                }
                if v["id"].as_str().map(|x| x.is_empty()).unwrap_or(true) {
                    v["id"] = json!(new_id());
                }
                match op["ruta"].as_str() {
                    Some("nodes") => {
                        v["label"] = json!(texto_corto(&v, "label", 40));
                        if !TIPOS_NODO.contains(&v["type"].as_str().unwrap_or("")) {
                            v["type"] = json!("externo");
                        }
                        if v["x"].as_f64().is_none() || v["y"].as_f64().is_none() {
                            let (x, y) = posicion_libre(&nodos);
                            v["x"] = json!(x);
                            v["y"] = json!(y);
                        }
                        nodos.push(v);
                    }
                    Some("connections") => {
                        let (f, t) = (v["from"].as_str().unwrap_or(""), v["to"].as_str().unwrap_or(""));
                        let existe = |id: &str| nodos.iter().any(|n| n["id"] == id);
                        if !existe(f) || !existe(t) {
                            return Err(ApiError::bad_request("Una conexión apunta a un componente que no existe"));
                        }
                        conex.push(v);
                    }
                    _ => return Err(ApiError::bad_request("Ruta inválida para la arquitectura")),
                }
            }
            Ok(json!({ "nodes": nodos, "connections": conex }).to_string())
        }
        "disenoTecnico" => {
            let mut d = parse_o_texto(actual);
            if !d.is_object() {
                d = json!({});
            }
            for op in lista {
                let ruta = op["ruta"].as_str().unwrap_or("");
                if op["op"] != "agregar" || !["entidades", "stack", "integraciones", "decisiones"].contains(&ruta) {
                    return Err(ApiError::bad_request("En el diseño solo se pueden agregar entidades, stack, integraciones o decisiones"));
                }
                let mut v = op["valor"].clone();
                if !v.is_object() {
                    return Err(ApiError::bad_request("Cada valor debe ser un objeto"));
                }
                if v["id"].as_str().map(|x| x.is_empty()).unwrap_or(true) {
                    v["id"] = json!(new_id());
                }
                if !d[ruta].is_array() {
                    d[ruta] = json!([]);
                }
                d[ruta].as_array_mut().map(|a| a.push(v));
            }
            Ok(d.to_string())
        }
        "cronograma" => {
            let mut fases = match parse_o_texto(actual) {
                Value::Array(a) => a,
                Value::Null => vec![],
                _ if actual.trim().is_empty() => vec![],
                _ => return Err(ApiError::bad_request("El cronograma tiene un formato que no se puede editar con propuestas")),
            };
            for op in lista {
                match op["op"].as_str() {
                    Some("agregar_fase") => {
                        let v = &op["valor"];
                        let (ini, fin) = (v["fechaInicio"].as_str().unwrap_or(""), v["fechaFin"].as_str().unwrap_or(""));
                        if !fecha_ok(ini) || !fecha_ok(fin) || fin < ini {
                            return Err(ApiError::bad_request("La fase nueva necesita fechaInicio y fechaFin válidas (AAAA-MM-DD) y el fin no puede ser anterior al inicio"));
                        }
                        let depende = v["dependeDe"].as_str().filter(|d| !d.is_empty());
                        if let Some(d) = depende {
                            if !fases.iter().any(|f| f["id"].as_str() == Some(d)) {
                                return Err(ApiError::bad_request("dependeDe apunta a una fase que no existe"));
                            }
                        }
                        let mut nueva = json!({
                            "id": new_id(), "fase": v["fase"].as_str().unwrap_or("").trim().chars().take(120).collect::<String>(), "fechaInicio": ini, "fechaFin": fin,
                            "estado": match v["estado"].as_str() { Some(e) if ["PENDIENTE", "EN_CURSO", "COMPLETADA"].contains(&e) => e, _ => "PENDIENTE" },
                        });
                        if let Some(d) = depende {
                            nueva["dependeDe"] = json!(d);
                        }
                        fases.push(nueva);
                        continue;
                    }
                    Some("editar_fase") => {
                        let id = op["id"].as_str().unwrap_or("");
                        let pos = fases.iter().position(|f| f["id"].as_str() == Some(id)).ok_or_else(|| ApiError::bad_request("La fase a editar no existe"))?;
                        let c = &op["cambios"];
                        for k in ["fase", "fechaInicio", "fechaFin", "estado", "dependeDe"] {
                            let Some(v) = c.get(k) else { continue };
                            match k {
                                "fase" => fases[pos]["fase"] = json!(v.as_str().unwrap_or("").trim().chars().take(120).collect::<String>()),
                                "fechaInicio" | "fechaFin" => {
                                    if !fecha_ok(v.as_str().unwrap_or("")) {
                                        return Err(ApiError::bad_request("Las fechas deben ser AAAA-MM-DD"));
                                    }
                                    fases[pos][k] = v.clone();
                                }
                                "estado" => {
                                    if !matches!(v.as_str(), Some("PENDIENTE") | Some("EN_CURSO") | Some("COMPLETADA")) {
                                        return Err(ApiError::bad_request("estado debe ser PENDIENTE, EN_CURSO o COMPLETADA"));
                                    }
                                    fases[pos]["estado"] = v.clone();
                                }
                                _ => match v.as_str().filter(|d| !d.is_empty()) {
                                    None => {
                                        if let Some(o) = fases[pos].as_object_mut() {
                                            o.remove("dependeDe");
                                        }
                                    }
                                    Some(d) => {
                                        if d == id || !fases.iter().any(|f| f["id"].as_str() == Some(d)) {
                                            return Err(ApiError::bad_request("dependeDe debe ser otra fase existente"));
                                        }
                                        fases[pos]["dependeDe"] = json!(d);
                                    }
                                },
                            }
                        }
                        let (i, f) = (fases[pos]["fechaInicio"].as_str().unwrap_or(""), fases[pos]["fechaFin"].as_str().unwrap_or(""));
                        if f < i {
                            return Err(ApiError::bad_request("El fin de una fase no puede ser anterior a su inicio"));
                        }
                        continue;
                    }
                    Some("quitar_fase") => {
                        let id = op["id"].as_str().unwrap_or("").to_string();
                        let Some(pos) = fases.iter().position(|f| f["id"].as_str() == Some(id.as_str())) else { return Err(ApiError::bad_request("La fase a quitar no existe")) };
                        let heredada = fases[pos]["dependeDe"].as_str().map(String::from);
                        fases.remove(pos);
                        // Las fases que dependían de la quitada pasan a depender de la anterior de la cadena (o de nadie)
                        for f in fases.iter_mut().filter(|f| f["dependeDe"].as_str() == Some(id.as_str())) {
                            match &heredada {
                                Some(h) => f["dependeDe"] = json!(h),
                                None => {
                                    if let Some(o) = f.as_object_mut() {
                                        o.remove("dependeDe");
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
                if op["op"] != "desplazar" {
                    return Err(ApiError::bad_request("Operación desconocida en el cronograma"));
                }
                let dias = op["dias"].as_i64().filter(|d| *d != 0 && d.abs() <= 365).ok_or_else(|| ApiError::bad_request("dias debe estar entre -365 y 365 y no ser 0"))?;
                let origen = op["desdeId"].as_str().map(String::from);
                // La fase de origen solo alarga su fin; las que dependen de ella (en cadena) se corren completas.
                let mut afectadas: Vec<String> = origen.clone().into_iter().collect();
                loop {
                    let nuevas: Vec<String> = fases
                        .iter()
                        .filter(|f| f["dependeDe"].as_str().map(|d| afectadas.iter().any(|a| a == d)).unwrap_or(false))
                        .filter_map(|f| f["id"].as_str().map(String::from))
                        .filter(|id| !afectadas.contains(id))
                        .collect();
                    if nuevas.is_empty() {
                        break;
                    }
                    afectadas.extend(nuevas);
                }
                if origen.is_none() {
                    afectadas = fases.iter().filter(|f| f["estado"] != "COMPLETADA").filter_map(|f| f["id"].as_str().map(String::from)).collect();
                } else if !fases.iter().any(|f| f["id"].as_str() == origen.as_deref()) {
                    return Err(ApiError::bad_request("La fase de origen no existe"));
                }
                for f in fases.iter_mut() {
                    let id = f["id"].as_str().unwrap_or("").to_string();
                    if !afectadas.contains(&id) {
                        continue;
                    }
                    let es_origen = origen.as_deref() == Some(id.as_str());
                    if !es_origen {
                        f["fechaInicio"] = json!(sumar_dias(f["fechaInicio"].as_str().unwrap_or(""), dias));
                    }
                    f["fechaFin"] = json!(sumar_dias(f["fechaFin"].as_str().unwrap_or(""), dias));
                }
            }
            Ok(Value::Array(fases).to_string())
        }
        "planEjecucion" => {
            let mut base = match parse_o_texto(actual) {
                Value::Object(o) => Value::Object(o),
                Value::Null => json!({}),
                _ if actual.trim().is_empty() => json!({}),
                _ => return Err(ApiError::bad_request("El plan de ejecución actual está en un formato antiguo: ábrelo y guárdalo una vez antes de aceptar la propuesta")),
            };
            let limpio = |v: &Value, max: usize| -> String { v.as_str().unwrap_or("").trim().chars().take(max).collect() };
            for op in lista {
                let ruta = op["ruta"].as_str().unwrap_or("");
                match op["op"].as_str() {
                    Some("agregar") => {
                        let campos: &[&str] = match ruta {
                            "ambientes" => &["ambiente", "proposito", "despliega", "promocion"],
                            "raci" => &["actividad", "responsable", "aprueba", "consultado", "informado"],
                            "comunicacion" => &["que", "audiencia", "frecuencia", "canal", "responsable"],
                            _ => return Err(ApiError::bad_request("Ruta del plan de ejecución desconocida")),
                        };
                        let mut fila = json!({ "id": new_id() });
                        for c in campos {
                            fila[*c] = json!(limpio(&op["valor"][*c], 600));
                        }
                        if fila[campos[0]].as_str().map(str::is_empty).unwrap_or(true) {
                            return Err(ApiError::bad_request("Falta el dato principal de la fila"));
                        }
                        if !base[ruta].is_array() {
                            base[ruta] = json!([]);
                        }
                        base[ruta].as_array_mut().map(|a| a.push(fila));
                    }
                    Some("establecer") => {
                        if !matches!(ruta, "qa" | "release" | "cambios") {
                            return Err(ApiError::bad_request("Ruta del plan de ejecución desconocida"));
                        }
                        base[ruta] = json!(limpio(&op["valor"], 4000));
                    }
                    _ => return Err(ApiError::bad_request("Operación desconocida en el plan de ejecución")),
                }
            }
            Ok(base.to_string())
        }
        "prd" => {
            let mut base = match parse_o_texto(actual) {
                Value::Object(o) => Value::Object(o),
                Value::Null => json!({}),
                _ if actual.trim().is_empty() => json!({}),
                _ => return Err(ApiError::bad_request("El PRD actual está en un formato antiguo: ábrelo y guárdalo una vez antes de aceptar la propuesta")),
            };
            for op in lista {
                if op["op"] != "reemplazar" {
                    return Err(ApiError::bad_request("En el PRD solo se puede proponer el contenido nuevo"));
                }
                let Some(nuevo) = op["valor"].as_object() else { return Err(ApiError::bad_request("El valor debe ser un objeto")) };
                for (k, v) in nuevo {
                    if !CAMPOS_PRD.contains(&k.as_str()) {
                        continue;
                    }
                    if k == "requisitos" {
                        // Los requisitos que ya existen se conservan (el backlog los referencia por id): solo se agregan los nuevos.
                        let mut actuales = base["requisitos"].as_array().cloned().unwrap_or_default();
                        for mut r in v.as_array().cloned().unwrap_or_default() {
                            let t = r["texto"].as_str().unwrap_or("").trim().to_lowercase();
                            if t.is_empty() || actuales.iter().any(|a| a["texto"].as_str().unwrap_or("").trim().to_lowercase() == t) {
                                continue;
                            }
                            if r["id"].as_str().map(|x| x.is_empty()).unwrap_or(true) {
                                r["id"] = json!(new_id());
                            }
                            actuales.push(r);
                        }
                        base["requisitos"] = Value::Array(actuales);
                    } else {
                        let vacio = match v {
                            Value::String(s) => s.trim().is_empty(),
                            Value::Array(a) => a.is_empty(),
                            Value::Null => true,
                            _ => false,
                        };
                        if !vacio {
                            base[k.as_str()] = v.clone();
                        }
                    }
                }
            }
            Ok(base.to_string())
        }
        _ => Err(ApiError::bad_request("Este artefacto todavía no admite propuestas")),
    }
}

const CAMPOS_PRD: [&str; 14] = ["resumenEjecutivo", "problema", "objetivoGeneral", "objetivosEspecificos", "dentroDeAlcance", "fueraDeAlcance", "personas", "requisitos", "requisitosNoFuncionales", "metricas", "riesgos", "dependencias", "supuestos", "preguntasAbiertas"];

/// Crea una propuesta pendiente sobre un artefacto (la usan las sesiones de trabajo).
pub async fn crear_propuesta(st: &AppState, sol: &str, clave: &str, titulo: &str, detalle: Option<String>, ops: Value, creado_por: &str) -> Result<Value, ApiError> {
    clave_valida(clave)?;
    validar_forma(clave, &ops)?;
    fetch_json_opt(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "ArtefactoPropuesta" (id, "solucionId", clave, titulo, detalle, ops, origen, "tareaId", "creadoPor", "createdAt")
             SELECT $1, so.id, $3, $4, $5, $6, 'PERSONA', NULL, $7, NOW() FROM "Solucion" so WHERE so.id = $2 RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[B::T(new_id()), B::T(sol.into()), B::T(clave.into()), B::T(titulo.chars().take(200).collect()), B::OT(detalle), B::J(ops), B::T(creado_por.into())],
    )
    .await?
    .ok_or_else(|| ApiError::not_found("No encontrado"))
}

/// `YYYY-MM-DD` + días (vacío o inválido queda igual).
/// `AAAA-MM-DD` con mes y día razonables.
pub fn fecha_ok(f: &str) -> bool {
    let p: Vec<i64> = f.split('-').filter_map(|x| x.parse().ok()).collect();
    f.len() == 10 && p.len() == 3 && (1..=12).contains(&p[1]) && (1..=31).contains(&p[2]) && p[0] >= 2000
}

pub fn sumar_dias(fecha: &str, dias: i64) -> String {
    let p: Vec<i64> = fecha.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() != 3 {
        return fecha.to_string();
    }
    let (y, m, d) = civil_desde_dias(dias_desde_civil(p[0], p[1], p[2]) + dias);
    format!("{y:04}-{m:02}-{d:02}")
}

async fn propuestas_listar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p."createdAt" DESC), '[]'::jsonb) FROM "ArtefactoPropuesta" p
               WHERE p."solucionId" = $1 AND ($2::text IS NULL OR p.clave = $2) AND ($3::text IS NULL OR p.estado = $3)"#,
            &[B::T(id), B::OT(q.get("clave").cloned().filter(|x| !x.is_empty())), B::OT(q.get("estado").cloned().filter(|x| !x.is_empty()))],
        )
        .await?,
    ))
}

async fn propuesta_crear(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    let clave = s(&body, "clave").unwrap_or_default();
    clave_valida(&clave)?;
    let Some(titulo) = s(&body, "titulo").map(|t| t.trim().chars().take(200).collect::<String>()).filter(|t| !t.is_empty()) else {
        return Err(ApiError::bad_request("titulo requerido"));
    };
    validar_forma(&clave, &body["ops"])?;
    let origen = if sesion.is_service { s_o_nulo(&body, "origen").unwrap_or_else(|| "TAREA".into()) } else { "PERSONA".into() };
    let fila = fetch_json_opt(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "ArtefactoPropuesta" (id, "solucionId", clave, titulo, detalle, ops, origen, "tareaId", "creadoPor", "createdAt")
             SELECT $1, so.id, $3, $4, $5, $6, $7, $8, $9, NOW() FROM "Solucion" so WHERE so.id = $2 RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::T(id),
            B::T(clave),
            B::T(titulo),
            B::OT(s_o_nulo(&body, "detalle")),
            B::J(body["ops"].clone()),
            B::T(origen),
            B::OT(s_o_nulo(&body, "tareaId")),
            B::OT(Some(nombre_de(&sesion))),
        ],
    )
    .await?;
    fila.map(Json).ok_or_else(|| ApiError::not_found("No encontrado"))
}

/// Forma mínima de las operaciones al crear la propuesta (el detalle se vuelve a comprobar al aceptarla, contra el contenido de ese momento).
fn validar_forma(clave: &str, ops: &Value) -> Result<(), ApiError> {
    let lista = ops.as_array().filter(|o| !o.is_empty() && o.len() <= 40).ok_or_else(|| ApiError::bad_request("La propuesta debe traer entre 1 y 40 operaciones"))?;
    for op in lista {
        let ok = match clave {
            "arquitectura" => op["op"] == "agregar" && matches!(op["ruta"].as_str(), Some("nodes") | Some("connections")) && op["valor"].is_object(),
            "disenoTecnico" => op["op"] == "agregar" && matches!(op["ruta"].as_str(), Some("entidades") | Some("stack") | Some("integraciones") | Some("decisiones")) && op["valor"].is_object(),
            "prd" => lista.len() == 1 && op["op"] == "reemplazar" && op["valor"].is_object(),
            "cronograma" => match op["op"].as_str() {
                Some("desplazar") => op["dias"].as_i64().map(|d| d != 0 && d.abs() <= 365).unwrap_or(false),
                Some("agregar_fase") => op["valor"]["fase"].as_str().map(|f| !f.trim().is_empty()).unwrap_or(false),
                Some("editar_fase") => op["id"].as_str().is_some() && op["cambios"].is_object(),
                Some("quitar_fase") => op["id"].as_str().is_some(),
                _ => false,
            },
            "planEjecucion" => match op["op"].as_str() {
                Some("agregar") => matches!(op["ruta"].as_str(), Some("ambientes") | Some("raci") | Some("comunicacion")) && op["valor"].is_object(),
                Some("establecer") => matches!(op["ruta"].as_str(), Some("qa") | Some("release") | Some("cambios")) && op["valor"].is_string(),
                _ => false,
            },
            _ => return Err(ApiError::bad_request("Este artefacto todavía no admite propuestas")),
        };
        if !ok {
            return Err(ApiError::bad_request("Una operación de la propuesta no es válida para este artefacto"));
        }
    }
    Ok(())
}

async fn propuesta_decidir(State(st): State<AppState>, sesion: Session, Path(pid): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let p = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(p) FROM "ArtefactoPropuesta" p WHERE p.id = $1"#, &[B::T(pid.clone())]).await?.ok_or_else(|| ApiError::not_found("No encontrada"))?;
    if p["estado"] != "PENDIENTE" {
        return Err(ApiError::bad_request("Esa propuesta ya fue decidida"));
    }
    let nota = s_o_nulo(&body, "nota");
    let (sol, clave) = (p["solucionId"].as_str().unwrap_or_default().to_string(), p["clave"].as_str().unwrap_or_default().to_string());
    let nuevo_estado = match s(&body, "accion").as_deref() {
        Some("aceptar") => {
            let aprobado = {
                let txt = texto_de(&st, &sol, &clave).await?.unwrap_or_default();
                if CON_ESTADO_PROPIO.contains(&clave.as_str()) {
                    parse_o_texto(&txt)["estadoDocumento"] == "APROBADO"
                } else {
                    fetch_text_opt(&st.pool, r#"SELECT estado FROM "ArtefactoEstado" WHERE "solucionId" = $1 AND clave = $2"#, &[B::T(sol.clone()), B::T(clave.clone())]).await?.as_deref() == Some("APROBADO")
                }
            };
            if aprobado && !es_admin(&sesion) {
                return Err(ApiError::forbidden("El artefacto está aprobado: solo un administrador acepta cambios"));
            }
            let ops = p["ops"].clone();
            let (marca, quien) = (ahora(&st).await, nombre_de(&sesion));
            let (viejo, _) = actualizar_columna(&st, &sol, &clave, |actual| {
                let mut nuevo = aplicar_ops(&clave, actual, &ops)?;
                if clave == "disenoTecnico" || clave == "prd" || clave == "planEjecucion" {
                    let mut d = parse_o_texto(&nuevo);
                    if d["estadoDocumento"] == "APROBADO" {
                        d["estadoDocumento"] = json!("EN_REVISION");
                        d["reabierto"] = json!({ "porNombre": quien, "en": marca, "motivo": "Se aceptó una propuesta sobre un documento aprobado." });
                        if let Value::Object(o) = &mut d {
                            o.remove("aprobacion");
                        }
                        nuevo = d.to_string();
                    }
                }
                Ok(nuevo)
            })
            .await?;
            if !viejo.trim().is_empty() {
                guardar_version(&st, &sol, &clave, &viejo, &format!("Antes de aceptar la propuesta «{}»", p["titulo"].as_str().unwrap_or("")), &nombre_de(&sesion)).await;
            }
            reabrir_si_aprobado(&st, &sol, &clave, &nombre_de(&sesion)).await;
            super::fases::salida_aceptada(&st, &sol, &clave).await;
            "ACEPTADA"
        }
        Some("rechazar") => "RECHAZADA",
        _ => return Err(ApiError::bad_request("accion debe ser aceptar o rechazar")),
    };
    let fila = fetch_json_opt(
        &st.pool,
        r#"WITH up AS (UPDATE "ArtefactoPropuesta" SET estado = $2, "decididoPor" = $3, "decididoEn" = NOW(), "decisionNota" = $4 WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#,
        &[B::T(pid), B::T(nuevo_estado.into()), B::T(nombre_de(&sesion)), B::OT(nota)],
    )
    .await?
    .unwrap_or(Value::Null);
    Ok(Json(fila))
}

/// El motor declaró (o el revisor detectó) que el código se apartó del diseño: queda como propuesta de decisión técnica
/// en el diseño del hub, a la espera de que una persona la acepte o la rechace.
pub async fn proponer_decision_de_diseno(st: &AppState, solucion: &str, tarea: &str, codigo: Option<&str>, origen: &str, texto: &str) {
    let tarea_ref = codigo.unwrap_or(tarea);
    let titulo = if origen == "REVISOR" { format!("El revisor detectó un desvío del diseño en la tarea {tarea_ref}") } else { format!("La tarea {tarea_ref} declaró un cambio de diseño") };
    let ya = fetch_text_opt(&st.pool, r#"SELECT id FROM "ArtefactoPropuesta" WHERE "solucionId" = $1 AND "tareaId" = $2 AND origen = $3 AND estado = 'PENDIENTE' LIMIT 1"#, &[B::T(solucion.into()), B::T(tarea.into()), B::T(origen.into())]).await;
    if matches!(ya, Ok(Some(_)) | Err(_)) {
        return;
    }
    let limpio: String = texto.trim().chars().take(400).collect();
    let ops = json!([{ "op": "agregar", "ruta": "decisiones", "valor": { "decision": limpio, "alternativas": "", "justificacion": format!("Registrada desde la tarea {tarea_ref}") } }]);
    let r = exec(
        &st.pool,
        r#"INSERT INTO "ArtefactoPropuesta" (id, "solucionId", clave, titulo, detalle, ops, origen, "tareaId", "creadoPor", "createdAt")
           VALUES ($1, $2, 'disenoTecnico', $3, $4, $5, $6, $7, 'Motor', NOW())"#,
        &[B::T(new_id()), B::T(solucion.into()), B::T(titulo), B::T(limpio), B::J(ops), B::T(origen.into()), B::T(tarea.into())],
    )
    .await;
    if let Err(e) = r {
        tracing::warn!("proponer_decision_de_diseno: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suma_de_dias_cruza_meses_y_anios() {
        assert_eq!(sumar_dias("2026-10-28", 5), "2026-11-02");
        assert_eq!(sumar_dias("2026-12-30", 3), "2027-01-02");
        assert_eq!(sumar_dias("2026-03-01", -1), "2026-02-28");
        assert_eq!(sumar_dias("", 5), "");
    }

    #[test]
    fn agregar_un_componente_le_busca_lugar_y_valida_el_tipo() {
        let actual = r#"{"nodes":[{"id":"a","label":"Next","type":"backend","x":16,"y":100}],"connections":[]}"#;
        let ops = json!([{ "op": "agregar", "ruta": "nodes", "valor": { "id": "q", "label": "Cola", "type": "inventado" } }, { "op": "agregar", "ruta": "connections", "valor": { "from": "a", "to": "q" } }]);
        let r: Value = serde_json::from_str(&aplicar_ops("arquitectura", actual, &ops).unwrap()).unwrap();
        assert_eq!(r["nodes"][1]["type"], "externo");
        assert_eq!(r["nodes"][1]["y"], 188);
        assert_eq!(r["connections"][0]["to"], "q");
        let mala = json!([{ "op": "agregar", "ruta": "connections", "valor": { "from": "a", "to": "no-existe" } }]);
        assert!(aplicar_ops("arquitectura", actual, &mala).is_err());
    }

    #[test]
    fn el_cronograma_corre_la_cadena_de_dependencias() {
        let actual = r#"[{"id":"a","fase":"Construcción","fechaInicio":"2026-10-26","fechaFin":"2026-11-30","estado":"EN_CURSO"},
          {"id":"b","fase":"QA","fechaInicio":"2026-11-30","fechaFin":"2026-12-14","estado":"PENDIENTE","dependeDe":"a"},
          {"id":"c","fase":"Despliegue","fechaInicio":"2026-12-14","fechaFin":"2026-12-21","estado":"PENDIENTE","dependeDe":"b"},
          {"id":"z","fase":"Otra","fechaInicio":"2026-10-01","fechaFin":"2026-10-05","estado":"PENDIENTE"}]"#;
        let r: Value = serde_json::from_str(&aplicar_ops("cronograma", actual, &json!([{ "op": "desplazar", "dias": 5, "desdeId": "a" }])).unwrap()).unwrap();
        assert_eq!((r[0]["fechaInicio"].as_str(), r[0]["fechaFin"].as_str()), (Some("2026-10-26"), Some("2026-12-05")));
        assert_eq!((r[1]["fechaInicio"].as_str(), r[1]["fechaFin"].as_str()), (Some("2026-12-05"), Some("2026-12-19")));
        assert_eq!(r[2]["fechaFin"], "2026-12-26");
        assert_eq!(r[3]["fechaFin"], "2026-10-05");
        assert!(aplicar_ops("cronograma", actual, &json!([{ "op": "desplazar", "dias": 0 }])).is_err());
        assert!(aplicar_ops("cronograma", actual, &json!([{ "op": "desplazar", "dias": 2, "desdeId": "nada" }])).is_err());
    }

    #[test]
    fn el_cronograma_se_reestructura_con_propuestas() {
        let base = r#"[{"id":"a","fase":"Diseño","fechaInicio":"2026-11-01","fechaFin":"2026-11-10","estado":"PENDIENTE"},{"id":"b","fase":"QA","fechaInicio":"2026-11-10","fechaFin":"2026-11-20","estado":"PENDIENTE","dependeDe":"a"}]"#;
        let ops = json!([{ "op": "agregar_fase", "valor": { "fase": "Despliegue", "fechaInicio": "2026-11-20", "fechaFin": "2026-11-25", "dependeDe": "b" } }]);
        let r: Value = serde_json::from_str(&aplicar_ops("cronograma", base, &ops).unwrap()).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 3);
        assert_eq!(r[2]["fase"], "Despliegue");
        assert!(r[2]["id"].is_string() && r[2]["estado"] == "PENDIENTE");
        // editar: fechas y dependencia
        let ops = json!([{ "op": "editar_fase", "id": "b", "cambios": { "fase": "Pruebas", "fechaFin": "2026-11-30", "estado": "EN_CURSO" } }]);
        let r: Value = serde_json::from_str(&aplicar_ops("cronograma", base, &ops).unwrap()).unwrap();
        assert_eq!((r[1]["fase"].as_str(), r[1]["fechaFin"].as_str(), r[1]["estado"].as_str()), (Some("Pruebas"), Some("2026-11-30"), Some("EN_CURSO")));
        // quitar: lo que dependía de la quitada pasa a depender de la anterior
        let tres = r#"[{"id":"a","fase":"A","fechaInicio":"2026-11-01","fechaFin":"2026-11-02"},{"id":"b","fase":"B","fechaInicio":"2026-11-02","fechaFin":"2026-11-03","dependeDe":"a"},{"id":"c","fase":"C","fechaInicio":"2026-11-03","fechaFin":"2026-11-04","dependeDe":"b"}]"#;
        let r: Value = serde_json::from_str(&aplicar_ops("cronograma", tres, &json!([{ "op": "quitar_fase", "id": "b" }])).unwrap()).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
        assert_eq!(r[1]["dependeDe"], "a");
        // un cronograma vacío admite fases nuevas
        assert!(aplicar_ops("cronograma", "", &json!([{ "op": "agregar_fase", "valor": { "fase": "Primera", "fechaInicio": "2026-12-01", "fechaFin": "2026-12-05" } }])).is_ok());
        // rechazos
        for mal in [
            json!([{ "op": "agregar_fase", "valor": { "fase": "X", "fechaInicio": "2026-11-20", "fechaFin": "2026-11-10" } }]),
            json!([{ "op": "agregar_fase", "valor": { "fase": "X", "fechaInicio": "mañana", "fechaFin": "2026-11-10" } }]),
            json!([{ "op": "agregar_fase", "valor": { "fase": "X", "fechaInicio": "2026-11-01", "fechaFin": "2026-11-10", "dependeDe": "zzz" } }]),
            json!([{ "op": "editar_fase", "id": "zzz", "cambios": { "fase": "X" } }]),
            json!([{ "op": "editar_fase", "id": "a", "cambios": { "dependeDe": "a" } }]),
            json!([{ "op": "quitar_fase", "id": "zzz" }]),
            json!([{ "op": "borrar_todo" }]),
        ] {
            assert!(aplicar_ops("cronograma", base, &mal).is_err(), "{mal}");
        }
        assert!(validar_forma("cronograma", &json!([{ "op": "quitar_fase", "id": "a" }])).is_ok());
        assert!(validar_forma("cronograma", &json!([{ "op": "borrar_todo" }])).is_err());
    }

    #[test]
    fn el_plan_de_ejecucion_admite_propuestas_acotadas() {
        let ops = json!([
            { "op": "agregar", "ruta": "ambientes", "valor": { "ambiente": "Staging", "proposito": "Pruebas previas", "despliega": "Motor", "promocion": "PR revisado", "extra": "se ignora" } },
            { "op": "agregar", "ruta": "raci", "valor": { "actividad": "Aprobar el alcance", "responsable": "Marta", "aprueba": "Dirección" } },
            { "op": "establecer", "ruta": "qa", "valor": "Cada tarea de código se prueba antes de integrarse." },
        ]);
        let r: Value = serde_json::from_str(&aplicar_ops("planEjecucion", r#"{"estadoDocumento":"BORRADOR","qa":""}"#, &ops).unwrap()).unwrap();
        assert_eq!(r["ambientes"][0]["ambiente"], "Staging");
        assert!(r["ambientes"][0].get("extra").is_none(), "solo los campos conocidos");
        assert_eq!(r["raci"][0]["responsable"], "Marta");
        assert_eq!(r["qa"], "Cada tarea de código se prueba antes de integrarse.");
        assert_eq!(r["estadoDocumento"], "BORRADOR", "no toca el estado del documento");
        assert!(aplicar_ops("planEjecucion", "{}", &json!([{ "op": "agregar", "ruta": "estadoDocumento", "valor": { "x": "y" } }])).is_err());
        assert!(aplicar_ops("planEjecucion", "{}", &json!([{ "op": "agregar", "ruta": "raci", "valor": { "responsable": "sin actividad" } }])).is_err(), "falta el dato principal");
        assert!(aplicar_ops("planEjecucion", "{}", &json!([{ "op": "establecer", "ruta": "estadoDocumento", "valor": "APROBADO" }])).is_err(), "no se puede aprobar por propuesta");
        assert!(validar_forma("planEjecucion", &json!([{ "op": "establecer", "ruta": "qa", "valor": "x" }])).is_ok());
        assert!(validar_forma("planEjecucion", &json!([{ "op": "establecer", "ruta": "qa", "valor": { "a": 1 } }])).is_err());
    }

    #[test]
    fn el_diseno_acepta_decisiones_y_rechaza_rutas_raras() {
        let ops = json!([{ "op": "agregar", "ruta": "decisiones", "valor": { "decision": "Usar cola" } }]);
        let r: Value = serde_json::from_str(&aplicar_ops("disenoTecnico", "{}", &ops).unwrap()).unwrap();
        assert_eq!(r["decisiones"][0]["decision"], "Usar cola");
        assert!(r["decisiones"][0]["id"].is_string());
        assert!(aplicar_ops("disenoTecnico", "{}", &json!([{ "op": "agregar", "ruta": "estadoDocumento", "valor": {} }])).is_err());
        assert!(aplicar_ops("prd", "{}", &ops).is_err());
    }
}
