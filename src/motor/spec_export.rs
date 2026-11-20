//! Exporta la spec aprobada de una Solución al repositorio del proyecto (MASD-0026-0003).
//!
//! Escribe `spec/` (README, PRD, diseño, arquitectura, plan de ejecución y un archivo por requisito `REQ-###.md`) en la rama
//! `spec/actualizacion` creada desde `origin/main`, y abre (o reutiliza) un PR. NUNCA mergea: una persona revisa. Así la spec
//! vive junto al código, con historial, y el agente puede leerla del repositorio. La Solución debe tener repositorio propio
//! (nunca se escribe en el repo del portal). Solo corre en el servicio privilegiado.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::repo::{self, R};
use crate::{
    state::AppState,
    util::{fetch_json, fetch_json_opt, B},
};

const IDENT: [&str; 4] = ["-c", "user.email=masd@architechia.local", "-c", "user.name=Motor Agéntico SDD"];
const RAMA: &str = "spec/actualizacion";
/// Claves que mantiene el servidor y que se resumen en la cabecera, no en el cuerpo.
const VOLATILES: [&str; 3] = ["estadoDocumento", "aprobacion", "reabierto"];

fn humano(k: &str) -> String {
    let mut s = String::new();
    for (i, c) in k.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            s.push(' ');
            s.extend(c.to_lowercase());
        } else if i == 0 {
            s.extend(c.to_uppercase());
        } else {
            s.push(c);
        }
    }
    s.replace('_', " ")
}

fn escalar(v: &Value) -> Option<String> {
    match v {
        Value::String(x) => Some(x.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "sí".into() } else { "no".into() }),
        _ => None,
    }
}

/// Convierte un valor JSON cualquiera en Markdown legible (la forma exacta del PRD/diseño puede variar).
pub(crate) fn cuerpo(v: &Value, nivel: usize, out: &mut String) {
    let h = "#".repeat(nivel.clamp(2, 5));
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if VOLATILES.contains(&k.as_str()) {
                    continue;
                }
                match x {
                    Value::Null => {}
                    Value::String(t) if t.trim().is_empty() => {}
                    Value::Array(a) if a.is_empty() => {}
                    Value::Object(m) if m.is_empty() => {}
                    Value::String(t) if t.contains('\n') || t.len() > 90 => {
                        out.push_str(&format!("{h} {}\n\n{}\n\n", humano(k), t.trim()));
                    }
                    Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                        out.push_str(&format!("**{}:** {}\n\n", humano(k), escalar(x).unwrap_or_default()));
                    }
                    _ => {
                        out.push_str(&format!("{h} {}\n\n", humano(k)));
                        cuerpo(x, nivel + 1, out);
                    }
                }
            }
        }
        Value::Array(a) => {
            if a.iter().all(|x| escalar(x).is_some()) {
                for x in a {
                    out.push_str(&format!("- {}\n", escalar(x).unwrap_or_default()));
                }
                out.push('\n');
            } else {
                for (i, x) in a.iter().enumerate() {
                    match x {
                        Value::Object(o) => {
                            let titulo = ["titulo", "nombre", "texto", "descripcion", "id"].iter().find_map(|k| o.get(*k).and_then(escalar)).unwrap_or_else(|| format!("Elemento {}", i + 1));
                            out.push_str(&format!("- **{}**\n", titulo.lines().next().unwrap_or("")));
                            for (k, y) in o {
                                if VOLATILES.contains(&k.as_str()) || ["titulo", "nombre", "texto", "descripcion"].contains(&k.as_str()) && escalar(y).as_deref() == Some(titulo.as_str()) {
                                    continue;
                                }
                                if let Some(t) = escalar(y).filter(|t| !t.is_empty()) {
                                    out.push_str(&format!("  - {}: {}\n", humano(k), t.replace('\n', " ")));
                                } else if let Value::Array(l) = y {
                                    let items: Vec<String> = l.iter().filter_map(escalar).collect();
                                    if !items.is_empty() {
                                        out.push_str(&format!("  - {}: {}\n", humano(k), items.join("; ")));
                                    }
                                }
                            }
                        }
                        otro => out.push_str(&format!("- {}\n", escalar(otro).unwrap_or_default())),
                    }
                }
                out.push('\n');
            }
        }
        otro => {
            if let Some(t) = escalar(otro) {
                out.push_str(&format!("{t}\n\n"));
            }
        }
    }
}

