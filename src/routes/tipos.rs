//! Tipos de proyecto: la lista que se elige en la cabecera del proyecto y en «Nueva iniciativa». Cada tipo tiene una naturaleza (comercial o interna) y de ella sale la
//! plantilla de fases. Los cinco de siempre son del sistema (se renombran y reordenan, no se borran ni se desactivan); los que crea un administrador se pueden editar,
//! desactivar y, mientras ningún proyecto los use, borrar. La naturaleza de cada tipo se guarda en memoria y se refresca cada 30 s y tras cada cambio.

use std::{
    collections::HashMap,
    sync::{LazyLock, RwLock},
    time::Duration,
};

use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, B},
};

static NATURALEZAS: LazyLock<RwLock<HashMap<String, &'static str>>> = LazyLock::new(Default::default);

/// Naturaleza guardada de un tipo (None si todavía no se cargó o el tipo no está en la tabla: entonces vale la regla de siempre).
pub(crate) fn naturaleza_guardada(tipo: &str) -> Option<&'static str> {
    NATURALEZAS.read().ok()?.get(tipo).copied()
}

pub async fn recargar(st: &AppState) {
    let Ok(v) = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('clave', clave, 'naturaleza', naturaleza)), '[]'::jsonb) FROM "TipoProyecto""#, &[]).await else { return };
    let mut m: HashMap<String, &'static str> = HashMap::new();
    for t in v.as_array().cloned().unwrap_or_default() {
        if let Some(c) = t["clave"].as_str() {
            m.insert(c.to_string(), if t["naturaleza"] == "INTERNO" { "INTERNO" } else { "COMERCIAL" });
        }
    }
    if let Ok(mut w) = NATURALEZAS.write() {
        *w = m;
    }
}

pub fn iniciar_refresco(st: AppState) {
    tokio::spawn(async move {
        loop {
            recargar(&st).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tipos-proyecto", get(listar).post(crear))
        .route("/api/tipos-proyecto/orden", post(ordenar))
        .route("/api/tipos-proyecto/{clave}", put(editar).delete(borrar))
}

fn admin(se: &Session) -> Result<(), ApiError> {
    if se.is_admin() || se.is_service {
        Ok(())
    } else {
        Err(ApiError::forbidden("Solo un administrador cambia los tipos de proyecto"))
    }
}

/// «Consultoría interna» → «CONSULTORIA_INTERNA».
pub(crate) fn clave_de(etiqueta: &str) -> String {
    let limpia: String = etiqueta
        .chars()
        .map(|c| match c {
            'á' | 'Á' => 'A',
            'é' | 'É' => 'E',
            'í' | 'Í' => 'I',
            'ó' | 'Ó' => 'O',
            'ú' | 'Ú' | 'ü' | 'Ü' => 'U',
            'ñ' | 'Ñ' => 'N',
            c => c.to_ascii_uppercase(),
        })
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let clave = limpia.split('_').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("_");
    clave.chars().take(40).collect()
}

fn etiqueta_valida(v: &Value) -> Result<String, ApiError> {
    let e = v.as_str().map(|s| s.trim().to_string()).unwrap_or_default();
    let n = e.chars().count();
    if !(3..=40).contains(&n) {
        return Err(ApiError::bad_request("El nombre del tipo debe tener entre 3 y 40 caracteres"));
    }
    Ok(e)
}

fn naturaleza_valida(v: &Value) -> Result<&'static str, ApiError> {
    match v.as_str() {
        Some("COMERCIAL") => Ok("COMERCIAL"),
        Some("INTERNO") => Ok("INTERNO"),
        _ => Err(ApiError::bad_request("La naturaleza debe ser COMERCIAL o INTERNO")),
    }
}

const SQL_UNO: &str = r#"SELECT to_jsonb(x) FROM (SELECT t.clave, t.etiqueta, t.naturaleza, t.orden, t.activo, t.sistema,
      (SELECT COUNT(*)::int FROM "Solucion" s WHERE s.tipo = t.clave) AS proyectos FROM "TipoProyecto" t WHERE t.clave = $1) x"#;

async fn listar(State(st): State<AppState>, se: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    if !se.is_service {
        se.require_user()?;
    }
    let todos = q.get("todos").map(|v| v == "1" || v == "true").unwrap_or(false);
    let sql = r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x.orden, x.etiqueta), '[]'::jsonb) FROM (
        SELECT t.clave, t.etiqueta, t.naturaleza, t.orden, t.activo, t.sistema, (SELECT COUNT(*)::int FROM "Solucion" s WHERE s.tipo = t.clave) AS proyectos
          FROM "TipoProyecto" t WHERE ($1::bool OR t.activo)) x"#;
    Ok(Json(fetch_json(&st.pool, sql, &[B::Bo(todos)]).await?))
}

async fn crear(State(st): State<AppState>, se: Session, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    admin(&se)?;
    let etiqueta = etiqueta_valida(&b["etiqueta"])?;
    let naturaleza = naturaleza_valida(&b["naturaleza"])?;
    if fetch_text_opt(&st.pool, r#"SELECT clave FROM "TipoProyecto" WHERE translate(lower(etiqueta), 'áéíóúüñ', 'aeiouun') = translate(lower($1), 'áéíóúüñ', 'aeiouun')"#, &[B::T(etiqueta.clone())]).await?.is_some() {
        return Err(ApiError::bad_request("Ya existe un tipo con ese nombre"));
    }
    let base = clave_de(&etiqueta);
    if base.len() < 3 {
        return Err(ApiError::bad_request("El nombre debe tener al menos tres letras o números"));
    }
    let mut clave = base.clone();
    let mut n = 2;
    while fetch_text_opt(&st.pool, r#"SELECT clave FROM "TipoProyecto" WHERE clave = $1"#, &[B::T(clave.clone())]).await?.is_some() {
        clave = format!("{base}_{n}");
        n += 1;
        if n > 50 {
            return Err(ApiError::bad_request("No se pudo generar una clave única para ese nombre"));
        }
    }
    exec(
        &st.pool,
        r#"INSERT INTO "TipoProyecto" (clave, etiqueta, naturaleza, orden, activo, sistema) VALUES ($1, $2, $3, (SELECT COALESCE(MAX(orden), 0) + 10 FROM "TipoProyecto"), true, false)"#,
        &[B::T(clave.clone()), B::T(etiqueta), B::T(naturaleza.into())],
    )
    .await?;
    recargar(&st).await;
    Ok(Json(json!({ "ok": true, "clave": clave })))
}

