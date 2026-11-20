//! Biblioteca de conocimiento y diseño (MASD-0026-0010).
//!
//! Skills (instrucciones y referencias), sistemas de diseño y plantillas de paneles que viven como ARCHIVOS en `biblioteca/` del
//! repositorio de portalhub (carpeta `BIBLIOTECA_DIR`, por defecto /opt/portalhub/biblioteca (copia legible por el usuario sin privilegios; la fuente es biblioteca/ del repo)). Cada elemento es una carpeta
//! con `FICHA.json` (nombre, cuándo usarlo, palabras clave, versión, dueño, licencia) y su contenido principal
//! (`RESUMEN.md` en las skills, `README.md` en sistemas de diseño y plantillas).
//!
//! Carga por capas para los agentes: el motor añade al contexto de una tarea de INTERFAZ solo lo relevante (hasta 2 skills,
//! 1 plantilla y el sistema de diseño elegido para el proyecto), recortado a un presupuesto. Las personas pueden ver todo
//! el detalle desde el portal. La biblioteca se edita con git (revisión por PR), no desde la pantalla.

use std::{
    fs,
    path::{Path as RutaFs, PathBuf},
};

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    routes::solucion_hub::nombre_de,
    session::Session,
    state::AppState,
    util::{exec, fetch_text_opt, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/biblioteca", get(listar))
        .route("/api/biblioteca/{tipo}/{nombre}", get(ver))
        .route("/api/proyectos/{id}/sistema-diseno", get(sistema_ver).put(sistema_fijar))
}

const TIPOS: [&str; 3] = ["skills", "sistemas-diseno", "plantillas-ui"];
const PRESUPUESTO_SKILL: usize = 3500;
const PRESUPUESTO_PLANTILLA: usize = 4500;
const PRESUPUESTO_SISTEMA: usize = 3000;

fn raiz() -> PathBuf {
    PathBuf::from(std::env::var("BIBLIOTECA_DIR").unwrap_or_else(|_| "/opt/portalhub/biblioteca".to_string()))
}

fn nombre_valido(n: &str) -> bool {
    !n.is_empty() && n.len() <= 60 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Recorta a `max` caracteres (sin partir un carácter) y avisa si cortó.
pub(crate) fn corto(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max).collect();
    format!("{t}\n[…recortado: el detalle completo está en la biblioteca]")
}

fn principal(tipo: &str) -> &'static str {
    if tipo == "skills" { "RESUMEN.md" } else { "README.md" }
}

fn leer_ficha(carpeta: &RutaFs, tipo: &str) -> Option<Value> {
    let nombre = carpeta.file_name()?.to_str()?.to_string();
    let mut f: Value = serde_json::from_str(&fs::read_to_string(carpeta.join("FICHA.json")).ok()?).ok()?;
    f["tipo"] = json!(tipo);
    f["nombre"] = json!(nombre);
    Some(f)
}

/// Todas las fichas de la biblioteca (ordenadas por tipo y nombre).
pub(crate) fn fichas() -> Vec<Value> {
    let mut out = vec![];
    for tipo in TIPOS {
        let Ok(rd) = fs::read_dir(raiz().join(tipo)) else { continue };
        let mut carpetas: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
        carpetas.sort();
        out.extend(carpetas.iter().filter_map(|c| leer_ficha(c, tipo)));
    }
    out
}

fn texto_de(tipo: &str, nombre: &str, archivo: &str) -> Option<String> {
    if !TIPOS.contains(&tipo) || !nombre_valido(nombre) {
        return None;
    }
    fs::read_to_string(raiz().join(tipo).join(nombre).join(archivo)).ok()
}

