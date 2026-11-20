//! Verificación con pruebas de aceptación (MASD-0026-0004): «hecho» = las pruebas pasan.
//!
//! Solo se exige cuando el requisito que construye la tarea (`prdRequisitoId`) tiene pruebas de aceptación APROBADAS por una
//! persona. Entonces se comprueba, en este orden: (1) que alguna prueba del repositorio cite el código REQ-###; (2) que el
//! proyecto tenga `npm test`; (3) que haya dependencias instaladas; (4) que las pruebas PASEN, ejecutadas en un contenedor
//! aislado (sin red, sin privilegios, con límites de memoria, CPU y procesos), nunca en el servidor.
//! `MOTOR_PRUEBAS`: `exigir` (por defecto: si falla, la tarea falla), `informar` (se ejecutan y se anota el resultado sin
//! bloquear) o `apagado`.

use std::path::Path;

use crate::{
    state::AppState,
    util::{fetch_json, B},
};

use super::repo::sh_salida;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Modo {
    Exigir,
    Informar,
    Apagado,
}

pub fn modo() -> Modo {
    match std::env::var("MOTOR_PRUEBAS").unwrap_or_default().to_lowercase().as_str() {
        "informar" => Modo::Informar,
        "apagado" => Modo::Apagado,
        _ => Modo::Exigir,
    }
}

pub struct Resultado {
    pub paso: bool,
    pub detalle: String,
}

/// ¿El `package.json` define un script `test` que no sea el de relleno de `npm init`?
pub(crate) fn tiene_script_test(package_json: &str) -> bool {
    let Ok(j) = serde_json::from_str::<serde_json::Value>(package_json) else { return false };
    match j["scripts"]["test"].as_str() {
        Some(t) => !t.trim().is_empty() && !t.contains("no test specified"),
        None => false,
    }
}

/// ¿El texto cita el código del requisito (por ejemplo `REQ-003`)?
pub(crate) fn cita_requisito(texto: &str, req: &str) -> bool {
    texto.contains(req)
}

fn es_archivo_de_prueba(ruta: &str) -> bool {
    let r = ruta.replace('\\', "/");
    (r.contains(".test.") || r.contains(".spec.") || r.contains("/tests/") || r.contains("/__tests__/") || r.starts_with("tests/")) && [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"].iter().any(|e| r.ends_with(e))
}

fn alguna_prueba_cita(dir: &Path, raiz: &Path, req: &str, vistos: &mut usize) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        let nombre = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if ["node_modules", ".git", ".next", "dist", "build", "coverage", "spec"].contains(&nombre.as_str()) {
                continue;
            }
            if alguna_prueba_cita(&p, raiz, req, vistos) {
                return true;
            }
        } else if *vistos < 4000 {
            *vistos += 1;
            let rel = p.strip_prefix(raiz).unwrap_or(&p).to_string_lossy().to_string();
            if es_archivo_de_prueba(&rel) && std::fs::read_to_string(&p).map(|t| cita_requisito(&t, req)).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Código y cantidad de pruebas aprobadas del requisito que construye una tarea (`None` si no hay).
pub async fn requisito_con_pruebas_aprobadas(st: &AppState, solucion_id: &str, req_id: &str) -> Option<String> {
    let r = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(to_jsonb(MIN("reqCodigo")), 'null'::jsonb) FROM "PruebaAceptacion" WHERE "solucionId" = $1 AND "reqId" = $2 AND estado = 'APROBADA'"#,
        &[B::T(solucion_id.into()), B::T(req_id.into())],
    )
    .await
    .ok()?;
    r.as_str().map(String::from)
}

/// Comprueba y ejecuta las pruebas del proyecto para el requisito `req`.
pub async fn verificar(raiz: &Path, req: &str) -> Resultado {
    let mut vistos = 0;
    if !alguna_prueba_cita(raiz, raiz, req, &mut vistos) {
        return Resultado { paso: false, detalle: format!("Este requisito tiene pruebas de aceptación aprobadas, pero ninguna prueba automática del repositorio cita «{req}». Escribe las pruebas y nombra cada una incluyendo «{req}»." ) };
    }
    let Ok(pkg) = std::fs::read_to_string(raiz.join("package.json")) else {
        return Resultado { paso: false, detalle: "El proyecto no tiene package.json: no hay forma de ejecutar las pruebas.".into() };
    };
    if !tiene_script_test(&pkg) {
        return Resultado { paso: false, detalle: "El proyecto no define el script `npm test`. Añádelo (por ejemplo con vitest o jest) para poder ejecutar las pruebas de aceptación.".into() };
    }
    if !raiz.join("node_modules").exists() {
        return Resultado { paso: false, detalle: "No hay dependencias instaladas (falta `npm install`), así que las pruebas no se pueden ejecutar.".into() };
    }
    let nombre = format!("pruebas-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) % 100_000_000);
    let montaje = format!("{}:/work", raiz.to_string_lossy());
    // El código de salida de npm se guarda aparte: `sh` de Alpine no tiene PIPESTATUS.
    let script = "npm test --silent > /tmp/salida.txt 2>&1; c=$?; tail -c 3500 /tmp/salida.txt; exit $c";
    let a: Vec<&str> = vec![
        "run", "--rm", "--name", nombre.as_str(), "--memory=2g", "--memory-swap=2g", "--cpus=2", "--pids-limit=512", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--network=none",
        "--tmpfs", "/tmp:rw,size=256m", "-v", montaje.as_str(), "-w", "/work", "-e", "CI=1", "node:20-alpine", "sh", "-c", script,
    ];
    match sh_salida("docker", &a, None, 240).await {
        Err(e) => {
            let _ = sh_salida("docker", &["rm", "-f", &nombre], None, 30).await;
            Resultado { paso: false, detalle: format!("Las pruebas se detuvieron antes de terminar: {}", e.chars().take(200).collect::<String>()) }
        }
        Ok((ok, out, err)) => {
            let salida = format!("{out}{err}");
            let cola: String = salida.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            Resultado { paso: ok, detalle: if ok { format!("Las pruebas pasan. Final de la salida:\n{cola}") } else { format!("Las pruebas fallan. Final de la salida:\n{cola}") } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_un_script_de_prueba_real() {
        assert!(tiene_script_test(r#"{"scripts":{"test":"vitest run"}}"#));
        assert!(!tiene_script_test(r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#));
        assert!(!tiene_script_test(r#"{"scripts":{"build":"next build"}}"#));
        assert!(!tiene_script_test("no es json"));
    }

    #[test]
    fn la_cita_del_requisito_se_busca_en_el_texto() {
        assert!(cita_requisito("it('REQ-003 · recordatorio 24 h antes', () => {})", "REQ-003"));
        assert!(!cita_requisito("it('recordatorio', () => {})", "REQ-003"));
    }

    #[test]
    fn solo_cuentan_los_archivos_de_prueba() {
        assert!(es_archivo_de_prueba("src/lib/x.test.ts"));
        assert!(es_archivo_de_prueba("tests/aceptacion.js"));
        assert!(es_archivo_de_prueba("src/__tests__/a.tsx"));
        assert!(!es_archivo_de_prueba("src/lib/x.ts"));
        assert!(!es_archivo_de_prueba("README.md"));
    }

    #[test]
    fn el_modo_por_defecto_exige() {
        std::env::remove_var("MOTOR_PRUEBAS");
        assert_eq!(modo(), Modo::Exigir);
    }
}
