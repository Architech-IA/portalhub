//! El diseño técnico y la arquitectura de una Solución como REFERENCIA del Motor — MASD-0024.
//!
//! Antes el Motor ignoraba el hub: cada tarea de código inventaba tablas y estructura sobre la marcha y el verificador
//! solo miraba que compilara y que cumpliera el requisito. Ahora el diseño documentado (modelo de datos, stack,
//! integraciones, decisiones y diagrama de componentes) entra al contexto de cada tarea, al criterio del verificador y a un
//! aviso cuando el esquema de Prisma se aparta de las entidades documentadas.

use std::{collections::HashMap, sync::LazyLock};

use regex::Regex;
use serde_json::Value;

use crate::{
    state::AppState,
    util::{fetch_json_opt, B},
};

const MAX_DISENO: usize = 8000;
const MAX_ITEMS: usize = 40;
/// Marca que el agente escribe en su resumen cuando la tarea le obligó a salirse del diseño.
pub const MARCA_CAMBIO: &str = "CAMBIO DE DISEÑO:";
/// Marca que el revisor pone en el motivo de su criterio cuando el diff se aparta del diseño (no depende de que el agente lo declare).
pub const MARCA_REVISOR: &str = "DESVÍA DEL DISEÑO:";
/// Primera línea del bloque de diseño en el contexto (sirve para saber si el contexto lo trae).
pub const ENCABEZADO: &str = "=== DISEÑO TÉCNICO DE LA SOLUCIÓN";

