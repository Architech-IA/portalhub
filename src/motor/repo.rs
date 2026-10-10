//! Repositorios y worktrees de git del Motor (`lib/executor/repoConfig.ts` y `gitWorktree.ts`).
//! Solo corre en el servicio privilegiado: toca `/root/repos`, `/root/worktrees` y usa el token de GitHub.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde_json::{json, Value};
use tokio::process::Command;

use crate::{
    state::AppState,
    util::{exec, fetch_json_opt, B},
};

pub type R<T> = Result<T, String>;

const REPOS_EXTERNOS: &str = "/root/repos";
/// Carpeta de worktrees (`MOTOR_WORKTREES` solo se usa en las pruebas).
pub fn dir_worktrees() -> String {
    std::env::var("MOTOR_WORKTREES").unwrap_or_else(|_| "/root/worktrees".to_string())
}
const IDENTIDAD_GIT: [&str; 4] = ["-c", "user.email=masd@architechia.local", "-c", "user.name=Motor Agéntico SDD"];

pub fn ruta_portal() -> String {
    std::env::var("PORTAL_REPO_PATH").unwrap_or_else(|_| "/root/portal-architechia".to_string())
}

fn org_github() -> String {
    std::env::var("GITHUB_ORG").unwrap_or_else(|_| "Architech-IA".to_string())
}

/// Ejecuta un comando y devuelve su stdout; si falla, el error lleva la salida de error (como `execFile`).
pub async fn sh(cmd: &str, args: &[&str], cwd: Option<&Path>, timeout_s: u64) -> R<String> {
    let mut c = Command::new(cmd);
    c.args(args).kill_on_drop(true);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let salida = match tokio::time::timeout(std::time::Duration::from_secs(timeout_s), c.output()).await {
        Err(_) => return Err(format!("Command failed: {cmd} {} (timeout de {timeout_s}s)", args.join(" "))),
        Ok(Err(e)) => return Err(format!("No se pudo ejecutar {cmd}: {e}")),
        Ok(Ok(o)) => o,
    };
    if salida.status.success() {
        Ok(String::from_utf8_lossy(&salida.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&salida.stderr);
        let out = String::from_utf8_lossy(&salida.stdout);
        Err(format!("Command failed: {cmd} {}\n{}{}", args.join(" "), out, err).trim_end().to_string())
    }
}

pub async fn git(args: &[&str], cwd: &Path) -> R<String> {
    sh("git", args, Some(cwd), 300).await
}

/// git hacia GitHub con credenciales: el token viaja en una cabecera solo para este comando, nunca en la URL del remoto ni en los
/// mensajes de error. Los repositorios independientes se clonan sin token en la URL, así que sin esto `push` y `fetch` de un repo privado fallan.
pub async fn git_con_credenciales(st: &AppState, args: &[&str], cwd: &Path) -> R<String> {
    use base64::Engine;
    let Some(token) = st.cfg.github_token.clone().filter(|t| !t.is_empty()) else { return git(args, cwd).await };
    let b64 = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    let cabecera = format!("http.https://github.com/.extraheader=AUTHORIZATION: basic {b64}");
    let mut a: Vec<&str> = vec!["-c", cabecera.as_str()];
    a.extend_from_slice(args);
    git(&a, cwd).await.map_err(|e| e.replace(&b64, "***").replace(&token, "***"))
}

// ── GitHub ───────────────────────────────────────────────────────────────────────────────────
async fn github_api(st: &AppState, ruta: &str, metodo: reqwest::Method, cuerpo: Option<Value>) -> Result<reqwest::Response, String> {
    let token = st.cfg.github_token.clone().unwrap_or_default();
    let mut rb = st
        .http
        .request(metodo, format!("https://api.github.com{ruta}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "portalhub-motor")
        .timeout(std::time::Duration::from_secs(60));
    if let Some(c) = cuerpo {
        rb = rb.json(&c);
    }
    rb.send().await.map_err(|e| e.to_string())
}

static CUENTA_ES_USER: Mutex<Option<bool>> = Mutex::new(None);

async fn es_cuenta_de_usuario(st: &AppState) -> bool {
    if let Some(v) = *CUENTA_ES_USER.lock().unwrap_or_else(|e| e.into_inner()) {
        return v;
    }
    let res = match github_api(st, "/user", reqwest::Method::GET, None).await {
        Ok(r) if r.status().is_success() => r,
        _ => {
            *CUENTA_ES_USER.lock().unwrap_or_else(|e| e.into_inner()) = Some(false);
            return false;
        }
    };
    let d: Value = res.json().await.unwrap_or_else(|_| json!({}));
    let v = d["type"] == "User" && d["login"].as_str().map(|l| l.to_lowercase() == org_github().to_lowercase()).unwrap_or(false);
    *CUENTA_ES_USER.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
    v
}

fn sin_acento(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        otro => otro,
    }
}

/// Nombre válido de repositorio de GitHub: minúsculas, `[a-z0-9-]`, sin guiones repetidos ni en las puntas.
pub fn slug_texto(texto: &str, max: usize, defecto: &str) -> String {
    let mut s = String::new();
    let mut guion = false;
    for c in texto.to_lowercase().chars().map(sin_acento) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c);
            guion = false;
        } else if !guion {
            s.push('-');
            guion = true;
        }
    }
    let s = s.trim_matches('-').to_string();
    let s: String = s.chars().take(max).collect();
    if s.is_empty() {
        defecto.to_string()
    } else {
        s
    }
}

