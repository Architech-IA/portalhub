//! Revisión de diseño con reglas fijas para tareas de interfaz (MASD-0026-0010).
//!
//! Es una revisión ESTÁTICA del código que escribió la tarea, no una captura de pantalla: atrapa lo mecánico que dañaría
//! la accesibilidad o se saldría del sistema de diseño del proyecto (imágenes sin texto alternativo, foco invisible, zoom
//! bloqueado, botones de solo icono sin nombre, elementos clicables que el teclado no alcanza, colores fuera de los tokens).
//! No bloquea: sus hallazgos se anotan en el checklist de la tarea. Las capturas con navegador siguen pendientes.

use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
    sync::LazyLock,
};

use regex::Regex;

use crate::state::AppState;

#[derive(Debug, Clone, PartialEq)]
pub struct Hallazgo {
    pub regla: &'static str,
    pub archivo: String,
    pub detalle: String,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("regex")
}

static IMG: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<(img|Image)\b[^>]*?/?>"));
static OUTLINE_APAGADO: LazyLock<Regex> = LazyLock::new(|| re(r"outline\s*:\s*(none|0)\b|outline-none"));
static FOCO_PROPIO: LazyLock<Regex> = LazyLock::new(|| re(r"focus-visible|focus:ring|focus:outline|focus-within|:focus\s*\{|ring-"));
static ZOOM: LazyLock<Regex> = LazyLock::new(|| re(r"user-scalable\s*=\s*(no|0)|maximum-scale\s*=\s*1(\.0)?\b"));
static BOTON_ICONO: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)(<button\b[^>]*>)\s*<(svg|[A-Z][A-Za-z0-9]*)\b[^>]*/>\s*</button>"));
static DIV_CLICABLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<div\b[^>]*\bonClick=[^>]*>"));
static VH: LazyLock<Regex> = LazyLock::new(|| re(r"\b100vh\b"));
static HEX: LazyLock<Regex> = LazyLock::new(|| re(r"#[0-9a-fA-F]{6}\b"));

fn es_interfaz(ruta: &str) -> bool {
    let sin_prueba = !(ruta.contains("node_modules") || ruta.contains(".next/") || ruta.contains(".test.") || ruta.contains(".spec.") || ruta.contains("/__tests__/"));
    sin_prueba && [".tsx", ".jsx", ".html", ".css"].iter().any(|e| ruta.ends_with(e))
}

/// Revisa un archivo de interfaz. `tokens` son los colores (#rrggbb en minúscula) del sistema de diseño del proyecto; vacío = no se revisan colores.
pub fn revisar_archivo(ruta: &str, contenido: &str, tokens: &HashSet<String>) -> Vec<Hallazgo> {
    let mut h = vec![];
    if !es_interfaz(ruta) {
        return h;
    }
    let mut poner = |regla: &'static str, detalle: String| h.push(Hallazgo { regla, archivo: ruta.to_string(), detalle });

    let sin_alt = IMG.find_iter(contenido).filter(|m| !m.as_str().contains("alt=")).count();
    if sin_alt > 0 {
        poner("imagen-sin-alt", format!("{sin_alt} imagen(es) sin texto alternativo (alt). Describe la imagen o usa alt=\"\" si es decorativa."));
    }
    if OUTLINE_APAGADO.is_match(contenido) && !FOCO_PROPIO.is_match(contenido) {
        poner("foco-invisible", "Se quita el contorno de foco (outline) y el archivo no define otro estilo de foco. Quien navega con teclado deja de ver dónde está.".into());
    }
    if ZOOM.is_match(contenido) {
        poner("zoom-bloqueado", "Se bloquea el zoom (user-scalable=no o maximum-scale=1). Es una barrera de accesibilidad.".into());
    }
    let solo_icono = BOTON_ICONO
        .captures_iter(contenido)
        .filter(|c| {
            let apertura = c.get(1).map(|m| m.as_str()).unwrap_or("");
            !(apertura.contains("aria-label") || apertura.contains("aria-labelledby") || apertura.contains("title="))
        })
        .count();
    if solo_icono > 0 {
        poner("boton-sin-nombre", format!("{solo_icono} botón(es) con solo un icono y sin aria-label ni title: un lector de pantalla no sabe qué hacen."));
    }
    let div_clicable = DIV_CLICABLE.find_iter(contenido).filter(|m| !(m.as_str().contains("role=") || m.as_str().contains("onKeyDown") || m.as_str().contains("tabIndex"))).count();
    if div_clicable > 0 {
        poner("clic-sin-teclado", format!("{div_clicable} <div> con onClick sin role ni teclado. Usa un <button> o añade role, tabIndex y onKeyDown."));
    }
    if VH.is_match(contenido) {
        poner("altura-100vh", "Se usa 100vh: en móvil la barra del navegador lo desborda. Prefiere 100dvh o min-h-dvh.".into());
    }
    if !tokens.is_empty() {
        let fuera: BTreeSet<String> = HEX.find_iter(contenido).map(|m| m.as_str().to_lowercase()).filter(|c| !tokens.contains(c)).collect();
        if !fuera.is_empty() {
            let ejemplos: Vec<String> = fuera.iter().take(5).cloned().collect();
            poner("color-fuera-del-sistema", format!("{} color(es) fuera de los tokens del sistema de diseño del proyecto: {}. Usa las variables o los colores definidos en sistema.json.", fuera.len(), ejemplos.join(", ")));
        }
    }
    h
}

