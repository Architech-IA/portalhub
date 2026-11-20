//! Memoria compartida del proyecto (MASD-0026-0002): notas de traspaso, registro de decisiones y «preguntar al proyecto».
//!
//! - TRASPASO: al dejar un proyecto o cambiar de turno, una persona deja una nota en la memoria, bajo «## Traspasos» y FUERA del
//!   bloque del estado vivo (que se regenera solo), con su fecha y autor, la más reciente arriba.
//! - DECISIONES: lista de lo que se decidió (decisiones de actas aceptadas y propuestas de cambio aceptadas), con quién, cuándo y su cita.
//! - PREGUNTAR: busca en las fuentes, ítems de actas, memoria, documentos, notas, PRD, diseño y casos de operación, y la IA responde
//!   SOLO con esos fragmentos, citándolos. Si no hay respaldo, lo dice.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    routes::solucion_hub::nombre_de,
    session::Session,
    state::AppState,
    util::{fetch_json, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proyectos/{id}/traspaso", post(traspaso))
        .route("/api/proyectos/{id}/decisiones", get(decisiones))
        .route("/api/proyectos/{id}/preguntar", post(preguntar))
}

const ENCABEZADO: &str = "## Traspasos";

/// Añade una entrada al principio de la sección «## Traspasos» (la crea al final si no existe).
pub(crate) fn con_traspaso(contenido: &str, entrada: &str) -> String {
    match contenido.find(ENCABEZADO) {
        Some(pos) => {
            let fin_linea = contenido[pos..].find('\n').map(|i| pos + i).unwrap_or(contenido.len());
            let (cabeza, cola) = contenido.split_at(fin_linea);
            let resto = cola.trim_start_matches('\n');
            if resto.is_empty() { format!("{cabeza}\n\n{entrada}\n") } else { format!("{cabeza}\n\n{entrada}\n{resto}") }
        }
        None => {
            if contenido.trim().is_empty() { format!("{ENCABEZADO}\n\n{entrada}\n") } else { format!("{}\n\n{ENCABEZADO}\n\n{entrada}\n", contenido.trim_end()) }
        }
    }
}

