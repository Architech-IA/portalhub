//! Catálogo de tipos de artefacto (MASD-0026-0001), guiado por datos.
//!
//! Cada TIPO (tabla `ArtefactoTipo`) es la ficha de un documento del proyecto: fase dueña, de qué otros tipos deriva (`entradas`),
//! quién lo produce y aprueba, visibilidad, destino, consumidores y vigencia. `fuente` dice dónde vive su contenido hoy
//! (columna de Solucion, documento de fase, memoria del proyecto...) y `entregableClaves` lo enlaza con la plantilla de fases.
//! Ajustar un tipo es un UPDATE (o un PUT a esta API): no hace falta recompilar.
//!
//! Obsolescencia: `ArtefactoDerivacion` guarda con qué versión de cada entrada se produjo o revisó un artefacto. Si la entrada
//! avanza, el artefacto queda DESACTUALIZADO. La primera vez que se consulta un proyecto se toma como línea base la versión
//! actual de cada entrada (solo lo que cambie DESPUÉS marca desactualizados).

use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    routes::solucion_hub::{es_admin, huella, nombre_de, parse_o_texto},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/artefactos/tipos", get(tipos_listar))
        .route("/api/artefactos/tipos/{clave}", put(tipo_actualizar))
        .route("/api/soluciones/{id}/catalogo-artefactos", get(catalogo_de_solucion))
        .route("/api/soluciones/{id}/catalogo-artefactos/{clave}/derivar", post(derivar))
        .route("/api/soluciones/{id}/spec/exportar", post(spec_exportar))
}

/// Exporta la spec aprobada al repositorio del proyecto (carpeta `spec/` por PR). Lo hace el servicio privilegiado.
async fn spec_exportar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !es_admin(&sesion) {
        return Err(ApiError::forbidden("No autorizado"));
    }
    let r = crate::routes::motor::llamar_t(&st, &format!("/api/proyectos/{id}/spec/exportar"), json!({}), 180).await.map_err(ApiError::bad_request)?;
    Ok(Json(r))
}

const CON_ESTADO_PROPIO: [&str; 3] = ["prd", "disenoTecnico", "planEjecucion"];

// ═══════════════════════════ Lógica pura (con pruebas) ═══════════════════════════
/// Entradas cuya versión actual es mayor que la usada al producir el artefacto: (clave, usada, actual).
pub(crate) fn desactualizadas(entradas: &[String], actuales: &HashMap<String, i64>, usadas: &HashMap<String, i64>) -> Vec<(String, i64, i64)> {
    let mut out = vec![];
    for e in entradas {
        if let (Some(actual), Some(usada)) = (actuales.get(e), usadas.get(e)) {
            if actual > usada {
                out.push((e.clone(), *usada, *actual));
            }
        }
    }
    out
}

