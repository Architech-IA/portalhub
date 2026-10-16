//! Seguimiento automático de los proyectos: libera las tareas que una sesión tomó y dejó sin cerrar, abre avisos (tareas atascadas o
//! fallidas, sprints atrasados, cosas esperando a una persona…), los publica en la Bitácora del proyecto y arma el resumen de avance y costo.
//! Corre solo (cada 15 minutos) dentro del servicio público; también se puede lanzar a mano con la clave interna.

use std::{collections::HashSet, time::Duration};

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_text_opt, new_id, B},
};
use crate::routes::solucion_hub::es_admin;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proyectos/revision/ejecutar", post(ejecutar_ahora))
        .route("/api/proyectos/{id}/resumen", get(resumen))
        .route("/api/proyectos/{id}/costo", get(costo))
        .route("/api/proyectos/{id}/costo/presupuesto", axum::routing::put(costo_presupuesto))
        .route("/api/proyectos/{id}/alertas", get(alertas))
        .route("/api/proyectos/{id}/alertas/{aid}/descartar", post(descartar))
}

/// Lanza la revisión periódica (salvo que `REVISION_AUTOMATICA=0`).
pub fn iniciar(st: AppState) {
    if std::env::var("REVISION_AUTOMATICA").map(|v| v == "0").unwrap_or(false) {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(120)).await;
        loop {
            match revisar(&st).await {
                Ok(r) => tracing::info!("[seguimiento] {r}"),
                Err(e) => tracing::error!("[seguimiento] {e}"),
            }
            tokio::time::sleep(Duration::from_secs(900)).await;
        }
    });
}

struct Aviso {
    sol: String,
    tipo: &'static str,
    referencia: String,
    titulo: String,
    detalle: String,
}

fn err(e: sqlx::Error) -> String {
    e.to_string()
}