async fn editar(State(st): State<AppState>, se: Session, Path(clave): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    admin(&se)?;
    let t = fetch_json_opt(&st.pool, SQL_UNO, &[B::T(clave.clone())]).await?.ok_or_else(|| ApiError::not_found("Ese tipo no existe"))?;
    let sistema = t["sistema"] == true;
    let usados = t["proyectos"].as_i64().unwrap_or(0);
    if !b["etiqueta"].is_null() {
        let e = etiqueta_valida(&b["etiqueta"])?;
        if fetch_text_opt(&st.pool, r#"SELECT clave FROM "TipoProyecto" WHERE translate(lower(etiqueta), 'áéíóúüñ', 'aeiouun') = translate(lower($1), 'áéíóúüñ', 'aeiouun') AND clave <> $2"#, &[B::T(e.clone()), B::T(clave.clone())]).await?.is_some() {
            return Err(ApiError::bad_request("Ya existe un tipo con ese nombre"));
        }
        exec(&st.pool, r#"UPDATE "TipoProyecto" SET etiqueta = $2 WHERE clave = $1"#, &[B::T(clave.clone()), B::T(e)]).await?;
    }
    if !b["naturaleza"].is_null() {
        let nueva = naturaleza_valida(&b["naturaleza"])?;
        if t["naturaleza"] != nueva {
            if sistema {
                return Err(ApiError::bad_request("La naturaleza de un tipo del sistema no se cambia"));
            }
            if usados > 0 {
                return Err(ApiError::bad_request(format!("{usados} proyecto(s) usan este tipo: no se puede cambiar su naturaleza")));
            }
            exec(&st.pool, r#"UPDATE "TipoProyecto" SET naturaleza = $2 WHERE clave = $1"#, &[B::T(clave.clone()), B::T(nueva.into())]).await?;
        }
    }
    if let Some(activo) = b["activo"].as_bool() {
        if sistema && !activo {
            return Err(ApiError::bad_request("Un tipo del sistema no se puede desactivar"));
        }
        exec(&st.pool, r#"UPDATE "TipoProyecto" SET activo = $2 WHERE clave = $1"#, &[B::T(clave.clone()), B::Bo(activo)]).await?;
    }
    if let Some(o) = b["orden"].as_i64() {
        exec(&st.pool, r#"UPDATE "TipoProyecto" SET orden = $2::int WHERE clave = $1"#, &[B::T(clave.clone()), B::I(o.clamp(0, 10_000))]).await?;
    }
    recargar(&st).await;
    Ok(Json(json!({ "ok": true })))
}

/// Deja los tipos en el orden dado (claves de la lista completa, de arriba abajo).
async fn ordenar(State(st): State<AppState>, se: Session, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    admin(&se)?;
    let claves: Vec<String> = b["claves"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if claves.is_empty() || claves.len() > 200 {
        return Err(ApiError::bad_request("Falta la lista de claves"));
    }
    for (i, c) in claves.iter().enumerate() {
        exec(&st.pool, r#"UPDATE "TipoProyecto" SET orden = $2::int WHERE clave = $1"#, &[B::T(c.clone()), B::I(((i + 1) * 10) as i64)]).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn borrar(State(st): State<AppState>, se: Session, Path(clave): Path<String>) -> ApiResult<Json<Value>> {
    admin(&se)?;
    let t = fetch_json_opt(&st.pool, SQL_UNO, &[B::T(clave.clone())]).await?.ok_or_else(|| ApiError::not_found("Ese tipo no existe"))?;
    if t["sistema"] == true {
        return Err(ApiError::bad_request("Un tipo del sistema no se puede borrar"));
    }
    let usados = t["proyectos"].as_i64().unwrap_or(0);
    if usados > 0 {
        return Err(ApiError::bad_request(format!("{usados} proyecto(s) usan este tipo: desactívalo en vez de borrarlo")));
    }
    exec(&st.pool, r#"DELETE FROM "TipoProyecto" WHERE clave = $1 AND NOT sistema"#, &[B::T(clave)]).await?;
    recargar(&st).await;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn la_clave_sale_del_nombre() {
        assert_eq!(clave_de("Consultoría interna"), "CONSULTORIA_INTERNA");
        assert_eq!(clave_de("  Alianza  (2026) "), "ALIANZA_2026");
        assert_eq!(clave_de("Niño & Cía"), "NINO_CIA");
        assert_eq!(clave_de("--"), "");
        assert!(clave_de(&"a".repeat(80)).len() <= 40);
    }

    #[test]
    fn valida_nombre_y_naturaleza() {
        assert!(etiqueta_valida(&json!("ab")).is_err() && etiqueta_valida(&json!("Consultoría")).is_ok() && etiqueta_valida(&json!("x".repeat(41))).is_err() && etiqueta_valida(&json!(null)).is_err());
        assert!(naturaleza_valida(&json!("INTERNO")).is_ok() && naturaleza_valida(&json!("COMERCIAL")).is_ok() && naturaleza_valida(&json!("otra")).is_err());
    }
}