fn entradas_de(t: &Value) -> Vec<String> {
    t["entradas"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn sin_soporte() -> Value {
    json!({ "soporte": false, "existe": false, "estado": "SIN_SOPORTE", "version": Value::Null })
}

// ═══════════════════════════ Instancia de un tipo en un proyecto ═══════════════════════════
async fn instancia(st: &AppState, id: &str, sol: &Value, t: &Value) -> Result<Value, ApiError> {
    let fuente = &t["fuente"];
    match fuente["tipo"].as_str().unwrap_or("sin_soporte") {
        "solucion_columna" => {
            let col = fuente["columna"].as_str().unwrap_or("");
            if !crate::routes::artefactos::CLAVES.contains(&col) {
                return Ok(sin_soporte());
            }
            let txt = sol[col].as_str().unwrap_or("");
            let existe = !huella(col, txt).is_null();
            let estado = if CON_ESTADO_PROPIO.contains(&col) {
                parse_o_texto(txt)["estadoDocumento"].as_str().unwrap_or("BORRADOR").to_string()
            } else {
                fetch_text_opt(&st.pool, r#"SELECT estado FROM "ArtefactoEstado" WHERE "solucionId" = $1 AND clave = $2"#, &[B::T(id.into()), B::T(col.into())])
                    .await?
                    .unwrap_or_else(|| "BORRADOR".into())
            };
            let version = if col == "prd" {
                fetch_json(&st.pool, r#"SELECT to_jsonb(COALESCE(MAX(version), 0)::bigint) FROM "SolucionPrdVersion" WHERE "solucionId" = $1"#, &[B::T(id.into())]).await?
            } else {
                fetch_json(&st.pool, r#"SELECT to_jsonb(COALESCE(MAX(version), 0)::bigint) FROM "ArtefactoVersion" WHERE "solucionId" = $1 AND clave = $2"#, &[B::T(id.into()), B::T(col.into())]).await?
            };
            Ok(json!({ "soporte": true, "existe": existe, "estado": estado, "version": version.as_i64().unwrap_or(0) }))
        }
        "documento_fase" => {
            let claves = serde_json::to_string(&t["entregableClaves"]).unwrap_or_else(|_| "[]".into());
            let r = fetch_json(
                &st.pool,
                r#"SELECT jsonb_build_object('n', COUNT(*), 'version', COALESCE(MAX(version), 0), 'actualizado', MAX("updatedAt"), 'pendiente', COALESCE(bool_or(propuesta IS NOT NULL), false))
                   FROM "ProyectoDocumento" WHERE "solucionId" = $1 AND btrim(COALESCE(contenido, '')) <> ''
                     AND "entregableClave" IN (SELECT jsonb_array_elements_text($2::text::jsonb))"#,
                &[B::T(id.into()), B::T(claves)],
            )
            .await?;
            let existe = r["n"].as_i64().unwrap_or(0) > 0;
            if !existe {
                // Muchos entregables de preventa viven dentro de la nota de su fase (acta, briefing...): se cuentan como presentes, sin versión.
                if let Some(fd) = t["faseDuena"].as_str() {
                    let nota = fetch_json_opt(
                        &st.pool,
                        r#"SELECT jsonb_build_object('actualizado', "updatedAt") FROM "ProyectoNota" WHERE "solucionId" = $1 AND fase = $2 AND btrim(COALESCE(contenido, '')) <> ''"#,
                        &[B::T(id.into()), B::T(fd.into())],
                    )
                    .await?;
                    if let Some(n) = nota {
                        return Ok(json!({ "soporte": true, "existe": true, "estado": "EN_NOTA_DE_FASE", "version": Value::Null, "actualizado": n["actualizado"] }));
                    }
                }
            }
            let estado = if !existe { "SIN_CONTENIDO" } else if r["pendiente"].as_bool().unwrap_or(false) { "EN_REVISION" } else { "VIGENTE" };
            Ok(json!({ "soporte": true, "existe": existe, "estado": estado, "version": r["version"].as_i64().unwrap_or(0), "actualizado": r["actualizado"] }))
        }
        "memoria" => {
            let r = fetch_json_opt(
                &st.pool,
                r#"SELECT jsonb_build_object('version', version, 'actualizado', "updatedAt", 'largo', length(btrim(COALESCE(contenido, '')))) FROM "ProyectoMemoria" WHERE "solucionId" = $1"#,
                &[B::T(id.into())],
            )
            .await?;
            let existe = r.as_ref().map(|x| x["largo"].as_i64().unwrap_or(0) > 0).unwrap_or(false);
            let (version, act) = r.map(|x| (x["version"].as_i64().unwrap_or(0), x["actualizado"].clone())).unwrap_or((0, Value::Null));
            Ok(json!({ "soporte": true, "existe": existe, "estado": if existe { "VIGENTE" } else { "SIN_CONTENIDO" }, "version": version, "actualizado": act }))
        }
        "ficha" => {
            let n = fetch_json(&st.pool, r#"SELECT to_jsonb(COUNT(*)) FROM "ProyectoFicha" WHERE "solucionId" = $1"#, &[B::T(id.into())]).await?;
            let existe = n.as_i64().unwrap_or(0) > 0;
            Ok(json!({ "soporte": true, "existe": existe, "estado": if existe { "VIGENTE" } else { "SIN_CONTENIDO" }, "version": Value::Null }))
        }
        "fuentes" => {
            let tipos = serde_json::to_string(&fuente["tiposFuente"]).unwrap_or_else(|_| "[]".into());
            let r = fetch_json(
                &st.pool,
                r#"SELECT jsonb_build_object('n', COUNT(*), 'actualizado', MAX("updatedAt")) FROM "FuenteProyecto" WHERE "solucionId" = $1 AND tipo IN (SELECT jsonb_array_elements_text($2::text::jsonb))"#,
                &[B::T(id.into()), B::T(tipos)],
            )
            .await?;
            let existe = r["n"].as_i64().unwrap_or(0) > 0;
            Ok(json!({ "soporte": true, "existe": existe, "estado": if existe { "VIGENTE" } else { "SIN_CONTENIDO" }, "version": Value::Null, "actualizado": r["actualizado"], "cantidad": r["n"] }))
        }
        "pruebas_aceptacion" => {
            let r = fetch_json(
                &st.pool,
                r#"SELECT jsonb_build_object('aprobadas', COUNT(*) FILTER (WHERE estado = 'APROBADA'), 'propuestas', COUNT(*) FILTER (WHERE estado = 'PROPUESTA'), 'actualizado', MAX("updatedAt")) FROM "PruebaAceptacion" WHERE "solucionId" = $1"#,
                &[B::T(id.into())],
            )
            .await?;
            let (ap, pr) = (r["aprobadas"].as_i64().unwrap_or(0), r["propuestas"].as_i64().unwrap_or(0));
            let estado = if ap > 0 { "VIGENTE" } else if pr > 0 { "EN_REVISION" } else { "SIN_CONTENIDO" };
            Ok(json!({ "soporte": true, "existe": ap + pr > 0, "estado": estado, "version": Value::Null, "actualizado": r["actualizado"], "cantidad": ap }))
        }
        "prd_requisitos" => {
            let prd = parse_o_texto(sol["prd"].as_str().unwrap_or(""));
            let n = prd["requisitos"].as_array().map(|a| a.len()).unwrap_or(0);
            let version = fetch_json(&st.pool, r#"SELECT to_jsonb(COALESCE(MAX(version), 0)::bigint) FROM "SolucionPrdVersion" WHERE "solucionId" = $1"#, &[B::T(id.into())]).await?;
            Ok(json!({ "soporte": true, "existe": n > 0, "estado": if n > 0 { "VIGENTE" } else { "SIN_CONTENIDO" }, "version": version.as_i64().unwrap_or(0), "cantidad": n }))
        }
        _ => Ok(sin_soporte()),
    }
}

struct Calculo {
    tipos: Vec<Value>,
    instancias: HashMap<String, Value>,
    sol: Value,
}

async fn calcular(st: &AppState, id: &str) -> Result<Calculo, ApiError> {
    let sol = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(so) FROM "Solucion" so WHERE so.id = $1"#, &[B::T(id.into())])
        .await?
        .ok_or_else(|| ApiError::not_found("No encontrado"))?;
    let tipos = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.orden, t.clave), '[]'::jsonb) FROM "ArtefactoTipo" t WHERE t.activo"#, &[]).await?;
    let tipos: Vec<Value> = tipos.as_array().cloned().unwrap_or_default();
    let mut instancias = HashMap::new();
    for t in &tipos {
        let clave = t["clave"].as_str().unwrap_or("").to_string();
        let inst = instancia(st, id, &sol, t).await?;
        instancias.insert(clave, inst);
    }
    Ok(Calculo { tipos, instancias, sol })
}

fn versiones_actuales(c: &Calculo) -> HashMap<String, i64> {
    c.instancias.iter().filter(|(_, v)| v["existe"].as_bool().unwrap_or(false)).filter_map(|(k, v)| v["version"].as_i64().map(|n| (k.clone(), n))).collect()
}

// ═══════════════════════════ Rutas ═══════════════════════════
async fn tipos_listar(State(st): State<AppState>, sesion: Session) -> ApiResult<Json<Value>> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    let v = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.orden, t.clave), '[]'::jsonb) FROM "ArtefactoTipo" t"#, &[]).await?;
    Ok(Json(v))
}

async fn tipo_actualizar(State(st): State<AppState>, sesion: Session, Path(clave): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if !es_admin(&sesion) {
        return Err(ApiError::forbidden("No autorizado"));
    }
    let texto = |k: &str| body.get(k).and_then(|v| v.as_str()).map(|x| x.trim().to_string());
    let lista = |k: &str| -> Result<Option<String>, ApiError> {
        match body.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(a)) if a.iter().all(|x| x.is_string()) => Ok(Some(serde_json::to_string(a).unwrap_or_else(|_| "[]".into()))),
            _ => Err(ApiError::bad_request(format!("«{k}» debe ser una lista de textos"))),
        }
    };
    let visibilidad = texto("visibilidad");
    if let Some(v) = &visibilidad {
        if v != "INTERNO" && v != "COMPARTIBLE" {
            return Err(ApiError::bad_request("visibilidad: INTERNO o COMPARTIBLE"));
        }
    }
    let activo = body.get("activo").and_then(|v| v.as_bool());
    let n = exec(
        &st.pool,
        r#"UPDATE "ArtefactoTipo" SET nombre = COALESCE($2, nombre), descripcion = COALESCE($3, descripcion), "faseDuena" = COALESCE($4, "faseDuena"),
             productor = COALESCE($5, productor), aprobador = COALESCE($6, aprobador), visibilidad = COALESCE($7, visibilidad), destino = COALESCE($8, destino),
             vigencia = COALESCE($9, vigencia), entradas = COALESCE($10::text::jsonb, entradas), consumidores = COALESCE($11::text::jsonb, consumidores),
             activo = COALESCE($12, activo), "updatedAt" = NOW() WHERE clave = $1"#,
        &[
            B::T(clave.clone()),
            B::OT(texto("nombre")),
            B::OT(texto("descripcion")),
            B::OT(texto("faseDuena")),
            B::OT(texto("productor")),
            B::OT(texto("aprobador")),
            B::OT(visibilidad),
            B::OT(texto("destino")),
            B::OT(texto("vigencia")),
            B::OT(lista("entradas")?),
            B::OT(lista("consumidores")?),
            B::OBo(activo),
        ],
    )
    .await?;
    if n == 0 {
        return Err(ApiError::not_found("Tipo de artefacto desconocido"));
    }
    let v = fetch_json(&st.pool, r#"SELECT to_jsonb(t) FROM "ArtefactoTipo" t WHERE t.clave = $1"#, &[B::T(clave)]).await?;
    Ok(Json(v))
}

async fn catalogo_de_solucion(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    let (tipos, resumen) = filas(&st, &id).await?;
    Ok(Json(json!({ "solucionId": id, "tipos": tipos, "resumen": resumen })))
}

/// Una fila por tipo con su instancia en el proyecto y lo que la deja desactualizada. La usa también el paquete de cierre de fase.
pub(crate) async fn filas(st: &AppState, id: &str) -> Result<(Vec<Value>, Value), ApiError> {
    let c = calcular(st, id).await?;
    let actuales = versiones_actuales(&c);
    let nombres: HashMap<String, String> = c.tipos.iter().filter_map(|t| Some((t["clave"].as_str()?.to_string(), t["nombre"].as_str()?.to_string()))).collect();
    let der = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(d)), '[]'::jsonb) FROM "ArtefactoDerivacion" d WHERE d."solucionId" = $1"#, &[B::T(id.to_string())]).await?;
    let mut usadas: HashMap<String, HashMap<String, i64>> = HashMap::new();
    for d in der.as_array().cloned().unwrap_or_default() {
        if let (Some(h), Some(o)) = (d["claveHija"].as_str(), d["claveOrigen"].as_str()) {
            usadas.entry(h.to_string()).or_default().insert(o.to_string(), d["versionOrigen"].as_i64().unwrap_or(0));
        }
    }
    let mut salida = vec![];
    let (mut con_contenido, mut desact) = (0, 0);
    for t in &c.tipos {
        let clave = t["clave"].as_str().unwrap_or("").to_string();
        let inst = c.instancias.get(&clave).cloned().unwrap_or_else(sin_soporte);
        let existe = inst["existe"].as_bool().unwrap_or(false);
        let entradas = entradas_de(t);
        let mut obsoleto_por: Vec<Value> = vec![];
        if existe {
            con_contenido += 1;
            let ya = usadas.get(&clave).cloned().unwrap_or_default();
            // Línea base: la primera vez que se ve este artefacto, lo vigente de cada entrada cuenta como ya consumido.
            for e in &entradas {
                if !ya.contains_key(e) {
                    if let Some(v) = actuales.get(e) {
                        let _ = exec(
                            &st.pool,
                            r#"INSERT INTO "ArtefactoDerivacion" ("solucionId", "claveHija", "claveOrigen", "versionOrigen") VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING"#,
                            &[B::T(id.to_string()), B::T(clave.clone()), B::T(e.clone()), B::I(*v)],
                        )
                        .await;
                    }
                }
            }
            for (e, usada, actual) in desactualizadas(&entradas, &actuales, &ya) {
                obsoleto_por.push(json!({ "clave": e, "nombre": nombres.get(&e), "versionUsada": usada, "versionActual": actual }));
            }
        }
        if !obsoleto_por.is_empty() {
            desact += 1;
        }
        let mut fila = t.clone();
        fila["instancia"] = inst;
        fila["desactualizado"] = json!(!obsoleto_por.is_empty());
        fila["obsoletoPor"] = Value::Array(obsoleto_por);
        salida.push(fila);
    }
    let resumen = json!({ "tipos": c.tipos.len(), "conContenido": con_contenido, "desactualizados": desact });
    Ok((salida, resumen))
}

