//! Paquete de cierre de fase (MASD-0026-0002): al avanzar una fase, el motor deja un resumen determinista de cómo cerró
//! (criterios, actividades, artefactos vigentes y desactualizados, lo que queda pendiente y el estado del backlog).
//! Se guarda como documento de la fase (`entregableClave = paquete_cierre`), así el contexto de la fase siguiente lo recibe
//! (el armado de contexto ya incluye los documentos de las demás fases) y cualquier persona puede retomar el proyecto.
//! Es la mejor tarea posible: si algo falla, no frena el avance de fase.

use serde_json::Value;

use crate::{
    routes::catalogo_artefactos,
    state::AppState,
    util::{exec, fetch_json, fetch_text_opt, new_id, B},
};

pub(crate) async fn generar(st: &AppState, sol: &str, fase: &str, fase_nombre: &str, criterios: &[Value], quien: &str) {
    let md = construir(st, sol, fase, fase_nombre, criterios, quien).await;
    let titulo = format!("Paquete de cierre · {fase_nombre}");
    let r = exec(
        &st.pool,
        r#"INSERT INTO "ProyectoDocumento" (id, "solucionId", "faseClave", "entregableClave", titulo, contenido, version, "actualizadoPor")
           VALUES ($1, $2, $3, 'paquete_cierre', $4, $5, 1, $6)
           ON CONFLICT ("solucionId", "faseClave", "entregableClave")
           DO UPDATE SET contenido = EXCLUDED.contenido, titulo = EXCLUDED.titulo, version = "ProyectoDocumento".version + 1,
                         "actualizadoPor" = EXCLUDED."actualizadoPor", "updatedAt" = NOW()"#,
        &[B::T(new_id()), B::T(sol.into()), B::T(fase.into()), B::T(titulo), B::T(md), B::T(quien.into())],
    )
    .await;
    if let Err(e) = r {
        tracing::warn!("paquete de cierre de {fase} en {sol}: {e}");
    }
    // El estado vivo de la memoria refleja la fase que acaba de cerrarse.
    if let Err(e) = crate::routes::estado_vivo::actualizar(st, sol, &format!("cierre de la fase {fase_nombre}")).await {
        tracing::warn!("estado vivo de {sol}: {e}");
    }
}

async fn construir(st: &AppState, sol: &str, fase: &str, fase_nombre: &str, criterios: &[Value], quien: &str) -> String {
    let fecha = fetch_text_opt(&st.pool, r#"SELECT to_char(NOW() AT TIME ZONE 'America/Bogota', 'DD/MM/YYYY HH24:MI')"#, &[]).await.ok().flatten().unwrap_or_default();
    let mut m = String::new();
    m.push_str(&format!("# Paquete de cierre · {fase_nombre}\n\nFase cerrada el {fecha} (hora de Colombia) por {quien}. Lo genera el motor al avanzar la fase; la fase siguiente lo recibe como contexto.\n\n"));

    // Criterios de la puerta
    m.push_str("## Criterios de la puerta\n\n");
    if criterios.is_empty() {
        m.push_str("- Sin criterios definidos.\n");
    }
    for c in criterios {
        let ok = c["ok"] == true;
        m.push_str(&format!("- [{}] {}{}\n", if ok { "x" } else { " " }, c["texto"].as_str().unwrap_or_default(), if ok { String::new() } else { " (cerrada sin cumplir)".into() }));
    }

    // Actividades de la fase
    let acts = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('titulo', bi.title, 'estado', bi.status) ORDER BY bi."order"), '[]'::jsonb)
             FROM "FaseActividad" fa JOIN "BacklogItem" bi ON bi.id = fa."backlogItemId" WHERE fa."solucionId" = $1 AND fa.fase = $2"#,
        &[B::T(sol.into()), B::T(fase.into())],
    )
    .await
    .unwrap_or(Value::Null);
    let acts: Vec<Value> = acts.as_array().cloned().unwrap_or_default();
    m.push_str("\n## Actividades de la fase\n\n");
    if acts.is_empty() {
        m.push_str("- La fase no tenía actividades registradas.\n");
    }
    let mut sin_terminar: Vec<String> = vec![];
    for a in &acts {
        let estado = a["estado"].as_str().unwrap_or("");
        let titulo = a["titulo"].as_str().unwrap_or("");
        m.push_str(&format!("- {titulo} · {estado}\n"));
        if estado != "DONE" && estado != "CANCELLED" {
            sin_terminar.push(titulo.to_string());
        }
    }

    // Artefactos
    m.push_str("\n## Artefactos del proyecto\n\n");
    let mut desactualizados: Vec<String> = vec![];
    match catalogo_artefactos::filas(st, sol).await {
        Ok((filas, _)) => {
            let vigentes: Vec<String> = filas.iter().filter(|t| t["instancia"]["existe"] == true).map(|t| format!("{} (v{})", t["nombre"].as_str().unwrap_or(""), t["instancia"]["version"].as_i64().unwrap_or(0))).collect();
            m.push_str(&format!("Con contenido: {}.\n", if vigentes.is_empty() { "ninguno".to_string() } else { vigentes.join(", ") }));
            for t in filas.iter().filter(|t| t["desactualizado"] == true) {
                let causas: Vec<String> = t["obsoletoPor"].as_array().map(|a| a.iter().map(|o| format!("{} v{}→v{}", o["nombre"].as_str().unwrap_or(""), o["versionUsada"].as_i64().unwrap_or(0), o["versionActual"].as_i64().unwrap_or(0))).collect()).unwrap_or_default();
                desactualizados.push(format!("{} (cambió: {})", t["nombre"].as_str().unwrap_or(""), causas.join(", ")));
            }
            let faltan: Vec<String> = filas.iter().filter(|t| t["instancia"]["soporte"] == true && t["instancia"]["existe"] != true && t["faseDuena"] == fase).map(|t| t["nombre"].as_str().unwrap_or("").to_string()).collect();
            if !faltan.is_empty() {
                m.push_str(&format!("Esperados en esta fase y aún sin contenido: {}.\n", faltan.join(", ")));
            }
        }
        Err(_) => m.push_str("No se pudo leer el catálogo de artefactos.\n"),
    }

    // Backlog
    let bl = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_object_agg(status, n), '{}'::jsonb) FROM (SELECT status, COUNT(*) n FROM "BacklogItem" WHERE "solucionId" = $1 GROUP BY status) x"#,
        &[B::T(sol.into())],
    )
    .await
    .unwrap_or(Value::Null);
    if let Some(o) = bl.as_object() {
        if !o.is_empty() {
            let partes: Vec<String> = o.iter().map(|(k, v)| format!("{} {}", v.as_i64().unwrap_or(0), k)).collect();
            m.push_str(&format!("\n## Backlog del proyecto\n\n{}.\n", partes.join(" · ")));
        }
    }

    // Pendiente para la fase siguiente
    m.push_str("\n## Pendiente para la fase siguiente\n\n");
    let mut hay = false;
    for s in &sin_terminar {
        m.push_str(&format!("- Actividad sin terminar: {s}\n"));
        hay = true;
    }
    for c in criterios.iter().filter(|c| c["ok"] != true) {
        m.push_str(&format!("- Criterio cerrado sin cumplir: {}\n", c["texto"].as_str().unwrap_or_default()));
        hay = true;
    }
    for d in &desactualizados {
        m.push_str(&format!("- Revisar o regenerar: {d}\n"));
        hay = true;
    }
    if !hay {
        m.push_str("- Nada pendiente detectado.\n");
    }
    m
}
