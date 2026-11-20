//! Estado vivo del proyecto en la memoria versionada (MASD-0026-0002).
//!
//! Escribe un resumen DETERMINISTA y actual (fase, puerta, tareas, última actividad del motor, pendientes de revisión) dentro de
//! `ProyectoMemoria`, entre dos marcas. Solo toca ese bloque: lo que las personas escriban fuera de él no se modifica. Cada
//! cambio real crea una versión (`ProyectoMemoriaVersion`, origen IA); si el bloque no cambió, no se crea versión nueva.
//! Se actualiza al cerrar una fase, al terminar una tarea del motor y cuando alguien lo pide (POST).

use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    routes::{catalogo_artefactos, solucion_hub::nombre_de},
    session::Session,
    state::AppState,
    util::{fetch_json, fetch_json_opt, fetch_text_opt, B},
};

const INICIO: &str = "<!-- estado-vivo:inicio -->";
const FIN: &str = "<!-- estado-vivo:fin -->";
const MAX_CARACTERES: usize = 30_000;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/proyectos/{id}/estado-vivo", post(actualizar_ruta))
}

async fn actualizar_ruta(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    let quien = nombre_de(&sesion);
    let r = actualizar(&st, &id, &format!("pedido por {quien}")).await.map_err(ApiError::bad_request)?;
    Ok(Json(r))
}

fn lista(v: &Value, k: &str) -> Vec<Value> {
    v[k].as_array().cloned().unwrap_or_default()
}