fn parsear(t: &str) -> Value {
    serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_string()))
}

fn vacio(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn cabecera(doc: &Value) -> String {
    let estado = doc["estadoDocumento"].as_str().unwrap_or("BORRADOR");
    let quien = doc["aprobacion"]["porNombre"].as_str().map(|n| format!(" · aprobado por {n}")).unwrap_or_default();
    let vinculante = if estado == "APROBADO" { "vinculante" } else { "NO vinculante hasta que se apruebe" };
    format!("> Estado: **{estado}**{quien} ({vinculante}). Generado por el portal; para cambiarlo, edítalo en el hub y vuelve a exportar.\n\n")
}

/// Archivos de la spec: (ruta relativa, contenido).
pub(crate) fn construir_archivos(sol: &Value, pruebas: &[Value]) -> Vec<(String, String)> {
    let nombre = sol["name"].as_str().or_else(|| sol["nombre"].as_str()).or_else(|| sol["title"].as_str()).unwrap_or("Proyecto");
    let mut f: Vec<(String, String)> = vec![];
    let mut indice = String::new();

    let doc = |clave: &str, titulo: &str, archivo: &str, f: &mut Vec<(String, String)>, indice: &mut String| {
        let d = parsear(sol[clave].as_str().unwrap_or(""));
        if vacio(&d) {
            return;
        }
        let mut m = format!("# {titulo} · {nombre}\n\n{}", cabecera(&d));
        cuerpo(&d, 2, &mut m);
        f.push((format!("spec/{archivo}"), m));
        indice.push_str(&format!("- [{titulo}]({archivo})\n"));
    };
    doc("prd", "PRD", "prd.md", &mut f, &mut indice);
    doc("disenoTecnico", "Diseño técnico", "diseno.md", &mut f, &mut indice);
    doc("arquitectura", "Arquitectura", "arquitectura.md", &mut f, &mut indice);
    doc("planEjecucion", "Plan de ejecución", "plan-ejecucion.md", &mut f, &mut indice);

    // Un archivo por requisito del PRD.
    let prd = parsear(sol["prd"].as_str().unwrap_or(""));
    let mut reqs = String::new();
    if let Some(a) = prd["requisitos"].as_array() {
        for (i, r) in a.iter().enumerate() {
            let n = format!("REQ-{:03}", i + 1);
            let titulo = ["titulo", "nombre", "texto", "descripcion"].iter().find_map(|k| r.get(*k).and_then(escalar)).unwrap_or_else(|| n.clone());
            let mut m = format!("# {n} · {}\n\n", titulo.lines().next().unwrap_or(""));
            if let Some(id) = r["id"].as_str() {
                m.push_str(&format!("`prdRequisitoId: {id}`\n\n"));
            }
            let aprobadas: Vec<&Value> = pruebas.iter().filter(|p| p["reqCodigo"] == n.as_str()).collect();
            if aprobadas.is_empty() {
                m.push_str("> Pruebas de aceptación: todavía no hay ninguna aprobada para este requisito (se derivan y se aprueban en el hub).\n\n");
            } else {
                m.push_str(&format!("> Pruebas de aceptación: [{n}](../pruebas/{n}.md). Cada prueba automática debe llevar «{n}» en su nombre.\n\n"));
                let mut p = format!("# Pruebas de aceptación · {n}\n\n> Aprobadas por una persona en el hub. Cada prueba automática que las implemente debe llevar «{n}» en su nombre.\n\n");
                for a in &aprobadas {
                    p.push_str(&format!("## {}\n\n{}\n\n", a["titulo"].as_str().unwrap_or(""), a["escenarios"].as_str().unwrap_or("")));
                }
                f.push((format!("spec/pruebas/{n}.md"), p));
            }
            cuerpo(r, 2, &mut m);
            f.push((format!("spec/requisitos/{n}.md"), m));
            reqs.push_str(&format!("- [{n}](requisitos/{n}.md) · {}\n", titulo.lines().next().unwrap_or("")));
        }
    }

    let mut readme = format!(
        "# Spec de {nombre}\n\nLa spec manda sobre el código: lo aprobado aquí es lo que se construye y se prueba. La genera el portal desde la Solución (hub › artefactos) y se actualiza con un PR; **no se edita a mano en este repositorio**: se cambia en el hub y se vuelve a exportar.\n\n## Documentos\n\n{indice}"
    );
    if !reqs.is_empty() {
        readme.push_str(&format!("\n## Requisitos\n\n{reqs}"));
    }
    f.insert(0, ("spec/README.md".into(), readme));
    if f.len() > 1 {
        f.push(("spec/herramientas/trazabilidad.mjs".into(), SCRIPT_TRAZABILIDAD.into()));
    }
    f
}

/// Revisor de trazabilidad para el repositorio del proyecto: ¿cada requisito tiene una prueba que lo cite? (`node spec/herramientas/trazabilidad.mjs [--estricto]`)
const SCRIPT_TRAZABILIDAD: &str = r####"#!/usr/bin/env node
// Revisor de trazabilidad SDD: requisito -> prueba. Lo genera el portal junto con la spec; no lo edites a mano.
// Uso: node spec/herramientas/trazabilidad.mjs [--estricto]   (con --estricto sale con error si falta una prueba o hay un REQ huérfano)
import fs from 'node:fs'
import path from 'node:path'

const raiz = process.cwd()
const dirReq = path.join(raiz, 'spec', 'requisitos')
const reqs = fs.existsSync(dirReq) ? fs.readdirSync(dirReq).filter(f => /^REQ-\d+\.md$/.test(f)).map(f => f.replace('.md', '')) : []

const IGNORAR = new Set(['node_modules', '.git', '.next', 'dist', 'build', 'spec', 'coverage'])
function recorrer(dir, salida = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (IGNORAR.has(e.name)) continue
    const p = path.join(dir, e.name)
    if (e.isDirectory()) recorrer(p, salida)
    else salida.push(p)
  }
  return salida
}
const esPrueba = f => /(\.test\.|\.spec\.|[\\/]tests?[\\/]|__tests__)/.test(f)
const citas = new Map()
for (const f of recorrer(raiz)) {
  if (!/\.(ts|tsx|js|jsx|mjs|cjs)$/.test(f)) continue
  const texto = fs.readFileSync(f, 'utf8')
  for (const m of texto.matchAll(/REQ-\d{3}/g)) {
    const e = citas.get(m[0]) ?? { pruebas: new Set(), codigo: new Set() }
    ;(esPrueba(f) ? e.pruebas : e.codigo).add(path.relative(raiz, f))
    citas.set(m[0], e)
  }
}
const sinPrueba = reqs.filter(r => !(citas.get(r)?.pruebas.size))
const huerfanos = [...citas.keys()].filter(k => !reqs.includes(k))
console.log(`Requisitos: ${reqs.length} · con prueba: ${reqs.length - sinPrueba.length} · sin prueba: ${sinPrueba.length}`)
if (sinPrueba.length) console.log('Sin prueba que los cite:\n  ' + sinPrueba.join('\n  '))
if (huerfanos.length) console.log('Citados en el código pero sin requisito en spec/requisitos:\n  ' + huerfanos.join('\n  '))
process.exit(process.argv.includes('--estricto') && (sinPrueba.length || huerfanos.length) ? 1 : 0)
"####;