pub fn slug_repo(texto: &str) -> String {
    slug_texto(texto, 60, "proyecto")
}

/// Crea el repo en GitHub bajo `GITHUB_ORG` si no existe y lo clona en `/root/repos/<repositorio>`.
pub async fn asegurar_repo_externo(st: &AppState, repositorio: &str, privado: bool) -> R<(PathBuf, bool)> {
    let local = Path::new(REPOS_EXTERNOS).join(repositorio);
    if local.exists() {
        return Ok((local, false));
    }
    std::fs::create_dir_all(REPOS_EXTERNOS).map_err(|e| e.to_string())?;
    let Some(token) = st.cfg.github_token.clone() else {
        return Err(format!("No se puede crear/clonar el repositorio independiente \"{repositorio}\": falta GITHUB_TOKEN en el entorno."));
    };
    let org = org_github();
    let check = github_api(st, &format!("/repos/{org}/{repositorio}"), reqwest::Method::GET, None).await?;
    let mut creado = false;
    if check.status().as_u16() == 404 {
        let para_usuario = es_cuenta_de_usuario(st).await;
        let ruta = if para_usuario { "/user/repos".to_string() } else { format!("/orgs/{org}/repos") };
        let r = github_api(
            st,
            &ruta,
            reqwest::Method::POST,
            Some(json!({ "name": repositorio, "private": privado, "auto_init": true, "description": "Producto/demo independiente generado por el Motor Agéntico SDD de ArchiTechIA" })),
        )
        .await?;
        if !r.status().is_success() {
            let est = r.status().as_u16();
            let t = r.text().await.unwrap_or_default();
            return Err(format!("No se pudo crear el repositorio {org}/{repositorio} en GitHub ({}): {est} {t}", if para_usuario { "como cuenta de usuario" } else { "como organización" }));
        }
        creado = true;
    } else if !check.status().is_success() {
        return Err(format!("Error consultando el repositorio {org}/{repositorio} en GitHub: {}", check.status().as_u16()));
    }
    let destino = local.to_string_lossy().to_string();
    sh("git", &["clone", &format!("https://{token}@github.com/{org}/{repositorio}.git"), &destino], None, 600).await.map_err(|e| e.replace(&token, "***"))?;
    // El token queda embebido en la URL solo durante el clone: se reescribe el remote sin él.
    git(&["remote", "set-url", "origin", &format!("https://github.com/{org}/{repositorio}.git")], &local).await?;
    if creado {
        politica_por_defecto(&local).await;
    }
    if local.join("package.json").exists() {
        if let Err(e) = sh("npm", &["install"], Some(&local), 900).await {
            tracing::error!("[REPO_CONFIG] npm install falló en {repositorio} (no bloqueante): {e}");
        }
    }
    Ok((local, creado))
}

