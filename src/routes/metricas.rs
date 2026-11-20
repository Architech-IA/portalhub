//! Métricas por proyecto y niveles de autonomía por fase (MASD-0026-0009).
//!
//! Las métricas se calculan de lo que el portal ya guarda (ejecuciones del motor, backlog, propuestas de cambio de diseño,
//! historial de fases y catálogo de artefactos). Lo que todavía no se mide (requisitos con prueba, defectos tras la entrega)
//! se declara en `sinDato` en vez de inventarlo. El nivel de autonomía de cada fase queda registrado y a la vista; por ahora es
//! una DECLARACIÓN que ve el equipo y el contexto de las sesiones: el motor no despacha ni avanza fases por su cuenta
//! por el solo hecho de tener un nivel alto. Lo que siempre exige a una persona (aprobar PRD y diseño, cambios de spec,
//! merge a producción, desplegar, enviar al cliente) no depende del nivel.

use axum::{
    extract::{Path, State},
    routing::{get, put},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    routes::{catalogo_artefactos, solucion_hub::{es_admin, nombre_de}},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proyectos/{id}/metricas", get(metricas))
        .route("/api/proyectos/{id}/autonomia", get(autonomia_ver))
        .route("/api/proyectos/{id}/autonomia/{fase}", put(autonomia_fijar))
}

const NIVELES: [&str; 3] = ["ASISTIDA", "SUPERVISADA", "AUTONOMA"];

/// Umbrales para SUGERIR un nivel mayor (a revisar con datos reales; son un punto de partida).
const MIN_EJECUCIONES_SUPERVISADA: i64 = 5;
const MIN_TASA_SUPERVISADA: f64 = 0.70;
const MIN_EJECUCIONES_AUTONOMA: i64 = 15;
const MIN_TASA_AUTONOMA: f64 = 0.85;

/// Nivel de partida: preventa y postventa asistidas; la ejecución, supervisada.
pub(crate) fn nivel_por_defecto(fase: &str, bloque: &str) -> &'static str {
    if bloque == "EJECUCION" && fase != "estabilizacion" && fase != "operacion_evolucion" {
        "SUPERVISADA"
    } else {
        "ASISTIDA"
    }
}

/// Datos mínimos para sugerir un nivel.
pub(crate) struct Evidencia {
    pub ejecuciones: i64,
    pub tareas_con_ejecucion: i64,
    pub primer_intento_ok: i64,
    pub fallidas_ultimas_10: i64,
    pub propuestas_pendientes: i64,
    pub desactualizados: i64,
}

impl Evidencia {
    pub(crate) fn tasa(&self) -> f64 {
        if self.tareas_con_ejecucion == 0 { 0.0 } else { self.primer_intento_ok as f64 / self.tareas_con_ejecucion as f64 }
    }
}

/// Qué nivel respaldan los datos del proyecto. Devuelve (nivel sugerido, motivos).
pub(crate) fn sugerir(e: &Evidencia) -> (&'static str, Vec<String>) {
    let tasa = e.tasa();
    let mut motivos = vec![format!("{} ejecuciones del motor; {:.0}% de las tareas pasó al primer intento", e.ejecuciones, tasa * 100.0)];
    if e.ejecuciones >= MIN_EJECUCIONES_AUTONOMA && tasa >= MIN_TASA_AUTONOMA && e.fallidas_ultimas_10 == 0 && e.propuestas_pendientes == 0 && e.desactualizados == 0 {
        motivos.push("Cumple los umbrales de autonomía: sin fallos en las últimas 10 ejecuciones, sin propuestas de cambio pendientes ni artefactos desactualizados".into());
        return ("AUTONOMA", motivos);
    }
    if e.ejecuciones >= MIN_EJECUCIONES_SUPERVISADA && tasa >= MIN_TASA_SUPERVISADA {
        let mut faltan = vec![];
        if e.ejecuciones < MIN_EJECUCIONES_AUTONOMA { faltan.push(format!("llegar a {MIN_EJECUCIONES_AUTONOMA} ejecuciones")); }
        if tasa < MIN_TASA_AUTONOMA { faltan.push(format!("subir el primer intento a {:.0}%", MIN_TASA_AUTONOMA * 100.0)); }
        if e.fallidas_ultimas_10 > 0 { faltan.push(format!("{} fallo(s) en las últimas 10 ejecuciones", e.fallidas_ultimas_10)); }
        if e.propuestas_pendientes > 0 { faltan.push(format!("{} propuesta(s) de cambio pendientes", e.propuestas_pendientes)); }
        if e.desactualizados > 0 { faltan.push(format!("{} artefacto(s) desactualizados", e.desactualizados)); }
        if !faltan.is_empty() {
            motivos.push(format!("Para autonomía falta: {}", faltan.join("; ")));
        }
        return ("SUPERVISADA", motivos);
    }
    motivos.push(format!("Aún no hay evidencia para subir de nivel (mínimo {MIN_EJECUCIONES_SUPERVISADA} ejecuciones y {:.0}% al primer intento)", MIN_TASA_SUPERVISADA * 100.0));
    ("ASISTIDA", motivos)
}

