//! Conversión entre HTML simple y texto plano (puerto de `lib/textoAHtml.ts` y de los helpers
//! `htmlATexto` / `stripHtml` repartidos en las rutas de Next). El crate `regex` no soporta
//! lookahead ni backreferences, así que esas dos expresiones se resuelven a mano.

use std::sync::LazyLock;

use regex::{Captures, Regex};

fn re(p: &str) -> Regex {
    Regex::new(p).expect("regex")
}

static RE_CIERRE_BLOQUE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</(p|h[1-6]|div|li)>"));
static RE_BR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<br\s*/?>"));
static RE_LI_ABRE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<li[^>]*>"));
static RE_TAG: LazyLock<Regex> = LazyLock::new(|| re(r"<[^>]+>"));
static RE_ESPACIOS: LazyLock<Regex> = LazyLock::new(|| re(r"[ \t]+"));
static RE_SALTOS3: LazyLock<Regex> = LazyLock::new(|| re(r"\n{3,}"));
static RE_WS: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));

fn entidades(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// `htmlATextoPlano` de `lib/textoAHtml.ts` (el que usa el contexto del lead y el asistente).
pub fn html_a_texto_plano(html: &str) -> String {
    let s = RE_CIERRE_BLOQUE.replace_all(html, "\n");
    let s = RE_BR.replace_all(&s, "\n");
    let s = RE_LI_ABRE.replace_all(&s, "- ");
    let s = RE_TAG.replace_all(&s, "");
    let s = entidades(&s);
    let s = RE_ESPACIOS.replace_all(&s, " ");
    let s = RE_SALTOS3.replace_all(&s, "\n\n");
    s.trim().to_string()
}

/// `htmlATexto` de `meetings/[id]/acta-generate` (variante que no colapsa espacios).
pub fn html_a_texto_acta(html: &str) -> String {
    let s = RE_CIERRE_BLOQUE.replace_all(html, "\n");
    let s = RE_BR.replace_all(&s, "\n");
    let s = RE_LI_ABRE.replace_all(&s, "- ");
    let s = RE_TAG.replace_all(&s, "");
    let s = entidades(&s);
    let s = RE_SALTOS3.replace_all(&s, "\n\n");
    s.trim().to_string()
}

/// `stripHtml` de las rutas de generación: etiquetas → espacio, espacios colapsados.
pub fn strip_html(s: &str) -> String {
    let s = RE_TAG.replace_all(s, " ");
    RE_WS.replace_all(&s, " ").trim().to_string()
}

pub fn corta(t: &str, max: usize) -> String {
    if t.chars().count() > max {
        let mut o: String = t.chars().take(max).collect();
        o.push('…');
        o
    } else {
        t.to_string()
    }
}

pub fn truncar(t: &str, max: usize) -> String {
    t.chars().take(max).collect()
}

// ── texto → HTML del editor ─────────────────────────────────────────────────────────────────
pub fn escapar_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

static RE_NEGRITA: LazyLock<Regex> = LazyLock::new(|| re(r"\*\*(.+?)\*\*"));

/// `**negrita**` y `*cursiva*` dentro de una línea ya escapada. La cursiva
/// (`(^|[^*])\*(?!\s)(.+?)\*(?!\*)` en JS) se resuelve con un recorrido manual.
fn inline(s: &str) -> String {
    let con_negrita = RE_NEGRITA.replace_all(&escapar_html(s), "<strong>$1</strong>").to_string();
    let c: Vec<char> = con_negrita.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let prev_ok = i == 0 || c[i - 1] != '*';
        if c[i] == '*' && prev_ok && i + 1 < c.len() && !c[i + 1].is_whitespace() && c[i + 1] != '*' {
            // buscar el cierre: un '*' que no vaya seguido de otro '*'
            let mut j = i + 2;
            let mut cierre = None;
            while j < c.len() {
                if c[j] == '*' && (j + 1 >= c.len() || c[j + 1] != '*') {
                    cierre = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(j) = cierre {
                out.push_str("<em>");
                out.extend(&c[i + 1..j]);
                out.push_str("</em>");
                i = j + 1;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

static RE_H2: LazyLock<Regex> = LazyLock::new(|| re(r"^#{1,2}\s+(.*)$"));
static RE_H3: LazyLock<Regex> = LazyLock::new(|| re(r"^#{3,6}\s+(.*)$"));
static RE_UL: LazyLock<Regex> = LazyLock::new(|| re(r"^[-*•]\s+(.*)$"));
static RE_OL: LazyLock<Regex> = LazyLock::new(|| re(r"^\d+[.)]\s+(.*)$"));

pub fn texto_a_html(texto: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut lista: Option<&str> = None;
    fn cerrar(out: &mut Vec<String>, lista: &mut Option<&str>) {
        if let Some(l) = lista.take() {
            out.push(format!("</{l}>"));
        }
    }
    for cruda in texto.replace('\r', "").split('\n') {
        let l = cruda.trim();
        if l.is_empty() {
            cerrar(&mut out, &mut lista);
            continue;
        }
        if let Some(m) = RE_H2.captures(l) {
            cerrar(&mut out, &mut lista);
            out.push(format!("<h2>{}</h2>", inline(&m[1])));
        } else if let Some(m) = RE_H3.captures(l) {
            cerrar(&mut out, &mut lista);
            out.push(format!("<h3>{}</h3>", inline(&m[1])));
        } else if let Some(m) = RE_UL.captures(l) {
            if lista != Some("ul") {
                cerrar(&mut out, &mut lista);
                out.push("<ul>".into());
                lista = Some("ul");
            }
            out.push(format!("<li><p>{}</p></li>", inline(&m[1])));
        } else if let Some(m) = RE_OL.captures(l) {
            if lista != Some("ol") {
                cerrar(&mut out, &mut lista);
                out.push("<ol>".into());
                lista = Some("ol");
            }
            out.push(format!("<li><p>{}</p></li>", inline(&m[1])));
        } else {
            cerrar(&mut out, &mut lista);
            out.push(format!("<p>{}</p>", inline(l)));
        }
    }
    cerrar(&mut out, &mut lista);
    out.join("")
}

static RE_FENCE_INI: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^```(?:html)?\s*"));
static RE_FENCE_FIN: LazyLock<Regex> = LazyLock::new(|| re(r"\s*```$"));
static RE_BLOQUE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<(p|h2|h3|ul|ol|li)[\s>]"));
static RE_PELIGROSOS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    ["script", "style", "iframe", "object", "embed"]
        .iter()
        .map(|t| re(&format!(r"(?i)<{t}[\s\S]*?</{t}>")))
        .collect()
});
static RE_ETIQUETA: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</?([a-z][a-z0-9]*)\b[^>]*>"));
static RE_ATRIBUTOS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<(p|h2|h3|ul|ol|li|strong|em|u)\s+[^>]*>"));

const PERMITIDAS: [&str; 10] = ["p", "h2", "h3", "ul", "ol", "li", "strong", "em", "u", "br"];

/// `sanearHtml`: solo etiquetas permitidas y sin atributos; si no trae bloques, convierte el texto.
pub fn sanear_html(h: &str) -> String {
    let s = h.trim();
    let s = RE_FENCE_INI.replace(s, "").to_string();
    let s = RE_FENCE_FIN.replace(&s, "").trim().to_string();
    if !RE_BLOQUE.is_match(&s) {
        return texto_a_html(&RE_TAG.replace_all(&s, ""));
    }
    let mut s = s;
    for r in RE_PELIGROSOS.iter() {
        s = r.replace_all(&s, "").to_string();
    }
    let s = RE_ETIQUETA
        .replace_all(&s, |c: &Captures| {
            let nombre = c[1].to_lowercase();
            // (igual que en JS: <h1> no está permitida y se elimina; el replace h1→h2 posterior
            // en el original nunca llega a ejecutarse sobre nada)
            if PERMITIDAS.contains(&nombre.as_str()) {
                c[0].to_string()
            } else {
                String::new()
            }
        })
        .to_string();
    let s = RE_ATRIBUTOS.replace_all(&s, "<$1>").to_string();
    s.replace("<h1>", "<h2>").replace("</h1>", "</h2>").trim().to_string()
}