// ── Tareas huérfanas ─────────────────────────────────────────────────────────────────────────
/// Una tarea que una sesión de desarrollo tomó y quedó en curso, pero la sesión ya no existe, se cerró o lleva 12 h sin actividad,
/// vuelve a la cola con una nota. Devuelve los avisos que corresponden.
async fn liberar_huerfanas(st: &AppState) -> Result<Vec<Aviso>, String> {
    let filas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('lid', l.id, 'sesion', l."sesionId", 'tarea', l."tareaId", 'sol', l."solucionId", 'estado', s.estado, 'existe', s.id IS NOT NULL,
                  'codigo', b."taskCode", 'titulo', b.title)), '[]'::jsonb)
             FROM "ProyectoSesionTarea" l JOIN "BacklogItem" b ON b.id = l."tareaId" LEFT JOIN "ProyectoSesion" s ON s.id = l."sesionId"
            WHERE l.estado = 'EN_CURSO' AND b.status = 'IN_PROGRESS' AND (s.id IS NULL OR s.estado <> 'ACTIVA' OR s."updatedAt" < NOW() - interval '12 hours')"#,
        &[],
    )
    .await
    .map_err(err)?;
    let mut avisos = vec![];
    for f in filas.as_array().cloned().unwrap_or_default() {
        let (tarea, sol, lid) = (f["tarea"].as_str().unwrap_or("").to_string(), f["sol"].as_str().unwrap_or("").to_string(), f["lid"].as_str().unwrap_or("").to_string());
        let motivo = if f["existe"] != true { "la sesión ya no existe" } else if f["estado"] != "ACTIVA" { "la sesión se cerró" } else { "la sesión lleva más de 12 horas sin actividad" };
        let nota = format!("[Liberada automáticamente: {motivo}. La tarea volvió a la cola sin terminar; lo avanzado quedó en la rama de la sesión.]");
        exec(&st.pool, r#"UPDATE "BacklogItem" SET status = 'BACKLOG', "assigneeName" = NULL, resultado = $2, "updatedAt" = NOW() WHERE id = $1 AND status = 'IN_PROGRESS'"#, &[B::T(tarea.clone()), B::T(nota.clone())]).await.map_err(err)?;
        exec(&st.pool, r#"UPDATE "ProyectoSesionTarea" SET estado = 'LIBERADA', "cerradaEn" = NOW() WHERE id = $1"#, &[B::T(lid)]).await.map_err(err)?;
        if let Some(e) = fetch_text_opt(&st.pool, r#"SELECT id FROM "TaskExecution" WHERE "backlogItemId" = $1 AND status = 'RUNNING' AND "agentName" LIKE 'Sesión de desarrollo%' ORDER BY "startedAt" DESC LIMIT 1"#, &[B::T(tarea.clone())]).await.map_err(err)? {
            exec(&st.pool, r#"UPDATE "TaskExecution" SET status = 'FAILED', "resultSummary" = $2, "finishedAt" = NOW() WHERE id = $1"#, &[B::T(e.clone()), B::T(nota.clone())]).await.map_err(err)?;
            let _ = exec(&st.pool, r#"INSERT INTO "TaskExecutionEvent" (id, "taskId", "execId", kind, message) VALUES (gen_random_uuid()::text, $1, $2, 'info', $3)"#, &[B::T(tarea.clone()), B::T(e), B::T(nota.clone())]).await;
        }
        avisos.push(Aviso { sol, tipo: "TAREA_LIBERADA", referencia: tarea, titulo: format!("Tarea liberada: {}", f["codigo"].as_str().unwrap_or("")), detalle: format!("{} — {motivo}; volvió a la cola.", f["titulo"].as_str().unwrap_or("")) });
    }
    Ok(avisos)
}

// ── Detección de avisos ─────────────────────────────────────────────────────────────────────
async fn consulta(st: &AppState, tipo: &'static str, titulo: &str, sql: &str) -> Result<Vec<Aviso>, String> {
    let filas = fetch_json(&st.pool, &format!(r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('sol', x.sol, 'ref', x.ref, 'detalle', x.detalle)), '[]'::jsonb) FROM ({sql}) x"#), &[]).await.map_err(err)?;
    Ok(filas
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|f| f["sol"].as_str().map(|s| !s.is_empty()).unwrap_or(false))
        .map(|f| Aviso { sol: f["sol"].as_str().unwrap_or("").into(), tipo, referencia: f["ref"].as_str().unwrap_or("").into(), titulo: titulo.into(), detalle: f["detalle"].as_str().unwrap_or("").into() })
        .collect())
}

async fn detectar(st: &AppState) -> Result<Vec<Aviso>, String> {
    let mut v = vec![];
    v.extend(
        consulta(
            st,
            "TAREA_ATASCADA",
            "Tarea en curso hace más de 24 horas",
            r#"SELECT b."solucionId" sol, b.id ref, COALESCE(b."taskCode", '') || ' · ' || b.title detalle FROM "BacklogItem" b
                WHERE b.status = 'IN_PROGRESS' AND b."solucionId" IS NOT NULL AND COALESCE(b."fechaEjecucion", b."fechaInicio", b."updatedAt") < NOW() - interval '24 hours'
                  AND NOT EXISTS (SELECT 1 FROM "ProyectoSesionTarea" l JOIN "ProyectoSesion" s ON s.id = l."sesionId" WHERE l."tareaId" = b.id AND l.estado = 'EN_CURSO' AND s."updatedAt" > NOW() - interval '12 hours')"#,
        )
        .await?,
    );
    v.extend(
        consulta(
            st,
            "TAREA_FALLIDA",
            "Tarea fallida",
            r#"SELECT b."solucionId" sol, b.id ref, COALESCE(b."taskCode", '') || ' · ' || b.title detalle FROM "BacklogItem" b WHERE b.status = 'FAILED' AND b."solucionId" IS NOT NULL AND b."updatedAt" > NOW() - interval '72 hours'"#,
        )
        .await?,
    );
    v.extend(
        consulta(
            st,
            "SPRINT_ATRASADO",
            "Sprint pasado de su fecha de fin",
            r#"SELECT sp."solucionId" sol, sp.id ref, COALESCE(sp."sprintCode", '') || ' · ' || sp.name || ' (fin ' || to_char(sp."endDate", 'YYYY-MM-DD') || ')' detalle FROM "Sprint" sp
                WHERE sp.status = 'ACTIVE' AND sp."endDate" IS NOT NULL AND sp."endDate" < NOW() AND sp."solucionId" IS NOT NULL"#,
        )
        .await?,
    );
    v.extend(
        consulta(
            st,
            "ACCION_IA_PENDIENTE",
            "Una acción de la IA espera confirmación hace más de 24 horas",
            r#"SELECT a."solucionId" sol, a.id ref, a.titulo detalle FROM "ProyectoAccionIA" a WHERE a.estado = 'PENDIENTE' AND a."createdAt" < NOW() - interval '24 hours'"#,
        )
        .await?,
    );
    v.extend(
        consulta(
            st,
            "PROPUESTA_PENDIENTE",
            "Una propuesta sobre un artefacto espera decisión hace más de 48 horas",
            r#"SELECT p."solucionId" sol, p.id ref, p.titulo detalle FROM "ArtefactoPropuesta" p WHERE p.estado = 'PENDIENTE' AND p."createdAt" < NOW() - interval '48 hours'"#,
        )
        .await?,
    );
    v.extend(
        consulta(
            st,
            "ENTREGABLE_POR_REVISAR",
            "Un archivo generado por la IA espera revisión hace más de 48 horas",
            r#"SELECT a."solucionId" sol, a.id ref, a.nombre detalle FROM "ProyectoEntregableArchivo" a WHERE a.estado = 'BORRADOR' AND a."createdAt" < NOW() - interval '48 hours'"#,
        )
        .await?,
    );
    v.extend(
        consulta(
            st,
            "FASE_SIN_MOVIMIENTO",
            "La fase lleva más de 14 días sin movimientos",
            r#"SELECT f."solucionId" sol, f."faseActual" ref, 'Fase ' || f."faseActual" detalle FROM "ProyectoFase" f WHERE f.estado = 'EN_CURSO' AND f."updatedAt" < NOW() - interval '14 days'"#,
        )
        .await?,
    );
    Ok(v)
}

/// Abre los avisos nuevos (o reabiertos), cierra los que dejaron de cumplirse y devuelve los que hay que anunciar.
async fn sincronizar(st: &AppState, detectados: &[Aviso], informativos: &[Aviso]) -> Result<Vec<usize>, String> {
    let mut a_anunciar: Vec<usize> = vec![];
    let todos: Vec<&Aviso> = detectados.iter().chain(informativos.iter()).collect();
    let claves: HashSet<(String, &str, String)> = todos.iter().map(|a| (a.sol.clone(), a.tipo, a.referencia.clone())).collect();
    for (i, a) in todos.iter().enumerate() {
        let previo = fetch_text_opt(&st.pool, r#"SELECT estado FROM "ProyectoAlerta" WHERE "solucionId" = $1 AND tipo = $2 AND ref = $3"#, &[B::T(a.sol.clone()), B::T(a.tipo.into()), B::T(a.referencia.clone())]).await.map_err(err)?;
        match previo.as_deref() {
            Some("ABIERTA") => {
                exec(&st.pool, r#"UPDATE "ProyectoAlerta" SET titulo = $4, detalle = $5 WHERE "solucionId" = $1 AND tipo = $2 AND ref = $3"#, &[B::T(a.sol.clone()), B::T(a.tipo.into()), B::T(a.referencia.clone()), B::T(a.titulo.clone()), B::T(a.detalle.clone())]).await.map_err(err)?;
            }
            Some("DESCARTADA") => {}
            Some(_) => {
                exec(&st.pool, r#"UPDATE "ProyectoAlerta" SET estado = 'ABIERTA', titulo = $4, detalle = $5, "resueltaEn" = NULL, "creadaEn" = NOW() WHERE "solucionId" = $1 AND tipo = $2 AND ref = $3"#, &[B::T(a.sol.clone()), B::T(a.tipo.into()), B::T(a.referencia.clone()), B::T(a.titulo.clone()), B::T(a.detalle.clone())]).await.map_err(err)?;
                a_anunciar.push(i);
            }
            None => {
                exec(&st.pool, r#"INSERT INTO "ProyectoAlerta" (id, "solucionId", tipo, ref, titulo, detalle) VALUES ($1, $2, $3, $4, $5, $6)"#, &[B::T(new_id()), B::T(a.sol.clone()), B::T(a.tipo.into()), B::T(a.referencia.clone()), B::T(a.titulo.clone()), B::T(a.detalle.clone())]).await.map_err(err)?;
                a_anunciar.push(i);
            }
        }
    }
    // Los que ya no se cumplen se resuelven solos (los informativos de «tarea liberada» se cierran a los 3 días)
    let abiertas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'sol', "solucionId", 'tipo', tipo, 'ref', ref)), '[]'::jsonb) FROM "ProyectoAlerta" WHERE estado IN ('ABIERTA', 'DESCARTADA') AND tipo <> 'TAREA_LIBERADA'"#, &[]).await.map_err(err)?;
    for f in abiertas.as_array().cloned().unwrap_or_default() {
        let tipos = ["TAREA_ATASCADA", "TAREA_FALLIDA", "SPRINT_ATRASADO", "ACCION_IA_PENDIENTE", "PROPUESTA_PENDIENTE", "ENTREGABLE_POR_REVISAR", "FASE_SIN_MOVIMIENTO"];
        let tipo = tipos.iter().find(|t| **t == f["tipo"].as_str().unwrap_or("")).copied();
        let Some(tipo) = tipo else { continue };
        if !claves.contains(&(f["sol"].as_str().unwrap_or("").to_string(), tipo, f["ref"].as_str().unwrap_or("").to_string())) {
            exec(&st.pool, r#"UPDATE "ProyectoAlerta" SET estado = 'RESUELTA', "resueltaEn" = NOW() WHERE id = $1"#, &[B::T(f["id"].as_str().unwrap_or("").into())]).await.map_err(err)?;
        }
    }
    exec(&st.pool, r#"UPDATE "ProyectoAlerta" SET estado = 'RESUELTA', "resueltaEn" = NOW() WHERE tipo = 'TAREA_LIBERADA' AND estado = 'ABIERTA' AND "creadaEn" < NOW() - interval '3 days'"#, &[]).await.map_err(err)?;
    Ok(a_anunciar)
}

fn icono(tipo: &str) -> &'static str {
    match tipo {
        "TAREA_ATASCADA" => "⏱",
        "TAREA_FALLIDA" => "❌",
        "TAREA_LIBERADA" => "♻️",
        "SPRINT_ATRASADO" => "📅",
        "ACCION_IA_PENDIENTE" | "PROPUESTA_PENDIENTE" | "ENTREGABLE_POR_REVISAR" => "⏳",
        _ => "🔔",
    }
}

/// Una pasada completa: libera, detecta, abre avisos y publica en la Bitácora lo nuevo.
pub async fn revisar(st: &AppState) -> Result<String, String> {
    let informativos = liberar_huerfanas(st).await?;
    let detectados = detectar(st).await?;
    let nuevos = sincronizar(st, &detectados, &informativos).await?;
    let todos: Vec<&Aviso> = detectados.iter().chain(informativos.iter()).collect();
    let mut por_proyecto: std::collections::BTreeMap<String, Vec<&Aviso>> = Default::default();
    for i in &nuevos {
        por_proyecto.entry(todos[*i].sol.clone()).or_default().push(todos[*i]);
    }
    for (sol, lista) in &por_proyecto {
        let mut lineas: Vec<String> = lista.iter().take(10).map(|a| format!("- {} **{}** — {}", icono(a.tipo), a.titulo, a.detalle)).collect();
        if lista.len() > 10 {
            lineas.push(format!("- … y {} más (mira el resumen del proyecto)", lista.len() - 10));
        }
        let texto = format!("🔔 **Avisos del proyecto** ({} nuevo(s)):\n{}", lista.len(), lineas.join("\n"));
        if let Ok(b) = crate::routes::proyectos::obtener_bitacora(st, sol).await {
            let _ = crate::routes::proyectos::publicar_en_sesion(st, &b, &texto, json!({ "origen": "seguimiento" })).await;
        }
    }
    Ok(format!("{} liberada(s), {} aviso(s) detectado(s), {} nuevo(s) en {} proyecto(s)", informativos.len(), detectados.len(), nuevos.len(), por_proyecto.len()))
}

async fn ejecutar_ahora(State(st): State<AppState>, se: Session) -> ApiResult<Json<Value>> {
    if !(se.is_service || se.is_admin()) {
        return Err(ApiError::forbidden("Solo un administrador puede lanzar la revisión"));
    }
    let r = revisar(&st).await.map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true, "resultado": r })))
}