/// Cuerpo del bloque SIN la línea de fecha (sirve para saber si algo cambió de verdad).
pub(crate) async fn cuerpo(st: &AppState, sol: &str) -> Result<String, String> {
    let e = |x: sqlx::Error| x.to_string();
    let nombre = fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Solucion" WHERE id = $1"#, &[B::T(sol.into())]).await.map_err(e)?.ok_or("La Solución no existe")?;
    let fase = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('actual', "faseActual", 'estado', estado, 'def', definicion, 'crit', criterios) FROM "ProyectoFase" WHERE "solucionId" = $1"#, &[B::T(sol.into())]).await.map_err(e)?;
    let mut m = String::new();
    m.push_str(&format!("**Proyecto:** {nombre}\n\n"));

    // Fase y puerta
    match &fase {
        None => m.push_str("**Fase:** el motor de fases todavía no está iniciado.\n\n"),
        Some(f) => {
            let actual = f["actual"].as_str().unwrap_or("");
            let fases = lista(&f["def"], "fases");
            let idx = fases.iter().position(|x| x["clave"] == actual);
            let nombre_fase = idx.map(|i| fases[i]["nombre"].as_str().unwrap_or(actual)).unwrap_or(actual);
            m.push_str(&format!("**Fase:** {} ({} de {}) · {}\n", nombre_fase, idx.map(|i| i + 1).unwrap_or(0), fases.len(), f["estado"].as_str().unwrap_or("")));
            if let Some(i) = idx {
                let textos: Vec<String> = fases[i]["puerta"]["criterios"].as_array().map(|a| a.iter().map(|c| c.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
                let marcados = f["crit"][actual].as_array().cloned().unwrap_or_default();
                let hechos = marcados.iter().filter(|c| c["ok"] == true).count();
                m.push_str(&format!("**Puerta:** {hechos} de {} criterios cumplidos.\n", textos.len()));
                for (k, t) in textos.iter().enumerate() {
                    if marcados.get(k).map(|c| c["ok"] != true).unwrap_or(true) {
                        m.push_str(&format!("- Falta: {t}\n"));
                    }
                }
            }
            m.push('\n');
        }
    }

    // Tareas
    let t = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'porEstado', COALESCE((SELECT jsonb_object_agg(status, n) FROM (SELECT status, COUNT(*) n FROM "BacklogItem" WHERE "solucionId" = $1 GROUP BY status) x), '{}'::jsonb),
             'enCurso', COALESCE((SELECT jsonb_agg(title) FROM (SELECT title FROM "BacklogItem" WHERE "solucionId" = $1 AND status = 'IN_PROGRESS' ORDER BY "updatedAt" DESC LIMIT 5) y), '[]'::jsonb),
             'siguientes', COALESCE((SELECT jsonb_agg(title) FROM (SELECT title FROM "BacklogItem" WHERE "solucionId" = $1 AND status = 'BACKLOG'
                 ORDER BY CASE priority WHEN 'CRITICAL' THEN 0 WHEN 'HIGH' THEN 1 WHEN 'MEDIUM' THEN 2 ELSE 3 END, "createdAt" LIMIT 5) z), '[]'::jsonb),
             'recientes', COALESCE((SELECT jsonb_agg(title) FROM (SELECT title FROM "BacklogItem" WHERE "solucionId" = $1 AND status = 'DONE' ORDER BY "updatedAt" DESC LIMIT 5) w), '[]'::jsonb))"#,
        &[B::T(sol.into())],
    )
    .await
    .map_err(e)?;
    let por = t["porEstado"].as_object().cloned().unwrap_or_default();
    if !por.is_empty() {
        let partes: Vec<String> = por.iter().map(|(k, v)| format!("{} {}", v.as_i64().unwrap_or(0), k)).collect();
        m.push_str(&format!("**Tareas:** {}.\n", partes.join(" · ")));
    }
    for (rotulo, clave) in [("En curso", "enCurso"), ("Siguientes", "siguientes"), ("Hechas recientemente", "recientes")] {
        let xs = lista(&t, clave);
        if !xs.is_empty() {
            m.push_str(&format!("- {rotulo}: {}\n", xs.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join("; ")));
        }
    }
    m.push('\n');

    // Última actividad del motor
    let u = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('estado', te.status, 'titulo', bi.title, 'agente', te."agentName") FROM "TaskExecution" te JOIN "BacklogItem" bi ON bi.id = te."backlogItemId"
            WHERE bi."solucionId" = $1 ORDER BY te."startedAt" DESC LIMIT 1"#,
        &[B::T(sol.into())],
    )
    .await
    .map_err(e)?;
    if let Some(u) = u {
        m.push_str(&format!("**Última ejecución del motor:** {} · {} ({})\n\n", u["titulo"].as_str().unwrap_or(""), u["estado"].as_str().unwrap_or(""), u["agente"].as_str().unwrap_or("")));
    }

    // Pendientes de revisión
    let pend = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'propuestas', (SELECT COUNT(*) FROM "ArtefactoPropuesta" WHERE "solucionId" = $1 AND estado = 'PENDIENTE'),
             'actas', (SELECT COUNT(*) FROM "ActaItem" WHERE "solucionId" = $1 AND estado = 'PENDIENTE'),
             'pruebas', (SELECT COUNT(*) FROM "PruebaAceptacion" WHERE "solucionId" = $1 AND estado = 'PROPUESTA'),
             'riesgos', COALESCE((SELECT jsonb_agg(texto) FROM (SELECT texto FROM "ActaItem" WHERE "solucionId" = $1 AND tipo = 'RIESGO' AND estado = 'ACEPTADO' ORDER BY "createdAt" DESC LIMIT 5) r), '[]'::jsonb),
             'preguntas', COALESCE((SELECT jsonb_agg(texto) FROM (SELECT texto FROM "ActaItem" WHERE "solucionId" = $1 AND tipo = 'PREGUNTA' AND estado = 'ACEPTADO' ORDER BY "createdAt" DESC LIMIT 5) q), '[]'::jsonb))"#,
        &[B::T(sol.into())],
    )
    .await
    .map_err(e)?;
    let desact: Vec<String> = match catalogo_artefactos::filas(st, sol).await {
        Ok((filas, _)) => filas.iter().filter(|t| t["desactualizado"] == true).map(|t| t["nombre"].as_str().unwrap_or("").to_string()).collect(),
        Err(_) => vec![],
    };
    // Operación: incidentes y solicitudes de cambio abiertos.
    let op = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object('incidentes', COUNT(*) FILTER (WHERE tipo = 'INCIDENTE' AND estado NOT IN ('CERRADO', 'RECHAZADO')), 'solicitudes', COUNT(*) FILTER (WHERE tipo = 'SOLICITUD_CAMBIO' AND estado NOT IN ('CERRADO', 'RECHAZADO')))
           FROM "CasoOperacion" WHERE "solucionId" = $1"#,
        &[B::T(sol.into())],
    )
    .await
    .map_err(e)?;
    let (inc, sc) = (op["incidentes"].as_i64().unwrap_or(0), op["solicitudes"].as_i64().unwrap_or(0));
    if inc + sc > 0 {
        m.push_str(&format!("**Operación:** {inc} incidente(s) y {sc} solicitud(es) de cambio abiertos.\n\n"));
    }
    m.push_str("**Por revisar:**\n");
    m.push_str(&format!("- Propuestas de cambio pendientes: {}\n", pend["propuestas"].as_i64().unwrap_or(0)));
    m.push_str(&format!("- Ítems de actas y notas sin decidir: {}\n", pend["actas"].as_i64().unwrap_or(0)));
    m.push_str(&format!("- Pruebas de aceptación propuestas sin aprobar: {}\n", pend["pruebas"].as_i64().unwrap_or(0)));
    if !desact.is_empty() {
        m.push_str(&format!("- Artefactos desactualizados: {}\n", desact.join(", ")));
    }
    for (rotulo, clave) in [("Riesgos abiertos", "riesgos"), ("Preguntas abiertas", "preguntas")] {
        let xs = lista(&pend, clave);
        if !xs.is_empty() {
            m.push_str(&format!("\n**{rotulo}:**\n"));
            for x in xs {
                m.push_str(&format!("- {}\n", x.as_str().unwrap_or("")));
            }
        }
    }
    Ok(m)
}

/// Reemplaza (o agrega al principio) el bloque del estado vivo dentro del contenido de la memoria.
pub(crate) fn con_bloque(contenido: &str, bloque: &str) -> String {
    match (contenido.find(INICIO), contenido.find(FIN)) {
        (Some(a), Some(b)) if b > a => format!("{}{}{}", &contenido[..a], bloque, &contenido[b + FIN.len()..]),
        _ => {
            if contenido.trim().is_empty() {
                bloque.to_string()
            } else {
                format!("{bloque}\n\n{}", contenido.trim_start())
            }
        }
    }
}