fn palabras(t: &str) -> Vec<String> {
    t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

const PALABRAS_UI: [&str; 28] = [
    "interfaz", "ui", "ux", "pantalla", "pantallas", "componente", "componentes", "diseño", "diseno", "tailwind", "css", "estilo", "estilos", "panel", "paneles", "formulario", "pagina", "página",
    "landing", "login", "dashboard", "tabla", "modal", "boton", "botón", "layout", "tipografia", "responsive",
];

/// ¿La tarea toca interfaz?
pub(crate) fn es_de_interfaz(texto: &str) -> bool {
    palabras(texto).iter().any(|w| PALABRAS_UI.contains(&w.as_str()))
}

/// Relevancia de una ficha para una tarea: palabras clave que aparecen (2 puntos cada una) y, si ya hay alguna, 1 por área coincidente.
pub(crate) fn puntuar(ficha: &Value, texto: &str, area: &str) -> usize {
    let t = texto.to_lowercase();
    let mut p = ficha["palabrasClave"].as_array().map(|a| a.iter().filter_map(|k| k.as_str()).filter(|k| t.contains(&k.to_lowercase())).count() * 2).unwrap_or(0);
    if p > 0 && ficha["roles"].as_array().map(|a| a.iter().any(|r| r.as_str().map(|r| r.eq_ignore_ascii_case(area)).unwrap_or(false))).unwrap_or(false) {
        p += 1;
    }
    p
}

fn mejores<'a>(fichas: &'a [Value], tipo: &str, texto: &str, area: &str, n: usize) -> Vec<&'a Value> {
    let mut c: Vec<(usize, &Value)> = fichas.iter().filter(|f| f["tipo"] == tipo).map(|f| (puntuar(f, texto, area), f)).filter(|(p, _)| *p >= 2).collect();
    c.sort_by(|a, b| b.0.cmp(&a.0));
    c.into_iter().take(n).map(|(_, f)| f).collect()
}

/// Bloque de contexto para una tarea de interfaz (o `None` si la tarea no toca interfaz o no hay biblioteca).
pub async fn bloque_para_tarea(st: &AppState, solucion_id: Option<&str>, titulo: &str, descripcion: &str, area: &str) -> Option<String> {
    let texto = format!("{titulo} {descripcion}");
    if !es_de_interfaz(&texto) {
        return None;
    }
    let todas = fichas();
    if todas.is_empty() {
        return None;
    }
    let mut partes: Vec<String> = vec![];
    // El sistema de diseño elegido para el proyecto manda sobre cualquier otra referencia.
    if let Some(sid) = solucion_id {
        let elegido = fetch_text_opt(&st.pool, r#"SELECT sistema FROM "ProyectoSistemaDiseno" WHERE "solucionId" = $1"#, &[B::T(sid.into())]).await.ok().flatten();
        if let Some(n) = elegido.filter(|n| nombre_valido(n)) {
            if let Some(f) = todas.iter().find(|f| f["tipo"] == "sistemas-diseno" && f["nombre"] == n.as_str()) {
                let tokens = texto_de("sistemas-diseno", &n, "sistema.json").unwrap_or_default();
                let guia = texto_de("sistemas-diseno", &n, "README.md").unwrap_or_default();
                partes.push(format!("SISTEMA DE DISEÑO DEL PROYECTO: {} (manda sobre cualquier otra referencia; no inventes colores ni fuentes fuera de estos tokens)\n{}\nTOKENS:\n{}", f["descripcion"].as_str().unwrap_or(&n), corto(&guia, PRESUPUESTO_SISTEMA / 2), corto(&tokens, PRESUPUESTO_SISTEMA)));
            }
        }
    }
    for f in mejores(&todas, "plantillas-ui", &texto, area, 1) {
        let n = f["nombre"].as_str().unwrap_or("");
        if let Some(c) = texto_de("plantillas-ui", n, "README.md") {
            partes.push(format!("PLANTILLA DE PANEL «{n}» ({}): úsala como punto de partida, adaptada a los datos de la tarea\n{}", f["descripcion"].as_str().unwrap_or(""), corto(&c, PRESUPUESTO_PLANTILLA)));
        }
    }
    for f in mejores(&todas, "skills", &texto, area, 2) {
        let n = f["nombre"].as_str().unwrap_or("");
        if let Some(c) = texto_de("skills", n, "RESUMEN.md") {
            partes.push(format!("SKILL «{n}» — {}\n{}", f["cuandoUsar"].as_str().unwrap_or(""), corto(&c, PRESUPUESTO_SKILL)));
        }
    }
    if partes.is_empty() {
        return None;
    }
    Some(format!("=== BIBLIOTECA DE DISEÑO (solo si la tarea toca interfaz) ===\n{}", partes.join("\n\n")))
}

// ═══════════════════════════ Rutas ═══════════════════════════
fn exigir_lectura(sesion: &Session) -> Result<(), ApiError> {
    if !sesion.is_service {
        sesion.require_user()?;
    }
    Ok(())
}

async fn listar(sesion: Session) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion)?;
    Ok(Json(json!({ "raiz": raiz().to_string_lossy(), "elementos": fichas() })))
}