// ── Avisos del proyecto ─────────────────────────────────────────────────────────────────────
async fn alertas(State(st): State<AppState>, se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !se.is_service {
        se.require_user()?;
    }
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'tipo', tipo, 'titulo', titulo, 'detalle', detalle, 'creadaEn', "creadaEn") ORDER BY "creadaEn" DESC), '[]'::jsonb) FROM "ProyectoAlerta" WHERE "solucionId" = $1 AND estado = 'ABIERTA'"#,
            &[B::T(id)],
        )
        .await?,
    ))
}

async fn descartar(State(st): State<AppState>, se: Session, Path((id, aid)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    if !se.is_service {
        se.require_user()?;
    }
    let n = exec(&st.pool, r#"UPDATE "ProyectoAlerta" SET estado = 'DESCARTADA', "resueltaEn" = NOW() WHERE id = $1 AND "solucionId" = $2 AND estado = 'ABIERTA'"#, &[B::T(aid), B::T(id)]).await?;
    if n == 0 {
        return Err(ApiError::not_found("Ese aviso no está abierto"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ── Resumen de avance y costo ───────────────────────────────────────────────────────────────
async fn resumen(State(st): State<AppState>, se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !se.is_service {
        se.require_user()?;
    }
    let tareas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_object_agg(status, n), '{}'::jsonb) FROM (SELECT status, COUNT(*)::int n FROM "BacklogItem" WHERE "solucionId" = $1 GROUP BY status) t"#, &[B::T(id.clone())]).await?;
    let sprints = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object('total', COUNT(*)::int, 'activos', COUNT(*) FILTER (WHERE status = 'ACTIVE')::int, 'planificados', COUNT(*) FILTER (WHERE status = 'PLANNED')::int,
                  'cerrados', COUNT(*) FILTER (WHERE status IN ('CLOSED', 'REVIEW_PENDING'))::int, 'atrasados', COUNT(*) FILTER (WHERE status = 'ACTIVE' AND "endDate" < NOW())::int) FROM "Sprint" WHERE "solucionId" = $1"#,
        &[B::T(id.clone())],
    )
    .await?;
    let pendientes = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'accionesIA', (SELECT COUNT(*)::int FROM "ProyectoAccionIA" WHERE "solucionId" = $1 AND estado = 'PENDIENTE'),
             'propuestas', (SELECT COUNT(*)::int FROM "ArtefactoPropuesta" WHERE "solucionId" = $1 AND estado = 'PENDIENTE'),
             'archivos', (SELECT COUNT(*)::int FROM "ProyectoEntregableArchivo" WHERE "solucionId" = $1 AND estado = 'BORRADOR'))"#,
        &[B::T(id.clone())],
    )
    .await?;
    // Costo: las sesiones guardan su uso en cada mensaje; el ejecutor lo deja en un evento de cada tarea; el resto, en el registro de uso
    let sesiones = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object('mensajes', COUNT(*)::int, 'entrada', COALESCE(SUM((m.metadata->'uso'->>'promptTokens')::bigint), 0)::text, 'cache', COALESCE(SUM((m.metadata->'uso'->>'cachedTokens')::bigint), 0)::text,
                  'salida', COALESCE(SUM((m.metadata->'uso'->>'completionTokens')::bigint), 0)::text)
             FROM "ProyectoMensaje" m JOIN "ProyectoSesion" s ON s.id = m."sesionId" WHERE s."solucionId" = $1 AND m.metadata ? 'uso'"#,
        &[B::T(id.clone())],
    )
    .await?;
    let ejecutor = fetch_json(
        &st.pool,
        r#"WITH u AS (SELECT regexp_replace(substring(e.message from '— ([0-9.]+) en total'), '\.', '', 'g')::bigint t,
                             regexp_replace(substring(e.message from '\(([0-9.]+) entrada'), '\.', '', 'g')::bigint ent
                        FROM "TaskExecutionEvent" e JOIN "BacklogItem" b ON b.id = e."taskId" WHERE b."solucionId" = $1 AND e.message ~ '— [0-9.]+ en total')
           SELECT jsonb_build_object('ejecuciones', COUNT(*)::int, 'total', COALESCE(SUM(t), 0)::text, 'entrada', COALESCE(SUM(ent), 0)::text) FROM u"#,
        &[B::T(id.clone())],
    )
    .await?;
    let otros = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('origen', origen, 'llamadas', n, 'entrada', ent::text, 'salida', sal::text, 'cache', cac::text)), '[]'::jsonb)
             FROM (SELECT origen, COUNT(*)::int n, SUM("promptTokens")::bigint ent, SUM("completionTokens")::bigint sal, SUM("cachedTokens")::bigint cac FROM "UsoIA" WHERE clave LIKE '%' || $1 || '%' GROUP BY origen) t"#,
        &[B::T(id.clone())],
    )
    .await?;
    let alertas = fetch_json(&st.pool, r#"SELECT to_jsonb(COUNT(*)::int) FROM "ProyectoAlerta" WHERE "solucionId" = $1 AND estado = 'ABIERTA'"#, &[B::T(id)]).await?;
    Ok(Json(json!({ "tareas": tareas, "sprints": sprints, "pendientes": pendientes, "alertasAbiertas": alertas, "uso": { "sesiones": sesiones, "ejecutor": ejecutor, "otros": otros } })))
}