/// Política de escritura de los agentes para un repositorio NUEVO de proyecto (la lee `file_tools.py` del worker; el agente no puede
/// modificarla). Además de las rutas habituales (src/, app/, tests/, docs/…) permite la configuración de Next.js —el prompt del worker
/// pide `output: 'standalone'`, que el despliegue aprovecha— y el esquema de Prisma, que hace falta para migraciones. El repo del portal
/// NO la tiene: ahí esos archivos siguen protegidos.
const POLITICA_PROYECTO: &str = "{\n  \"escritura\": [\"next.config.js\", \"next.config.mjs\", \"next.config.ts\", \"prisma/schema.prisma\"]\n}\n";

async fn politica_por_defecto(local: &Path) {
    let archivo = local.join(".masd-policy.json");
    if archivo.exists() || std::fs::write(&archivo, POLITICA_PROYECTO).is_err() {
        return;
    }
    let _ = git(&["add", ".masd-policy.json"], local).await;
    let mut a: Vec<&str> = IDENTIDAD_GIT.to_vec();
    a.extend(["commit", "-m", "Política de escritura de los agentes (Motor Agéntico SDD)"]);
    if let Err(e) = git(&a, local).await {
        tracing::error!("[REPO_CONFIG] no se pudo commitear .masd-policy.json en {}: {e}", local.display());
    }
}

/// Crea (o reutiliza) un repositorio de GitHub para una Solución y lo asocia. No pisa uno ya asociado.
pub async fn crear_repositorio_para_solucion(st: &AppState, solucion_id: &str, nombre: &str, privado: bool) -> R<Value> {
    let sol = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('repositorio', repositorio) FROM "Solucion" WHERE id = $1"#, &[B::T(solucion_id.into())]).await.map_err(|e| e.to_string())?;
    let Some(sol) = sol else { return Err("La Solución no existe.".into()) };
    if let Some(r) = sol["repositorio"].as_str().map(str::trim).filter(|x| !x.is_empty()) {
        return Err(format!("Esta Solución ya tiene un repositorio asociado ({r}). Si hay que cambiarlo, se saca primero a mano desde el Hub de la Solución."));
    }
    let repo = slug_repo(nombre);
    let (_, creado) = asegurar_repo_externo(st, &repo, privado).await?;
    exec(&st.pool, r#"UPDATE "Solucion" SET repositorio = $1, "updatedAt" = NOW() WHERE id = $2"#, &[B::T(repo.clone()), B::T(solucion_id.into())]).await.map_err(|e| e.to_string())?;
    Ok(json!({ "repoName": repo, "url": format!("https://github.com/{}/{repo}", org_github()), "creado": creado }))
}

/// El campo `Solucion.repositorio` puede traer la URL completa de GitHub: se queda solo con el nombre.
fn nombre_de_repo(valor: &str) -> String {
    static R: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)github\.com[:/]+([^/]+)/([^/.]+?)(?:\.git)?/?$").expect("re"));
    R.captures(valor).and_then(|c| c.get(2)).map(|m| m.as_str().to_string()).unwrap_or_else(|| valor.to_string())
}

/// Dónde vive el código de una Solución: el repo del portal o uno independiente (clonado aparte).
pub async fn resolver_repo(st: &AppState, solucion_id: Option<&str>) -> R<PathBuf> {
    let portal = || PathBuf::from(ruta_portal());
    let Some(id) = solucion_id.filter(|s| !s.is_empty()) else { return Ok(portal()) };
    let fila = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('repositorio', repositorio) FROM "Solucion" WHERE id = $1"#, &[B::T(id.into())]).await.map_err(|e| e.to_string())?;
    let crudo = fila.as_ref().and_then(|f| f["repositorio"].as_str()).map(str::trim).unwrap_or("").to_string();
    if crudo.is_empty() || crudo == "portal-architechia" {
        return Ok(portal());
    }
    let repo = nombre_de_repo(&crudo);
    if repo == "portal-architechia" {
        return Ok(portal());
    }
    Ok(asegurar_repo_externo(st, &repo, true).await?.0)
}

