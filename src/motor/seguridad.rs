//! Revisión de seguridad con reglas fijas (MASD-0026-0004): corre sobre los archivos que escribió una tarea, antes de darla
//! por hecha. No reemplaza al verificador semántico ni a una persona: atrapa lo mecánico que ni las pruebas ni el juez LLM
//! suelen ver (rutas o acciones que modifican datos sin comprobar sesión o rol, secretos en el código, ejecución dinámica...).
//! Caso que motivó la regla: en la prueba del CRM, `crearUsuarioAction` (alta de jefatura) no comprobaba el rol y pasó pruebas y revisor.
//!
//! `MOTOR_SEGURIDAD=advertir` baja las graves a avisos (no bloquean); por defecto una gravedad ALTA marca la tarea como FALLIDA.

use std::{path::Path, sync::LazyLock};

use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub struct Hallazgo {
    pub gravedad: &'static str,
    pub regla: &'static str,
    pub archivo: String,
    pub detalle: String,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("regex")
}

static RUTA_API: LazyLock<Regex> = LazyLock::new(|| re(r"(^|/)(app/api/.+/route|pages/api/.+)\.(t|j)sx?$"));
static RUTA_PUBLICA: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(login|signin|sign-in|register|signup|webhook|health|publico|public|callback|logout)"));
static METODO_ESCRIBE: LazyLock<Regex> = LazyLock::new(|| re(r"export\s+(async\s+)?(function\s+(POST|PUT|PATCH|DELETE)\b|const\s+(POST|PUT|PATCH|DELETE)\b)"));
static MARCA_AUTH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(getToken|getServerSession|\bauth\(|requireUser|requireAdmin|requireRole|requirePermiso|exigirRol|exigirSesion|verifyToken|verificarSesion|verificarRol|getSession|currentUser|getCurrentUser|\bsession\b|authorization|cookies\(\)|\bjwt\b)")
});
static ACCION_SERVIDOR: LazyLock<Regex> = LazyLock::new(|| re(r#"^\s*(//[^\n]*\n|/\*.*?\*/\s*)*['"]use server['"]"#));
static ESCRIBE_BD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(prisma\.\w+\.(create|createMany|update|updateMany|delete|deleteMany|upsert)\b|\.insert\(|\.update\(|\.delete\(|INSERT\s+INTO|UPDATE\s+\w+\s+SET|DELETE\s+FROM)"));
static SECRETO: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)(api[_-]?key|secret|passw(or)?d|token|private[_-]?key)\w*["']?\s*[:=]\s*["'][^"'\s$`{}]{12,}["']"#));
static EVAL: LazyLock<Regex> = LazyLock::new(|| re(r"\beval\s*\(|new\s+Function\s*\("));
static SQL_CRUDO: LazyLock<Regex> = LazyLock::new(|| re(r"\$(queryRawUnsafe|executeRawUnsafe)\s*\(.*\$\{"));
static HTML_PELIGROSO: LazyLock<Regex> = LazyLock::new(|| re(r"dangerouslySetInnerHTML"));
static CORS_ABIERTO: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)(Access-Control-Allow-Origin['"]?\s*[:,]\s*['"]\*['"]|origin\s*:\s*['"]\*['"])"#));
static TLS_APAGADO: LazyLock<Regex> = LazyLock::new(|| re(r#"rejectUnauthorized\s*:\s*false|NODE_TLS_REJECT_UNAUTHORIZED\s*=\s*['"]?0"#));

fn es_codigo(ruta: &str) -> bool {
    let sin_prueba = !(ruta.contains("node_modules") || ruta.contains(".next/") || ruta.contains(".test.") || ruta.contains(".spec.") || ruta.contains("/tests/") || ruta.contains("/__tests__/") || ruta.starts_with("tests/"));
    sin_prueba && [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"].iter().any(|e| ruta.ends_with(e))
}

/// Revisa el contenido completo de un archivo de código.
pub fn revisar_archivo(ruta: &str, contenido: &str) -> Vec<Hallazgo> {
    let mut h = vec![];
    if !es_codigo(ruta) {
        return h;
    }
    let mut poner = |gravedad: &'static str, regla: &'static str, detalle: String| h.push(Hallazgo { gravedad, regla, archivo: ruta.to_string(), detalle });

    if RUTA_API.is_match(ruta) && !RUTA_PUBLICA.is_match(ruta) && METODO_ESCRIBE.is_match(contenido) && !MARCA_AUTH.is_match(contenido) {
        poner("ALTA", "sesion-en-ruta", "La ruta modifica datos (POST/PUT/PATCH/DELETE) y no comprueba sesión ni rol. Valida quién llama y qué rol tiene antes de escribir.".into());
    }
    if ACCION_SERVIDOR.is_match(contenido) && ESCRIBE_BD.is_match(contenido) && !MARCA_AUTH.is_match(contenido) {
        poner("ALTA", "rol-en-accion", "La acción de servidor escribe en la base y no comprueba sesión ni rol. Una acción de servidor se puede invocar directamente: valida quién la llama y su rol dentro de la propia acción.".into());
    }
    for (n, linea) in contenido.lines().enumerate() {
        let l = linea.trim();
        if l.starts_with("//") || l.starts_with('*') {
            continue;
        }
        let en = |r: &Regex| r.is_match(linea);
        let ejemplo = |t: &str| t.contains("process.env") || t.to_lowercase().contains("example") || t.to_lowercase().contains("placeholder") || t.to_lowercase().contains("changeme");
        if en(&SECRETO) && !ejemplo(linea) {
            poner("ALTA", "secreto-en-codigo", format!("Línea {}: parece un secreto escrito en el código. Léelo de una variable de entorno.", n + 1));
        }
        if en(&EVAL) {
            poner("ALTA", "ejecucion-dinamica", format!("Línea {}: eval o new Function ejecutan texto como código.", n + 1));
        }
        if en(&SQL_CRUDO) {
            poner("ALTA", "sql-concatenado", format!("Línea {}: consulta SQL cruda con datos interpolados. Usa parámetros.", n + 1));
        }
        if en(&HTML_PELIGROSO) {
            poner("MEDIA", "html-sin-sanear", format!("Línea {}: dangerouslySetInnerHTML; asegúrate de sanear el contenido.", n + 1));
        }
        if en(&CORS_ABIERTO) {
            poner("MEDIA", "cors-abierto", format!("Línea {}: CORS abierto a cualquier origen.", n + 1));
        }
        if en(&TLS_APAGADO) {
            poner("MEDIA", "tls-apagado", format!("Línea {}: se desactiva la verificación de TLS.", n + 1));
        }
    }
    h
}

/// Revisa los archivos que escribió una tarea (rutas relativas a la raíz del repo o worktree).
pub async fn revisar_tarea(raiz: &Path, archivos: &[String]) -> Vec<Hallazgo> {
    let mut out = vec![];
    for a in archivos {
        let ruta = a.trim_start_matches("./");
        if !es_codigo(ruta) {
            continue;
        }
        if let Ok(c) = tokio::fs::read_to_string(raiz.join(ruta)).await {
            out.extend(revisar_archivo(ruta, &c));
        }
    }
    out
}

/// `true` si las graves solo deben avisar (`MOTOR_SEGURIDAD=advertir`).
pub fn solo_advertir() -> bool {
    std::env::var("MOTOR_SEGURIDAD").map(|v| v.eq_ignore_ascii_case("advertir")).unwrap_or(false)
}

/// Texto breve para la traza y el checklist.
pub fn resumir(h: &[Hallazgo]) -> String {
    h.iter().take(8).map(|x| format!("[{}] {} · {} — {}", x.gravedad, x.regla, x.archivo, x.detalle)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accion_de_servidor_sin_rol_es_grave() {
        let c = "'use server'\nimport { prisma } from '@/lib/db'\nexport async function crearUsuarioAction(d: FormData) {\n  await prisma.user.create({ data: { rol: 'jefatura' } })\n}\n";
        let h = revisar_archivo("app/(app)/admin/actions.ts", c);
        assert!(h.iter().any(|x| x.regla == "rol-en-accion" && x.gravedad == "ALTA"));
    }

    #[test]
    fn accion_con_comprobacion_de_rol_pasa() {
        let c = "'use server'\nexport async function crearUsuarioAction(d: FormData) {\n  const s = await getSession()\n  if (s.rol !== 'admin') throw new Error('no')\n  await prisma.user.create({ data: {} })\n}\n";
        assert!(revisar_archivo("app/admin/actions.ts", c).is_empty());
    }

    #[test]
    fn ruta_que_escribe_sin_sesion_es_grave_pero_login_no() {
        let c = "export async function POST(req: Request) { await prisma.lote.update({ where: {}, data: {} }); return Response.json({}) }";
        assert!(revisar_archivo("app/api/lotes/route.ts", c).iter().any(|x| x.regla == "sesion-en-ruta"));
        assert!(revisar_archivo("app/api/login/route.ts", c).is_empty());
        let con = "import { getToken } from 'next-auth/jwt'\nexport async function POST(req: Request) { const t = await getToken({ req }); return Response.json({}) }";
        assert!(revisar_archivo("app/api/lotes/route.ts", con).is_empty());
    }

    #[test]
    fn detecta_secretos_y_eval_pero_no_variables_de_entorno() {
        let c = "const apiKey = 'sk_live_abcdefghijklmnop1234'\nconst ok = process.env.API_KEY\neval(x)\n";
        let h = revisar_archivo("lib/x.ts", c);
        assert!(h.iter().any(|x| x.regla == "secreto-en-codigo"));
        assert!(h.iter().any(|x| x.regla == "ejecucion-dinamica"));
        assert_eq!(h.iter().filter(|x| x.regla == "secreto-en-codigo").count(), 1);
    }

    #[test]
    fn ignora_pruebas_y_archivos_que_no_son_codigo() {
        assert!(revisar_archivo("lib/x.test.ts", "eval(1)").is_empty());
        assert!(revisar_archivo("README.md", "eval(1)").is_empty());
    }
}