/// Se pone al final del prompt de la tarea: una revisión obligatoria antes de dar la tarea por terminada.
pub fn recordatorio() -> String {
    [
        "---".to_string(),
        "ANTES DE TERMINAR, revisa tu trabajo contra el DISEÑO TÉCNICO de arriba:".into(),
        "¿Tu cambio agrega o modifica entidades, atributos, relaciones, el stack o una decisión que el diseño técnico NO contiene (o contiene distinto)?".into(),
        format!("Si la respuesta es sí, tu resumen final DEBE incluir una línea que empiece con «{MARCA_CAMBIO}» y diga qué se salió del diseño y por qué. Si es no, no escribas esa línea."),
    ]
    .join("
")
}

pub struct Diseno {
    /// BORRADOR, EN_REVISION o APROBADO.
    pub estado: String,
    /// Bloque listo para el contexto del agente (incluye las reglas).
    pub texto: String,
    /// Nombres de las entidades del modelo de datos.
    pub entidades: Vec<String>,
}

impl Diseno {
    /// Criterio extra para el verificador semántico.
    pub fn criterio(&self) -> String {
        let ent = if self.entidades.is_empty() { String::new() } else { format!(" Entidades documentadas: {}.", self.entidades.join(", ")) };
        format!(
            "El cambio no contradice el diseño técnico documentado (modelo de datos y relaciones, stack, decisiones y arquitectura).{ent} Si la tarea no toca datos ni estructura, este criterio se cumple; si hay duda, cúmplelo salvo contradicción clara. Aunque la tarea haya pedido el cambio, si el diff introduce entidades, campos, relaciones, tecnologías o decisiones que el diseño no contiene, empieza el campo reason de este criterio con «{MARCA_REVISOR}» y di cuál; si no hay desviación, no uses esa frase. El agente debía además declarar el cambio con «{MARCA_CAMBIO}» en su resumen."
        )
    }
}

fn cortar(t: &str, n: usize) -> String {
    if t.chars().count() <= n { t.to_string() } else { format!("{}…", t.chars().take(n).collect::<String>()) }
}

fn plano(s: &str) -> String {
    super::ejecutor::prd_html_a_plano(s).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn texto(v: &Value, k: &str) -> String {
    plano(v[k].as_str().unwrap_or(""))
}

/// Arma el bloque de diseño a partir de los dos JSON que guarda el hub (`disenoTecnico` y `arquitectura`). `None` si no hay nada útil.
pub fn resumir(diseno: Option<&str>, arquitectura: Option<&str>) -> Option<Diseno> {
    let d: Value = diseno.and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    let a: Value = arquitectura.and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    let lista = |k: &str| d[k].as_array().cloned().unwrap_or_default();
    let (entidades, stack, integ, decis) = (lista("entidades"), lista("stack"), lista("integraciones"), lista("decisiones"));
    let nodos = a["nodes"].as_array().cloned().unwrap_or_default();
    let conexiones = a["connections"].as_array().cloned().unwrap_or_default();
    let arq_texto = texto(&d, "arquitectura");
    if entidades.is_empty() && stack.is_empty() && integ.is_empty() && decis.is_empty() && nodos.is_empty() && arq_texto.is_empty() {
        return None;
    }
    let estado = d["estadoDocumento"].as_str().unwrap_or("BORRADOR").to_string();

    let mut l: Vec<String> = vec![format!("{ENCABEZADO} (estado: {estado}) ===")];
    l.push("REGLAS: este diseño es la referencia del proyecto. (1) Usa estos nombres de entidades, atributos y relaciones: no inventes tablas ni cambies relaciones por tu cuenta. (2) Respeta el stack, las integraciones y las decisiones. (3) Si la tarea te obliga a salirte del diseño, hazlo solo si la tarea lo pide expresamente y escribe en tu resumen final una línea que empiece con «CAMBIO DE DISEÑO:» explicando qué cambiaste y por qué.".into());
    if !arq_texto.is_empty() {
        l.push(format!("ARQUITECTURA: {}", cortar(&arq_texto, 700)));
    }
    if !nodos.is_empty() {
        let etiquetas: HashMap<&str, &str> = nodos.iter().filter_map(|n| Some((n["id"].as_str()?, n["label"].as_str()?))).collect();
        let nombres: Vec<String> = nodos.iter().take(MAX_ITEMS).map(|n| format!("{} ({})", n["label"].as_str().unwrap_or("?"), n["type"].as_str().unwrap_or("?"))).collect();
        l.push(format!("DIAGRAMA DE COMPONENTES: {}", nombres.join(", ")));
        let cx: Vec<String> = conexiones
            .iter()
            .take(MAX_ITEMS * 2)
            .filter_map(|c| Some(format!("{} → {}", etiquetas.get(c["from"].as_str()?)?, etiquetas.get(c["to"].as_str()?)?)))
            .collect();
        if !cx.is_empty() {
            l.push(format!("CONEXIONES: {}", cx.join("; ")));
        }
    }
    let mut nombres_entidades: Vec<String> = vec![];
    if !entidades.is_empty() {
        l.push("MODELO DE DATOS (entidades):".into());
        for e in entidades.iter().take(MAX_ITEMS) {
            let nombre = texto(e, "nombre");
            let rel = texto(e, "relaciones");
            l.push(format!("- {nombre}: {}{}", cortar(&texto(e, "atributos"), 220), if rel.is_empty() { String::new() } else { format!(" | relaciones: {}", cortar(&rel, 160)) }));
            if !nombre.is_empty() {
                nombres_entidades.push(nombre);
            }
        }
    }
    if !stack.is_empty() {
        l.push("STACK:".into());
        for s in stack.iter().take(MAX_ITEMS) {
            let j = texto(s, "justificacion");
            l.push(format!("- {}: {}{}", texto(s, "capa"), texto(s, "tecnologia"), if j.is_empty() { String::new() } else { format!(" ({})", cortar(&j, 120)) }));
        }
    }
    if !integ.is_empty() {
        l.push("INTEGRACIONES:".into());
        for i in integ.iter().take(MAX_ITEMS) {
            l.push(format!("- {}: {} — {} — si falla: {}", texto(i, "sistema"), cortar(&texto(i, "proposito"), 100), cortar(&texto(i, "detalle"), 120), cortar(&texto(i, "siFalla"), 100)));
        }
    }
    if !decis.is_empty() {
        l.push("DECISIONES TÉCNICAS:".into());
        for x in decis.iter().take(MAX_ITEMS) {
            l.push(format!("- {} — {}", cortar(&texto(x, "decision"), 140), cortar(&texto(x, "justificacion"), 140)));
        }
    }
    let mut t = l.join("\n");
    if t.chars().count() > MAX_DISENO {
        t = format!("{}\n[... diseño truncado por tamaño ...]", t.chars().take(MAX_DISENO).collect::<String>());
    }
    Some(Diseno { estado, texto: t, entidades: nombres_entidades })
}

/// Lee el diseño de la Solución. Nunca falla: sin diseño (o con error de lectura) el Motor sigue como antes.
pub async fn cargar(st: &AppState, solucion: &str) -> Option<Diseno> {
    let v = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('d', "disenoTecnico", 'a', arquitectura) FROM "Solucion" WHERE id = $1"#, &[B::T(solucion.to_string())])
        .await
        .ok()
        .flatten()?;
    resumir(v["d"].as_str(), v["a"].as_str())
}

// ── Esquema de Prisma vs entidades documentadas ─────────────────────────────────────────────
static RE_MODELO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*model\s+([A-Za-z0-9_]+)\s*\{").expect("regex modelo"));

pub fn modelos_prisma(schema: &str) -> Vec<String> {
    RE_MODELO.captures_iter(schema).map(|c| c[1].to_string()).collect()
}

/// Minúsculas, sin tildes ni signos y en singular simple: «Cotizaciones» y «cotizacion» dan lo mismo.
fn norma(s: &str) -> String {
    let base: String = s
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ë' => 'e',
            'í' | 'ì' | 'ï' => 'i',
            'ó' | 'ò' | 'ö' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            'ñ' => 'n',
            c => c,
        })
        .filter(|c| c.is_alphanumeric())
        .collect();
    if base.len() > 5 && base.ends_with("es") {
        base[..base.len() - 2].to_string()
    } else if base.len() > 3 && base.ends_with('s') {
        base[..base.len() - 1].to_string()
    } else {
        base
    }
}

/// Un nombre documentado puede traer alternativas: «Cotización (Quote)», «Cliente / Empresa».
fn variantes(nombre: &str) -> Vec<String> {
    nombre.split(|c: char| matches!(c, '(' | ')' | '/' | ',' | '|' | ';')).map(norma).filter(|v| !v.is_empty()).collect()
}

/// (modelos del esquema que el diseño no menciona, entidades del diseño sin modelo en el esquema).
pub fn comparar(entidades: &[String], modelos: &[String]) -> (Vec<String>, Vec<String>) {
    let documentadas: Vec<Vec<String>> = entidades.iter().map(|e| variantes(e)).collect();
    let del_esquema: Vec<String> = modelos.iter().map(|m| norma(m)).collect();
    let sin_documentar = modelos.iter().zip(&del_esquema).filter(|(_, n)| !documentadas.iter().any(|v| v.contains(n))).map(|(m, _)| m.clone()).collect();
    let sin_modelo = entidades.iter().zip(&documentadas).filter(|(_, v)| !v.iter().any(|x| del_esquema.contains(x))).map(|(e, _)| e.clone()).collect();
    (sin_documentar, sin_modelo)
}

/// Avisos (solo informativos) si el esquema de Prisma se aparta del modelo de datos documentado.
pub fn avisos_schema(d: &Diseno, schema: &str) -> Vec<String> {
    if d.entidades.is_empty() {
        return vec![];
    }
    let (sobran, faltan) = comparar(&d.entidades, &modelos_prisma(schema));
    let mut v = vec![];
    if !sobran.is_empty() {
        v.push(format!("el esquema de Prisma tiene modelos que el diseño técnico no menciona: {} (si son los mismos con otro nombre, ajusta el diseño)", sobran.join(", ")));
    }
    if !faltan.is_empty() {
        v.push(format!("el diseño técnico lista entidades que aún no tienen modelo en el esquema: {}", faltan.join(", ")));
    }
    v
}

#[cfg(test)]
mod pruebas {
    use super::*;

    const DISENO: &str = r#"{"estadoDocumento":"APROBADO","arquitectura":"<p>Monolito <b>Next.js</b> con Prisma</p>",
        "entidades":[{"id":"1","nombre":"Cotización","atributos":"id, cliente, fecha, total","relaciones":"pertenece a Cliente"},
                     {"id":"2","nombre":"Cliente (Customer)","atributos":"id, nombre","relaciones":""}],
        "stack":[{"id":"s","capa":"Frontend","tecnologia":"Next.js","justificacion":"ya lo usamos"}],
        "integraciones":[{"id":"i","sistema":"API de Allianz","proposito":"cotizar","detalle":"REST","siFalla":"reintenta"}],
        "decisiones":[{"id":"d","decision":"Una sola base","alternativas":"x","justificacion":"simplicidad"}]}"#;
    const ARQ: &str = r#"{"nodes":[{"id":"a","label":"Web","type":"frontend"},{"id":"b","label":"Postgres","type":"database"}],"connections":[{"id":"c","from":"a","to":"b"}]}"#;

    #[test]
    fn resume_el_diseno_con_reglas_y_diagrama() {
        let d = resumir(Some(DISENO), Some(ARQ)).expect("hay diseño");
        assert_eq!(d.estado, "APROBADO");
        assert_eq!(d.entidades, vec!["Cotización", "Cliente (Customer)"]);
        assert!(d.texto.contains("REGLAS:") && d.texto.contains("CAMBIO DE DISEÑO:"));
        assert!(d.texto.contains("Monolito Next.js con Prisma"), "el HTML se vuelve texto");
        assert!(d.texto.contains("Web → Postgres"), "las conexiones usan las etiquetas");
        assert!(d.texto.contains("- Cotización: id, cliente, fecha, total | relaciones: pertenece a Cliente"));
        assert!(d.texto.contains("Una sola base"));
        assert!(d.criterio().contains("Cotización, Cliente (Customer)"));
        assert!(d.criterio().contains(MARCA_REVISOR) && d.criterio().contains(MARCA_CAMBIO));
        assert!(d.texto.starts_with(ENCABEZADO), "el recordatorio se activa por este encabezado");
        assert!(recordatorio().contains("CAMBIO DE DISEÑO:") && recordatorio().contains("ANTES DE TERMINAR"));
    }

    #[test]
    fn sin_contenido_no_hay_diseno() {
        assert!(resumir(None, None).is_none());
        assert!(resumir(Some("{}"), Some("{}")).is_none());
        assert!(resumir(Some(r#"{"estadoDocumento":"BORRADOR","entidades":[]}"#), None).is_none());
        assert!(resumir(Some("no es json"), None).is_none());
    }

    #[test]
    fn el_bloque_no_pasa_del_tope() {
        let muchas: Vec<String> = (0..40).map(|i| format!(r#"{{"id":"{i}","nombre":"Entidad{i}","atributos":"{}","relaciones":"{}"}}"#, "a".repeat(300), "r".repeat(300))).collect();
        let d = resumir(Some(&format!(r#"{{"entidades":[{}]}}"#, muchas.join(","))), None).unwrap();
        assert!(d.texto.chars().count() < MAX_DISENO + 80);
        assert!(d.texto.contains("diseño truncado"));
    }

    #[test]
    fn lee_los_modelos_de_prisma() {
        let s = "generator client {\n}\nmodel User {\n  id String @id\n}\n\nmodel  Quote_Item {\n  id Int\n}\nenum Rol { A }\n";
        assert_eq!(modelos_prisma(s), vec!["User", "Quote_Item"]);
    }

    #[test]
    fn compara_nombres_con_tildes_plurales_y_alternativas() {
        let ent = vec!["Cotización".to_string(), "Cliente (Customer)".to_string(), "Pagos".to_string()];
        let mod_ = vec!["Cotizacion".to_string(), "Customer".to_string(), "Auditoria".to_string()];
        let (sobran, faltan) = comparar(&ent, &mod_);
        assert_eq!(sobran, vec!["Auditoria"]);
        assert_eq!(faltan, vec!["Pagos"]);
        assert_eq!(norma("Cotizaciones"), norma("cotización"));
    }

    #[test]
    fn avisos_solo_si_hay_entidades_documentadas() {
        let d = resumir(Some(DISENO), None).unwrap();
        let v = avisos_schema(&d, "model Cotizacion {\n}\nmodel Cliente {\n}\nmodel Extra {\n}\n");
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("Extra"));
        let sin = resumir(Some(r#"{"stack":[{"capa":"x","tecnologia":"y","justificacion":""}]}"#), None).unwrap();
        assert!(avisos_schema(&sin, "model A {\n}\n").is_empty());
    }
}