async fn ver(sesion: Session, Path((tipo, nombre)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion)?;
    if !TIPOS.contains(&tipo.as_str()) || !nombre_valido(&nombre) {
        return Err(ApiError::bad_request("Elemento de biblioteca inválido"));
    }
    let carpeta = raiz().join(&tipo).join(&nombre);
    let ficha = leer_ficha(&carpeta, &tipo).ok_or_else(|| ApiError::not_found("No existe en la biblioteca"))?;
    // Archivos de la carpeta (un nivel de subcarpeta).
    let mut archivos: Vec<String> = vec![];
    if let Ok(rd) = fs::read_dir(&carpeta) {
        for e in rd.filter_map(|e| e.ok()) {
            let n = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() {
                if let Ok(sub) = fs::read_dir(e.path()) {
                    archivos.extend(sub.filter_map(|s| s.ok()).map(|s| format!("{n}/{}", s.file_name().to_string_lossy())));
                }
            } else {
                archivos.push(n);
            }
        }
    }
    archivos.sort();
    let pedido = q.get("archivo").cloned().unwrap_or_else(|| principal(&tipo).to_string());
    // Solo archivos listados: evita salir de la carpeta.
    if !archivos.contains(&pedido) {
        return Err(ApiError::not_found("Ese archivo no existe en el elemento"));
    }
    let contenido = fs::read_to_string(carpeta.join(&pedido)).unwrap_or_default();
    Ok(Json(json!({ "ficha": ficha, "archivo": pedido, "archivos": archivos, "contenido": corto(&contenido, 60_000) })))
}

async fn sistema_ver(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    exigir_lectura(&sesion)?;
    let elegido = fetch_text_opt(&st.pool, r#"SELECT sistema FROM "ProyectoSistemaDiseno" WHERE "solucionId" = $1"#, &[B::T(id)]).await?;
    let disponibles: Vec<Value> = fichas().into_iter().filter(|f| f["tipo"] == "sistemas-diseno").map(|f| json!({ "nombre": f["nombre"], "descripcion": f["descripcion"] })).collect();
    Ok(Json(json!({ "sistema": elegido, "disponibles": disponibles })))
}

async fn sistema_fijar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user()?;
    let nombre = body["sistema"].as_str().map(str::trim).unwrap_or("");
    if nombre.is_empty() {
        exec(&st.pool, r#"DELETE FROM "ProyectoSistemaDiseno" WHERE "solucionId" = $1"#, &[B::T(id)]).await?;
        return Ok(Json(json!({ "ok": true, "sistema": Value::Null })));
    }
    if !fichas().iter().any(|f| f["tipo"] == "sistemas-diseno" && f["nombre"] == nombre) {
        return Err(ApiError::bad_request("Ese sistema de diseño no existe en la biblioteca"));
    }
    exec(
        &st.pool,
        r#"INSERT INTO "ProyectoSistemaDiseno" ("solucionId", sistema, "actualizadoPor", "updatedAt") VALUES ($1, $2, $3, NOW())
           ON CONFLICT ("solucionId") DO UPDATE SET sistema = EXCLUDED.sistema, "actualizadoPor" = EXCLUDED."actualizadoPor", "updatedAt" = NOW()"#,
        &[B::T(id), B::T(nombre.into()), B::T(nombre_de(&sesion))],
    )
    .await?;
    Ok(Json(json!({ "ok": true, "sistema": nombre })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_tareas_de_interfaz() {
        assert!(es_de_interfaz("Crear la pantalla de login con Tailwind"));
        assert!(es_de_interfaz("Panel de métricas del dashboard"));
        assert!(!es_de_interfaz("Migrar la base de datos y crear el endpoint de pagos"));
        assert!(!es_de_interfaz("Construir el cliente del servicio"), "«ui» dentro de «construir» no cuenta");
    }

    #[test]
    fn puntua_por_palabras_clave_y_area() {
        let f = json!({ "palabrasClave": ["tabla", "filtros"], "roles": ["Development"] });
        assert_eq!(puntuar(&f, "Una tabla con filtros", "Quality & Testing"), 4);
        assert_eq!(puntuar(&f, "Una tabla con filtros", "development"), 5);
        assert_eq!(puntuar(&f, "Algo sin relación", "Development"), 0, "el área sola no basta");
    }

    #[test]
    fn corto_no_parte_caracteres_y_avisa() {
        assert_eq!(corto("abc", 5), "abc");
        let c = corto("ñññññññ", 3);
        assert!(c.starts_with("ñññ\n") && c.contains("recortado"));
    }

    #[test]
    fn nombres_validos() {
        assert!(nombre_valido("portal-oscuro"));
        assert!(!nombre_valido("../x"));
        assert!(!nombre_valido("A b"));
    }
}
