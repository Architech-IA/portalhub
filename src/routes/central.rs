//! Portada del Motor — MASD-0024. Una sola respuesta que junta lo que antes estaba repartido: la
//! cartera de proyectos con su fase, las puertas que esperan una persona, lo que están haciendo los
//! agentes (corriendo, fallido, bloqueado, atascado), el estado de la cola del Harness y el uso de
//! tokens por proyecto. Solo lectura: las acciones siguen en las rutas de cada parte.

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

use crate::{
    error::ApiResult,
    routes::fases::{criterios_fase, indice, lista},
    session::Session,
    state::AppState,
    util::{fetch_json, B},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/motor/resumen", get(resumen))
}

/// Una ejecución que lleva más de esto en RUNNING se considera atascada.
const ATASCADA_HORAS: i64 = 2;

const SQL_CARTERA: &str = r#"
SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."enFaseDesde" DESC NULLS LAST), '[]'::jsonb) FROM (
  SELECT s.id, s.nombre, s.tipo, s."solucionCode" AS codigo, pf."faseActual", pf.estado AS "estadoMotor", pf.definicion, pf.criterios,
         (SELECT MAX(h."createdAt") FROM "ProyectoFaseHistorial" h WHERE h."solucionId" = s.id) AS "enFaseDesde",
         l."companyName" AS cliente, l.status::text AS "leadStatus",
         (SELECT jsonb_build_object('total', COUNT(*), 'hechas', COUNT(*) FILTER (WHERE b.status = 'DONE'),
                  'enCurso', COUNT(*) FILTER (WHERE b.status = 'IN_PROGRESS'), 'fallidas', COUNT(*) FILTER (WHERE b.status = 'FAILED'),
                  'bloqueadas', COUNT(*) FILTER (WHERE b.status = 'BLOCKED'))
            FROM "BacklogItem" b WHERE b."solucionId" = s.id) AS tareas
    FROM "ProyectoFase" pf JOIN "Solucion" s ON s.id = pf."solucionId" LEFT JOIN "Lead" l ON l.id = s."leadId") x"#;

/// Corriendo = tarea IN_PROGRESS cuya última ejecución arrancó hace menos de $1 horas; si no, está atascada.
const SQL_CORRIENDO: &str = r#"
SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x.desde DESC NULLS LAST), '[]'::jsonb) FROM (
  SELECT b.id, b."taskCode", b.title, b."solucionId", s.nombre AS solucion, COALESCE(le.desde, b."updatedAt") AS desde
    FROM "BacklogItem" b LEFT JOIN "Solucion" s ON s.id = b."solucionId"
    LEFT JOIN LATERAL (SELECT MAX(e."startedAt") AS desde FROM "TaskExecution" e WHERE e."backlogItemId" = b.id) le ON true
   WHERE b.status = 'IN_PROGRESS' AND COALESCE(le.desde, b."updatedAt") >= NOW() - ($1::int * INTERVAL '1 hour') LIMIT 30) x"#;

const SQL_PROBLEMAS: &str = r#"
SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."updatedAt" DESC), '[]'::jsonb) FROM (
  SELECT b.id, b."taskCode", b.title, b.status, b."solucionId", s.nombre AS solucion, b."updatedAt"
    FROM "BacklogItem" b LEFT JOIN "Solucion" s ON s.id = b."solucionId"
   WHERE b.status IN ('FAILED', 'BLOCKED') ORDER BY b."updatedAt" DESC LIMIT 20) x"#;

/// Atascada = IN_PROGRESS sin ejecución reciente, o con una ejecución RUNNING de hace más de $1 horas.
const SQL_ATASCADAS: &str = r#"
SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x."startedAt"), '[]'::jsonb) FROM (
  SELECT b.id, b.id AS "backlogItemId", b."taskCode", b.title, COALESCE(le.desde, b."updatedAt") AS "startedAt"
    FROM "BacklogItem" b LEFT JOIN LATERAL (SELECT MAX(e."startedAt") AS desde FROM "TaskExecution" e WHERE e."backlogItemId" = b.id) le ON true
   WHERE (b.status = 'IN_PROGRESS' AND COALESCE(le.desde, b."updatedAt") < NOW() - ($1::int * INTERVAL '1 hour'))
      OR EXISTS (SELECT 1 FROM "TaskExecution" e WHERE e."backlogItemId" = b.id AND e.status = 'RUNNING' AND e."startedAt" < NOW() - ($1::int * INTERVAL '1 hour'))
   LIMIT 20) x"#;

