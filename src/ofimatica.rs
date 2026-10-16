//! Archivos de oficina generados por el motor (Word, Excel, PDF) sin dependencias nuevas:
//! DOCX y XLSX son zips de XML (el crate `zip` ya está) y el PDF simple se escribe a mano (Helvetica, WinAnsi).
//! Los documentos con tablas, diagramas e imágenes también salen como PDF de calidad: aquí se arma el HTML y lo imprime Chromium (servicio web).
//! El contenido llega como JSON que redactó la IA; aquí solo se le da forma.

use serde_json::Value;
use std::collections::HashMap;
use std::io::{Cursor, Write};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub struct Tabla {
    pub titulo: String,
    pub columnas: Vec<String>,
    pub filas: Vec<Vec<String>>,
}
pub struct Diagrama {
    pub titulo: String,
    pub nodos: Vec<(String, String)>,
    pub conexiones: Vec<(String, String, String)>,
}
pub struct Imagen {
    pub captura: String,
    pub leyenda: String,
}
pub struct Seccion {
    pub titulo: String,
    pub parrafos: Vec<String>,
    pub puntos: Vec<String>,
    pub tablas: Vec<Tabla>,
    pub diagramas: Vec<Diagrama>,
    pub imagenes: Vec<Imagen>,
}
pub struct Documento {
    pub titulo: String,
    pub secciones: Vec<Seccion>,
}
pub struct Hoja {
    pub nombre: String,
    pub columnas: Vec<String>,
    pub filas: Vec<Vec<Value>>,
}

fn texto(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn lista_de_textos(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().map(texto).filter(|t| !t.is_empty()).collect(),
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_string()],
        _ => vec![],
    }
}

fn corto(t: String, n: usize) -> String {
    t.chars().take(n).collect()
}

