//! Grafo de dependencias de un conjunto de tareas (`lib/executor/taskGraph.ts`). Reemplaza a LangGraph:
//! cada tarea es una corrutina que espera a la tarea de la que depende (dentro del conjunto), la despacha,
//! espera su cierre real en la base y deja su resultado. Una tarea que no llega a DONE marca su fallo y sus
//! hijas se saltan en cascada; las ramas independientes siguen su curso.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::sync::watch;

use super::{ejecutor, repo::R};
use crate::{
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, B},
};

const SONDEO: Duration = Duration::from_secs(3);
const ESPERA_MAX: Duration = Duration::from_secs(20 * 60);

#[derive(Clone)]
struct Resultado {
    ok: bool,
    texto: String,
}

async fn esperar_cierre(st: &AppState, tarea: &str) -> R<(String, Option<String>)> {
    let ini = Instant::now();
    while ini.elapsed() < ESPERA_MAX {
        let fila = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('status', status, 'resultado', resultado) FROM "BacklogItem" WHERE id = $1"#, &[B::T(tarea.into())]).await.map_err(|e| e.to_string())?;
        if let Some(f) = fila {
            let estado = f["status"].as_str().unwrap_or("").to_string();
            if matches!(estado.as_str(), "DONE" | "FAILED" | "BLOCKED") {
                return Ok((estado, f["resultado"].as_str().map(String::from)));
            }
        } else {
            return Err(format!("La tarea {tarea} ya no existe"));
        }
        tokio::time::sleep(SONDEO).await;
    }
    Err(format!("Timeout esperando el cierre de la tarea {tarea} (>{}ms)", ESPERA_MAX.as_millis()))
}

async fn nodo(st: AppState, id: String, padre: Option<watch::Receiver<Option<Resultado>>>) -> Resultado {
    let fallo = || Resultado { ok: false, texto: String::new() };
    if let Some(mut rx) = padre {
        // Espera a que el padre termine (bien o mal).
        let r = loop {
            if let Some(r) = rx.borrow().clone() {
                break r;
            }
            if rx.changed().await.is_err() {
                break fallo();
            }
        };
        if !r.ok {
            // El padre no llegó a DONE: la tarea queda BLOCKED con el motivo visible en el tablero.
            let padre_id = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('dep', "dependsOnTaskId") FROM "BacklogItem" WHERE id = $1"#, &[B::T(id.clone())]).await.ok().flatten().and_then(|f| f["dep"].as_str().map(String::from));
            let etiqueta = match &padre_id {
                Some(p) => fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('taskCode', "taskCode", 'title', title) FROM "BacklogItem" WHERE id = $1"#, &[B::T(p.clone())])
                    .await
                    .ok()
                    .flatten()
                    .and_then(|t| t["taskCode"].as_str().filter(|c| !c.is_empty()).map(|c| format!("{c} ({})", t["title"].as_str().unwrap_or(""))))
                    .unwrap_or_else(|| p.clone()),
                None => String::new(),
            };
            let _ = exec(
                &st.pool,
                r#"UPDATE "BacklogItem" SET status = 'BLOCKED', resultado = $2 WHERE id = $1 AND status = 'BACKLOG'"#,
                &[B::T(id.clone()), B::T(format!("No se ejecutó: depende de la tarea {etiqueta}, que no llegó a DONE (falló, quedó bloqueada, o dependía a su vez de otra que falló)."))],
            )
            .await;
            return fallo();
        }
    }
    // Resumible: lo que ya está en un estado terminal no se repite; lo que está en curso se espera.
    let actual = match fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('status', status, 'resultado', resultado) FROM "BacklogItem" WHERE id = $1"#, &[B::T(id.clone())]).await {
        Ok(Some(f)) => f,
        _ => return fallo(),
    };
    match actual["status"].as_str() {
        Some("DONE") => return Resultado { ok: true, texto: actual["resultado"].as_str().unwrap_or("").to_string() },
        Some("FAILED") | Some("BLOCKED") => return fallo(),
        _ => {}
    }
    let r: R<Resultado> = async {
        if actual["status"].as_str() == Some("BACKLOG") {
            ejecutor::despachar(&st, &id, None).await?;
        }
        let (estado, resultado) = esperar_cierre(&st, &id).await?;
        Ok(if estado == "DONE" { Resultado { ok: true, texto: resultado.unwrap_or_default() } } else { fallo() })
    }
    .await;
    match r {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[TASK_GRAPH] Error real despachando/esperando {id}: {e}");
            fallo()
        }
    }
}

/// Corre el grafo real de dependencias (`dependsOnTaskId`) de las tareas y devuelve `id → resultado` de las que llegaron a DONE.
pub async fn correr_cadena(st: &AppState, ids: Vec<String>) -> R<Value> {
    if ids.is_empty() {
        return Ok(json!({}));
    }
    let conjunto: HashSet<String> = ids.iter().cloned().collect();
    // Lista como literal de arreglo de Postgres (los ids son cuid/uuid: sin comillas ni comas).
    let lista = format!("{{{}}}", ids.iter().map(|i| format!("\"{}\"", i.replace('"', ""))).collect::<Vec<_>>().join(","));
    let filas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'dep', "dependsOnTaskId")), '[]'::jsonb) FROM "BacklogItem" WHERE id = ANY($1::text[])"#, &[B::T(lista)]).await.map_err(|e| e.to_string())?;
    let mut padre: HashMap<String, Option<String>> = HashMap::new();
    for f in filas.as_array().cloned().unwrap_or_default() {
        padre.insert(f["id"].as_str().unwrap_or("").to_string(), f["dep"].as_str().map(String::from));
    }
    // Solo cuenta el padre que está DENTRO del conjunto; un ciclo se corta tratando la tarea como raíz.
    let mut efectivo: HashMap<String, Option<String>> = HashMap::new();
    for id in &ids {
        let p = padre.get(id).cloned().flatten().filter(|p| conjunto.contains(p));
        efectivo.insert(id.clone(), p);
    }
    for id in &ids {
        let mut visto: HashSet<String> = HashSet::from([id.clone()]);
        let mut actual = efectivo.get(id).cloned().flatten();
        while let Some(a) = actual {
            if !visto.insert(a.clone()) {
                efectivo.insert(id.clone(), None);
                break;
            }
            actual = efectivo.get(&a).cloned().flatten();
        }
    }
    let mut emisores: HashMap<String, watch::Sender<Option<Resultado>>> = HashMap::new();
    let mut receptores: HashMap<String, watch::Receiver<Option<Resultado>>> = HashMap::new();
    for id in &ids {
        let (tx, rx) = watch::channel(None);
        emisores.insert(id.clone(), tx);
        receptores.insert(id.clone(), rx);
    }
    let mut tareas = tokio::task::JoinSet::new();
    for id in &ids {
        let rx_padre = efectivo.get(id).cloned().flatten().and_then(|p| receptores.get(&p).cloned());
        let tx = emisores.remove(id);
        let (st2, id2) = (st.clone(), id.clone());
        tareas.spawn(async move {
            let r = nodo(st2, id2.clone(), rx_padre).await;
            if let Some(tx) = tx {
                let _ = tx.send(Some(r.clone()));
            }
            (id2, r)
        });
    }
    let mut resultados = serde_json::Map::new();
    while let Some(r) = tareas.join_next().await {
        if let Ok((id, res)) = r {
            if res.ok {
                resultados.insert(id, Value::String(res.texto));
            }
        }
    }
    Ok(Value::Object(resultados))
}