/// Texto del bloque actual (entre marcas), sin las marcas ni la línea de fecha.
pub(crate) fn bloque_sin_fecha(contenido: &str) -> String {
    let (Some(a), Some(b)) = (contenido.find(INICIO), contenido.find(FIN)) else { return String::new() };
    if b <= a {
        return String::new();
    }
    contenido[a + INICIO.len()..b].lines().filter(|l| !l.starts_with("_Actualizado")).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Actualiza el estado vivo. Devuelve `{ cambio, version }`.
pub async fn actualizar(st: &AppState, sol: &str, motivo: &str) -> Result<Value, String> {
    let nuevo_cuerpo = cuerpo(st, sol).await?;
    let fecha = fetch_text_opt(&st.pool, r#"SELECT to_char(NOW() AT TIME ZONE 'America/Bogota', 'DD/MM/YYYY HH24:MI')"#, &[]).await.ok().flatten().unwrap_or_default();
    let bloque = format!("{INICIO}\n## Estado vivo del proyecto\n_Actualizado {fecha} (hora de Colombia) · {motivo}. Se regenera solo; lo que escribas fuera de este bloque no se toca._\n\n{}\n{FIN}", nuevo_cuerpo.trim());
    let fallo = |e: sqlx::Error| e.to_string();
    let mut tx = st.pool.begin().await.map_err(fallo)?;
    let actual: Option<(i32, String)> = sqlx::query_as(r#"SELECT version, contenido FROM "ProyectoMemoria" WHERE "solucionId" = $1 FOR UPDATE"#).bind(sol).fetch_optional(&mut *tx).await.map_err(fallo)?;
    let (version, contenido) = actual.map(|a| (a.0 as i64, a.1)).unwrap_or((0, String::new()));
    if bloque_sin_fecha(&contenido) == bloque_sin_fecha(&bloque) {
        return Ok(json!({ "cambio": false, "version": version }));
    }
    let nuevo = con_bloque(&contenido, &bloque);
    if nuevo.chars().count() > MAX_CARACTERES {
        return Err("La memoria superaría los 30.000 caracteres: reduce el texto escrito a mano".into());
    }
    let v = version + 1;
    sqlx::query(r#"INSERT INTO "ProyectoMemoriaVersion" ("solucionId", version, contenido, origen, autor, nota) VALUES ($1, $2::int, $3, 'IA', 'Motor (estado vivo)', $4)"#)
        .bind(sol)
        .bind(v)
        .bind(&nuevo)
        .bind(motivo.chars().take(200).collect::<String>())
        .execute(&mut *tx)
        .await
        .map_err(fallo)?;
    sqlx::query(
        r#"INSERT INTO "ProyectoMemoria" ("solucionId", contenido, version, "actualizadoPor", "updatedAt") VALUES ($1, $2, $3::int, 'Motor (estado vivo)', NOW())
           ON CONFLICT ("solucionId") DO UPDATE SET contenido = $2, version = $3::int, "actualizadoPor" = 'Motor (estado vivo)', "updatedAt" = NOW()"#,
    )
    .bind(sol)
    .bind(&nuevo)
    .bind(v)
    .execute(&mut *tx)
    .await
    .map_err(fallo)?;
    tx.commit().await.map_err(fallo)?;
    Ok(json!({ "cambio": true, "version": v }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bloque(texto: &str) -> String {
        format!("{INICIO}\n## Estado\n_Actualizado hoy_\n\n{texto}\n{FIN}")
    }

    #[test]
    fn agrega_el_bloque_al_principio_y_respeta_lo_escrito() {
        let r = con_bloque("Notas del equipo", &bloque("A"));
        assert!(r.starts_with(INICIO) && r.ends_with("Notas del equipo"));
    }

    #[test]
    fn reemplaza_solo_el_bloque() {
        let antes = format!("Intro\n\n{}\n\nDespués", bloque("A"));
        let r = con_bloque(&antes, &bloque("B"));
        assert!(r.starts_with("Intro") && r.ends_with("Después") && r.contains("\nB\n") && !r.contains("\nA\n"));
    }

    #[test]
    fn memoria_vacia_queda_solo_con_el_bloque() {
        assert_eq!(con_bloque("", &bloque("A")), bloque("A"));
    }

    #[test]
    fn la_fecha_no_cuenta_como_cambio() {
        let a = format!("{INICIO}\n## E\n_Actualizado 01/01_\n\ncuerpo\n{FIN}");
        let b = format!("{INICIO}\n## E\n_Actualizado 02/01_\n\ncuerpo\n{FIN}");
        let c = format!("{INICIO}\n## E\n_Actualizado 02/01_\n\notro\n{FIN}");
        assert_eq!(bloque_sin_fecha(&a), bloque_sin_fecha(&b));
        assert_ne!(bloque_sin_fecha(&a), bloque_sin_fecha(&c));
        assert_eq!(bloque_sin_fecha("sin marcas"), "");
    }
}