fn tablas_de(v: &Value) -> Vec<Tabla> {
    v.as_array()
        .map(|a| {
            a.iter()
                .take(6)
                .map(|t| {
                    let columnas: Vec<String> = lista_de_textos(&t["columnas"]).into_iter().take(8).map(|c| corto(c, 60)).collect();
                    let filas: Vec<Vec<String>> = t["filas"].as_array().map(|f| f.iter().take(40).filter_map(|r| r.as_array().map(|c| c.iter().take(8).map(|x| corto(texto(x), 240)).collect())).collect()).unwrap_or_default();
                    Tabla { titulo: corto(texto(&t["titulo"]), 120), columnas, filas }
                })
                .filter(|t| !t.columnas.is_empty() && !t.filas.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn diagramas_de(v: &Value) -> Vec<Diagrama> {
    v.as_array()
        .map(|a| {
            a.iter()
                .take(4)
                .map(|d| {
                    let nodos: Vec<(String, String)> = d["nodos"]
                        .as_array()
                        .map(|n| n.iter().take(14).filter_map(|x| { let id = texto(&x["id"]); let et = texto(&x["etiqueta"]); (!id.is_empty()).then(|| (corto(id, 40), corto(if et.is_empty() { texto(&x["id"]) } else { et }, 80))) }).collect())
                        .unwrap_or_default();
                    let conexiones: Vec<(String, String, String)> = d["conexiones"]
                        .as_array()
                        .map(|c| c.iter().take(24).map(|x| (corto(texto(&x["de"]), 40), corto(texto(&x["a"]), 40), corto(texto(&x["etiqueta"]), 40))).filter(|(a, b, _)| nodos.iter().any(|n| &n.0 == a) && nodos.iter().any(|n| &n.0 == b)).collect())
                        .unwrap_or_default();
                    Diagrama { titulo: corto(texto(&d["titulo"]), 120), nodos, conexiones }
                })
                .filter(|d| d.nodos.len() >= 2)
                .collect()
        })
        .unwrap_or_default()
}

fn imagenes_de(v: &Value) -> Vec<Imagen> {
    v.as_array()
        .map(|a| {
            a.iter()
                .take(6)
                .map(|i| Imagen { captura: texto(&i["captura"]), leyenda: corto(texto(&i["leyenda"]), 160) })
                .filter(|i| !i.captura.is_empty() && i.captura.ends_with(".png") && i.captura.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && !i.captura.contains(".."))
                .collect()
        })
        .unwrap_or_default()
}

/// `{ "titulo": "...", "secciones": [ { "titulo": "...", "parrafos": [...], "puntos": [...], "tablas": [...], "diagramas": [...], "imagenes": [...] } ] }`
pub fn documento_de(v: &Value) -> Option<Documento> {
    let titulo = texto(&v["titulo"]);
    let secciones: Vec<Seccion> = v["secciones"]
        .as_array()?
        .iter()
        .map(|s| Seccion {
            titulo: texto(&s["titulo"]),
            parrafos: lista_de_textos(&s["parrafos"]),
            puntos: lista_de_textos(&s["puntos"]),
            tablas: tablas_de(&s["tablas"]),
            diagramas: diagramas_de(&s["diagramas"]),
            imagenes: imagenes_de(&s["imagenes"]),
        })
        .filter(|s| !s.titulo.is_empty() || !s.parrafos.is_empty() || !s.puntos.is_empty() || !s.tablas.is_empty() || !s.diagramas.is_empty() || !s.imagenes.is_empty())
        .collect();
    if secciones.is_empty() {
        return None;
    }
    Some(Documento { titulo: if titulo.is_empty() { "Documento".into() } else { titulo }, secciones })
}

/// `{ "titulo": "...", "hojas": [ { "nombre": "...", "columnas": ["..."], "filas": [["...", 3]] } ] }`
pub fn hojas_de(v: &Value) -> Option<Vec<Hoja>> {
    let hojas: Vec<Hoja> = v["hojas"]
        .as_array()?
        .iter()
        .take(10)
        .map(|h| Hoja {
            nombre: texto(&h["nombre"]),
            columnas: lista_de_textos(&h["columnas"]),
            filas: h["filas"].as_array().map(|f| f.iter().take(2000).filter_map(|r| r.as_array().cloned()).collect()).unwrap_or_default(),
        })
        .filter(|h| !h.columnas.is_empty() || !h.filas.is_empty())
        .collect();
    if hojas.is_empty() {
        None
    } else {
        Some(hojas)
    }
}

fn esc(t: &str) -> String {
    t.chars()
        .filter(|c| matches!(*c, '\t' | '\n' | '\r') || (*c as u32) >= 0x20)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn empaquetar(partes: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, String> {
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let op = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (nombre, contenido) in partes {
        z.start_file(nombre, op).map_err(|e| e.to_string())?;
        z.write_all(&contenido).map_err(|e| e.to_string())?;
    }
    Ok(z.finish().map_err(|e| e.to_string())?.into_inner())
}

fn parte(n: &str, c: String) -> (String, Vec<u8>) {
    (n.to_string(), c.into_bytes())
}

const CABECERA_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;

// ── Diagramas (SVG calculado a mano: capas de izquierda a derecha) ───────────────────────────
fn lineas_de(t: &str, max: usize, tope: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut actual = String::new();
    for p in t.split_whitespace() {
        let mut p = p.to_string();
        while p.chars().count() > max {
            if !actual.is_empty() {
                out.push(std::mem::take(&mut actual));
            }
            out.push(p.chars().take(max).collect());
            p = p.chars().skip(max).collect();
        }
        if !actual.is_empty() && actual.chars().count() + 1 + p.chars().count() > max {
            out.push(std::mem::take(&mut actual));
        }
        if !actual.is_empty() {
            actual.push(' ');
        }
        actual.push_str(&p);
    }
    if !actual.is_empty() {
        out.push(actual);
    }
    if out.len() > tope {
        out.truncate(tope);
        if let Some(u) = out.last_mut() {
            *u = format!("{}…", u.chars().take(max.saturating_sub(1)).collect::<String>());
        }
    }
    out
}

/// Dibuja un diagrama de nodos y conexiones. Devuelve el SVG y su tamaño en píxeles.
pub fn svg_diagrama(d: &Diagrama) -> Option<(String, u32, u32)> {
    let n = d.nodos.len();
    if n < 2 {
        return None;
    }
    let (nw, nh, gx, gy, m) = (176i32, 56i32, 64i32, 28i32, 20i32);
    let idx = |id: &str| d.nodos.iter().position(|x| x.0 == id);
    let mut nivel = vec![0usize; n];
    for _ in 0..n {
        let mut cambio = false;
        for (a, b, _) in &d.conexiones {
            if let (Some(i), Some(j)) = (idx(a), idx(b)) {
                if i != j && nivel[j] < nivel[i] + 1 && nivel[i] + 1 < n {
                    nivel[j] = nivel[i] + 1;
                    cambio = true;
                }
            }
        }
        if !cambio {
            break;
        }
    }
    let niveles = nivel.iter().max().copied().unwrap_or(0) + 1;
    let mut columnas: Vec<Vec<usize>> = vec![vec![]; niveles];
    for (i, l) in nivel.iter().enumerate() {
        columnas[*l].push(i);
    }
    let mas_alta = columnas.iter().map(|c| c.len()).max().unwrap_or(1) as i32;
    let ancho = 2 * m + niveles as i32 * (nw + gx) - gx;
    let alto = 2 * m + mas_alta * (nh + gy) - gy;
    let mut pos = vec![(0i32, 0i32); n];
    for (l, col) in columnas.iter().enumerate() {
        let extra = (mas_alta - col.len() as i32) * (nh + gy) / 2;
        for (k, i) in col.iter().enumerate() {
            pos[*i] = (m + l as i32 * (nw + gx), m + extra + k as i32 * (nh + gy));
        }
    }
    let mut s = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{ancho}" height="{alto}" viewBox="0 0 {ancho} {alto}" font-family="Helvetica, Arial, sans-serif"><defs><marker id="f" markerWidth="9" markerHeight="9" refX="8" refY="4.5" orient="auto"><path d="M0,0 L9,4.5 L0,9 z" fill="#475569"/></marker></defs><rect width="{ancho}" height="{alto}" fill="#ffffff"/>"##
    );
    for (a, b, et) in &d.conexiones {
        let (Some(i), Some(j)) = (idx(a), idx(b)) else { continue };
        let ((xa, ya), (xb, yb)) = (pos[i], pos[j]);
        let (x1, y1, x2, y2) = if xa == xb { if ya <= yb { (xa + nw / 2, ya + nh, xb + nw / 2, yb) } else { (xa + nw / 2, ya, xb + nw / 2, yb + nh) } } else if xa < xb { (xa + nw, ya + nh / 2, xb, yb + nh / 2) } else { (xa, ya + nh / 2, xb + nw, yb + nh / 2) };
        let medio = (x1 + x2) / 2;
        s.push_str(&format!(r##"<path d="M{x1},{y1} C{medio},{y1} {medio},{y2} {x2},{y2}" fill="none" stroke="#475569" stroke-width="1.5" marker-end="url(#f)"/>"##));
        if !et.is_empty() {
            s.push_str(&format!(r##"<text x="{medio}" y="{}" font-size="10" text-anchor="middle" fill="#334155" stroke="#ffffff" stroke-width="3" paint-order="stroke">{}</text>"##, (y1 + y2) / 2 - 4, esc(et)));
        }
    }
    for (i, (_, et)) in d.nodos.iter().enumerate() {
        let (x, y) = pos[i];
        s.push_str(&format!(r##"<rect x="{x}" y="{y}" width="{nw}" height="{nh}" rx="8" fill="#eef2ff" stroke="#6366f1" stroke-width="1.5"/>"##));
        let ls = lineas_de(et, 24, 3);
        let y0 = y + nh / 2 - (ls.len() as i32 - 1) * 7 + 4;
        for (k, l) in ls.iter().enumerate() {
            s.push_str(&format!(r##"<text x="{}" y="{}" font-size="12" text-anchor="middle" fill="#1e293b">{}</text>"##, x + nw / 2, y0 + k as i32 * 14, esc(l)));
        }
    }
    s.push_str("</svg>");
    Some((s, ancho as u32, alto as u32))
}

// ── HTML (para imprimir como PDF con Chromium) ───────────────────────────────────────────────
/// `capturas`: nombre de la captura → PNG en base64.
pub fn html(d: &Documento, capturas: &HashMap<String, String>) -> String {
    let mut h = String::from(
        r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><style>
body{font-family:Helvetica,Arial,sans-serif;color:#1e293b;font-size:11pt;line-height:1.5}
h1{font-size:22pt;margin:0 0 14pt;color:#0f172a}
h2{font-size:14pt;margin:20pt 0 6pt;color:#0f172a;border-bottom:1px solid #e2e8f0;padding-bottom:3pt}
h3{font-size:11pt;margin:12pt 0 4pt;color:#334155}
p{margin:0 0 7pt}ul{margin:0 0 8pt;padding-left:18pt}li{margin-bottom:2pt}
table{border-collapse:collapse;width:100%;margin:4pt 0 10pt;font-size:9.5pt}
th{background:#f1f5f9;text-align:left;font-weight:700}
th,td{border:1px solid #cbd5e1;padding:4pt 6pt;vertical-align:top}
tr{page-break-inside:avoid}
figure{margin:8pt 0 12pt;text-align:center;page-break-inside:avoid}
figure svg,figure img{max-width:100%;height:auto}
figcaption{font-size:9pt;color:#64748b;margin-top:3pt}
</style></head><body>"#,
    );
    h.push_str(&format!("<h1>{}</h1>", esc(&d.titulo)));
    for s in &d.secciones {
        if !s.titulo.is_empty() {
            h.push_str(&format!("<h2>{}</h2>", esc(&s.titulo)));
        }
        for p in &s.parrafos {
            h.push_str(&format!("<p>{}</p>", esc(p).replace('\n', "<br>")));
        }
        if !s.puntos.is_empty() {
            h.push_str("<ul>");
            for p in &s.puntos {
                h.push_str(&format!("<li>{}</li>", esc(p)));
            }
            h.push_str("</ul>");
        }
        for t in &s.tablas {
            if !t.titulo.is_empty() {
                h.push_str(&format!("<h3>{}</h3>", esc(&t.titulo)));
            }
            h.push_str("<table><thead><tr>");
            for c in &t.columnas {
                h.push_str(&format!("<th>{}</th>", esc(c)));
            }
            h.push_str("</tr></thead><tbody>");
            for f in &t.filas {
                h.push_str("<tr>");
                for i in 0..t.columnas.len() {
                    h.push_str(&format!("<td>{}</td>", esc(f.get(i).map(String::as_str).unwrap_or(""))));
                }
                h.push_str("</tr>");
            }
            h.push_str("</tbody></table>");
        }
        for dg in &s.diagramas {
            if let Some((svg, _, _)) = svg_diagrama(dg) {
                h.push_str(&format!("<figure>{svg}{}</figure>", if dg.titulo.is_empty() { String::new() } else { format!("<figcaption>{}</figcaption>", esc(&dg.titulo)) }));
            }
        }
        for im in &s.imagenes {
            if let Some(b64) = capturas.get(&im.captura) {
                h.push_str(&format!("<figure><img src=\"data:image/png;base64,{b64}\" alt=\"{}\">{}</figure>", esc(&im.leyenda), if im.leyenda.is_empty() { String::new() } else { format!("<figcaption>{}</figcaption>", esc(&im.leyenda)) }));
            }
        }
    }
    h.push_str("</body></html>");
    h
}

/// Un HTML mínimo para fotografiar un SVG con Chromium (lo usa Word, que no admite SVG de forma fiable).
pub fn html_de_svg(svg: &str) -> String {
    format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body style=\"margin:0;background:#fff\">{svg}</body></html>")
}

// ── Word ────────────────────────────────────────────────────────────────────────────────────
fn parrafo(t: &str, negrita: bool, medio_punto: u32, antes: u32) -> String {
    format!(
        r#"<w:p><w:pPr><w:spacing w:before="{antes}" w:after="120"/></w:pPr><w:r><w:rPr>{}<w:sz w:val="{medio_punto}"/></w:rPr><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
        if negrita { "<w:b/>" } else { "" },
        esc(t)
    )
}

fn punto(t: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:ind w:left="360" w:hanging="220"/><w:spacing w:after="60"/></w:pPr><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t xml:space="preserve">• {}</w:t></w:r></w:p>"#,
        esc(t)
    )
}

fn celda(t: &str, cabecera: bool, ancho: u32) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="{ancho}" w:type="dxa"/>{}</w:tcPr><w:p><w:pPr><w:spacing w:before="40" w:after="40"/></w:pPr><w:r><w:rPr>{}<w:sz w:val="19"/></w:rPr><w:t xml:space="preserve">{}</w:t></w:r></w:p></w:tc>"#,
        if cabecera { r#"<w:shd w:val="clear" w:color="auto" w:fill="F1F5F9"/>"# } else { "" },
        if cabecera { "<w:b/>" } else { "" },
        esc(t)
    )
}

fn tabla_xml(t: &Tabla) -> String {
    let n = t.columnas.len().max(1) as u32;
    let ancho = 9400 / n;
    let borde = |l: &str| format!(r#"<w:{l} w:val="single" w:sz="4" w:space="0" w:color="CBD5E1"/>"#);
    let mut x = String::new();
    if !t.titulo.is_empty() {
        x.push_str(&parrafo(&t.titulo, true, 22, 120));
    }
    x.push_str(&format!(r#"<w:tbl><w:tblPr><w:tblW w:w="9400" w:type="dxa"/><w:tblBorders>{}{}{}{}{}{}</w:tblBorders><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid>"#, borde("top"), borde("left"), borde("bottom"), borde("right"), borde("insideH"), borde("insideV")));
    for _ in 0..n {
        x.push_str(&format!(r#"<w:gridCol w:w="{ancho}"/>"#));
    }
    x.push_str("</w:tblGrid><w:tr><w:trPr><w:cantSplit/><w:tblHeader/></w:trPr>");
    for c in &t.columnas {
        x.push_str(&celda(c, true, ancho));
    }
    x.push_str("</w:tr>");
    for f in &t.filas {
        x.push_str("<w:tr><w:trPr><w:cantSplit/></w:trPr>");
        for i in 0..t.columnas.len() {
            x.push_str(&celda(f.get(i).map(String::as_str).unwrap_or(""), false, ancho));
        }
        x.push_str("</w:tr>");
    }
    x.push_str("</w:tbl>");
    x.push_str(r#"<w:p><w:pPr><w:spacing w:after="60"/></w:pPr></w:p>"#);
    x
}

/// Ancho y alto de un PNG (de su cabecera).
fn dim_png(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[1..4] != b"PNG" {
        return None;
    }
    Some((u32::from_be_bytes([b[16], b[17], b[18], b[19]]), u32::from_be_bytes([b[20], b[21], b[22], b[23]])))
}

fn imagen_xml(rid: usize, n: usize, w: u32, h: u32) -> String {
    let max: u64 = 5_700_000; // ≈ 15,8 cm
    let (cx, cy) = {
        let cx = (w as u64 * 9525).min(max);
        (cx, cx * h as u64 / w.max(1) as u64)
    };
    format!(
        r#"<w:p><w:pPr><w:jc w:val="center"/><w:spacing w:before="80" w:after="40"/></w:pPr><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="{n}" name="Imagen {n}"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="{n}" name="imagen{n}.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    )
}

fn leyenda(t: &str) -> String {
    if t.is_empty() {
        return String::new();
    }
    format!(r#"<w:p><w:pPr><w:jc w:val="center"/><w:spacing w:after="160"/></w:pPr><w:r><w:rPr><w:i/><w:color w:val="64748B"/><w:sz w:val="18"/></w:rPr><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#, esc(t))
}

/// `imagenes`: PNG de cada diagrama (`d-<sección>-<n>`) y de cada captura (`c-<nombre>`). Lo que falte se omite.
pub fn docx(d: &Documento, imagenes: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
    let mut cuerpo = parrafo(&d.titulo, true, 40, 0);
    let mut medios: Vec<Vec<u8>> = vec![];
    let mut poner = |clave: &str, medios: &mut Vec<Vec<u8>>| -> Option<String> {
        let b = imagenes.get(clave)?;
        let (w, h) = dim_png(b)?;
        medios.push(b.clone());
        let n = medios.len();
        Some(imagen_xml(n, n, w, h))
    };
    for (si, s) in d.secciones.iter().enumerate() {
        if !s.titulo.is_empty() {
            cuerpo.push_str(&parrafo(&s.titulo, true, 28, 240));
        }
        for p in &s.parrafos {
            cuerpo.push_str(&parrafo(p, false, 22, 0));
        }
        for p in &s.puntos {
            cuerpo.push_str(&punto(p));
        }
        for t in &s.tablas {
            cuerpo.push_str(&tabla_xml(t));
        }
        for (k, dg) in s.diagramas.iter().enumerate() {
            if let Some(x) = poner(&format!("d-{si}-{k}"), &mut medios) {
                cuerpo.push_str(&x);
                cuerpo.push_str(&leyenda(&dg.titulo));
            }
        }
        for im in &s.imagenes {
            if let Some(x) = poner(&format!("c-{}", im.captura), &mut medios) {
                cuerpo.push_str(&x);
                cuerpo.push_str(&leyenda(&im.leyenda));
            }
        }
    }
    let mut rels = String::new();
    for i in 1..=medios.len() {
        rels.push_str(&format!(r#"<Relationship Id="rId{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/imagen{i}.png"/>"#));
    }
    let mut partes = vec![
        parte(
            "[Content_Types].xml",
            format!(r#"{CABECERA_XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#),
        ),
        parte(
            "_rels/.rels",
            format!(r#"{CABECERA_XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
        ),
        parte("word/_rels/document.xml.rels", format!(r#"{CABECERA_XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#)),
        parte(
            "word/document.xml",
            format!(r#"{CABECERA_XML}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><w:body>{cuerpo}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#),
        ),
    ];
    for (i, m) in medios.into_iter().enumerate() {
        partes.push((format!("word/media/imagen{}.png", i + 1), m));
    }
    empaquetar(partes)
}

// ── Excel ───────────────────────────────────────────────────────────────────────────────────
fn columna(mut n: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}

fn nombre_de_hoja(n: &str, i: usize, usados: &[String]) -> String {
    let mut base: String = n.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
    if base.trim().is_empty() {
        base = format!("Hoja{}", i + 1);
    }
    let mut cand = base.clone();
    let mut k = 2;
    while usados.iter().any(|u| u.eq_ignore_ascii_case(&cand)) {
        let suf = format!(" ({k})");
        cand = format!("{}{}", base.chars().take(31 - suf.chars().count()).collect::<String>(), suf);
        k += 1;
    }
    cand
}

fn celda_xlsx(ref_: &str, v: &Value, estilo: bool) -> String {
    let s = if estilo { r#" s="1""# } else { "" };
    match v {
        Value::Number(n) => format!(r#"<c r="{ref_}"{s}><v>{n}</v></c>"#),
        Value::Null => String::new(),
        otro => {
            let t = texto(otro);
            if t.is_empty() {
                String::new()
            } else {
                format!(r#"<c r="{ref_}"{s} t="inlineStr"><is><t xml:space="preserve">{}</t></is></c>"#, esc(&t))
            }
        }
    }
}

fn hoja_xml(h: &Hoja) -> String {
    let ancho = h.columnas.len().max(h.filas.iter().map(|f| f.len()).max().unwrap_or(0)).max(1);
    let mut filas = String::new();
    let mut r = 1;
    if !h.columnas.is_empty() {
        filas.push_str(&format!(r#"<row r="{r}">"#));
        for (i, c) in h.columnas.iter().enumerate() {
            filas.push_str(&celda_xlsx(&format!("{}{r}", columna(i)), &Value::String(c.clone()), true));
        }
        filas.push_str("</row>");
        r += 1;
    }
    for f in &h.filas {
        filas.push_str(&format!(r#"<row r="{r}">"#));
        for (i, v) in f.iter().enumerate() {
            filas.push_str(&celda_xlsx(&format!("{}{r}", columna(i)), v, false));
        }
        filas.push_str("</row>");
        r += 1;
    }
    format!(
        r#"{CABECERA_XML}<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="{ancho}" width="26" customWidth="1"/></cols><sheetData>{filas}</sheetData></worksheet>"#
    )
}

pub fn xlsx(hojas: &[Hoja]) -> Result<Vec<u8>, String> {
    let mut nombres: Vec<String> = vec![];
    for (i, h) in hojas.iter().enumerate() {
        let n = nombre_de_hoja(&h.nombre, i, &nombres);
        nombres.push(n);
    }
    let n = hojas.len();
    let mut tipos = String::from(r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>"#);
    let mut hojas_wb = String::new();
    let mut rels = String::new();
    for i in 0..n {
        tipos.push_str(&format!(r#"<Override PartName="/xl/worksheets/sheet{}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#, i + 1));
        hojas_wb.push_str(&format!(r#"<sheet name="{}" sheetId="{}" r:id="rId{}"/>"#, esc(&nombres[i]), i + 1, i + 1));
        rels.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{}.xml"/>"#, i + 1, i + 1));
    }
    rels.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#, n + 1));
    let mut partes = vec![
        parte("[Content_Types].xml", format!(r#"{CABECERA_XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">{tipos}</Types>"#)),
        parte("_rels/.rels", format!(r#"{CABECERA_XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#)),
        parte("xl/workbook.xml", format!(r#"{CABECERA_XML}<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{hojas_wb}</sheets></workbook>"#)),
        parte("xl/_rels/workbook.xml.rels", format!(r#"{CABECERA_XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#)),
        parte(
            "xl/styles.xml",
            format!(r#"{CABECERA_XML}<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><b/><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/></cellXfs></styleSheet>"#),
        ),
    ];
    for (i, h) in hojas.iter().enumerate() {
        partes.push((format!("xl/worksheets/sheet{}.xml", i + 1), hoja_xml(h).into_bytes()));
    }
    empaquetar(partes)
}

// ── PDF sencillo (a mano; sin tablas ni dibujos: para cuando Chromium no está) ────────────────
/// Un carácter a su byte en WinAnsi (cp1252); lo que no existe ahí queda como «?».
fn winansi(c: char) -> u8 {
    match c {
        '€' => 0x80,
        '…' => 0x85,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '™' => 0x99,
        '\t' => b' ',
        c if (c as u32) >= 0x20 && (c as u32) < 0x80 => c as u8,
        c if (c as u32) >= 0xA0 && (c as u32) <= 0xFF => c as u32 as u8,
        _ => b'?',
    }
}

fn ajustar(t: &str, max: usize) -> Vec<String> {
    let mut lineas = vec![];
    for linea in t.split('\n') {
        let mut actual = String::new();
        for palabra in linea.split_whitespace() {
            let mut p = palabra.to_string();
            while p.chars().count() > max {
                if !actual.is_empty() {
                    lineas.push(std::mem::take(&mut actual));
                }
                lineas.push(p.chars().take(max).collect());
                p = p.chars().skip(max).collect();
            }
            let largo = actual.chars().count() + if actual.is_empty() { 0 } else { 1 } + p.chars().count();
            if largo > max && !actual.is_empty() {
                lineas.push(std::mem::take(&mut actual));
            }
            if !actual.is_empty() {
                actual.push(' ');
            }
            actual.push_str(&p);
        }
        lineas.push(actual);
    }
    lineas
}

pub fn pdf(d: &Documento) -> Vec<u8> {
    // (texto, negrita, tamaño, espacio previo)
    let mut lineas: Vec<(String, bool, f32, f32)> = vec![];
    let mut agregar = |t: &str, negrita: bool, size: f32, antes: f32, sangria: &str| {
        let max = (483.0 / (size * 0.54)) as usize;
        for (i, l) in ajustar(t, max).into_iter().enumerate() {
            lineas.push((format!("{}{l}", if i == 0 { "" } else { sangria }), negrita, size, if i == 0 { antes } else { 0.0 }));
        }
    };
    agregar(&d.titulo, true, 18.0, 0.0, "");
    for s in &d.secciones {
        if !s.titulo.is_empty() {
            agregar(&s.titulo, true, 13.0, 14.0, "");
        }
        for p in &s.parrafos {
            agregar(p, false, 11.0, 5.0, "");
        }
        for p in &s.puntos {
            agregar(&format!("• {p}"), false, 11.0, 3.0, "  ");
        }
        for t in &s.tablas {
            if !t.titulo.is_empty() {
                agregar(&t.titulo, true, 11.0, 8.0, "");
            }
            agregar(&t.columnas.join("  |  "), true, 10.0, 3.0, "    ");
            for f in &t.filas {
                agregar(&f.join("  |  "), false, 10.0, 1.0, "    ");
            }
        }
    }
    let (alto, margen) = (842.0f32, 56.0f32);
    let mut paginas: Vec<Vec<u8>> = vec![vec![]];
    let mut y = alto - margen;
    for (t, negrita, size, antes) in &lineas {
        let paso = size * 1.35 + antes;
        if y - paso < margen {
            paginas.push(vec![]);
            y = alto - margen;
        }
        y -= paso;
        let pagina = paginas.last_mut().expect("pagina");
        pagina.extend_from_slice(format!("BT /F{} {size:.1} Tf 1 0 0 1 {margen:.1} {y:.1} Tm (", if *negrita { 2 } else { 1 }).as_bytes());
        for c in t.chars() {
            let b = winansi(c);
            if matches!(b, b'(' | b')' | b'\\') {
                pagina.push(b'\\');
            }
            pagina.push(b);
        }
        pagina.extend_from_slice(b") Tj ET\n");
    }
    // Objetos: 1 catálogo, 2 páginas, 3-4 fuentes, luego (página, contenido) por cada hoja
    let n = paginas.len();
    let kids: String = (0..n).map(|i| format!("{} 0 R", 5 + 2 * i)).collect::<Vec<_>>().join(" ");
    let mut objetos: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>").into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_vec(),
    ];
    for (i, contenido) in paginas.iter().enumerate() {
        objetos.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {} 0 R >>", 6 + 2 * i).into_bytes());
        let mut flujo = format!("<< /Length {} >>\nstream\n", contenido.len()).into_bytes();
        flujo.extend_from_slice(contenido);
        flujo.extend_from_slice(b"endstream");
        objetos.push(flujo);
    }
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objetos.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objetos.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objetos.len() + 1).as_bytes());
    out
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use serde_json::json;

    fn doc() -> Documento {
        documento_de(&json!({ "titulo": "Acta de arranque", "secciones": [
            { "titulo": "Objetivo", "parrafos": ["Definir el alcance & los (límites) del proyecto, con acentos: ñandú ¿sí?"], "puntos": ["Primero", "Segundo"] },
            { "titulo": "Pendientes por confirmar", "puntos": ["Fecha de inicio"] } ] }))
        .expect("doc")
    }

    fn doc_rico() -> Documento {
        documento_de(&json!({ "titulo": "Arquitectura", "secciones": [{
            "titulo": "Componentes",
            "parrafos": ["Resumen."],
            "tablas": [{ "titulo": "Responsables", "columnas": ["Componente", "Responsable"], "filas": [["API & backend", "Marta"], ["Web <admin>", "Luis"]] }],
            "diagramas": [{ "titulo": "Flujo", "nodos": [{ "id": "web", "etiqueta": "Aplicación web" }, { "id": "api", "etiqueta": "API" }, { "id": "db", "etiqueta": "Base de datos" }], "conexiones": [{ "de": "web", "a": "api", "etiqueta": "HTTPS" }, { "de": "api", "a": "db" }, { "de": "api", "a": "nadie" }] }],
            "imagenes": [{ "captura": "1791-1.png", "leyenda": "Pantalla de inicio" }, { "captura": "../x.png", "leyenda": "no" }, { "captura": "malo.txt" }]
        }] }))
        .expect("doc rico")
    }

    /// Un PNG mínimo de 3×2 píxeles (solo importa la cabecera).
    fn png() -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        b.extend_from_slice(&3u32.to_be_bytes());
        b.extend_from_slice(&2u32.to_be_bytes());
        b.extend_from_slice(&[8, 2, 0, 0, 0]);
        b
    }

    fn leer(b: Vec<u8>, parte: &str) -> String {
        let mut z = zip::ZipArchive::new(Cursor::new(b)).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut z.by_name(parte).unwrap(), &mut s).unwrap();
        s
    }

    #[test]
    fn el_docx_es_un_zip_valido_con_el_documento() {
        let b = docx(&doc(), &HashMap::new()).unwrap();
        assert_eq!(&b[..2], b"PK");
        let xml = leer(b, "word/document.xml");
        assert!(xml.contains("Acta de arranque") && xml.contains("&amp;") && xml.contains("ñandú"));
    }

    #[test]
    fn el_documento_rico_se_valida_y_se_limpia() {
        let d = doc_rico();
        let s = &d.secciones[0];
        assert_eq!(s.tablas.len(), 1);
        assert_eq!(s.diagramas.len(), 1);
        assert_eq!(s.diagramas[0].conexiones.len(), 2, "la conexión a un nodo que no existe se descarta");
        assert_eq!(s.imagenes.len(), 1, "solo capturas con nombre válido (.png, sin «..»)");
        assert!(documento_de(&json!({ "titulo": "x", "secciones": [{ "diagramas": [{ "nodos": [{ "id": "a" }] }] }] })).is_none(), "un diagrama de un solo nodo no cuenta");
    }

    #[test]
    fn el_word_lleva_tablas_reales_e_imagenes_incrustadas() {
        let d = doc_rico();
        let mut imgs: HashMap<String, Vec<u8>> = HashMap::new();
        imgs.insert("d-0-0".into(), png());
        imgs.insert("c-1791-1.png".into(), png());
        let b = docx(&d, &imgs).unwrap();
        let xml = leer(b.clone(), "word/document.xml");
        assert!(xml.contains("<w:tbl>") && xml.contains("API &amp; backend") && xml.contains("Web &lt;admin&gt;"), "tabla escapada");
        assert_eq!(xml.matches("<w:drawing>").count(), 2, "diagrama y captura");
        assert!(xml.contains("Pantalla de inicio") && xml.contains("Flujo"));
        let rels = leer(b.clone(), "word/_rels/document.xml.rels");
        assert!(rels.contains("rId1") && rels.contains("rId2") && rels.contains("media/imagen2.png"));
        assert!(leer(b.clone(), "[Content_Types].xml").contains(r#"Extension="png""#));
        let mut z = zip::ZipArchive::new(Cursor::new(b)).unwrap();
        assert!(z.by_name("word/media/imagen1.png").is_ok() && z.by_name("word/media/imagen2.png").is_ok());
        // sin imágenes disponibles, el documento sigue saliendo (se omiten)
        assert_eq!(leer(docx(&d, &HashMap::new()).unwrap(), "word/document.xml").matches("<w:drawing>").count(), 0);
    }

    #[test]
    fn el_svg_del_diagrama_ordena_por_capas_y_escapa() {
        let d = &doc_rico().secciones[0].diagramas[0];
        let (svg, w, h) = svg_diagrama(d).unwrap();
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>") && w > 300 && h > 50);
        assert!(svg.contains("Aplicación web") && svg.contains("HTTPS") && svg.contains("marker-end"));
        assert_eq!(svg.matches("<rect x=").count(), 3, "un rectángulo por nodo");
        let con_peligro = Diagrama { titulo: String::new(), nodos: vec![("a".into(), "<script>x</script>".into()), ("b".into(), "B".into())], conexiones: vec![("a".into(), "b".into(), String::new()), ("b".into(), "a".into(), String::new())] };
        let (s2, _, _) = svg_diagrama(&con_peligro).unwrap();
        assert!(!s2.contains("<script>") && s2.contains("&lt;script&gt;"), "el texto va escapado y un ciclo no cuelga el dibujo");
    }

    #[test]
    fn el_html_para_imprimir_lleva_tablas_diagramas_e_imagenes() {
        let d = doc_rico();
        let mut caps = HashMap::new();
        caps.insert("1791-1.png".to_string(), "AAAA".to_string());
        let h = html(&d, &caps);
        assert!(h.contains("<table>") && h.contains("API &amp; backend") && h.contains("<svg") && h.contains("data:image/png;base64,AAAA") && h.contains("Pantalla de inicio"));
        assert!(!html(&d, &HashMap::new()).contains("data:image"), "una captura que no está se omite");
    }

    #[test]
    fn el_xlsx_trae_hojas_con_nombre_valido_y_numeros() {
        let h = hojas_de(&json!({ "hojas": [
            { "nombre": "Cronograma: [fase]/1", "columnas": ["Fase", "Días"], "filas": [["Diseño", 10], ["Construcción", 30]] },
            { "nombre": "Cronograma: [fase]/1", "columnas": ["A"], "filas": [] } ] }))
        .expect("hojas");
        let b = xlsx(&h).unwrap();
        let wb = leer(b.clone(), "xl/workbook.xml");
        assert!(wb.contains("Cronograma fase1") && wb.contains("Cronograma fase1 (2)"), "{wb}");
        let s1 = leer(b, "xl/worksheets/sheet1.xml");
        assert!(s1.contains("<v>30</v>") && s1.contains("B3") && s1.contains("Construcción"));
        assert_eq!(columna(0), "A");
        assert_eq!(columna(26), "AA");
    }

    #[test]
    fn el_pdf_sencillo_tiene_estructura_y_escapa_parentesis() {
        let b = pdf(&doc());
        assert!(b.starts_with(b"%PDF-1.4") && b.ends_with(b"%%EOF\n"));
        let t = String::from_utf8_lossy(&b);
        assert!(t.contains("\\(límites\\)") || t.contains("\\(l\u{fffd}mites\\)"), "los paréntesis deben ir escapados");
        assert!(t.contains("/Count 1"));
        assert!(String::from_utf8_lossy(&pdf(&doc_rico())).contains("API & backend"), "las tablas salen como texto en el PDF sencillo");
    }

    #[test]
    fn un_documento_largo_pagina_y_sin_contenido_se_rechaza() {
        let secciones: Vec<Value> = (0..80).map(|i| json!({ "titulo": format!("S{i}"), "parrafos": ["texto ".repeat(60)] })).collect();
        let d = documento_de(&json!({ "titulo": "Largo", "secciones": secciones })).unwrap();
        assert!(String::from_utf8_lossy(&pdf(&d)).contains("/Count ") && !String::from_utf8_lossy(&pdf(&d)).contains("/Count 1 "));
        assert!(documento_de(&json!({ "titulo": "Vacío", "secciones": [] })).is_none());
        assert!(hojas_de(&json!({ "hojas": [] })).is_none());
    }
}