// ── Worktrees ────────────────────────────────────────────────────────────────────────────────
pub fn rama_sprint(codigo: &str) -> String {
    format!("masd/sprint-{codigo}")
}
pub fn rama_tarea(codigo: &str) -> String {
    format!("masd/{codigo}")
}
pub fn worktree_sprint(codigo: &str) -> PathBuf {
    Path::new(&dir_worktrees()).join(format!("sprint-{codigo}"))
}
pub fn worktree_tarea(codigo: &str) -> PathBuf {
    Path::new(&dir_worktrees()).join(codigo)
}

#[derive(Debug)]
pub enum ErrorMerge {
    Conflicto { rama: String, archivos: Vec<String> },
    Otro(String),
}

async fn rama_existe(rama: &str, raiz: &Path) -> bool {
    git(&["rev-parse", "--verify", rama], raiz).await.is_ok()
}

/// Rama y worktree de integración del sprint (`masd/sprint-<code>`), creados desde `main` la primera vez.
pub async fn asegurar_rama_sprint(codigo: &str, raiz: &Path) -> R<(String, PathBuf)> {
    let rama = rama_sprint(codigo);
    let wt = worktree_sprint(codigo);
    std::fs::create_dir_all(dir_worktrees()).map_err(|e| e.to_string())?;
    if !rama_existe(&rama, raiz).await {
        git(&["branch", &rama, "main"], raiz).await?;
    }
    if !wt.exists() {
        git(&["worktree", "add", &wt.to_string_lossy(), &rama], raiz).await?;
    }
    Ok((rama, wt))
}

/// Worktree aislado para una tarea CODE, ramificado desde `base`.
pub async fn crear_worktree_tarea(codigo: &str, base: &str, raiz: &Path) -> R<(String, PathBuf)> {
    let rama = rama_tarea(codigo);
    let wt = worktree_tarea(codigo);
    std::fs::create_dir_all(dir_worktrees()).map_err(|e| e.to_string())?;
    let wt_s = wt.to_string_lossy().to_string();
    if wt.exists() {
        let _ = git(&["worktree", "remove", &wt_s, "--force"], raiz).await;
    }
    if rama_existe(&rama, raiz).await {
        let _ = git(&["branch", "-D", &rama], raiz).await;
    }
    git(&["worktree", "add", "-b", &rama, &wt_s, base], raiz).await?;
    // node_modules no es parte del historial: se enlaza el real en lugar de instalar por tarea.
    let real = raiz.join("node_modules");
    let enlace = wt.join("node_modules");
    if real.exists() && !enlace.exists() {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&real, &enlace);
    }
    Ok((rama, wt))
}

/// Al cerrar una tarea CODE en DONE: commitea su worktree y lo mergea a la rama de integración del sprint.
pub async fn commit_y_merge(codigo: &str, wt_tarea: &Path, rama_tarea_: &str, wt_sprint: &Path, raiz: &Path) -> Result<bool, ErrorMerge> {
    let otro = |e: String| ErrorMerge::Otro(e);
    let estado = git(&["status", "--porcelain"], wt_tarea).await.map_err(otro)?;
    if !estado.trim().is_empty() {
        git(&["add", "-A"], wt_tarea).await.map_err(otro)?;
        let mut a: Vec<&str> = IDENTIDAD_GIT.to_vec();
        let msg = format!("{codigo}: cambios generados por el agente");
        a.extend(["commit", "-m", &msg]);
        git(&a, wt_tarea).await.map_err(otro)?;
    }
    let mut mergeado = false;
    let log = git(&["log", &format!("main..{rama_tarea_}"), "--oneline"], raiz).await.map_err(otro)?;
    if !log.trim().is_empty() {
        let mut a: Vec<&str> = IDENTIDAD_GIT.to_vec();
        let msg = format!("Merge {codigo} a la rama de integración del sprint");
        a.extend(["merge", "--no-ff", rama_tarea_, "-m", &msg]);
        match git(&a, wt_sprint).await {
            Ok(_) => mergeado = true,
            Err(_) => {
                let conf = git(&["diff", "--name-only", "--diff-filter=U"], wt_sprint).await.unwrap_or_default();
                let archivos: Vec<String> = conf.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                let _ = git(&["merge", "--abort"], wt_sprint).await;
                return Err(ErrorMerge::Conflicto { rama: rama_tarea_.to_string(), archivos });
            }
        }
    }
    let _ = git(&["worktree", "remove", &wt_tarea.to_string_lossy(), "--force"], raiz).await;
    Ok(mergeado)
}

