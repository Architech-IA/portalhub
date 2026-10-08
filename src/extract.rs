//! Extracción de texto de adjuntos guardados en base64 (hub de leads, documentos de propuestas)
//! para dárselo como contexto al asistente de IA. Puerto de `src/lib/extraerTexto.ts`.
//! Soporta PDF, Word (.docx), PowerPoint (.pptx), Excel (.xlsx, solo textos) y texto plano.
//! Imágenes y binarios no se leen. Hay caché en memoria por archivo (clave = id + tamaño).

use std::{
    collections::{HashMap, VecDeque},
    io::{Cursor, Read},
    sync::{LazyLock, Mutex},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use regex::Regex;

const EXT_TEXTO: [&str; 9] = [".txt", ".md", ".csv", ".json", ".log", ".html", ".htm", ".xml", ".rtf"];
const EXT_OFFICE: [&str; 4] = [".pdf", ".docx", ".pptx", ".xlsx"];
const MAX_BYTES: usize = 6 * 1024 * 1024;
const TIEMPO_MAX: Duration = Duration::from_secs(12);
const MAX_CACHE: usize = 200;
const MAX_TEXTO: usize = 20_000;

struct Cache {
    mapa: HashMap<String, String>,
    orden: VecDeque<String>,
}
static CACHE: LazyLock<Mutex<Cache>> =
    LazyLock::new(|| Mutex::new(Cache { mapa: HashMap::new(), orden: VecDeque::new() }));

fn extension(nombre: &str) -> String {
    match nombre.rfind('.') {
        Some(i) if i > 0 => nombre[i..].to_lowercase(),
        _ => String::new(),
    }
}

pub fn es_legible(nombre: &str) -> bool {
    let e = extension(nombre);
    EXT_OFFICE.contains(&e.as_str()) || EXT_TEXTO.contains(&e.as_str())
}

/// `None` = no está en caché; `Some(None)` = ya se intentó y no se pudo leer / estaba vacío.
pub fn texto_en_cache(clave: &str) -> Option<Option<String>> {
    let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    c.mapa.get(clave).map(|v| if v.is_empty() { None } else { Some(v.clone()) })
}

fn guardar_en_cache(clave: &str, texto: &str) {
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if c.mapa.len() >= MAX_CACHE {
        if let Some(viejo) = c.orden.pop_front() {
            c.mapa.remove(&viejo);
        }
    }
    if c.mapa.insert(clave.to_string(), texto.to_string()).is_none() {
        c.orden.push_back(clave.to_string());
    }
}

fn decodificar(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn base64_a_bytes(b64: &str) -> Option<Vec<u8>> {
    let crudo = if b64.starts_with("data:") && b64.contains(',') { &b64[b64.find(',')? + 1..] } else { b64 };
    let limpio: String = crudo.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD.decode(limpio.trim_end_matches('=')).ok().or_else(|| STANDARD.decode(&limpio).ok())
}

static RE_A_T: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<a:t>([^<]*)</a:t>").expect("re"));
static RE_T: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<t[^>]*>([^<]*)</t>").expect("re"));
static RE_W: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<w:t(?:\s[^>]*)?>([^<]*)</w:t>|<w:tab\s*/>|<w:br\s*/?>").expect("re"));
static RE_SLIDE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ppt/slides/slide(\d+)\.xml$").expect("re"));
static RE_SCRIPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<script[\s\S]*?</script>").expect("re"));
static RE_STYLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<style[\s\S]*?</style>").expect("re"));
static RE_TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("re"));
static RE_ESP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+").expect("re"));
static RE_SALTOS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").expect("re"));

fn leer_zip(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, nombre: &str) -> Option<String> {
    let mut f = zip.by_name(nombre).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn extraer(bytes: &[u8], ext: &str) -> Result<String, String> {
    match ext {
        ".pdf" => pdf_extract::extract_text_from_mem(bytes).map_err(|e| e.to_string()),
        ".docx" => {
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            let xml = leer_zip(&mut zip, "word/document.xml").unwrap_or_default();
            // Un bloque por párrafo; dentro, los runs de texto, tabulaciones y saltos.
            let parrafos: Vec<String> = xml
                .split("</w:p>")
                .map(|p| {
                    let mut t = String::new();
                    for c in RE_W.captures_iter(p) {
                        if let Some(g) = c.get(1) {
                            t.push_str(&decodificar(g.as_str()));
                        } else if c[0].starts_with("<w:tab") {
                            t.push('\t');
                        } else {
                            t.push('\n');
                        }
                    }
                    t
                })
                .filter(|t| !t.trim().is_empty())
                .collect();
            Ok(parrafos.join("\n\n"))
        }
        ".pptx" => {
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            let mut slides: Vec<(u32, String)> = zip
                .file_names()
                .filter_map(|n| RE_SLIDE.captures(n).map(|c| (c[1].parse::<u32>().unwrap_or(0), n.to_string())))
                .collect();
            slides.sort();
            let mut partes = Vec::new();
            for (n, nombre) in slides {
                let xml = leer_zip(&mut zip, &nombre).unwrap_or_default();
                let textos: Vec<String> = RE_A_T
                    .captures_iter(&xml)
                    .map(|m| decodificar(&m[1]).trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                if !textos.is_empty() {
                    partes.push(format!("Diapositiva {n}: {}", textos.join(" ")));
                }
            }
            Ok(partes.join("\n"))
        }
        ".xlsx" => {
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            let Some(xml) = leer_zip(&mut zip, "xl/sharedStrings.xml") else { return Ok(String::new()) };
            Ok(RE_T
                .captures_iter(&xml)
                .map(|m| decodificar(&m[1]).trim().to_string())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" | "))
        }
        _ => {
            let mut t = String::from_utf8_lossy(bytes).to_string();
            if matches!(ext, ".html" | ".htm" | ".xml") {
                t = RE_SCRIPT.replace_all(&t, "").to_string();
                t = RE_STYLE.replace_all(&t, "").to_string();
                t = decodificar(&RE_TAGS.replace_all(&t, " "));
            }
            Ok(t)
        }
    }
}

fn normalizar(t: &str) -> String {
    let t = t.replace('\r', "");
    let t = RE_ESP.replace_all(&t, " ");
    let t = RE_SALTOS.replace_all(&t, "\n\n");
    t.trim().chars().take(MAX_TEXTO).collect()
}

/// Devuelve el texto (normalizado) o None si el formato no se puede leer / falla / es muy grande.
pub async fn texto_de_archivo(clave: &str, nombre: &str, base64: &str) -> Option<String> {
    let ext = extension(nombre);
    if !(EXT_OFFICE.contains(&ext.as_str()) || EXT_TEXTO.contains(&ext.as_str())) {
        return None;
    }
    if let Some(hit) = texto_en_cache(clave) {
        return hit;
    }
    let bytes = base64_a_bytes(base64)?;
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let ext2 = ext.clone();
    let trabajo = tokio::task::spawn_blocking(move || extraer(&bytes, &ext2));
    let resultado = match tokio::time::timeout(TIEMPO_MAX, trabajo).await {
        Ok(Ok(Ok(t))) => t,
        Ok(Ok(Err(e))) => {
            tracing::error!("[extraerTexto] {nombre}: {e}");
            return None;
        }
        Ok(Err(e)) => {
            tracing::error!("[extraerTexto] {nombre}: {e}");
            return None;
        }
        Err(_) => {
            tracing::error!("[extraerTexto] {nombre}: tiempo agotado leyendo el archivo");
            return None;
        }
    };
    let limpio = normalizar(&resultado);
    guardar_en_cache(clave, &limpio);
    if limpio.is_empty() { None } else { Some(limpio) }
}