/// Marca el artefacto como revisado o regenerado con las versiones actuales de sus entradas.
async fn derivar(State(st): State<AppState>, sesion: Session, Path((id, clave)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    let c = calcular(&st, &id).await?;
    let _ = &c.sol;
    let tipo = c.tipos.iter().find(|t| t["clave"] == clave.as_str()).ok_or_else(|| ApiError::not_found("Tipo de artefacto desconocido"))?;
    let actuales = versiones_actuales(&c);
    let quien = nombre_de(&sesion);
    let mut n = 0;
    for e in entradas_de(tipo) {
        if let Some(v) = actuales.get(&e) {
            exec(
                &st.pool,
                r#"INSERT INTO "ArtefactoDerivacion" ("solucionId", "claveHija", "claveOrigen", "versionOrigen", "registradaEn", "registradaPor") VALUES ($1, $2, $3, $4, NOW(), $5)
                   ON CONFLICT ("solucionId", "claveHija", "claveOrigen") DO UPDATE SET "versionOrigen" = EXCLUDED."versionOrigen", "registradaEn" = NOW(), "registradaPor" = EXCLUDED."registradaPor""#,
                &[B::T(id.clone()), B::T(clave.clone()), B::T(e), B::I(*v), B::T(quien.clone())],
            )
            .await?;
            n += 1;
        }
    }
    Ok(Json(json!({ "ok": true, "clave": clave, "entradasRegistradas": n })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapa(v: &[(&str, i64)]) -> HashMap<String, i64> {
        v.iter().map(|(k, n)| (k.to_string(), *n)).collect()
    }

    #[test]
    fn marca_desactualizado_solo_si_la_entrada_avanzo() {
        let entradas = vec!["prd".to_string(), "acta".to_string()];
        let usadas = mapa(&[("prd", 2), ("acta", 1)]);
        assert!(desactualizadas(&entradas, &mapa(&[("prd", 2), ("acta", 1)]), &usadas).is_empty());
        let r = desactualizadas(&entradas, &mapa(&[("prd", 3), ("acta", 1)]), &usadas);
        assert_eq!(r, vec![("prd".to_string(), 2, 3)]);
    }

    #[test]
    fn ignora_entradas_sin_version_conocida() {
        let entradas = vec!["prd".to_string()];
        assert!(desactualizadas(&entradas, &mapa(&[]), &mapa(&[("prd", 1)])).is_empty());
        assert!(desactualizadas(&entradas, &mapa(&[("prd", 5)]), &mapa(&[])).is_empty());
    }
}