const SQL_COSTO: &str = r#"
SELECT jsonb_build_object(
  'ejecuciones', (SELECT COUNT(*) FROM "TaskExecution"),
  'conUso', COUNT(*),
  'tokens', COALESCE(SUM(u.total), 0), 'entrada', COALESCE(SUM(u.entrada), 0), 'salida', COALESCE(SUM(u.salida), 0),
  'porProyecto', COALESCE((SELECT jsonb_agg(to_jsonb(p) ORDER BY p.tokens DESC) FROM (
      SELECT COALESCE(s.id, '') AS id, COALESCE(s.nombre, 'Sin proyecto') AS nombre, COUNT(*) AS tareas, SUM(u2.total) AS tokens
        FROM (SELECT e."backlogItemId", (e.artifacts->'usage'->>'total_tokens')::bigint AS total FROM "TaskExecution" e
               WHERE jsonb_typeof(e.artifacts) = 'object' AND e.artifacts->'usage' IS NOT NULL) u2
        LEFT JOIN "BacklogItem" b ON b.id = u2."backlogItemId" LEFT JOIN "Solucion" s ON s.id = b."solucionId"
       GROUP BY s.id, s.nombre ORDER BY tokens DESC LIMIT 12) p), '[]'::jsonb))
FROM (SELECT (e.artifacts->'usage'->>'total_tokens')::bigint AS total, (e.artifacts->'usage'->>'prompt_tokens')::bigint AS entrada,
             (e.artifacts->'usage'->>'completion_tokens')::bigint AS salida
        FROM "TaskExecution" e WHERE jsonb_typeof(e.artifacts) = 'object' AND e.artifacts->'usage' IS NOT NULL) u"#;

/// Fila de la cartera: la fase con su nombre y avance de puerta, calculados con la plantilla del proyecto.
fn fila_cartera(p: &Value) -> Value {
    let def = &p["definicion"];
    let actual = p["faseActual"].as_str().unwrap_or_default();
    let idx = indice(def, actual).unwrap_or(0);
    let f = lista(def).get(idx).cloned().unwrap_or(Value::Null);
    let crit = criterios_fase(def, &p["criterios"], idx);
    let ok = crit.iter().filter(|c| c["ok"] == true).count();
    json!({
        "id": p["id"], "nombre": p["nombre"], "tipo": p["tipo"], "codigo": p["codigo"], "cliente": p["cliente"], "leadStatus": p["leadStatus"],
        "estadoMotor": p["estadoMotor"], "faseClave": actual, "faseNumero": f["numero"], "faseNombre": f["nombre"], "bloque": f["bloque"],
        "totalFases": lista(def).len(),
        "puerta": { "aprobador": f["puerta"]["aprobador"], "tipo": f["puerta"]["tipo"], "ok": ok, "total": crit.len(), "lista": !crit.is_empty() && ok == crit.len() },
        "enFaseDesde": p["enFaseDesde"], "tareas": p["tareas"],
    })
}