async fn exigir_lectura(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

const SQL_EJECUCIONES: &str = r#"
WITH ej AS (SELECT te.id, te."backlogItemId", te."startedAt", te.status, te."durationMs", te.artifacts
              FROM "TaskExecution" te JOIN "BacklogItem" bi ON bi.id = te."backlogItemId" WHERE bi."solucionId" = $1),
primera AS (SELECT DISTINCT ON ("backlogItemId") "backlogItemId", status FROM ej ORDER BY "backlogItemId", "startedAt")
SELECT jsonb_build_object(
  'ejecuciones', (SELECT COUNT(*) FROM ej),
  'hechas', (SELECT COUNT(*) FROM ej WHERE status = 'DONE'),
  'fallidas', (SELECT COUNT(*) FROM ej WHERE status = 'FAILED'),
  'enCurso', (SELECT COUNT(*) FROM ej WHERE status = 'RUNNING'),
  'tareasConEjecucion', (SELECT COUNT(*) FROM primera),
  'primerIntentoOk', (SELECT COUNT(*) FROM primera WHERE status = 'DONE'),
  'fallidasUltimas10', (SELECT COUNT(*) FROM (SELECT status FROM ej ORDER BY "startedAt" DESC LIMIT 10) x WHERE status = 'FAILED'),
  'minutosAgente', (SELECT COALESCE(ROUND(SUM("durationMs") / 60000.0), 0) FROM ej),
  'tokens', (SELECT COALESCE(SUM(
      CASE WHEN (artifacts->'usage'->>'promptTokens') ~ '^[0-9]+$' THEN (artifacts->'usage'->>'promptTokens')::bigint ELSE 0 END +
      CASE WHEN (artifacts->'usage'->>'completionTokens') ~ '^[0-9]+$' THEN (artifacts->'usage'->>'completionTokens')::bigint ELSE 0 END), 0) FROM ej)
)"#;

async fn evidencia_de(st: &AppState, id: &str) -> Result<(Value, Evidencia), ApiError> {
    let ej = fetch_json(&st.pool, SQL_EJECUCIONES, &[B::T(id.into())]).await?;
    let n = |k: &str| ej[k].as_i64().unwrap_or(0);
    let propuestas = fetch_json(&st.pool, r#"SELECT to_jsonb(COUNT(*)) FROM "ArtefactoPropuesta" WHERE "solucionId" = $1 AND estado = 'PENDIENTE'"#, &[B::T(id.into())]).await?.as_i64().unwrap_or(0);
    let desact = match catalogo_artefactos::filas(st, id).await {
        Ok((_, r)) => r["desactualizados"].as_i64().unwrap_or(0),
        Err(_) => 0,
    };
    let e = Evidencia { ejecuciones: n("ejecuciones"), tareas_con_ejecucion: n("tareasConEjecucion"), primer_intento_ok: n("primerIntentoOk"), fallidas_ultimas_10: n("fallidasUltimas10"), propuestas_pendientes: propuestas, desactualizados: desact };
    Ok((ej, e))
}

async fn metricas(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion).await?;
    let existe = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(1) FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    if existe.is_none() {
        return Err(ApiError::not_found("No encontrado"));
    }
    let (ej, e) = evidencia_de(&st, &id).await?;
    let backlog = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_object_agg(status, n), '{}'::jsonb) FROM (SELECT status, COUNT(*) n FROM "BacklogItem" WHERE "solucionId" = $1 GROUP BY status) x"#, &[B::T(id.clone())]).await?;
    let propuestas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_object_agg(estado, n), '{}'::jsonb) FROM (SELECT estado, COUNT(*) n FROM "ArtefactoPropuesta" WHERE "solucionId" = $1 GROUP BY estado) x"#, &[B::T(id.clone())]).await?;
    let hist = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('fase', fase, 'accion', accion, 'en', "createdAt") ORDER BY "createdAt"), '[]'::jsonb) FROM "ProyectoFaseHistorial" WHERE "solucionId" = $1"#, &[B::T(id.clone())]).await?;
    // Tiempo en cada fase: desde que entra hasta la siguiente entrada del historial (la última queda abierta).
    let h = hist.as_array().cloned().unwrap_or_default();
    let mut fases: Vec<Value> = vec![];
    for (i, x) in h.iter().enumerate() {
        let dias = match (h.get(i + 1), x["en"].as_str()) {
            (Some(sig), Some(a)) => dias_entre(a, sig["en"].as_str().unwrap_or("")),
            _ => None,
        };
        fases.push(json!({ "fase": x["fase"], "accion": x["accion"], "entro": x["en"], "dias": dias }));
    }
    let (cont, desact) = match catalogo_artefactos::filas(&st, &id).await {
        Ok((_, r)) => (r["conContenido"].as_i64().unwrap_or(0), r["desactualizados"].as_i64().unwrap_or(0)),
        Err(_) => (0, 0),
    };
    let (sug, motivos) = sugerir(&e);
    Ok(Json(json!({
        "solucionId": id,
        "ejecuciones": { "total": e.ejecuciones, "hechas": ej["hechas"], "fallidas": ej["fallidas"], "enCurso": ej["enCurso"] },
        "primerIntento": { "tareas": e.tareas_con_ejecucion, "ok": e.primer_intento_ok, "tasa": (e.tasa() * 1000.0).round() / 1000.0 },
        "consumo": { "tokens": ej["tokens"], "minutosAgente": ej["minutosAgente"] },
        "backlog": backlog,
        "propuestasDeCambio": propuestas,
        "artefactos": { "conContenido": cont, "desactualizados": desact },
        "fases": fases,
        "sugerencia": { "nivel": sug, "motivos": motivos, "umbrales": { "supervisada": { "ejecuciones": MIN_EJECUCIONES_SUPERVISADA, "tasa": MIN_TASA_SUPERVISADA }, "autonoma": { "ejecuciones": MIN_EJECUCIONES_AUTONOMA, "tasa": MIN_TASA_AUTONOMA } } },
        "sinDato": ["requisitos con prueba (espera la carpeta spec/ y las pruebas de aceptación)", "defectos tras la entrega (espera el tipo Incidente)"],
    })))
}