pub async fn exportar(st: &AppState, solucion_id: &str) -> R<Value> {
    let sol = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(so) FROM "Solucion" so WHERE so.id = $1"#, &[B::T(solucion_id.into())]).await.map_err(|e| e.to_string())?.ok_or("La Solución no existe.")?;
    let raiz = repo::resolver_repo(st, Some(solucion_id)).await?;
    if raiz == PathBuf::from(repo::ruta_portal()) {
        return Err("Esta Solución no tiene repositorio propio: la spec solo se exporta a repositorios de proyecto, nunca al del portal.".into());
    }
    let pruebas = fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('reqCodigo', "reqCodigo", 'titulo', titulo, 'escenarios', escenarios) ORDER BY "reqCodigo", "createdAt"), '[]'::jsonb) FROM "PruebaAceptacion" WHERE "solucionId" = $1 AND estado = 'APROBADA'"#, &[B::T(solucion_id.into())]).await.map_err(|e| e.to_string())?;
    let archivos = construir_archivos(&sol, pruebas.as_array().map(|a| a.as_slice()).unwrap_or(&[]));
    if archivos.len() <= 1 {
        return Err("La Solución todavía no tiene PRD ni diseño con contenido para exportar.".into());
    }
    repo::git_con_credenciales(st, &["fetch", "origin", "main"], &raiz).await?;
    let corto: String = solucion_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    let wt = Path::new(&repo::dir_worktrees()).join(format!("spec-{corto}"));
    std::fs::create_dir_all(repo::dir_worktrees()).map_err(|e| e.to_string())?;
    if wt.exists() {
        repo::descartar_worktree(&wt, &raiz).await;
        let _ = std::fs::remove_dir_all(&wt);
    }
    repo::git(&["worktree", "add", "-B", RAMA, &wt.to_string_lossy(), "origin/main"], &raiz).await?;
    let resultado = publicar(st, &wt, &raiz, &archivos).await;
    repo::descartar_worktree(&wt, &raiz).await;
    resultado
}