/// Palabras útiles de una pregunta para buscar (minúsculas, de 4 o más letras, sin repetir, hasta 8).
pub(crate) fn terminos(pregunta: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for w in pregunta.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if w.chars().count() >= 4 && !out.iter().any(|x| x == w) {
            out.push(w.to_string());
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn exigir(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

async fn traspaso(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let mensaje = b["mensaje"].as_str().map(str::trim).unwrap_or("");
    if mensaje.is_empty() {
        return Err(ApiError::bad_request("Escribe la nota de traspaso"));
    }
    if mensaje.chars().count() > 2000 {
        return Err(ApiError::bad_request("La nota no puede superar los 2.000 caracteres"));
    }
    let existe = fetch_json(&st.pool, r#"SELECT to_jsonb(COUNT(*)) FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    if existe.as_i64().unwrap_or(0) == 0 {
        return Err(ApiError::not_found("No encontrado"));
    }
    let quien = nombre_de(&sesion);
    let para = b["para"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(|p| format!(" → para {p}")).unwrap_or_default();
    let fecha = crate::util::fetch_text_opt(&st.pool, r#"SELECT to_char(NOW() AT TIME ZONE 'America/Bogota', 'DD/MM/YYYY HH24:MI')"#, &[]).await.ok().flatten().unwrap_or_default();
    let entrada = format!("- **{fecha} · {quien}**{para}: {}", mensaje.replace('\n', " "));
    let fallo = |e: sqlx::Error| {
        tracing::error!("[traspaso] {e}");
        ApiError::internal("No se pudo guardar la nota")
    };
    let mut tx = st.pool.begin().await.map_err(fallo)?;
    let actual: Option<(i32, String)> = sqlx::query_as(r#"SELECT version, contenido FROM "ProyectoMemoria" WHERE "solucionId" = $1 FOR UPDATE"#).bind(&id).fetch_optional(&mut *tx).await.map_err(fallo)?;
    let (version, contenido) = actual.map(|a| (a.0 as i64, a.1)).unwrap_or((0, String::new()));
    let nuevo = con_traspaso(&contenido, &entrada);
    if nuevo.chars().count() > 30_000 {
        return Err(ApiError::bad_request("La memoria superaría los 30.000 caracteres: reduce el texto escrito a mano"));
    }
    let v = version + 1;
    sqlx::query(r#"INSERT INTO "ProyectoMemoriaVersion" ("solucionId", version, contenido, origen, autor, nota) VALUES ($1, $2::int, $3, 'MANUAL', $4, 'Nota de traspaso')"#)
        .bind(&id)
        .bind(v)
        .bind(&nuevo)
        .bind(&quien)
        .execute(&mut *tx)
        .await
        .map_err(fallo)?;
    sqlx::query(
        r#"INSERT INTO "ProyectoMemoria" ("solucionId", contenido, version, "actualizadoPor", "updatedAt") VALUES ($1, $2, $3::int, $4, NOW())
           ON CONFLICT ("solucionId") DO UPDATE SET contenido = $2, version = $3::int, "actualizadoPor" = $4, "updatedAt" = NOW()"#,
    )
    .bind(&id)
    .bind(&nuevo)
    .bind(v)
    .bind(&quien)
    .execute(&mut *tx)
    .await
    .map_err(fallo)?;
    tx.commit().await.map_err(fallo)?;
    Ok(Json(json!({ "ok": true, "version": v })))
}

async fn decisiones(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir(&sesion)?;
    let v = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(t.x ORDER BY t.fecha DESC NULLS LAST), '[]'::jsonb) FROM (
             SELECT jsonb_build_object('origen', 'ACTA', 'texto', a.texto, 'cita', a.cita, 'minuto', a.minuto, 'fuente', f.titulo, 'destino', a."destinoArtefacto", 'por', a."decididoPor", 'fecha', a."decididoEn") AS x, a."decididoEn" AS fecha
               FROM "ActaItem" a JOIN "FuenteProyecto" f ON f.id = a."fuenteId" WHERE a."solucionId" = $1 AND a.tipo = 'DECISION' AND a.estado = 'ACEPTADO'
             UNION ALL
             SELECT jsonb_build_object('origen', p.origen, 'texto', p.titulo, 'cita', left(p.detalle, 300), 'fuente', NULL::text, 'destino', p.clave, 'por', p."decididoPor", 'fecha', p."decididoEn"), p."decididoEn"
               FROM "ArtefactoPropuesta" p WHERE p."solucionId" = $1 AND p.estado = 'ACEPTADA' AND p.origen IN ('TAREA', 'REVISOR', 'OPERACION')
           ) t"#,
        &[B::T(id)],
    )
    .await?;
    Ok(Json(json!({ "decisiones": v })))
}

/// (etiqueta, SQL) por cada lugar donde buscar; todas devuelven un arreglo de {tipo, titulo, id, texto, rank}.
const BUSQUEDAS: [(&str, &str); 8] = [
    ("fuente", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'fuente', 'titulo', f.titulo, 'id', f.id, 'texto', ts_headline('spanish', f.contenido, q, 'MaxWords=70,MinWords=25,MaxFragments=2'), 'rank', ts_rank(to_tsvector('spanish', f.contenido), q)) AS x
        FROM "FuenteProyecto" f, to_tsquery('spanish', $2) q WHERE f."solucionId" = $1 AND to_tsvector('spanish', f.contenido) @@ q ORDER BY ts_rank(to_tsvector('spanish', f.contenido), q) DESC LIMIT 3) s"#),
    ("item de acta", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'ítem de acta', 'titulo', a.tipo || ' (' || a.estado || ')', 'id', a.id, 'texto', a.texto || COALESCE(' — «' || a.cita || '»', ''), 'rank', ts_rank(to_tsvector('spanish', a.texto), q)) AS x
        FROM "ActaItem" a, to_tsquery('spanish', $2) q WHERE a."solucionId" = $1 AND to_tsvector('spanish', a.texto) @@ q ORDER BY ts_rank(to_tsvector('spanish', a.texto), q) DESC LIMIT 3) s"#),
    ("memoria", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'memoria', 'titulo', 'Memoria del proyecto', 'id', m."solucionId", 'texto', ts_headline('spanish', m.contenido, q, 'MaxWords=70,MinWords=25,MaxFragments=2'), 'rank', ts_rank(to_tsvector('spanish', m.contenido), q)) AS x
        FROM "ProyectoMemoria" m, to_tsquery('spanish', $2) q WHERE m."solucionId" = $1 AND to_tsvector('spanish', m.contenido) @@ q) s"#),
    ("documento", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'documento', 'titulo', d.titulo, 'id', d.id, 'texto', ts_headline('spanish', d.contenido, q, 'MaxWords=70,MinWords=25,MaxFragments=2'), 'rank', ts_rank(to_tsvector('spanish', d.contenido), q)) AS x
        FROM "ProyectoDocumento" d, to_tsquery('spanish', $2) q WHERE d."solucionId" = $1 AND to_tsvector('spanish', d.contenido) @@ q ORDER BY ts_rank(to_tsvector('spanish', d.contenido), q) DESC LIMIT 3) s"#),
    ("nota de fase", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'nota de fase', 'titulo', 'Nota de la fase ' || n.fase, 'id', n.id, 'texto', ts_headline('spanish', n.contenido, q, 'MaxWords=70,MinWords=25,MaxFragments=2'), 'rank', ts_rank(to_tsvector('spanish', n.contenido), q)) AS x
        FROM "ProyectoNota" n, to_tsquery('spanish', $2) q WHERE n."solucionId" = $1 AND to_tsvector('spanish', n.contenido) @@ q ORDER BY ts_rank(to_tsvector('spanish', n.contenido), q) DESC LIMIT 3) s"#),
    ("PRD", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'PRD', 'titulo', 'PRD', 'id', so.id, 'texto', ts_headline('spanish', so.prd, q, 'MaxWords=60,MinWords=20,MaxFragments=2'), 'rank', ts_rank(to_tsvector('spanish', so.prd), q)) AS x
        FROM "Solucion" so, to_tsquery('spanish', $2) q WHERE so.id = $1 AND so.prd IS NOT NULL AND to_tsvector('spanish', so.prd) @@ q) s"#),
    ("diseño", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'diseño', 'titulo', 'Diseño técnico', 'id', so.id, 'texto', ts_headline('spanish', so."disenoTecnico", q, 'MaxWords=60,MinWords=20,MaxFragments=2'), 'rank', ts_rank(to_tsvector('spanish', so."disenoTecnico"), q)) AS x
        FROM "Solucion" so, to_tsquery('spanish', $2) q WHERE so.id = $1 AND so."disenoTecnico" IS NOT NULL AND to_tsvector('spanish', so."disenoTecnico") @@ q) s"#),
    ("caso", r#"SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) FROM (SELECT jsonb_build_object('tipo', 'caso de operación', 'titulo', c.codigo || ' ' || c.titulo, 'id', c.id, 'texto', c.titulo || '. ' || c.descripcion, 'rank', ts_rank(to_tsvector('spanish', c.titulo || ' ' || c.descripcion), q)) AS x
        FROM "CasoOperacion" c, to_tsquery('spanish', $2) q WHERE c."solucionId" = $1 AND to_tsvector('spanish', c.titulo || ' ' || c.descripcion) @@ q ORDER BY 1 LIMIT 3) s"#),
];