/// Días (con un decimal) entre dos instantes ISO. `None` si alguno no se puede leer.
fn dias_entre(a: &str, b: &str) -> Option<f64> {
    let ms = |s: &str| -> Option<i64> {
        let d = s.get(0..10)?;
        let h = s.get(11..19).unwrap_or("00:00:00");
        let n = |x: &str| x.parse::<i64>().ok();
        let (y, m, dd) = (n(&d[0..4])?, n(&d[5..7])?, n(&d[8..10])?);
        let (hh, mi, ss) = (n(&h[0..2])?, n(&h[3..5])?, n(&h[6..8])?);
        let dias = crate::routes::misc::dias_desde_civil(y, m, dd);
        Some(dias * 86_400 + hh * 3600 + mi * 60 + ss)
    };
    let (x, y) = (ms(a)?, ms(b)?);
    Some(((y - x) as f64 / 86_400.0 * 10.0).round() / 10.0)
}

async fn autonomia_ver(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion).await?;
    let def = fetch_json_opt(&st.pool, r#"SELECT definicion FROM "ProyectoFase" WHERE "solucionId" = $1"#, &[B::T(id.clone())]).await?;
    let fijados = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_object_agg("faseClave", nivel), '{}'::jsonb) FROM "ProyectoAutonomia" WHERE "solucionId" = $1"#, &[B::T(id.clone())]).await?;
    let lista: Vec<Value> = def.as_ref().and_then(|d| d["fases"].as_array().cloned()).unwrap_or_default();
    let (_, e) = evidencia_de(&st, &id).await?;
    let (sug, motivos) = sugerir(&e);
    let fases: Vec<Value> = lista
        .iter()
        .map(|f| {
            let clave = f["clave"].as_str().unwrap_or("");
            let defecto = nivel_por_defecto(clave, f["bloque"].as_str().unwrap_or(""));
            let fijado = fijados[clave].as_str();
            json!({ "clave": clave, "nombre": f["nombre"], "bloque": f["bloque"], "nivel": fijado.unwrap_or(defecto), "porDefecto": fijado.is_none() })
        })
        .collect();
    Ok(Json(json!({
        "fases": fases,
        "niveles": NIVELES,
        "sugerencia": { "nivel": sug, "motivos": motivos },
        "nota": "El nivel es una declaración del equipo. Aprobar el PRD y el diseño, aceptar un cambio de spec, hacer merge a producción, desplegar y enviar algo al cliente siempre exigen a una persona.",
    })))
}