// ── Costo y presupuestos por sesión ──────────────────────────────────────────────────────────
/// Precios en USD por millón de tokens (PRECIO_MTOK_ENTRADA / _SALIDA / _CACHE). Sin precios configurados solo se muestran tokens.
fn precios() -> Option<(f64, f64, f64)> {
    let g = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok()).filter(|x| *x >= 0.0).unwrap_or(0.0);
    let (e, s, c) = (g("PRECIO_MTOK_ENTRADA"), g("PRECIO_MTOK_SALIDA"), g("PRECIO_MTOK_CACHE"));
    (e > 0.0 || s > 0.0).then_some((e, s, if c > 0.0 { c } else { e }))
}

async fn tokens_de(st: &AppState, sql: &str, id: &str) -> i64 {
    fetch_json(&st.pool, sql, &[B::T(id.into())]).await.ok().and_then(|v| v.as_i64()).unwrap_or(0)
}

/// Tokens (entrada + salida) que lleva gastados una sesión.
pub(crate) async fn gastado_sesion(st: &AppState, sid: &str) -> i64 {
    tokens_de(st, r#"SELECT to_jsonb(COALESCE(SUM((metadata->'uso'->>'promptTokens')::bigint + (metadata->'uso'->>'completionTokens')::bigint), 0)::bigint) FROM "ProyectoMensaje" WHERE "sesionId" = $1 AND metadata ? 'uso'"#, sid).await
}

pub(crate) async fn gastado_proyecto(st: &AppState, sol: &str) -> i64 {
    tokens_de(st, r#"SELECT to_jsonb(COALESCE(SUM((m.metadata->'uso'->>'promptTokens')::bigint + (m.metadata->'uso'->>'completionTokens')::bigint), 0)::bigint) FROM "ProyectoMensaje" m JOIN "ProyectoSesion" s ON s.id = m."sesionId" WHERE s."solucionId" = $1 AND m.metadata ? 'uso'"#, sol).await
}

async fn limite(st: &AppState, clave: &str) -> Option<i64> {
    fetch_json(&st.pool, r#"SELECT COALESCE((SELECT to_jsonb(tokens) FROM "ProyectoPresupuesto" WHERE clave = $1), 'null'::jsonb)"#, &[B::T(clave.into())]).await.ok().and_then(|v| v.as_i64())
}

/// Presupuesto por defecto de cada sesión (PRESUPUESTO_SESION_TOKENS), si no tiene uno propio.
fn limite_por_defecto() -> Option<i64> {
    std::env::var("PRESUPUESTO_SESION_TOKENS").ok().and_then(|v| v.parse::<i64>().ok()).filter(|n| *n > 0)
}

/// ¿Puede la sesión seguir gastando? `extra` son los tokens que ya lleva la respuesta en curso (todavía no guardados).
pub(crate) async fn revisar_presupuesto(st: &AppState, sol: &str, sid: &str, extra: i64) -> Result<(), String> {
    if let Some(l) = limite(st, &format!("ses:{sid}")).await.or_else(limite_por_defecto) {
        let g = gastado_sesion(st, sid).await + extra;
        if g >= l {
            return Err(format!("Esta sesión llegó a su presupuesto de {l} tokens (lleva {g}). Un administrador puede subirlo en Resumen › Costo, o puedes abrir otra sesión."));
        }
    }
    if let Some(l) = limite(st, &format!("sol:{sol}")).await {
        let g = gastado_proyecto(st, sol).await + extra;
        if g >= l {
            return Err(format!("El proyecto llegó a su presupuesto de {l} tokens (lleva {g}). Un administrador puede subirlo en Resumen › Costo."));
        }
    }
    Ok(())
}

async fn costo(State(st): State<AppState>, se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !se.is_service {
        se.require_user()?;
    }
    let sesiones = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(to_jsonb(x) ORDER BY x.ultima DESC NULLS LAST), '[]'::jsonb) FROM (
             SELECT s.id, s.titulo, s.tipo, s.estado,
                    COUNT(m.id) FILTER (WHERE m.metadata ? 'uso')::int AS mensajes,
                    COALESCE(SUM((m.metadata->'uso'->>'promptTokens')::bigint), 0)::bigint AS entrada,
                    COALESCE(SUM((m.metadata->'uso'->>'cachedTokens')::bigint), 0)::bigint AS cache,
                    COALESCE(SUM((m.metadata->'uso'->>'completionTokens')::bigint), 0)::bigint AS salida,
                    to_char(MAX(m."createdAt"), 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS ultima,
                    p.tokens AS presupuesto
               FROM "ProyectoSesion" s
               LEFT JOIN "ProyectoMensaje" m ON m."sesionId" = s.id
               LEFT JOIN "ProyectoPresupuesto" p ON p.clave = 'ses:' || s.id
              WHERE s."solucionId" = $1 GROUP BY s.id, p.tokens) x"#,
        &[B::T(id.clone())],
    )
    .await?;
    let dias = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('dia', d, 'total', t) ORDER BY d), '[]'::jsonb) FROM (
             SELECT to_char(m."createdAt", 'YYYY-MM-DD') AS d, SUM((m.metadata->'uso'->>'promptTokens')::bigint + (m.metadata->'uso'->>'completionTokens')::bigint)::bigint AS t
               FROM "ProyectoMensaje" m JOIN "ProyectoSesion" s ON s.id = m."sesionId"
              WHERE s."solucionId" = $1 AND m.metadata ? 'uso' AND m."createdAt" > NOW() - interval '14 days' GROUP BY 1) x"#,
        &[B::T(id.clone())],
    )
    .await?;
    let filas = sesiones.as_array().cloned().unwrap_or_default();
    let suma = |k: &str| -> i64 { filas.iter().filter_map(|f| f[k].as_i64()).sum() };
    let (entrada, cache, salida) = (suma("entrada"), suma("cache"), suma("salida"));
    let estimado = precios().map(|(pe, ps, pc)| (((entrada - cache).max(0) as f64) * pe + (cache as f64) * pc + (salida as f64) * ps) / 1_000_000.0);
    let presupuesto = limite(&st, &format!("sol:{id}")).await;
    Ok(Json(json!({
        "sesiones": filas,
        "dias": dias,
        "proyecto": { "entrada": entrada, "cache": cache, "salida": salida, "total": entrada + salida, "presupuesto": presupuesto },
        "presupuestoSesionPorDefecto": limite_por_defecto(),
        "estimadoUsd": estimado,
        "conPrecios": precios().is_some(),
    })))
}