async fn resumen(State(st): State<AppState>, se: Session) -> ApiResult<Json<Value>> {
    let cartera_cruda = fetch_json(&st.pool, SQL_CARTERA, &[]).await?;
    let cartera: Vec<Value> = cartera_cruda.as_array().map(|a| a.iter().map(fila_cartera).collect()).unwrap_or_default();

    // Puertas que esperan a una persona: proyectos en curso con la puerta de la fase actual completa (o a medias).
    let mut aprobaciones: Vec<Value> = cartera
        .iter()
        .filter(|p| p["estadoMotor"] == "EN_CURSO" && p["puerta"]["total"].as_i64().unwrap_or(0) > 0)
        .map(|p| json!({
            "id": p["id"], "nombre": p["nombre"], "faseNombre": p["faseNombre"], "faseNumero": p["faseNumero"], "aprobador": p["puerta"]["aprobador"],
            "tipo": p["puerta"]["tipo"], "ok": p["puerta"]["ok"], "total": p["puerta"]["total"], "lista": p["puerta"]["lista"], "enFaseDesde": p["enFaseDesde"],
        }))
        .collect();
    // Primero las que ya se pueden aprobar, luego las más avanzadas.
    aprobaciones.sort_by_key(|a| (a["lista"] != true, std::cmp::Reverse(a["ok"].as_i64().unwrap_or(0))));

    let corriendo = fetch_json(&st.pool, SQL_CORRIENDO, &[B::I(ATASCADA_HORAS)]).await?;
    let problemas = fetch_json(&st.pool, SQL_PROBLEMAS, &[]).await?;
    let atascadas = fetch_json(&st.pool, SQL_ATASCADAS, &[B::I(ATASCADA_HORAS)]).await?;
    let costo = fetch_json(&st.pool, SQL_COSTO, &[]).await?;
    let sin_motor = crate::util::fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "Solucion" s WHERE NOT EXISTS (SELECT 1 FROM "ProyectoFase" p WHERE p."solucionId" = s.id) AND s."leadId" IS NOT NULL"#, &[]).await?;

    // Cola del Harness: si no contesta, se dice (los workers que sacan de esa cola dependen de ella).
    let url = std::env::var("HARNESS_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8767".to_string());
    let harness = match st.http.get(format!("{url}/queues")).timeout(std::time::Duration::from_secs(3)).send().await {
        Ok(r) if r.status().is_success() => {
            let j: Value = r.json().await.unwrap_or(Value::Null);
            json!({ "ok": true, "colas": j["queues"] })
        }
        _ => json!({ "ok": false, "colas": Value::Null }),
    };

    let en_curso = cartera.iter().filter(|p| p["estadoMotor"] == "EN_CURSO").count();
    Ok(Json(json!({
        "puedeAprobar": se.is_admin() || se.is_service,
        "totales": {
            "proyectos": cartera.len(), "enCurso": en_curso,
            "porAprobar": aprobaciones.iter().filter(|a| a["lista"] == true).count(),
            "sinMotor": sin_motor,
            "corriendo": corriendo.as_array().map(|a| a.len()).unwrap_or(0),
            "problemas": problemas.as_array().map(|a| a.len()).unwrap_or(0) + atascadas.as_array().map(|a| a.len()).unwrap_or(0),
        },
        "cartera": cartera,
        "aprobaciones": aprobaciones,
        "ejecucion": { "corriendo": corriendo, "problemas": problemas, "atascadas": atascadas, "atascadaHoras": ATASCADA_HORAS, "harness": harness },
        "costo": costo,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_fila_de_cartera_resume_la_puerta_de_la_fase_actual() {
        let def: Value = serde_json::from_str(include_str!("../../plantillas/proyecto_completo.json")).unwrap();
        let mut criterios = json!({});
        criterios["contacto"] = json!([{ "ok": true }, { "ok": false }]);
        let p = json!({ "id": "x", "nombre": "N", "faseActual": "contacto", "estadoMotor": "EN_CURSO", "definicion": def, "criterios": criterios });
        let f = fila_cartera(&p);
        assert_eq!(f["faseNombre"], "Contacto");
        assert_eq!(f["faseNumero"], 2);
        assert_eq!(f["totalFases"], 12);
        assert_eq!(f["puerta"]["ok"], 1);
        assert_eq!(f["puerta"]["total"], 2);
        assert_eq!(f["puerta"]["lista"], false);
        criterios["contacto"] = json!([{ "ok": true }, { "ok": true }]);
        let p = json!({ "faseActual": "contacto", "definicion": p["definicion"], "criterios": criterios });
        assert_eq!(fila_cartera(&p)["puerta"]["lista"], true);
    }
}