async fn preguntar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let pregunta = b["pregunta"].as_str().map(str::trim).unwrap_or("");
    if pregunta.is_empty() || pregunta.chars().count() > 500 {
        return Err(ApiError::bad_request("Escribe una pregunta de hasta 500 caracteres"));
    }
    let t = terminos(pregunta);
    if t.is_empty() {
        return Err(ApiError::bad_request("Escribe una pregunta más concreta (con alguna palabra clave)"));
    }
    let tsq = t.join(" | ");
    let mut frag: Vec<Value> = vec![];
    for (_, sql) in BUSQUEDAS {
        if let Ok(v) = fetch_json(&st.pool, sql, &[B::T(id.clone()), B::T(tsq.clone())]).await {
            frag.extend(v.as_array().cloned().unwrap_or_default());
        }
    }
    frag.sort_by(|a, b| b["rank"].as_f64().unwrap_or(0.0).partial_cmp(&a["rank"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
    frag.truncate(10);
    if frag.is_empty() {
        return Ok(Json(json!({ "respuesta": "No encontré respaldo para esto en las fuentes, actas, memoria, documentos, PRD ni diseño del proyecto.", "citas": [], "sinRespaldo": true })));
    }
    let mut contexto = String::new();
    for (i, f) in frag.iter().enumerate() {
        let texto: String = f["texto"].as_str().unwrap_or("").replace('\n', " ").chars().take(700).collect();
        contexto.push_str(&format!("[{}] ({} «{}») {}\n", i + 1, f["tipo"].as_str().unwrap_or(""), f["titulo"].as_str().unwrap_or(""), texto));
    }
    let sistema = "Responde la pregunta usando SOLO los fragmentos numerados que recibes. Cita cada afirmación con su número entre corchetes, por ejemplo [2]. Si los fragmentos no alcanzan para responder, dilo claramente y no inventes nada. Responde en español, breve y concreto.";
    let usuario = format!("PREGUNTA: {pregunta}\n\nFRAGMENTOS:\n{contexto}");
    let respuesta = crate::llm::call_open_code(&st, sistema, &usuario, &format!("preguntar-{id}"), 900, 40).await.map_err(|e| ApiError::bad_request(format!("El modelo no respondió: {e}")))?;
    let citas: Vec<Value> = frag.iter().enumerate().map(|(i, f)| json!({ "n": i + 1, "tipo": f["tipo"], "titulo": f["titulo"], "id": f["id"] })).collect();
    Ok(Json(json!({ "respuesta": respuesta.trim(), "citas": citas, "sinRespaldo": false })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crea_la_seccion_de_traspasos_al_final() {
        let r = con_traspaso("Notas del equipo", "- **hoy** A");
        assert_eq!(r, "Notas del equipo\n\n## Traspasos\n\n- **hoy** A\n");
        assert_eq!(con_traspaso("", "- X"), "## Traspasos\n\n- X\n");
    }

    #[test]
    fn la_entrada_nueva_queda_arriba_de_las_anteriores() {
        let uno = con_traspaso("Intro", "- primera");
        let dos = con_traspaso(&uno, "- segunda");
        let (a, b) = (dos.find("- segunda").unwrap(), dos.find("- primera").unwrap());
        assert!(a < b && dos.starts_with("Intro"));
        assert_eq!(dos.matches("## Traspasos").count(), 1);
    }

    #[test]
    fn respeta_lo_que_viene_despues_de_la_seccion() {
        let c = "## Traspasos\n\n- vieja\n\n## Otra sección\ntexto";
        let r = con_traspaso(c, "- nueva");
        assert!(r.contains("- nueva\n- vieja") && r.ends_with("texto"));
    }

    #[test]
    fn los_terminos_son_palabras_utiles_sin_repetir() {
        let t = terminos("¿Qué se decidió sobre los recordatorios por WhatsApp? Los recordatorios");
        assert!(t.contains(&"decidió".to_string()) && t.contains(&"recordatorios".to_string()) && t.contains(&"whatsapp".to_string()));
        assert_eq!(t.iter().filter(|x| *x == "recordatorios").count(), 1);
        assert!(!t.contains(&"qué".to_string()) && !t.contains(&"los".to_string()));
        assert!(terminos("¿y eso?").is_empty());
    }
}