/// Fija (o quita, con tokens = null) el presupuesto de una sesión o, sin sesionId, del proyecto. Solo administradores.
async fn costo_presupuesto(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    if !se.is_service {
        se.require_user()?;
    }
    if !es_admin(&se) {
        return Err(ApiError::forbidden("Solo un administrador cambia los presupuestos"));
    }
    let clave = match b["sesionId"].as_str().filter(|s| !s.is_empty()) {
        Some(sid) => {
            let existe = fetch_text_opt(&st.pool, r#"SELECT id FROM "ProyectoSesion" WHERE id = $1 AND "solucionId" = $2"#, &[B::T(sid.into()), B::T(id.clone())]).await?.is_some();
            if !existe {
                return Err(ApiError::bad_request("La sesión no es de este proyecto"));
            }
            format!("ses:{sid}")
        }
        None => format!("sol:{id}"),
    };
    match b["tokens"].as_i64() {
        Some(t) if (1_000..=2_000_000_000).contains(&t) => {
            exec(&st.pool, r#"INSERT INTO "ProyectoPresupuesto" (clave, tokens, "actualizadoEn") VALUES ($1, $2::bigint, NOW()) ON CONFLICT (clave) DO UPDATE SET tokens = EXCLUDED.tokens, "actualizadoEn" = NOW()"#, &[B::T(clave), B::I(t)]).await?;
        }
        Some(_) => return Err(ApiError::bad_request("El presupuesto debe estar entre 1.000 y 2.000.000.000 de tokens")),
        None => {
            exec(&st.pool, r#"DELETE FROM "ProyectoPresupuesto" WHERE clave = $1"#, &[B::T(clave)]).await?;
        }
    }
    Ok(Json(json!({ "ok": true })))
}