async fn autonomia_fijar(State(st): State<AppState>, sesion: Session, Path((id, fase)): Path<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if !es_admin(&sesion) {
        return Err(ApiError::forbidden("No autorizado"));
    }
    let nivel = body["nivel"].as_str().unwrap_or("");
    if !NIVELES.contains(&nivel) {
        return Err(ApiError::bad_request("nivel: ASISTIDA, SUPERVISADA o AUTONOMA"));
    }
    let def = fetch_json_opt(&st.pool, r#"SELECT definicion FROM "ProyectoFase" WHERE "solucionId" = $1"#, &[B::T(id.clone())]).await?.ok_or_else(|| ApiError::bad_request("El proyecto no tiene el motor de fases iniciado"))?;
    if !def["fases"].as_array().map(|a| a.iter().any(|f| f["clave"] == fase.as_str())).unwrap_or(false) {
        return Err(ApiError::bad_request("Esa fase no existe en este proyecto"));
    }
    exec(
        &st.pool,
        r#"INSERT INTO "ProyectoAutonomia" ("solucionId", "faseClave", nivel, "actualizadoPor", "updatedAt") VALUES ($1, $2, $3, $4, NOW())
           ON CONFLICT ("solucionId", "faseClave") DO UPDATE SET nivel = EXCLUDED.nivel, "actualizadoPor" = EXCLUDED."actualizadoPor", "updatedAt" = NOW()"#,
        &[B::T(id), B::T(fase.clone()), B::T(nivel.into()), B::T(nombre_de(&sesion))],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "fase": fase, "nivel": nivel })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ejec: i64, con: i64, ok: i64, fall10: i64, prop: i64, des: i64) -> Evidencia {
        Evidencia { ejecuciones: ejec, tareas_con_ejecucion: con, primer_intento_ok: ok, fallidas_ultimas_10: fall10, propuestas_pendientes: prop, desactualizados: des }
    }

    #[test]
    fn sin_evidencia_se_queda_asistida() {
        assert_eq!(sugerir(&ev(0, 0, 0, 0, 0, 0)).0, "ASISTIDA");
        assert_eq!(sugerir(&ev(3, 3, 3, 0, 0, 0)).0, "ASISTIDA");
    }

    #[test]
    fn buena_tasa_con_pocas_ejecuciones_es_supervisada() {
        let (n, m) = sugerir(&ev(8, 8, 6, 1, 0, 0));
        assert_eq!(n, "SUPERVISADA");
        assert!(m.iter().any(|x| x.contains("falta")));
    }

    #[test]
    fn autonoma_exige_todo_limpio() {
        assert_eq!(sugerir(&ev(20, 18, 17, 0, 0, 0)).0, "AUTONOMA");
        assert_eq!(sugerir(&ev(20, 18, 17, 0, 1, 0)).0, "SUPERVISADA", "una propuesta pendiente la baja");
        assert_eq!(sugerir(&ev(20, 18, 17, 1, 0, 0)).0, "SUPERVISADA", "un fallo reciente la baja");
    }

    #[test]
    fn nivel_por_defecto_por_bloque() {
        assert_eq!(nivel_por_defecto("diagnostico", "PREVENTA"), "ASISTIDA");
        assert_eq!(nivel_por_defecto("construccion", "EJECUCION"), "SUPERVISADA");
        assert_eq!(nivel_por_defecto("estabilizacion", "EJECUCION"), "ASISTIDA");
    }

    #[test]
    fn dias_entre_dos_instantes() {
        assert_eq!(dias_entre("2026-11-18T12:00:00Z", "2026-11-20T00:00:00Z"), Some(1.5));
        assert_eq!(dias_entre("basura", "2026-11-20T00:00:00Z"), None);
    }
}