/// Cambios reales de una tarea en su worktree (todavía sin commitear), como parche de git. Se le muestran al verificador para que juzgue el
/// código y no solo el resumen que escribió el agente. `add -A` es lo mismo que hace `commit_y_merge` más adelante; se dejan afuera los
/// archivos de bloqueo, que son enormes y no dicen nada del trabajo.
pub async fn diff_de_tarea(wt: &Path) -> String {
    let _ = git(&["add", "-A"], wt).await;
    git(&["diff", "--cached", "--no-color", "--unified=2", "--", ".", ":(exclude)package-lock.json", ":(exclude)pnpm-lock.yaml"], wt).await.unwrap_or_default()
}

pub async fn descartar_worktree(wt: &Path, raiz: &Path) {
    let _ = git(&["worktree", "remove", &wt.to_string_lossy(), "--force"], raiz).await;
}

/// Abre (o reutiliza) el PR de la rama de integración del sprint hacia `main`. Nunca mergea sola.
pub async fn abrir_pr_sprint(st: &AppState, rama: &str, wt_sprint: &Path, titulo: &str, cuerpo: &str, raiz: &Path) -> R<Option<String>> {
    git_con_credenciales(st, &["push", "-u", "origin", rama, "--force"], wt_sprint).await?;
    let remoto = git(&["remote", "get-url", "origin"], raiz).await?.trim().to_string();
    static R: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"github\.com[:/]([^/]+)/([^/.]+)").expect("re"));
    let Some(c) = R.captures(&remoto) else { return Ok(None) };
    let (owner, repo) = (c[1].to_string(), c[2].to_string());
    if st.cfg.github_token.is_none() {
        return Ok(None);
    }
    let existente = github_api(st, &format!("/repos/{owner}/{repo}/pulls?head={owner}:{rama}&state=open"), reqwest::Method::GET, None).await?;
    let lista: Value = existente.json().await.unwrap_or(Value::Null);
    if let Some(url) = lista.as_array().and_then(|a| a.first()).and_then(|p| p["html_url"].as_str()) {
        return Ok(Some(url.to_string()));
    }
    let r = github_api(st, &format!("/repos/{owner}/{repo}/pulls"), reqwest::Method::POST, Some(json!({ "title": titulo, "body": cuerpo, "head": rama, "base": "main" }))).await?;
    if !r.status().is_success() {
        let est = r.status().as_u16();
        return Err(format!("GitHub API error creando PR: {est} {}", r.text().await.unwrap_or_default()));
    }
    let d: Value = r.json().await.unwrap_or(Value::Null);
    Ok(d["html_url"].as_str().map(String::from))
}