/// Colores (#rrggbb en minúscula) de un texto cualquiera (por ejemplo un sistema.json).
pub fn hex_de(texto: &str) -> HashSet<String> {
    HEX.find_iter(texto).map(|m| m.as_str().to_lowercase()).collect()
}

/// Revisa los archivos de interfaz que escribió la tarea.
pub async fn revisar_tarea(st: &AppState, solucion_id: Option<&str>, raiz: &Path, archivos: &[String]) -> Vec<Hallazgo> {
    let tokens = match solucion_id {
        Some(sid) => crate::routes::biblioteca::hex_del_sistema_del_proyecto(st, sid).await,
        None => HashSet::new(),
    };
    let mut out = vec![];
    for a in archivos {
        let ruta = a.trim_start_matches("./");
        if !es_interfaz(ruta) {
            continue;
        }
        if let Ok(c) = tokio::fs::read_to_string(raiz.join(ruta)).await {
            out.extend(revisar_archivo(ruta, &c, &tokens));
        }
    }
    out
}

pub fn resumir(h: &[Hallazgo]) -> String {
    h.iter().take(8).map(|x| format!("{} · {} — {}", x.regla, x.archivo, x.detalle)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reglas(h: &[Hallazgo]) -> Vec<&str> {
        h.iter().map(|x| x.regla).collect()
    }

    #[test]
    fn detecta_los_problemas_de_accesibilidad_mas_comunes() {
        let c = "<img src=\"/a.png\" />\n<button onClick={x}><X size={12} /></button>\n<div onClick={y}>abrir</div>\n<style>a:focus { outline: none }</style>\n";
        let r = revisar_archivo("app/page.tsx", c, &HashSet::new());
        let r = reglas(&r);
        assert!(r.contains(&"imagen-sin-alt") && r.contains(&"boton-sin-nombre") && r.contains(&"clic-sin-teclado"));
    }

    #[test]
    fn no_marca_lo_que_esta_bien_hecho() {
        let c = "<img src=\"/a.png\" alt=\"Logo\" />\n<button aria-label=\"Cerrar\"><X size={12} /></button>\n<div role=\"button\" tabIndex={0} onClick={y} onKeyDown={z}>abrir</div>\n";
        assert!(revisar_archivo("app/page.tsx", c, &HashSet::new()).is_empty());
    }

    #[test]
    fn el_foco_quitado_solo_cuenta_si_no_hay_otro_estilo() {
        assert!(reglas(&revisar_archivo("a.css", "button { outline: none }", &HashSet::new())).contains(&"foco-invisible"));
        assert!(revisar_archivo("a.css", "button { outline: none } button:focus-visible { box-shadow: 0 0 0 2px red }", &HashSet::new()).is_empty());
    }

    #[test]
    fn marca_colores_fuera_de_los_tokens_solo_si_hay_sistema() {
        let tokens: HashSet<String> = hex_de("{\"acento\":\"#6366F1\"}");
        let c = "const a = '#6366f1'; const b = '#ff00aa';";
        let r = revisar_archivo("x.tsx", c, &tokens);
        assert_eq!(reglas(&r), vec!["color-fuera-del-sistema"]);
        assert!(r[0].detalle.contains("#ff00aa") && !r[0].detalle.contains("#6366f1"));
        assert!(revisar_archivo("x.tsx", c, &HashSet::new()).is_empty(), "sin sistema de diseño no se revisan colores");
    }

    #[test]
    fn ignora_pruebas_y_archivos_que_no_son_de_interfaz() {
        assert!(revisar_archivo("x.test.tsx", "<img src=\"a\" />", &HashSet::new()).is_empty());
        assert!(revisar_archivo("lib/x.ts", "<img src=\"a\" />", &HashSet::new()).is_empty());
    }
}