async fn publicar(st: &AppState, wt: &Path, raiz: &Path, archivos: &[(String, String)]) -> R<Value> {
    let _ = std::fs::remove_dir_all(wt.join("spec"));
    for (ruta, contenido) in archivos {
        let destino = wt.join(ruta);
        if let Some(p) = destino.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::write(&destino, contenido).map_err(|e| e.to_string())?;
    }
    repo::git(&["add", "-A", "spec"], wt).await?;
    let (sin_cambios, _, _) = repo::sh_salida("git", &["diff", "--cached", "--quiet"], Some(wt), 30).await?;
    if sin_cambios {
        return Ok(json!({ "cambios": false, "archivos": archivos.len(), "mensaje": "La spec del repositorio ya está al día." }));
    }
    let mut a: Vec<&str> = IDENT.to_vec();
    a.extend(["commit", "-m", "Spec: actualización exportada desde el portal"]);
    repo::git(&a, wt).await?;
    let cuerpo_pr = "Actualización de la carpeta `spec/` exportada desde el portal (PRD, diseño, arquitectura, plan y requisitos).\n\nNo edites estos archivos a mano: se cambian en el hub de la Solución y se vuelven a exportar. Revisa y mergea manualmente; el motor nunca mergea solo.";
    let pr = repo::abrir_pr_sprint(st, RAMA, wt, "Spec: actualización exportada desde el portal", cuerpo_pr, raiz).await?;
    Ok(json!({ "cambios": true, "archivos": archivos.len(), "pr": pr }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genera_un_archivo_por_requisito_y_el_readme() {
        let prd = json!({ "estadoDocumento": "APROBADO", "resumen": "Un CRM", "requisitos": [
            { "id": "r1", "texto": "Registrar interesados", "prioridad": "MUST" },
            { "id": "r2", "texto": "Mapa público", "prioridad": "SHOULD" } ] });
        let sol = json!({ "name": "CRM", "prd": prd.to_string(), "disenoTecnico": "" });
        let f = construir_archivos(&sol, &[]);
        let rutas: Vec<&str> = f.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(rutas, vec!["spec/README.md", "spec/prd.md", "spec/requisitos/REQ-001.md", "spec/requisitos/REQ-002.md", "spec/herramientas/trazabilidad.mjs"]);
        assert!(f[1].1.contains("vinculante"));
        assert!(f[2].1.contains("prdRequisitoId: r1"));
    }

    #[test]
    fn solo_readme_si_no_hay_contenido() {
        let f = construir_archivos(&json!({ "name": "X", "prd": "", "disenoTecnico": "" }), &[]);
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn exporta_las_pruebas_aprobadas_por_requisito() {
        let prd = json!({ "requisitos": [{ "id": "r1", "texto": "Registrar interesados" }, { "id": "r2", "texto": "Mapa público" }] });
        let sol = json!({ "name": "CRM", "prd": prd.to_string() });
        let pruebas = vec![json!({ "reqCodigo": "REQ-002", "titulo": "Sin datos privados", "escenarios": "- **Dado** x\n  **Cuando** y\n  **Entonces** z" })];
        let f = construir_archivos(&sol, &pruebas);
        let rutas: Vec<&str> = f.iter().map(|(r, _)| r.as_str()).collect();
        assert!(rutas.contains(&"spec/pruebas/REQ-002.md") && !rutas.contains(&"spec/pruebas/REQ-001.md"));
        let r2 = f.iter().find(|(r, _)| r == "spec/requisitos/REQ-002.md").unwrap();
        assert!(r2.1.contains("../pruebas/REQ-002.md"));
        let r1 = f.iter().find(|(r, _)| r == "spec/requisitos/REQ-001.md").unwrap();
        assert!(r1.1.contains("todavía no hay ninguna aprobada"));
    }
}