/// Como `sh`, pero devuelve `(ok, stdout, stderr)` sin convertir el fallo en error (para leer la salida de `tsc`).
pub async fn sh_salida(cmd: &str, args: &[&str], cwd: Option<&Path>, timeout_s: u64) -> R<(bool, String, String)> {
    let mut c = Command::new(cmd);
    c.args(args).kill_on_drop(true);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_s), c.output()).await {
        Err(_) => Err(format!("Command failed: {cmd} {} (timeout de {timeout_s}s)", args.join(" "))),
        Ok(Err(e)) => Err(format!("No se pudo ejecutar {cmd}: {e}")),
        Ok(Ok(o)) => Ok((o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())),
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    async fn g(args: &[&str], cwd: &Path) -> String {
        git(args, cwd).await.unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    /// Ciclo completo de una tarea CODE contra un repo local de juguete: rama del sprint, worktree de la tarea,
    /// commit + merge sin conflicto, y un conflicto REAL entre dos tareas (que debe abortar y dejar el sprint limpio).
    #[tokio::test]
    async fn worktrees_merge_y_conflicto() {
        let base = std::env::temp_dir().join(format!("motor-test-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::env::set_var("MOTOR_WORKTREES", base.join("wt").to_string_lossy().to_string());
        g(&["init", "-b", "main"], &repo).await;
        std::fs::write(repo.join("a.txt"), "uno\n").unwrap();
        g(&["add", "-A"], &repo).await;
        let mut c: Vec<&str> = IDENTIDAD_GIT.to_vec();
        c.extend(["commit", "-m", "inicial"]);
        g(&c, &repo).await;

        let (rama, wt_sprint) = asegurar_rama_sprint("T-0001", &repo).await.unwrap();
        assert_eq!(rama, "masd/sprint-T-0001");
        assert!(wt_sprint.exists());
        // idempotente
        asegurar_rama_sprint("T-0001", &repo).await.unwrap();

        // Tarea 1: edita a.txt y crea b.txt
        let (r1, w1) = crear_worktree_tarea("T-0001-001", &rama, &repo).await.unwrap();
        std::fs::write(w1.join("a.txt"), "uno\nuno-de-tarea-1\n").unwrap();
        std::fs::write(w1.join("b.txt"), "nuevo\n").unwrap();
        assert!(commit_y_merge("T-0001-001", &w1, &r1, &wt_sprint, &repo).await.unwrap());
        assert!(!w1.exists(), "el worktree de la tarea se borra al integrarse");
        assert!(wt_sprint.join("b.txt").exists());

        // Tarea sin cambios propios (misma semántica que Next: se intenta el merge, que no cambia nada)
        let (r0, w0) = crear_worktree_tarea("T-0001-002", &rama, &repo).await.unwrap();
        commit_y_merge("T-0001-002", &w0, &r0, &wt_sprint, &repo).await.unwrap();
        assert!(!w0.exists());

        // Tareas 3 y 4 parten del mismo punto y tocan la misma línea → conflicto real en la segunda
        let (r3, w3) = crear_worktree_tarea("T-0001-003", &rama, &repo).await.unwrap();
        let (r4, w4) = crear_worktree_tarea("T-0001-004", &rama, &repo).await.unwrap();
        std::fs::write(w3.join("a.txt"), "tres\n").unwrap();
        std::fs::write(w4.join("a.txt"), "cuatro\n").unwrap();
        assert!(commit_y_merge("T-0001-003", &w3, &r3, &wt_sprint, &repo).await.unwrap());
        match commit_y_merge("T-0001-004", &w4, &r4, &wt_sprint, &repo).await {
            Err(ErrorMerge::Conflicto { rama, archivos }) => {
                assert_eq!(rama, "masd/T-0001-004");
                assert_eq!(archivos, vec!["a.txt".to_string()]);
            }
            otro => panic!("se esperaba conflicto, salió {:?}", otro.is_ok()),
        }
        assert!(w4.exists(), "ante un conflicto el trabajo de la tarea queda intacto");
        let estado = g(&["status", "--porcelain"], &wt_sprint).await;
        assert!(estado.trim().is_empty(), "el sprint debe quedar limpio tras abortar el merge: {estado}");
        assert_eq!(std::fs::read_to_string(wt_sprint.join("a.txt")).unwrap().replace("\r\n", "\n"), "tres\n");

        // Descartar una tarea fallida
        let (_, w5) = crear_worktree_tarea("T-0001-005", &rama, &repo).await.unwrap();
        descartar_worktree(&w5, &repo).await;
        assert!(!w5.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug_repo("Mi Proyecto Ñandú — v2!"), "mi-proyecto-nandu-v2");
        assert_eq!(slug_repo("   "), "proyecto");
        assert_eq!(slug_texto("a".repeat(100).as_str(), 40, "x").len(), 40);
        assert_eq!(nombre_de_repo("https://github.com/Architech-IA/mi-repo.git"), "mi-repo");
        assert_eq!(nombre_de_repo("mi-repo"), "mi-repo");
    }
}
