//! Sesiones de desarrollo: cada sesión trabaja en su propia rama y worktree del repositorio del proyecto, edita archivos y
//! ejecuta comandos (pruebas, build, linter) en un contenedor desechable. La IA solo escribe en la rama de la sesión:
//! subir la rama y el merge son decisiones humanas. Solo corre en el servicio privilegiado.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

use serde_json::{json, Value};

use super::repo::{dir_worktrees, git, git_con_credenciales, resolver_repo, sh_salida, R};
use crate::{
    state::AppState,
    util::{fetch_json_opt, B},
};

const IDENTIDAD: [&str; 4] = ["-c", "user.email=ia@architechia.local", "-c", "user.name=Sesión de desarrollo (IA)"];
const MAX_ARCHIVO_LEER: u64 = 1_000_000;
const MAX_ARCHIVO_ESCRIBIR: usize = 500_000;
const MAX_SALIDA: usize = 8_000;

static OCUPADAS: std::sync::LazyLock<Mutex<HashSet<String>>> = std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

/// Tope GLOBAL de comandos y verificaciones simultáneos entre todas las sesiones (el VPS es compartido).
static CUPO: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(std::env::var("DEV_MAX_SIMULTANEAS").ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(2)));

async fn cupo() -> R<tokio::sync::SemaphorePermit<'static>> {
    match tokio::time::timeout(Duration::from_secs(90), CUPO.acquire()).await {
        Ok(Ok(p)) => Ok(p),
        _ => Err("El servidor está ocupado con otras sesiones de desarrollo (hay un tope de ejecuciones simultáneas). Vuelve a intentarlo en un minuto.".into()),
    }
}

/// Una sola ejecución a la vez por sesión; se libera sola al terminar o fallar.
struct Candado(String);
impl Candado {
    fn tomar(sid: &str) -> R<Self> {
        let mut s = OCUPADAS.lock().unwrap_or_else(|e| e.into_inner());
        if !s.insert(sid.to_string()) {
            return Err("Ya hay un comando corriendo en esta sesión: espera a que termine.".into());
        }
        Ok(Self(sid.to_string()))
    }
}
impl Drop for Candado {
    fn drop(&mut self) {
        OCUPADAS.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

fn txt(b: &Value, k: &str) -> String {
    b[k].as_str().unwrap_or("").to_string()
}
fn corto(sid: &str) -> String {
    sid.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect()
}
fn rama_de(sid: &str) -> String {
    format!("sesion/{}", corto(sid))
}
fn ruta_ws(sid: &str) -> PathBuf {
    PathBuf::from(dir_worktrees()).join(format!("dev-{}", corto(sid)))
}

fn cola(t: &str, n: usize) -> String {
    let total = t.chars().count();
    if total <= n {
        t.to_string()
    } else {
        format!("[… se omiten {} caracteres anteriores …]\n{}", total - n, t.chars().skip(total - n).collect::<String>())
    }
}
fn cabeza(t: &str, n: usize) -> String {
    let total = t.chars().count();
    if total <= n {
        t.to_string()
    } else {
        format!("{}\n[… recortado: {} caracteres más …]", t.chars().take(n).collect::<String>(), total - n)
    }
}

// ── Seguridad de rutas y secretos ───────────────────────────────────────────────────────────
fn es_secreto(ruta: &str) -> bool {
    let n = ruta.rsplit('/').next().unwrap_or(ruta).to_lowercase();
    (n == ".env" || (n.starts_with(".env.") && !n.ends_with(".example") && !n.ends_with(".sample") && !n.ends_with(".template")))
        || n.ends_with(".pem")
        || n.ends_with(".key")
        || n.starts_with("id_rsa")
        || n.starts_with("id_ed25519")
        || n == ".npmrc"
        || n == "credentials.json"
}

/// Archivos que la política del proyecto (.masd-policy.json, «escritura») reserva a las personas: el ejecutor tampoco los toca.
fn protegidos(ws: &Path) -> Vec<String> {
    std::fs::read_to_string(ws.join(".masd-policy.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["escritura"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().trim_start_matches("./").to_string()).filter(|s| !s.is_empty()).collect()))
        .unwrap_or_default()
}

fn proteger(ws: &Path, ruta: &str) -> R<()> {
    let r = ruta.trim().trim_start_matches("./");
    if r == ".masd-policy.json" || protegidos(ws).iter().any(|p| r == p || r.ends_with(&format!("/{p}"))) {
        return Err(format!("«{r}» está protegido por la política del proyecto (.masd-policy.json): solo lo cambia una persona."));
    }
    Ok(())
}

/// Ruta relativa dentro del worktree: sin `..`, sin absolutas, sin tocar `.git`, y sin salir por enlaces simbólicos.
fn seguro(ws: &Path, ruta: &str) -> R<PathBuf> {
    let r = ruta.trim().trim_start_matches("./");
    let mut p = PathBuf::new();
    for c in Path::new(if r.is_empty() { "." } else { r }).components() {
        match c {
            Component::Normal(x) => {
                if x == ".git" && p.as_os_str().is_empty() {
                    return Err("La carpeta .git no se toca.".into());
                }
                p.push(x)
            }
            Component::CurDir => {}
            _ => return Err("Ruta inválida: usa rutas relativas a la raíz del repositorio, sin «..».".into()),
        }
    }
    let completa = ws.join(&p);
    let raiz = ws.canonicalize().map_err(|e| e.to_string())?;
    let mut probar = completa.clone();
    while !probar.exists() {
        if !probar.pop() {
            break;
        }
    }
    let real = probar.canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&raiz) {
        return Err("Esa ruta sale del repositorio.".into());
    }
    Ok(completa)
}

// ── Espacio de trabajo ──────────────────────────────────────────────────────────────────────
async fn validar_repo(st: &AppState, sol: &str) -> R<()> {
    let fila = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('repositorio', repositorio) FROM "Solucion" WHERE id = $1"#, &[B::T(sol.into())]).await.map_err(|e| e.to_string())?;
    let repo = fila.as_ref().and_then(|f| f["repositorio"].as_str()).map(str::trim).unwrap_or("");
    if repo.is_empty() {
        return Err("Este proyecto NO tiene repositorio de código asociado, así que no hay dónde desarrollar. Pídele a la persona que asocie uno (la herramienta crear_repositorio o el Hub de la solución) antes de seguir.".into());
    }
    Ok(())
}

async fn base_de(ws: &Path) -> Option<String> {
    for c in ["origin/main", "origin/master", "main", "master"] {
        if git(&["rev-parse", "--verify", "--quiet", c], ws).await.is_ok() {
            return Some(c.to_string());
        }
    }
    None
}

/// Valida el repositorio del proyecto en CADA llamada y crea la rama y el worktree de la sesión si faltan.
async fn espacio(st: &AppState, sol: &str, sid: &str) -> R<(PathBuf, String, PathBuf)> {
    if sid.trim().is_empty() || sol.trim().is_empty() {
        return Err("Faltan solucionId o sesionId.".into());
    }
    validar_repo(st, sol).await?;
    let raiz = resolver_repo(st, Some(sol)).await?;
    let (ws, rama) = (ruta_ws(sid), rama_de(sid));
    if ws.join(".git").exists() {
        return Ok((ws, rama, raiz));
    }
    if git(&["rev-parse", "--verify", "--quiet", "HEAD"], &raiz).await.is_err() {
        return Err("El repositorio todavía no tiene ningún commit: sube uno inicial (aunque sea un README) y vuelve a intentar.".into());
    }
    let _ = git_con_credenciales(st, &["fetch", "origin", "--quiet"], &raiz).await;
    let base = {
        let mut b = String::new();
        for c in ["origin/main", "origin/master"] {
            if git(&["rev-parse", "--verify", "--quiet", c], &raiz).await.is_ok() {
                b = c.to_string();
                break;
            }
        }
        if b.is_empty() {
            b = git(&["rev-parse", "--abbrev-ref", "HEAD"], &raiz).await?.trim().to_string();
        }
        b
    };
    std::fs::create_dir_all(dir_worktrees()).map_err(|e| e.to_string())?;
    let _ = git(&["worktree", "prune"], &raiz).await;
    let ws_s = ws.to_string_lossy().to_string();
    if git(&["rev-parse", "--verify", "--quiet", &rama], &raiz).await.is_ok() {
        git(&["worktree", "add", &ws_s, &rama], &raiz).await?;
    } else {
        git(&["worktree", "add", "-b", &rama, &ws_s, &base], &raiz).await?;
    }
    Ok((ws, rama, raiz))
}

// ── Operaciones de lectura y edición ─────────────────────────────────────────────────────────
fn listar(ws: &Path, b: &Value) -> R<Value> {
    let inicio = seguro(ws, &txt(b, "ruta"))?;
    let prof = b["profundidad"].as_u64().unwrap_or(1).clamp(1, 3) as usize;
    let mut lineas: Vec<String> = vec![];
    fn recorrer(dir: &Path, base: &Path, nivel: usize, max: usize, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            if out.len() >= 300 {
                return;
            }
            let n = e.file_name().to_string_lossy().to_string();
            if n == ".git" || n == "node_modules" || n == "target" || n == ".next" || n == "__pycache__" || n == ".venv" {
                continue;
            }
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string();
            let es_dir = p.is_dir();
            out.push(if es_dir { format!("{rel}/") } else { format!("{rel} ({} bytes)", e.metadata().map(|m| m.len()).unwrap_or(0)) });
            if es_dir && nivel < max {
                recorrer(&p, base, nivel + 1, max, out);
            }
        }
    }
    recorrer(&inicio, ws, 1, prof, &mut lineas);
    Ok(json!({ "texto": if lineas.is_empty() { "(vacío)".to_string() } else { lineas.join("\n") } }))
}

fn leer(ws: &Path, b: &Value) -> R<Value> {
    let ruta = txt(b, "ruta");
    if es_secreto(&ruta) {
        return Err("Ese archivo parece contener secretos: no se lee.".into());
    }
    let p = seguro(ws, &ruta)?;
    let meta = std::fs::metadata(&p).map_err(|_| format!("No existe: {ruta}"))?;
    if meta.is_dir() {
        return Err("Es una carpeta: usa dev_listar.".into());
    }
    if meta.len() > MAX_ARCHIVO_LEER {
        return Err("El archivo pesa más de 1 MB: léelo por partes con un comando (sed/head) en dev_ejecutar.".into());
    }
    let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
    if bytes.contains(&0) {
        return Err("Es un archivo binario.".into());
    }
    let contenido = String::from_utf8_lossy(&bytes).to_string();
    let todas: Vec<&str> = contenido.lines().collect();
    let desde = b["desde"].as_u64().unwrap_or(1).max(1) as usize;
    let hasta = b["hasta"].as_u64().map(|h| h as usize).unwrap_or(desde + 399).min(desde + 399);
    let trozo: Vec<String> = todas.iter().enumerate().skip(desde - 1).take(hasta + 1 - desde).map(|(i, l)| format!("{}\t{}", i + 1, l)).collect();
    let fin = (desde + trozo.len()).saturating_sub(1);
    Ok(json!({ "texto": format!("{ruta} · líneas {desde}–{fin} de {}\n{}", todas.len(), cabeza(&trozo.join("\n"), 20_000)) }))
}

async fn buscar(ws: &Path, b: &Value) -> R<Value> {
    let patron = txt(b, "patron");
    if patron.trim().is_empty() {
        return Err("Falta el patrón.".into());
    }
    let ruta = txt(b, "ruta");
    seguro(ws, &ruta)?;
    let mut args = vec!["grep", "-n", "-I", "--untracked", "-E", "-e", patron.as_str(), "--"];
    if !ruta.trim().is_empty() {
        args.push(ruta.trim());
    }
    let (ok, out, err) = sh_salida("git", &args, Some(ws), 60).await?;
    if !ok && out.trim().is_empty() {
        return Ok(json!({ "texto": if err.trim().is_empty() { "Sin coincidencias.".to_string() } else { format!("Sin resultados o patrón inválido: {}", cabeza(err.trim(), 300)) } }));
    }
    let lineas: Vec<&str> = out.lines().filter(|l| !es_secreto(l.split(':').next().unwrap_or(""))).take(100).collect();
    Ok(json!({ "texto": cabeza(&lineas.join("\n"), MAX_SALIDA) }))
}

fn escribir(ws: &Path, b: &Value) -> R<Value> {
    let ruta = txt(b, "ruta");
    if ruta.trim().is_empty() || es_secreto(&ruta) {
        return Err("Ruta vacía o de un archivo de secretos: no se escribe.".into());
    }
    proteger(ws, &ruta)?;
    let contenido = txt(b, "contenido");
    if contenido.len() > MAX_ARCHIVO_ESCRIBIR {
        return Err("El contenido supera los 500 KB: divídelo en varios archivos.".into());
    }
    let p = seguro(ws, &ruta)?;
    if p.is_dir() {
        return Err("Esa ruta es una carpeta.".into());
    }
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        seguro(ws, &ruta)?; // vuelve a comprobar con las carpetas ya creadas
    }
    let existia = p.exists();
    std::fs::write(&p, contenido.as_bytes()).map_err(|e| e.to_string())?;
    Ok(json!({ "texto": format!("{} {ruta} ({} líneas).", if existia { "Reescrito" } else { "Creado" }, contenido.lines().count()) }))
}

fn editar(ws: &Path, b: &Value) -> R<Value> {
    let ruta = txt(b, "ruta");
    if es_secreto(&ruta) {
        return Err("Archivo de secretos: no se edita.".into());
    }
    proteger(ws, &ruta)?;
    let p = seguro(ws, &ruta)?;
    let (buscar, reemplazar) = (txt(b, "buscar"), txt(b, "reemplazar"));
    if buscar.is_empty() {
        return Err("Falta el texto a buscar.".into());
    }
    let actual = std::fs::read_to_string(&p).map_err(|_| format!("No existe o no es texto: {ruta}"))?;
    let n = actual.matches(buscar.as_str()).count();
    if n == 0 {
        return Err("No encontré ese texto exacto (respeta espacios, saltos de línea y sangría). Vuelve a leer el archivo con dev_leer.".into());
    }
    let todos = b["todos"].as_bool().unwrap_or(false);
    if n > 1 && !todos {
        return Err(format!("Ese texto aparece {n} veces: incluye más líneas de contexto para que sea único, o pasa todos=true para cambiarlas todas."));
    }
    let nuevo = if todos { actual.replace(buscar.as_str(), &reemplazar) } else { actual.replacen(buscar.as_str(), &reemplazar, 1) };
    if nuevo.len() > MAX_ARCHIVO_ESCRIBIR * 2 {
        return Err("El resultado sería demasiado grande.".into());
    }
    std::fs::write(&p, nuevo.as_bytes()).map_err(|e| e.to_string())?;
    Ok(json!({ "texto": format!("Editado {ruta}: {} reemplazo(s).", if todos { n } else { 1 }) }))
}

fn mover(ws: &Path, b: &Value) -> R<Value> {
    let (desde, hacia) = (txt(b, "desde"), txt(b, "hacia"));
    if desde.trim().is_empty() || hacia.trim().is_empty() {
        return Err("Faltan «desde» y «hacia».".into());
    }
    if es_secreto(&desde) || es_secreto(&hacia) {
        return Err("No se mueven archivos de secretos.".into());
    }
    proteger(ws, &desde)?;
    proteger(ws, &hacia)?;
    let (pd, ph) = (seguro(ws, &desde)?, seguro(ws, &hacia)?);
    if !pd.exists() {
        return Err(format!("No existe: {desde}"));
    }
    if ph.exists() {
        return Err(format!("Ya existe: {hacia}. Bórralo o elige otro destino."));
    }
    if let Some(dir) = ph.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        seguro(ws, &hacia)?;
    }
    std::fs::rename(&pd, &ph).map_err(|e| e.to_string())?;
    Ok(json!({ "texto": format!("Movido {desde} → {hacia}.") }))
}

fn borrar(ws: &Path, b: &Value) -> R<Value> {
    let ruta = txt(b, "ruta");
    if ruta.trim().is_empty() || ruta.trim() == "." {
        return Err("Indica la ruta a borrar (no se borra la raíz del repositorio).".into());
    }
    proteger(ws, &ruta)?;
    let p = seguro(ws, &ruta)?;
    if !p.exists() {
        return Err(format!("No existe: {ruta}"));
    }
    if p.is_dir() {
        if !b["recursivo"].as_bool().unwrap_or(false) {
            return Err("Es una carpeta: pasa recursivo=true para borrarla con todo su contenido.".into());
        }
        std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
    } else {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    Ok(json!({ "texto": format!("Borrado {ruta}. (Se recupera con dev_git restaurar mientras no hagas commit del borrado.)") }))
}

fn rev_seguro(r: &str) -> R<()> {
    if r.is_empty() || r.len() > 80 || r.starts_with('-') || r.contains("..") || !r.chars().all(|c| c.is_ascii_alphanumeric() || "_./-~^@".contains(c)) {
        return Err("Revisión inválida (usa un hash, HEAD, HEAD~1, o el nombre de una rama).".into());
    }
    Ok(())
}

/// Git de la rama de la sesión con acciones cerradas: nada que cambie de rama, reescriba lo ya subido ni toque otros remotos.
async fn git_avanzado(st: &AppState, ws: &Path, b: &Value) -> R<Value> {
    let texto = |t: String| Ok(json!({ "texto": t }));
    match txt(b, "accion").as_str() {
        "log" => {
            let n = b["n"].as_u64().unwrap_or(15).clamp(1, 50).to_string();
            let ruta = txt(b, "ruta");
            let mut a = vec!["log", "--oneline", "-n", n.as_str()];
            if !ruta.trim().is_empty() {
                seguro(ws, &ruta)?;
                a.extend(["--", ruta.trim()]);
            }
            texto(cabeza(git(&a, ws).await?.trim(), 6000))
        }
        "show" => {
            let rev = txt(b, "rev");
            rev_seguro(&rev)?;
            let mut a = vec!["show", "--format=%h %an %ad%n%s%n%b", "--date=short"];
            if !b["completo"].as_bool().unwrap_or(false) {
                a.push("--stat");
            }
            a.push(rev.as_str());
            texto(cabeza(git(&a, ws).await?.trim(), 9000))
        }
        "estado" => texto(git(&["status", "-sb"], ws).await?.trim().to_string()),
        "restaurar" => {
            let ruta = txt(b, "ruta");
            if ruta.trim().is_empty() {
                return Err("Falta la ruta.".into());
            }
            seguro(ws, &ruta)?;
            git(&["restore", "--source=HEAD", "--staged", "--worktree", "--", ruta.trim()], ws).await.map_err(|e| format!("No se pudo restaurar (¿el archivo es nuevo y no está en ningún commit? usa dev_borrar): {}", cabeza(&e, 300)))?;
            texto(format!("Restaurado {ruta} a como está en el último commit."))
        }
        "deshacer_commit" => {
            let base = base_de(ws).await.ok_or("No encuentro la rama principal para comparar.")?;
            let propios = git(&["log", "--oneline", &format!("{base}..HEAD")], ws).await.unwrap_or_default();
            if propios.trim().is_empty() {
                return Err("La rama no tiene commits propios que deshacer.".into());
            }
            git(&["reset", "--soft", "HEAD~1"], ws).await?;
            texto("Último commit deshecho: sus cambios quedaron sin commit en el espacio de trabajo.".into())
        }
        "actualizar_con_base" => {
            let _ = git_con_credenciales(st, &["fetch", "origin", "--quiet"], ws).await;
            let base = base_de(ws).await.ok_or("No encuentro la rama principal.")?;
            let mut a: Vec<&str> = IDENTIDAD.to_vec();
            a.extend(["merge", "--no-edit", base.as_str()]);
            let (ok, out, err) = sh_salida("git", &a, Some(ws), 120).await?;
            if ok {
                return texto(format!("Rama actualizada con {base}.\n{}", cabeza(out.trim(), 1500)));
            }
            let conflictos = git(&["diff", "--name-only", "--diff-filter=U"], ws).await.unwrap_or_default();
            if conflictos.trim().is_empty() {
                return Err(format!("No se pudo actualizar: {}", cabeza(err.trim(), 400)));
            }
            texto(format!("HAY CONFLICTOS en:\n{}\nEdita esos archivos (busca las marcas <<<<<<<, =======, >>>>>>>), deja la versión correcta, ejecuta las pruebas y haz dev_commit para concluir la fusión. Si prefieres dejarlo como estaba: dev_git abortar_fusion.", conflictos.trim()))
        }
        "abortar_fusion" => {
            git(&["merge", "--abort"], ws).await.map_err(|_| "No hay ninguna fusión en curso.".to_string())?;
            texto("Fusión abortada: la rama quedó como antes.".into())
        }
        otra => Err(format!("Acción de git no permitida: «{otra}». Disponibles: log, show, estado, restaurar, deshacer_commit, actualizar_con_base, abortar_fusion.")),
    }
}

async fn chequeo(ws: &Path, sol: &str, sid: &str) -> R<Value> {
    let ninguno = json!({ "aplica": false, "paso": true, "errores": "" });
    if !ws.join("tsconfig.json").exists() {
        return Ok(ninguno);
    }
    let base = base_de(ws).await.unwrap_or_else(|| "HEAD".to_string());
    let mut cambiados: Vec<String> = git(&["diff", "--name-only", &base], ws).await.unwrap_or_default().lines().map(String::from).collect();
    cambiados.extend(git(&["ls-files", "--others", "--exclude-standard"], ws).await.unwrap_or_default().lines().map(String::from));
    cambiados.retain(|f| f.ends_with(".ts") || f.ends_with(".tsx"));
    if cambiados.is_empty() {
        return Ok(ninguno);
    }
    if !ws.join("node_modules").exists() {
        return Ok(json!({ "aplica": true, "paso": false, "errores": "Faltan las dependencias: ejecuta npm ci (y npx prisma generate si el proyecto usa Prisma) y vuelve a cerrar la tarea." }));
    }
    let r = ejecutar(ws, sol, sid, &json!({ "comando": "npx tsc --noEmit", "timeout": 240 })).await?;
    let t = r["texto"].as_str().unwrap_or("").to_string();
    if t.starts_with("TERMINÓ CON ÉXITO") {
        return Ok(json!({ "aplica": true, "paso": true, "errores": "" }));
    }
    // Como el ejecutor: solo cuentan los errores de los archivos que esta rama cambió (los previos de otros archivos no bloquean).
    let relevantes: Vec<&str> = t.lines().filter(|l| cambiados.iter().any(|f| l.contains(f.as_str()))).collect();
    if relevantes.is_empty() && !t.contains("El comando se detuvo") {
        return Ok(json!({ "aplica": true, "paso": true, "errores": "", "nota": "tsc reporta errores, pero ninguno en los archivos que cambiaste." }));
    }
    Ok(json!({ "aplica": true, "paso": false, "errores": cabeza(&if relevantes.is_empty() { t.clone() } else { relevantes.join("\n") }, 2500) }))
}

// ── Integración con el sprint (la rama masd/sprint-<código> que también usa el ejecutor) ──────
async fn traer_sprint(raiz: &Path, ws: &Path, b: &Value) -> R<Value> {
    let codigo = txt(b, "sprintCode");
    if codigo.trim().is_empty() {
        return Err("Falta el código del sprint.".into());
    }
    let rama_s = super::repo::rama_sprint(&codigo);
    if !super::repo::rama_existe(&rama_s, raiz).await {
        return Ok(json!({ "texto": format!("El sprint {codigo} todavía no tiene rama de integración: no hay nada que traer.") }));
    }
    let mut a: Vec<&str> = IDENTIDAD.to_vec();
    a.extend(["merge", "--no-edit", rama_s.as_str()]);
    let (ok, out, err) = sh_salida("git", &a, Some(ws), 120).await?;
    if ok {
        return Ok(json!({ "texto": format!("Se trajo a tu rama el trabajo ya integrado del sprint ({rama_s}).\n{}", cabeza(out.trim(), 800)) }));
    }
    let conflictos = git(&["diff", "--name-only", "--diff-filter=U"], ws).await.unwrap_or_default();
    if conflictos.trim().is_empty() {
        return Err(format!("No se pudo traer el sprint: {}", cabeza(err.trim(), 300)));
    }
    Ok(json!({ "texto": format!("Se trajo el sprint pero HAY CONFLICTOS en:\n{}\nResuélvelos editando los archivos (marcas <<<<<<<), corre las pruebas y haz dev_commit; o dev_git abortar_fusion.", conflictos.trim()) }))
}

/// Une la rama de la sesión a la rama de integración del sprint, como hace el ejecutor con cada tarea. Si hay conflicto, aborta y lo informa.
async fn integrar(raiz: &Path, rama: &str, b: &Value) -> R<Value> {
    let codigo = txt(b, "sprintCode");
    if codigo.trim().is_empty() {
        return Err("Falta el código del sprint.".into());
    }
    let (rama_s, wt) = super::repo::asegurar_rama_sprint(&codigo, raiz).await?;
    let mut a: Vec<&str> = IDENTIDAD.to_vec();
    let msg = format!("Integrar {rama} en el sprint {codigo}");
    a.extend(["merge", "--no-ff", "-m", msg.as_str(), rama]);
    let (ok, _, err) = sh_salida("git", &a, Some(&wt), 120).await?;
    if ok {
        return Ok(json!({ "ok": true, "texto": format!("La rama {rama} quedó integrada en {rama_s}. El pull request del sprint sigue su camino de siempre.") }));
    }
    let conflictos = git(&["diff", "--name-only", "--diff-filter=U"], &wt).await.unwrap_or_default();
    let _ = git(&["merge", "--abort"], &wt).await;
    Err(if conflictos.trim().is_empty() { format!("No se pudo integrar: {}", cabeza(err.trim(), 300)) } else { format!("Conflicto al integrar en {rama_s} (se abortó, el sprint quedó como estaba). Archivos: {}. Trae el sprint a tu rama (dev_git no; usa la tarea) y resuélvelos.", conflictos.trim().replace('\n', ", ")) })
}

// ── Verificar la interfaz: se levanta la app en un contenedor SIN salida a internet y se abre con un navegador real ──
const SCRIPT_VERIFICAR: &str = "/root/pw/verificar-pagina.js";
const DIR_CAPTURAS: &str = "/root/dev-capturas";

fn dir_capturas(sid: &str) -> PathBuf {
    PathBuf::from(DIR_CAPTURAS).join(corto(sid))
}

fn node_bin() -> String {
    std::fs::read_dir("/root/.nvm/versions/node")
        .ok()
        .and_then(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path().join("bin/node")).find(|p| p.exists()))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "node".to_string())
}

/// Comando de arranque permitido para verificar la interfaz (servidores de desarrollo/previsualización de npm).
fn permitir_inicio(comando: &str) -> R<Vec<String>> {
    let t: Vec<&str> = comando.split_whitespace().collect();
    if t.is_empty() || !t.iter().all(|x| token_seguro(x)) {
        return Err("Comando de arranque inválido (sin shell: solo palabras, sin &&, |, comillas ni rutas absolutas).".into());
    }
    let ok = matches!((t.first().copied(), t.get(1).copied(), t.get(2).copied()), (Some("npm"), Some("start"), _) | (Some("npm"), Some("run"), Some("dev")) | (Some("npm"), Some("run"), Some("start")) | (Some("npm"), Some("run"), Some("preview")));
    if !ok {
        return Err("Comando de arranque no permitido. Se permite: npm start | npm run dev | npm run start | npm run preview (con opciones como -- --host 0.0.0.0).".into());
    }
    Ok(t.iter().map(|s| s.to_string()).collect())
}

struct LimpiarContenedor(String);
impl Drop for LimpiarContenedor {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

async fn verificar(st: &AppState, sol: &str, ws: &Path, sid: &str, b: &Value) -> R<Value> {
    let inicio = permitir_inicio(&txt(b, "comando_inicio"))?;
    let puerto = b["puerto"].as_u64().unwrap_or(3000).clamp(1024, 65535);
    let espera = b["espera"].as_u64().unwrap_or(60).clamp(10, 180);
    let mut rutas: Vec<String> = b["rutas"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).map(str::trim).filter(|r| r.starts_with('/') && r.len() <= 120 && !r.contains("..") && r.chars().all(|c| c.is_ascii_alphanumeric() || "/_.-?=&%#:".contains(c))).map(String::from).take(5).collect())
        .unwrap_or_default();
    if rutas.is_empty() {
        rutas.push("/".into());
    }
    let _candado = Candado::tomar(sid)?;
    let _permiso = cupo().await?;
    let _ = sh_salida("docker", &["network", "create", "--internal", "dev-interno"], None, 30).await;
    let nombre = format!("dev-ui-{}-{}", corto(sid), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) % 100_000_000);
    let ws_s = format!("{}:/work", ws.to_string_lossy());
    let (e_puerto, e_host) = (format!("PORT={puerto}"), "HOST=0.0.0.0".to_string());
    let extra_env: Vec<String> = {
        let mut e = entorno_servicios(sid, &servicios_activos(sid).await);
        e.extend(variables_de(sol));
        e.iter().flat_map(|(k, v)| ["-e".to_string(), format!("{k}={v}")]).collect()
    };
    let mut a: Vec<&str> = vec![
        "run", "-d", "--rm", "--name", &nombre, "--network", "dev-interno", "--memory=2g", "--memory-swap=2g", "--cpus=2", "--pids-limit=512", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--tmpfs", "/tmp:rw,exec,size=512m", "-v", &ws_s, "-w", "/work", "-e", "CI=1",
        "-e", "HOME=/tmp", "-e", &e_puerto, "-e", &e_host, "-e", "HOSTNAME=0.0.0.0",
    ];
    a.extend(extra_env.iter().map(String::as_str));
    a.push("node:22-bookworm");
    a.extend(inicio.iter().map(String::as_str));
    sh_salida("docker", &a, None, 180).await.and_then(|(ok, _, e)| if ok { Ok(()) } else { Err(format!("No se pudo arrancar la aplicación: {}", cabeza(e.trim(), 300))) })?;
    let _limpiar = LimpiarContenedor(nombre.clone());
    let ip = sh_salida("docker", &["inspect", "-f", "{{(index .NetworkSettings.Networks \"dev-interno\").IPAddress}}", &nombre], None, 30).await?.1.trim().to_string();
    let logs = || async { sh_salida("docker", &["logs", "--tail", "40", &nombre], None, 30).await.map(|(_, o, e)| cola(format!("{o}{e}").trim(), 1800)).unwrap_or_default() };
    // Esperar a que la aplicación responda (cualquier respuesta HTTP sirve)
    let inicio_t = std::time::Instant::now();
    let mut lista = false;
    while inicio_t.elapsed().as_secs() < espera {
        let corriendo = sh_salida("docker", &["inspect", "-f", "{{.State.Running}}", &nombre], None, 20).await.map(|(_, o, _)| o.trim() == "true").unwrap_or(false);
        if !corriendo {
            return Ok(json!({ "texto": format!("La aplicación se detuvo al arrancar. Registro:\n{}", logs().await) }));
        }
        if !ip.is_empty() && st.http.get(format!("http://{ip}:{puerto}/")).timeout(Duration::from_secs(3)).send().await.is_ok() {
            lista = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !lista {
        return Ok(json!({ "texto": format!("La aplicación no respondió en el puerto {puerto} en {espera} s (¿otro puerto? ¿falta una variable de entorno o la base de datos?). Registro:\n{}", logs().await) }));
    }
    let dir = dir_capturas(sid);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let node = node_bin();
    let mut informe: Vec<String> = vec![];
    for (i, ruta) in rutas.iter().enumerate() {
        let archivo = dir.join(format!("{}-{}.png", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), i + 1));
        let url = format!("http://{ip}:{puerto}{ruta}");
        match sh_salida(&node, &[SCRIPT_VERIFICAR, &url, &archivo.to_string_lossy()], None, 90).await {
            Ok((true, out, _)) => match serde_json::from_str::<Value>(out.trim()) {
                Ok(r) => {
                    let lista = |k: &str| r[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).take(8).collect::<Vec<_>>().join(" | ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "ninguno".into());
                    informe.push(format!(
                        "── {ruta} → HTTP {} · «{}»\nTexto visible: {}\nErrores de consola: {}\nErrores de la página: {}\nSolicitudes fallidas: {}\nCaptura: {}",
                        r["status"], r["titulo"].as_str().unwrap_or(""), cabeza(r["texto"].as_str().unwrap_or("").trim(), 1500).replace('\n', " ⏎ "), lista("erroresConsola"), lista("erroresPagina"), lista("solicitudesFallidas"),
                        archivo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                    ));
                }
                Err(_) => informe.push(format!("── {ruta} → respuesta ilegible del verificador")),
            },
            Ok((false, _, e)) => informe.push(format!("── {ruta} → no se pudo abrir: {}", cabeza(e.trim(), 300))),
            Err(e) => informe.push(format!("── {ruta} → {}", cabeza(&e, 200))),
        }
    }
    // Se conservan solo las últimas 20 capturas de la sesión
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut v: Vec<_> = rd.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "png").unwrap_or(false)).collect();
        v.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
        while v.len() > 20 {
            let _ = std::fs::remove_file(v.remove(0).path());
        }
    }
    Ok(json!({ "texto": format!("La aplicación respondió. Resultado:\n{}\n\nRegistro del servidor (final):\n{}", informe.join("\n\n"), logs().await) }))
}

fn lista_capturas(sid: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir_capturas(sid)) else { return vec![] };
    let mut v: Vec<_> = rd.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "png").unwrap_or(false)).collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
    v.iter().take(12).filter_map(|e| e.file_name().to_str().map(String::from)).collect()
}

fn captura(sid: &str, nombre: &str) -> R<Value> {
    if nombre.is_empty() || !nombre.ends_with(".png") || !nombre.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) || nombre.contains("..") {
        return Err("Nombre de captura inválido.".into());
    }
    use base64::Engine;
    let bytes = std::fs::read(dir_capturas(sid).join(nombre)).map_err(|_| "No existe esa captura.".to_string())?;
    Ok(json!({ "base64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
}

// ── Variables de prueba (valores de PRUEBA que pone un administrador; la IA solo ve los nombres) ──
const DIR_VARS: &str = "/root/deploys/env-prueba";

fn ruta_vars(sol: &str) -> PathBuf {
    let _ = std::fs::create_dir_all(DIR_VARS);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(DIR_VARS, std::fs::Permissions::from_mode(0o700));
    }
    let limpio: String = sol.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(60).collect();
    Path::new(DIR_VARS).join(format!("{limpio}.env"))
}

fn variables_de(sol: &str) -> Vec<(String, String)> {
    std::fs::read_to_string(ruta_vars(sol)).unwrap_or_default().lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn nombre_variable_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 60 && n.chars().enumerate().all(|(i, c)| c == '_' || c.is_ascii_uppercase() || (i > 0 && c.is_ascii_digit()))
}

fn variables(sol: &str, b: &Value) -> R<Value> {
    let mut vars = variables_de(sol);
    match txt(b, "op").as_str() {
        "listar" => Ok(json!({ "nombres": vars.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>() })),
        "guardar" => {
            let (n, v) = (txt(b, "nombre"), txt(b, "valor"));
            if !nombre_variable_ok(&n) {
                return Err("Nombre inválido: solo MAYÚSCULAS, números y guión bajo, sin empezar con número.".into());
            }
            if v.is_empty() || v.len() > 2000 || v.contains(['\n', '\r', '\0']) {
                return Err("El valor debe tener entre 1 y 2000 caracteres y una sola línea.".into());
            }
            if vars.len() >= 30 && !vars.iter().any(|(k, _)| *k == n) {
                return Err("Máximo 30 variables de prueba por proyecto.".into());
            }
            match vars.iter_mut().find(|(k, _)| *k == n) {
                Some(e) => e.1 = v,
                None => vars.push((n, v)),
            }
            guardar_vars(sol, &vars)?;
            Ok(json!({ "ok": true, "nombres": vars.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>() }))
        }
        "borrar" => {
            let n = txt(b, "nombre");
            vars.retain(|(k, _)| *k != n);
            guardar_vars(sol, &vars)?;
            Ok(json!({ "ok": true, "nombres": vars.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>() }))
        }
        _ => Err("Operación desconocida.".into()),
    }
}

fn guardar_vars(sol: &str, vars: &[(String, String)]) -> R<()> {
    let p = ruta_vars(sol);
    std::fs::write(&p, vars.iter().map(|(k, v)| format!("{k}={v}\n")).collect::<String>()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// La salida de un comando nunca devuelve los valores de las variables de prueba (un script podría imprimirlos).
fn ocultar(texto: &str, vars: &[(String, String)]) -> String {
    let mut t = texto.to_string();
    for (_, v) in vars {
        if v.chars().count() >= 6 {
            t = t.replace(v.as_str(), "«oculto»");
        }
    }
    t
}

// ── Servicios de apoyo de la sesión: bases de datos y la propia aplicación, en la red interna (sin internet) ──
const RED_INTERNA: &str = "dev-interno";
const MAX_SERVICIOS: usize = 4;

fn nombre_svc(sid: &str, tipo: &str) -> String {
    format!("dev-svc-{}-{tipo}", corto(sid))
}

fn ahora_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn asegurar_red() {
    let _ = sh_salida("docker", &["network", "create", "--internal", RED_INTERNA], None, 30).await;
}

/// Tipos de servicio que la sesión tiene en marcha.
async fn servicios_activos(sid: &str) -> Vec<String> {
    let filtro = format!("label=dev-sesion={}", corto(sid));
    let (ok, out, _) = sh_salida("docker", &["ps", "--filter", &filtro, "--format", "{{.Names}}"], None, 30).await.unwrap_or((false, String::new(), String::new()));
    if !ok {
        return vec![];
    }
    let prefijo = format!("dev-svc-{}-", corto(sid));
    out.lines().filter_map(|n| n.strip_prefix(prefijo.as_str()).map(String::from)).collect()
}

/// Variables de entorno que apuntan a los servicios en marcha (credenciales de PRUEBA fijas: no hay datos reales).
fn entorno_servicios(sid: &str, tipos: &[String]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![];
    let mut poner = |k: &str, val: String| {
        if !v.iter().any(|(x, _)| x == k) {
            v.push((k.to_string(), val));
        }
    };
    for t in ["postgres", "mysql", "mongo", "redis"] {
        if !tipos.iter().any(|x| x == t) {
            continue;
        }
        let h = nombre_svc(sid, t);
        match t {
            "postgres" => {
                let u = format!("postgresql://test:test@{h}:5432/test");
                poner("DATABASE_URL", u.clone());
                poner("POSTGRES_URL", u);
            }
            "mysql" => {
                let u = format!("mysql://root:test@{h}:3306/test");
                poner("DATABASE_URL", u.clone());
                poner("MYSQL_URL", u);
            }
            "mongo" => {
                let u = format!("mongodb://{h}:27017/test");
                poner("MONGO_URL", u.clone());
                poner("MONGODB_URI", u);
            }
            _ => poner("REDIS_URL", format!("redis://{h}:6379")),
        }
    }
    v
}

async fn ip_de(nombre: &str) -> String {
    sh_salida("docker", &["inspect", "-f", &format!("{{{{(index .NetworkSettings.Networks \"{RED_INTERNA}\").IPAddress}}}}"), nombre], None, 20).await.map(|(_, o, _)| o.trim().to_string()).unwrap_or_default()
}

async fn puerto_de_app(nombre: &str) -> Option<u64> {
    sh_salida("docker", &["inspect", "-f", "{{index .Config.Labels \"dev-puerto\"}}", nombre], None, 20).await.ok().and_then(|(_, o, _)| o.trim().parse().ok())
}

async fn servicio(st: &AppState, sol: &str, ws: &Path, sid: &str, b: &Value) -> R<Value> {
    let texto = |t: String| Ok(json!({ "texto": t }));
    match txt(b, "accion").as_str() {
        "estado" => {
            let ts = servicios_activos(sid).await;
            if ts.is_empty() {
                return texto("No hay servicios en marcha en esta sesión.".into());
            }
            let mut l = vec![];
            for t in &ts {
                let n = nombre_svc(sid, t);
                l.push(match (t.as_str(), puerto_de_app(&n).await) {
                    ("app", Some(p)) => format!("- app: http://{n}:{p}"),
                    _ => format!("- {t}: host {n}"),
                });
            }
            texto(format!("Servicios en marcha:\n{}\nVariables que reciben tus comandos: {}", l.join("\n"), entorno_servicios(sid, &ts).iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ")))
        }
        "detener" => {
            let tipo = txt(b, "tipo");
            let ts = servicios_activos(sid).await;
            let objetivo: Vec<&String> = ts.iter().filter(|t| tipo.is_empty() || **t == tipo).collect();
            for t in &objetivo {
                let _ = sh_salida("docker", &["rm", "-f", &nombre_svc(sid, t)], None, 60).await;
            }
            texto(format!("Detenidos: {}.", if objetivo.is_empty() { "ninguno".to_string() } else { objetivo.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ") }))
        }
        "logs" => {
            let tipo = txt(b, "tipo");
            if !servicios_activos(sid).await.contains(&tipo) {
                return Err("Ese servicio no está en marcha.".into());
            }
            let (_, o, e) = sh_salida("docker", &["logs", "--tail", "80", &nombre_svc(sid, &tipo)], None, 30).await?;
            texto(cola(format!("{o}{e}").trim(), 4000))
        }
        "iniciar" => {
            let tipo = txt(b, "tipo");
            if !["postgres", "redis", "mysql", "mongo", "app"].contains(&tipo.as_str()) {
                return Err("Tipo de servicio desconocido: postgres, redis, mysql, mongo o app.".into());
            }
            let activos = servicios_activos(sid).await;
            if activos.contains(&tipo) {
                return texto(format!("«{tipo}» ya está en marcha (host {}).", nombre_svc(sid, &tipo)));
            }
            if activos.len() >= MAX_SERVICIOS {
                return Err(format!("Máximo {MAX_SERVICIOS} servicios por sesión: detén alguno primero."));
            }
            let _c = Candado::tomar(sid)?;
            let _p = cupo().await?;
            asegurar_red().await;
            let n = nombre_svc(sid, &tipo);
            let (etq1, etq2) = (format!("dev-sesion={}", corto(sid)), format!("dev-expira={}", ahora_s() + 3 * 3600));
            let mut a: Vec<String> = ["run", "-d", "--rm", "--name", n.as_str(), "--network", RED_INTERNA, "--label", etq1.as_str(), "--label", etq2.as_str(), "--pids-limit=512"].iter().map(|s| s.to_string()).collect();
            let listo: Vec<&str>;
            if tipo == "app" {
                let inicio = permitir_inicio(&txt(b, "comando_inicio"))?;
                let puerto = b["puerto"].as_u64().unwrap_or(3000).clamp(1024, 65535);
                let mut entorno = entorno_servicios(sid, &activos);
                entorno.extend(variables_de(sol));
                a.extend(["--label".into(), format!("dev-puerto={puerto}"), "--memory=2g".into(), "--memory-swap=2g".into(), "--cpus=2".into(), "--cap-drop=ALL".into(), "--security-opt=no-new-privileges".into(), "--tmpfs".into(), "/tmp:rw,exec,size=512m".into(), "-v".into(), format!("{}:/work", ws.to_string_lossy()), "-w".into(), "/work".into()]);
                for (k, v) in [("CI", "1".to_string()), ("HOME", "/tmp".to_string()), ("PORT", puerto.to_string()), ("HOST", "0.0.0.0".to_string()), ("HOSTNAME", "0.0.0.0".to_string())].into_iter().map(|(k, v)| (k.to_string(), v)).chain(entorno) {
                    a.extend(["-e".into(), format!("{k}={v}")]);
                }
                a.push("node:22-bookworm".into());
                a.extend(inicio);
                listo = vec![];
            } else {
                let (imagen, env, tmpfs, mem, l): (&str, Vec<&str>, &str, &str, Vec<&str>) = match tipo.as_str() {
                    "postgres" => ("postgres:16-alpine", vec!["POSTGRES_USER=test", "POSTGRES_PASSWORD=test", "POSTGRES_DB=test"], "/var/lib/postgresql/data", "512m", vec!["pg_isready", "-U", "test", "-d", "test"]),
                    "redis" => ("redis:7-alpine", vec![], "/data", "256m", vec!["redis-cli", "ping"]),
                    "mysql" => ("mysql:8", vec!["MYSQL_ROOT_PASSWORD=test", "MYSQL_DATABASE=test"], "/var/lib/mysql", "768m", vec!["mysqladmin", "ping", "-h", "127.0.0.1", "-ptest", "--silent"]),
                    _ => ("mongo:7", vec![], "/data/db", "512m", vec!["mongosh", "--quiet", "--eval", "db.runCommand({ping:1}).ok"]),
                };
                a.extend([format!("--memory={mem}"), format!("--memory-swap={mem}"), "--cpus=1".into(), "--tmpfs".into(), format!("{tmpfs}:rw")]);
                for e in env {
                    a.extend(["-e".into(), e.into()]);
                }
                a.push(imagen.into());
                listo = l;
            }
            let args: Vec<&str> = a.iter().map(String::as_str).collect();
            let (ok, _, err) = sh_salida("docker", &args, None, 600).await?;
            if !ok {
                return Err(format!("No se pudo iniciar «{tipo}»: {}", cabeza(err.trim(), 400)));
            }
            // Esperar a que responda
            let t0 = std::time::Instant::now();
            let mut respondio = false;
            while t0.elapsed().as_secs() < if tipo == "app" { 90 } else { 60 } {
                if tipo == "app" {
                    let ip = ip_de(&n).await;
                    let puerto = puerto_de_app(&n).await.unwrap_or(3000);
                    let vivo = sh_salida("docker", &["inspect", "-f", "{{.State.Running}}", &n], None, 20).await.map(|(_, o, _)| o.trim() == "true").unwrap_or(false);
                    if !vivo {
                        let (_, o, e) = sh_salida("docker", &["logs", "--tail", "40", &n], None, 20).await.unwrap_or((false, String::new(), String::new()));
                        return Ok(json!({ "texto": format!("La aplicación se detuvo al arrancar. Registro:\n{}", cola(format!("{o}{e}").trim(), 1800)) }));
                    }
                    if !ip.is_empty() && st.http.get(format!("http://{ip}:{puerto}/")).timeout(Duration::from_secs(3)).send().await.is_ok() {
                        respondio = true;
                        break;
                    }
                } else {
                    let mut c: Vec<&str> = vec!["exec", n.as_str()];
                    c.extend(listo.iter().copied());
                    if sh_salida("docker", &c, None, 15).await.map(|(ok, _, _)| ok).unwrap_or(false) {
                        respondio = true;
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let nota = if tipo == "app" { format!("Aplicación en http://{n}:{}", b["puerto"].as_u64().unwrap_or(3000)) } else { format!("{tipo} listo en el host {n}") };
            texto(if respondio { format!("{nota}. Tus comandos reciben las variables de conexión y corren en la red interna (sin internet).") } else { format!("{nota}, pero todavía no responde; revisa dev_servicio logs y vuelve a intentar.") })
        }
        otro => Err(format!("Acción de servicio desconocida: «{otro}» (iniciar, estado, detener, logs).")),
    }
}

/// Quita los servicios cuya hora de vencimiento pasó (3 h), por si una sesión se abandona.
pub fn iniciar_limpieza() {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(600)).await;
            let Ok((true, out, _)) = sh_salida("docker", &["ps", "--filter", "label=dev-sesion", "--format", "{{.Names}}|{{.Label \"dev-expira\"}}"], None, 30).await else { continue };
            for l in out.lines() {
                if let Some((n, e)) = l.split_once('|') {
                    if e.parse::<u64>().map(|x| x < ahora_s()).unwrap_or(false) {
                        let _ = sh_salida("docker", &["rm", "-f", n], None, 60).await;
                        tracing::info!("[dev] servicio vencido retirado: {n}");
                    }
                }
            }
        }
    });
}

// ── Prueba de carga contra la aplicación en marcha ───────────────────────────────────────────
async fn carga(st: &AppState, sid: &str, b: &Value) -> R<Value> {
    let ruta = { let r = txt(b, "ruta"); if r.is_empty() { "/".to_string() } else { r } };
    if !ruta.starts_with('/') || ruta.len() > 160 || ruta.contains("..") || !ruta.chars().all(|c| c.is_ascii_alphanumeric() || "/_.-?=&%:".contains(c)) {
        return Err("Ruta inválida (debe empezar con «/»).".into());
    }
    let conexiones = b["conexiones"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
    let segundos = b["segundos"].as_u64().unwrap_or(10).clamp(1, 30);
    let n = nombre_svc(sid, "app");
    if !servicios_activos(sid).await.contains(&"app".to_string()) {
        return Err("No hay una aplicación en marcha: inícala con dev_servicio (accion iniciar, tipo app).".into());
    }
    let (ip, puerto) = (ip_de(&n).await, puerto_de_app(&n).await.unwrap_or(3000));
    let url = format!("http://{ip}:{puerto}{ruta}");
    let _c = Candado::tomar(sid)?;
    let _p = cupo().await?;
    let fin = std::time::Instant::now() + Duration::from_secs(segundos);
    let mut tareas = tokio::task::JoinSet::new();
    for _ in 0..conexiones {
        let (cli, u) = (st.http.clone(), url.clone());
        tareas.spawn(async move {
            let (mut lat, mut estados, mut errores, mut total) = (Vec::<u32>::new(), std::collections::BTreeMap::<u16, u64>::new(), 0u64, 0u64);
            while std::time::Instant::now() < fin {
                let t0 = std::time::Instant::now();
                match cli.get(&u).timeout(Duration::from_secs(10)).send().await {
                    Ok(r) => {
                        let s = r.status().as_u16();
                        let _ = r.bytes().await;
                        *estados.entry(s).or_insert(0) += 1;
                    }
                    Err(_) => errores += 1,
                }
                total += 1;
                if lat.len() < 20_000 {
                    lat.push(t0.elapsed().as_millis().min(60_000) as u32);
                }
            }
            (lat, estados, errores, total)
        });
    }
    let (mut lat, mut estados, mut errores, mut total) = (Vec::<u32>::new(), std::collections::BTreeMap::<u16, u64>::new(), 0u64, 0u64);
    while let Some(r) = tareas.join_next().await {
        if let Ok((l, e, er, t)) = r {
            lat.extend(l);
            for (k, v) in e {
                *estados.entry(k).or_insert(0) += v;
            }
            errores += er;
            total += t;
        }
    }
    lat.sort_unstable();
    let pct = |p: f64| lat.get(((lat.len() as f64 - 1.0) * p).round().max(0.0) as usize).copied().unwrap_or(0);
    let ok = estados.iter().filter(|(k, _)| **k < 400).map(|(_, v)| *v).sum::<u64>();
    Ok(json!({ "texto": format!(
        "Prueba de carga a {ruta}: {conexiones} conexiones simultáneas durante {segundos} s.\nSolicitudes: {total} · {:.1} por segundo · correctas (<400): {ok} · con error de red: {errores}\nCódigos: {}\nLatencia (ms): mediana {} · p95 {} · p99 {} · máxima {}\n(La aplicación corre en un contenedor de 2 CPU y 2 GB: sirve para comparar versiones y detectar degradación, no para dimensionar producción.)",
        total as f64 / segundos as f64,
        estados.iter().map(|(k, v)| format!("{k}×{v}")).collect::<Vec<_>>().join(", "),
        pct(0.5), pct(0.95), pct(0.99), lat.last().copied().unwrap_or(0)
    ) }))
}

// ── Pruebas de flujo (varios pasos) en un navegador real ─────────────────────────────────────
const SCRIPT_FLUJO: &str = "/root/pw/flujo-pagina.js";

async fn flujo(sid: &str, b: &Value) -> R<Value> {
    let n = nombre_svc(sid, "app");
    if !servicios_activos(sid).await.contains(&"app".to_string()) {
        return Err("No hay una aplicación en marcha: inícala con dev_servicio (accion iniciar, tipo app).".into());
    }
    let mut pasos: Vec<Value> = vec![];
    for p in b["pasos"].as_array().ok_or("Faltan los pasos.")?.iter().take(30) {
        let accion = p["accion"].as_str().unwrap_or("");
        if !["ir", "clic", "escribir", "seleccionar", "esperar_texto", "esperar_selector", "verificar_texto", "verificar_url", "verificar_visible", "capturar"].contains(&accion) {
            return Err(format!("Paso desconocido: «{accion}»."));
        }
        let mut o = json!({ "accion": accion });
        for k in ["ruta", "selector", "valor", "texto", "contiene"] {
            if let Some(v) = p[k].as_str() {
                if v.chars().count() > 300 {
                    return Err(format!("El campo «{k}» es demasiado largo."));
                }
                o[k] = json!(v);
            }
        }
        if accion == "ir" && !o["ruta"].as_str().map(|r| r.starts_with('/') && !r.contains("..")).unwrap_or(false) {
            return Err("El paso «ir» necesita una ruta que empiece con «/».".into());
        }
        if ["clic", "escribir", "seleccionar", "esperar_selector", "verificar_visible"].contains(&accion) && o["selector"].as_str().map(str::is_empty).unwrap_or(true) {
            return Err(format!("El paso «{accion}» necesita un selector."));
        }
        pasos.push(o);
    }
    if pasos.is_empty() {
        return Err("No hay pasos.".into());
    }
    let (ip, puerto) = (ip_de(&n).await, puerto_de_app(&n).await.unwrap_or(3000));
    let dir = dir_capturas(sid);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _c = Candado::tomar(sid)?;
    let _p = cupo().await?;
    let prefijo = format!("{}-flujo", ahora_s());
    let base = format!("http://{ip}:{puerto}");
    let mut cmd = tokio::process::Command::new(node_bin());
    cmd.args([SCRIPT_FLUJO, base.as_str(), &*dir.to_string_lossy(), prefijo.as_str()]).env("FLUJO", Value::Array(pasos).to_string()).kill_on_drop(true);
    let out = match tokio::time::timeout(Duration::from_secs(240), cmd.output()).await {
        Err(_) => return Ok(json!({ "texto": "El flujo tardó más de 4 minutos y se detuvo." })),
        Ok(Err(e)) => return Err(e.to_string()),
        Ok(Ok(o)) => o,
    };
    if !out.status.success() {
        return Err(format!("No se pudo ejecutar el flujo: {}", cabeza(String::from_utf8_lossy(&out.stderr).trim(), 300)));
    }
    let r: Value = serde_json::from_slice(&out.stdout).map_err(|_| "Respuesta ilegible del verificador de flujos.".to_string())?;
    let res = r["resultados"].as_array().cloned().unwrap_or_default();
    let fallo = res.iter().any(|x| x["ok"] == false);
    let lineas: Vec<String> = res.iter().map(|x| format!("{} {}. {} — {}", if x["ok"] == true { "✓" } else { "✗" }, x["n"], x["paso"].as_str().unwrap_or(""), x["detalle"].as_str().unwrap_or(""))).collect();
    let total = r["total"].as_u64().unwrap_or(0) as usize;
    let consola = r["erroresConsola"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" | ")).filter(|s| !s.is_empty());
    Ok(json!({ "texto": format!(
        "{} ({} de {} pasos){}\n{}\nURL final: {}{}\nCapturas: {}",
        if fallo { "EL FLUJO FALLÓ" } else if res.len() < total { "FLUJO INCOMPLETO" } else { "EL FLUJO PASÓ" },
        res.iter().filter(|x| x["ok"] == true).count(), total,
        if fallo { " — se detuvo en el primer paso que falló" } else { "" },
        lineas.join("\n"),
        r["urlFinal"].as_str().unwrap_or("/"),
        consola.map(|c| format!("\nErrores de consola: {c}")).unwrap_or_default(),
        r["capturas"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "ninguna".into())
    ) }))
}

// ── Ejecución en contenedor ─────────────────────────────────────────────────────────────────
/// Un comando permitido ya desglosado: imagen, argumentos y si necesita red (solo las instalaciones de dependencias).
struct Permitido {
    imagen: &'static str,
    args: Vec<String>,
    red: bool,
}

fn token_seguro(t: &str) -> bool {
    !t.is_empty() && t.len() <= 120 && t.chars().all(|c| c.is_ascii_alphanumeric() || "_./:=@,+-".contains(c)) && !t.starts_with('/') && !t.split('/').any(|seg| seg == "..")
}

/// Lista CERRADA de comandos. Se ejecutan directamente (sin shell): no hay encadenado, redirecciones ni sustituciones.
/// Sin red, salvo las instalaciones de dependencias (npm ci/install, cargo fetch, go mod download).
/// Un filtro de salida («| head -n 20»). Lo aplica el servidor sobre el texto que devolvió el comando: no hay shell ni tuberías reales.
#[derive(Debug, PartialEq)]
enum Filtro {
    Head(usize),
    Tail(usize),
    Grep { patron: String, ignorar: bool, invertir: bool, contar: bool },
    Lineas,
    Ordenar,
    Unicas,
}

/// Separa «comando | filtro | filtro». Solo filtros de una lista cerrada, hasta 3.
fn separar_filtros(comando: &str) -> R<(String, Vec<Filtro>)> {
    let mut partes = comando.split('|');
    let cmd = partes.next().unwrap_or("").trim().to_string();
    let mut fs = vec![];
    for p in partes {
        if fs.len() >= 3 {
            return Err("Se permiten hasta 3 filtros después del comando.".into());
        }
        fs.push(parsear_filtro(p.trim())?);
    }
    Ok((cmd, fs))
}

fn parsear_filtro(p: &str) -> R<Filtro> {
    let t: Vec<&str> = p.split_whitespace().collect();
    let malo = || -> R<Filtro> { Err(format!("Filtro no permitido: «{p}». Después de «|» solo: head [-n N] | tail [-n N] | grep [-i] [-v] [-c] <texto> | wc -l | sort | uniq.")) };
    let cuenta = |a: &[&str]| -> Option<usize> {
        match a {
            [] => Some(10),
            [x] if x.len() > 1 && x.starts_with('-') && x[1..].chars().all(|c| c.is_ascii_digit()) => x[1..].parse().ok(),
            ["-n", n] => n.parse().ok(),
            _ => None,
        }
    };
    match t.first().copied() {
        Some("head") => cuenta(&t[1..]).map(|n| Filtro::Head(n.min(2000))).map_or_else(malo, Ok),
        Some("tail") => cuenta(&t[1..]).map(|n| Filtro::Tail(n.min(2000))).map_or_else(malo, Ok),
        Some("wc") if t.len() == 2 && t[1] == "-l" => Ok(Filtro::Lineas),
        Some("sort") if t.len() == 1 => Ok(Filtro::Ordenar),
        Some("uniq") if t.len() == 1 => Ok(Filtro::Unicas),
        Some("grep") => {
            let (mut ignorar, mut invertir, mut contar) = (false, false, false);
            let mut i = 1;
            while i < t.len() && matches!(t[i], "-i" | "-v" | "-c") {
                match t[i] {
                    "-i" => ignorar = true,
                    "-v" => invertir = true,
                    _ => contar = true,
                }
                i += 1;
            }
            if t.len() != i + 1 || t[i].len() > 80 {
                return malo();
            }
            Ok(Filtro::Grep { patron: t[i].to_string(), ignorar, invertir, contar })
        }
        _ => malo(),
    }
}

fn aplicar_filtros(texto: &str, fs: &[Filtro]) -> String {
    let mut l: Vec<String> = texto.lines().map(String::from).collect();
    for f in fs {
        match f {
            Filtro::Head(n) => l.truncate(*n),
            Filtro::Tail(n) => {
                let k = l.len().saturating_sub(*n);
                l = l.split_off(k);
            }
            Filtro::Grep { patron, ignorar, invertir, contar } => {
                let p = if *ignorar { patron.to_lowercase() } else { patron.clone() };
                l.retain(|x| (if *ignorar { x.to_lowercase() } else { x.clone() }).contains(&p) != *invertir);
                if *contar {
                    l = vec![l.len().to_string()];
                }
            }
            Filtro::Lineas => l = vec![l.len().to_string()],
            Filtro::Ordenar => l.sort(),
            Filtro::Unicas => l.dedup(),
        }
    }
    l.join("\n")
}

/// Código suelto (el equivalente a node -e): se pasa como UN argumento al intérprete dentro del contenedor, sin shell del servidor, sin red y con las mismas
/// limitaciones que un script del repositorio.
fn permitir_codigo(lenguaje: &str, codigo: &str) -> R<Permitido> {
    if codigo.trim().is_empty() || codigo.chars().count() > 4000 {
        return Err("El código debe tener entre 1 y 4000 caracteres.".into());
    }
    let (imagen, base): (&'static str, [&str; 2]) = match lenguaje.trim() {
        "" | "node" => ("node:22-bookworm", ["node", "-e"]),
        "python" | "python3" => ("python:3.12-bookworm", ["python", "-c"]),
        "bash" | "sh" => ("node:22-bookworm", ["bash", "-c"]),
        _ => return Err("El lenguaje debe ser node, python o bash.".into()),
    };
    Ok(Permitido { imagen, args: base.iter().map(|s| s.to_string()).chain([codigo.to_string()]).collect(), red: false })
}

/// curl de solo lectura (GET, o HEAD con -I) a un dominio público. SIEMPRE necesita internet, así que solo corre si una persona lo confirmó.
fn permitir_curl(resto: &str) -> R<Permitido> {
    let t: Vec<&str> = resto.split_whitespace().collect();
    let (cabeceras, url) = match t.as_slice() {
        [u] => (false, *u),
        ["-I", u] => (true, *u),
        _ => return Err("curl solo admite «curl <url>» o «curl -I <url>» (lectura, sin otras opciones). Necesita internet: pídelo con red=true y un administrador lo confirma.".into()),
    };
    if url.len() > 300 || !(url.starts_with("http://") || url.starts_with("https://")) || !url.chars().all(|c| c.is_ascii_alphanumeric() || "-._~:/?#@&=+,%;".contains(c)) {
        return Err("URL no válida: debe empezar por http:// o https:// y no llevar espacios ni comillas.".into());
    }
    let autoridad = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("");
    if autoridad.contains('@') {
        return Err("La URL no puede llevar usuario ni clave.".into());
    }
    let host = autoridad.split(':').next().unwrap_or("").to_lowercase();
    let es_ip = !host.is_empty() && host.chars().all(|c| c.is_ascii_digit() || c == '.');
    if host.is_empty() || !host.contains('.') || es_ip || host == "localhost" || host.ends_with(".local") || host.ends_with(".internal") || host.ends_with(".localhost") {
        return Err("Solo se permite un nombre de dominio público (no localhost, IP ni nombres internos).".into());
    }
    let mut args: Vec<String> = ["curl", "-sS", "-L", "--max-time", "30", "--max-redirs", "5", "--max-filesize", "5000000", "--proto", "=http,https", "--proto-redir", "=http,https"].iter().map(|s| s.to_string()).collect();
    if cabeceras {
        args.push("-I".into());
    }
    args.push(url.to_string());
    Ok(Permitido { imagen: "node:22-bookworm", args, red: false })
}

fn permitir(comando: &str) -> R<Permitido> {
    let c = comando.trim();
    if c.is_empty() || c.len() > 300 || c.contains(|ch: char| ch.is_control()) {
        return Err("Comando vacío o inválido.".into());
    }
    if let Some(r) = c.strip_prefix("curl ") {
        return permitir_curl(r.trim());
    }
    let t: Vec<&str> = c.split_whitespace().collect();
    if !t.iter().all(|x| token_seguro(x)) {
        return Err("El comando tiene caracteres no permitidos: se ejecuta tal cual, sin shell (nada de &&, ;, >, $, comillas ni rutas con «..» o absolutas). Para filtrar la salida puedes usar «| head|tail|grep|wc -l|sort|uniq»; para código suelto usa los campos codigo y lenguaje.".into());
    }
    let resto = |desde: usize| -> Vec<String> { t.iter().skip(desde).map(|s| s.to_string()).collect() };
    let con = |base: &[&str], extra: Vec<String>| -> Vec<String> { base.iter().map(|s| s.to_string()).chain(extra).collect() };
    let lista = "npm test | npm run <cualquier script de package.json> | curl [-I] <url> (solo con red=true y confirmación) | node|python|bash <archivo del repositorio> | npx tsx <archivo.ts> | make [objetivo] | npm ci | npm install (siempre con --ignore-scripts) | npm rebuild [paquete] | npx <tsc|eslint|jest|vitest|prettier> … | npx prisma generate | npx prisma db push | npx prisma migrate deploy (estos dos contra la base de pruebas) | cargo <test|check|build|clippy|fmt|fetch> | go <test|build|vet> | go mod download | pytest | python -m <pytest|unittest|ruff|mypy>";
    let nada = || -> R<Permitido> { Err(format!("Comando fuera de la lista permitida. Se permite: {lista}.")) };
    match (t.first().copied(), t.get(1).copied()) {
        (Some("npm"), Some("test")) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npm", "test"], resto(2)), red: false }),
        (Some("npm"), Some("run")) if t.get(2).map(|s| !s.starts_with('-')).unwrap_or(false) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npm", "run"], resto(2)), red: false }),
        (Some("npm"), Some("ci")) | (Some("npm"), Some("install")) => {
            // Los scripts de instalación de los paquetes NO corren con red: se instala con --ignore-scripts. Si un paquete necesita su script
            // (p. ej. esbuild), se ejecuta después, sin red, con «npm rebuild <paquete>».
            if t.iter().skip(2).any(|x| x.contains("script")) {
                return Err("No se permiten opciones sobre los scripts de instalación: la instalación siempre usa --ignore-scripts. Usa «npm rebuild <paquete>» (sin red) si un paquete lo necesita.".into());
            }
            Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npm", t[1], "--ignore-scripts", "--no-audit", "--no-fund"], resto(2)), red: true })
        }
        (Some("npm"), Some("rebuild")) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npm", "rebuild"], resto(2)), red: false }),
        (Some("npx"), Some(x)) if ["tsc", "eslint", "jest", "vitest", "prettier"].contains(&x) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npx", "--no-install"], resto(1)), red: false }),
        (Some("npx"), Some("prisma")) if t.get(2).copied() == Some("generate") => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npx", "--no-install", "prisma", "generate"], resto(3)), red: true }),
        (Some("npx"), Some("prisma")) if matches!((t.get(2).copied(), t.get(3).copied()), (Some("db"), Some("push")) | (Some("migrate"), Some("deploy"))) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npx", "--no-install", "prisma", t[2], t[3]], (if t[2] == "db" { vec!["--skip-generate".to_string()] } else { vec![] }).into_iter().chain(resto(4)).collect()), red: false }),
        (Some("cargo"), Some("fetch")) => Ok(Permitido { imagen: "rust:1-bookworm", args: con(&["cargo", "fetch"], resto(2)), red: true }),
        (Some("cargo"), Some(x)) if ["test", "check", "build", "clippy"].contains(&x) => Ok(Permitido { imagen: "rust:1-bookworm", args: con(&["cargo", x, "--offline"], resto(2)), red: false }),
        (Some("cargo"), Some("fmt")) => Ok(Permitido { imagen: "rust:1-bookworm", args: con(&["cargo", "fmt"], resto(2)), red: false }),
        (Some("go"), Some("mod")) if t.get(2).copied() == Some("download") && t.len() == 3 => Ok(Permitido { imagen: "golang:1.23-bookworm", args: con(&["go", "mod", "download"], vec![]), red: true }),
        (Some("go"), Some(x)) if ["test", "build", "vet"].contains(&x) => Ok(Permitido { imagen: "golang:1.23-bookworm", args: con(&["go", x], resto(2)), red: false }),
        (Some("pytest"), _) => Ok(Permitido { imagen: "python:3.12-bookworm", args: con(&["pytest"], resto(1)), red: false }),
        (Some("python"), Some("-m")) | (Some("python3"), Some("-m")) if t.get(2).map(|s| ["pytest", "unittest", "ruff", "mypy"].contains(s)).unwrap_or(false) => Ok(Permitido { imagen: "python:3.12-bookworm", args: con(&["python", "-m"], resto(2)), red: false }),
        // Scripts propios del repositorio (los que la IA escribe o ya existían): corren en el contenedor, sin red
        (Some("node"), Some(f)) if !f.starts_with('-') && (f.ends_with(".js") || f.ends_with(".mjs") || f.ends_with(".cjs")) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["node"], resto(1)), red: false }),
        (Some("python"), Some(f)) | (Some("python3"), Some(f)) if !f.starts_with('-') && f.ends_with(".py") => Ok(Permitido { imagen: "python:3.12-bookworm", args: con(&["python"], resto(1)), red: false }),
        (Some("sh"), Some(f)) | (Some("bash"), Some(f)) if !f.starts_with('-') && f.ends_with(".sh") => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["bash"], resto(1)), red: false }),
        (Some("npx"), Some("tsx")) if t.get(2).map(|f| !f.starts_with('-') && (f.ends_with(".ts") || f.ends_with(".mts"))).unwrap_or(false) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["npx", "--no-install"], resto(1)), red: false }),
        (Some("make"), _) => Ok(Permitido { imagen: "node:22-bookworm", args: con(&["make"], resto(1)), red: false }),
        _ => nada(),
    }
}

async fn ejecutar(ws: &Path, sol: &str, sid: &str, b: &Value) -> R<Value> {
    let codigo = txt(b, "codigo");
    let (cmd_txt, filtros) = if codigo.trim().is_empty() { separar_filtros(&txt(b, "comando"))? } else { (String::new(), vec![]) };
    let p = if codigo.trim().is_empty() { permitir(&cmd_txt)? } else { permitir_codigo(&txt(b, "lenguaje"), &codigo)? };
    // «red_aprobada» solo la pone el servidor cuando una persona confirmó ese comando con red (nunca viene de la IA)
    let aprobado = b["red_aprobada"] == true;
    if p.args.first().map(String::as_str) == Some("curl") && !aprobado {
        return Err("curl necesita internet: pídelo con red=true y un administrador debe confirmarlo.".into());
    }
    let con_red = p.red || aprobado;
    let svcs = servicios_activos(sid).await;
    let vars = variables_de(sol);
    let mut entorno = entorno_servicios(sid, &svcs);
    entorno.extend(vars.iter().cloned());
    let t = b["timeout"].as_u64().unwrap_or(300).clamp(10, 900);
    let _candado = Candado::tomar(sid)?;
    let _permiso = cupo().await?;
    let nombre = format!("dev-{}-{}", corto(sid), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) % 100_000_000);
    let ws_s = format!("{}:/work", ws.to_string_lossy());
    let mut a: Vec<String> = ["run", "--rm", "--name", nombre.as_str(), "--memory=2g", "--memory-swap=2g", "--cpus=2", "--pids-limit=512", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--tmpfs", "/tmp:rw,exec,size=512m", "-v", ws_s.as_str(), "-w", "/work", "-e", "CI=1", "-e", "HOME=/tmp", "-e", "npm_config_cache=/tmp/.npm"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    a.push(if con_red { "--network=bridge".into() } else if !svcs.is_empty() { format!("--network={RED_INTERNA}") } else { "--network=none".into() });
    for (k, v) in &entorno {
        a.extend(["-e".to_string(), format!("{k}={v}")]);
    }
    if p.imagen.starts_with("rust") {
        a.extend(["-v".to_string(), "dev-cargo-registry:/usr/local/cargo/registry".to_string()]);
    }
    if p.imagen.starts_with("golang") {
        a.extend(["-v".to_string(), "dev-go-mod:/go/pkg/mod".to_string(), "-e".to_string(), "GOFLAGS=-mod=mod".to_string()]);
        if !p.red {
            a.extend(["-e".to_string(), "GOPROXY=off".to_string()]);
        }
    }
    a.push(p.imagen.to_string());
    a.extend(p.args.iter().cloned());
    let args: Vec<&str> = a.iter().map(String::as_str).collect();
    // +60 s de margen para descargar la imagen la primera vez
    match sh_salida("docker", &args, None, t + 60).await {
        Err(e) => {
            let _ = sh_salida("docker", &["rm", "-f", &nombre], None, 30).await;
            Ok(json!({ "texto": format!("El comando se detuvo: {} (límite {t} s).", cabeza(&e, 200)) }))
        }
        Ok((ok, out, err)) => {
            let mut salida = out;
            if !err.trim().is_empty() {
                salida.push_str(&format!("\n--- stderr ---\n{err}"));
            }
            let salida = ocultar(&salida, &vars);
            let salida = if filtros.is_empty() { salida } else { aplicar_filtros(&salida, &filtros) };
            Ok(json!({ "texto": format!("{}{}\n{}", if ok { "TERMINÓ CON ÉXITO (código 0)" } else { "FALLÓ (código distinto de 0)" }, if con_red { " · con red" } else if !svcs.is_empty() { " · red interna con tus servicios (sin internet)" } else { " · sin red" }, cola(salida.trim(), MAX_SALIDA)) }))
        }
    }
}

// ── Git de la sesión ────────────────────────────────────────────────────────────────────────
async fn resumen_cambios(ws: &Path) -> Value {
    let _ = git(&["add", "-N", "."], ws).await;
    let status = git(&["status", "--short"], ws).await.unwrap_or_default();
    let stat = git(&["diff", "--stat", "HEAD"], ws).await.unwrap_or_default();
    let commits = match base_de(ws).await {
        Some(base) => git(&["log", "--oneline", &format!("{base}..HEAD")], ws).await.unwrap_or_default(),
        None => String::new(),
    };
    json!({ "archivos": status.lines().map(str::to_string).collect::<Vec<_>>(), "stat": stat.trim(), "commits": commits.lines().map(str::to_string).collect::<Vec<_>>() })
}

async fn diff(ws: &Path, b: &Value) -> R<Value> {
    let r = resumen_cambios(ws).await;
    let mut t = format!(
        "ARCHIVOS CAMBIADOS (sin commit):\n{}\n\nRESUMEN:\n{}\n\nCOMMITS DE LA RAMA:\n{}",
        r["archivos"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("\n")).filter(|s| !s.is_empty()).unwrap_or_else(|| "(ninguno)".into()),
        r["stat"].as_str().filter(|s| !s.is_empty()).unwrap_or("(sin cambios)"),
        r["commits"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("\n")).filter(|s| !s.is_empty()).unwrap_or_else(|| "(ninguno todavía)".into())
    );
    if b["completo"].as_bool().unwrap_or(false) {
        let d = git(&["diff", "HEAD"], ws).await.unwrap_or_default();
        t.push_str(&format!("\n\nDIFF:\n{}", cabeza(&d, 12_000)));
    }
    Ok(json!({ "texto": t }))
}

async fn commit(ws: &Path, b: &Value) -> R<Value> {
    let mensaje = txt(b, "mensaje");
    let mensaje = mensaje.trim();
    if mensaje.is_empty() {
        return Err("Falta el mensaje del commit.".into());
    }
    let status = git(&["status", "--porcelain"], ws).await?;
    if status.trim().is_empty() {
        return Err("No hay cambios para commitear.".into());
    }
    let secretos: Vec<&str> = status.lines().map(|l| l.get(3..).unwrap_or("").trim()).filter(|p| es_secreto(p)).collect();
    if !secretos.is_empty() {
        return Err(format!("Hay archivos de secretos entre los cambios ({}): quítalos o agrégalos al .gitignore antes de commitear.", secretos.join(", ")));
    }
    git(&["add", "-A"], ws).await?;
    let mut a: Vec<&str> = IDENTIDAD.to_vec();
    let m: String = mensaje.chars().take(300).collect();
    a.extend_from_slice(&["commit", "-q", "-m", m.as_str()]);
    git(&a, ws).await?;
    let sha = git(&["rev-parse", "--short", "HEAD"], ws).await?;
    Ok(json!({ "texto": format!("Commit {} creado en la rama de la sesión. No se sube: lo sube una persona.", sha.trim()) }))
}

async fn subir(st: &AppState, ws: &Path, rama: &str) -> R<Value> {
    let r = resumen_cambios(ws).await;
    if r["commits"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err("La rama todavía no tiene commits propios: la sesión debe hacer commit antes de subirla.".into());
    }
    git_con_credenciales(st, &["push", "-u", "origin", rama], ws).await?;
    let remoto = git(&["remote", "get-url", "origin"], ws).await.unwrap_or_default();
    static R: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)github\.com[:/]+([^/]+)/([^/]+?)(?:\.git)?/?$").expect("re"));
    let url = R.captures(remoto.trim()).map(|c| format!("https://github.com/{}/{}/compare/{}?expand=1", &c[1], &c[2], rama.replace('/', "%2F")));
    Ok(json!({ "ok": true, "rama": rama, "urlPr": url }))
}

/// Contenedores de servicio de la sesión dueña de este espacio de trabajo (el nombre del espacio lleva el código corto).
async fn servicios_activos_de_ws(ws: &Path) -> Vec<String> {
    let corto_ws = ws.file_name().map(|n| n.to_string_lossy().trim_start_matches("dev-").to_string()).unwrap_or_default();
    if corto_ws.is_empty() {
        return vec![];
    }
    let filtro = format!("label=dev-sesion={corto_ws}");
    sh_salida("docker", &["ps", "--filter", &filtro, "--format", "{{.Names}}"], None, 30).await.map(|(_, o, _)| o.lines().map(String::from).collect()).unwrap_or_default()
}

async fn descartar(raiz: &Path, ws: &Path, rama: &str) -> R<Value> {
    for t in servicios_activos_de_ws(ws).await {
        let _ = sh_salida("docker", &["rm", "-f", &t], None, 60).await;
    }
    let ws_s = ws.to_string_lossy().to_string();
    let _ = git(&["worktree", "remove", "--force", &ws_s], raiz).await;
    let _ = git(&["branch", "-D", rama], raiz).await;
    let _ = git(&["worktree", "prune"], raiz).await;
    Ok(json!({ "ok": true }))
}

/// Punto único de entrada de `/api/dev/{accion}`.
pub async fn manejar(st: &AppState, accion: &str, b: &Value) -> R<Value> {
    let (sol, sid) = (txt(b, "solucionId"), txt(b, "sesionId"));
    if accion == "variables" {
        return variables(&sol, b);
    }
    if accion == "estado" {
        validar_repo(st, &sol).await?;
        let ws = ruta_ws(&sid);
        if !ws.join(".git").exists() {
            return Ok(json!({ "existe": false, "rama": rama_de(&sid) }));
        }
        let mut r = resumen_cambios(&ws).await;
        r["existe"] = json!(true);
        r["rama"] = json!(rama_de(&sid));
        r["capturas"] = json!(lista_capturas(&sid));
        r["servicios"] = json!(servicios_activos(&sid).await);
        return Ok(r);
    }
    if accion == "captura" {
        return captura(&sid, &txt(b, "nombre"));
    }
    let (ws, rama, raiz) = espacio(st, &sol, &sid).await?;
    match accion {
        "mover" => mover(&ws, b),
        "borrar" => borrar(&ws, b),
        "git" => git_avanzado(st, &ws, b).await,
        "verificar" => verificar(st, &sol, &ws, &sid, b).await,
        "chequeo" => chequeo(&ws, &sol, &sid).await,
        "servicio" => servicio(st, &sol, &ws, &sid, b).await,
        "carga" => carga(st, &sid, b).await,
        "flujo" => flujo(&sid, b).await,
        "traer_sprint" => traer_sprint(&raiz, &ws, b).await,
        "integrar" => integrar(&raiz, &rama, b).await,
        "listar" => listar(&ws, b),
        "leer" => leer(&ws, b),
        "buscar" => buscar(&ws, b).await,
        "escribir" => escribir(&ws, b),
        "editar" => editar(&ws, b),
        "ejecutar" => ejecutar(&ws, &sol, &sid, b).await,
        "diff" => diff(&ws, b).await,
        "commit" => commit(&ws, b).await,
        "subir" => subir(st, &ws, &rama).await,
        "descartar" => descartar(&raiz, &ws, &rama).await,
        otra => Err(format!("Acción desconocida: {otra}")),
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn las_rutas_no_salen_del_repositorio() {
        let base = std::env::temp_dir().join(format!("dev-test-{}", std::process::id()));
        std::fs::create_dir_all(base.join("src")).unwrap();
        assert!(seguro(&base, "src/a.rs").is_ok());
        assert!(seguro(&base, "./src/../src/a.rs").is_err());
        assert!(seguro(&base, "../fuera").is_err());
        assert!(seguro(&base, "/etc/passwd").is_err());
        assert!(seguro(&base, ".git/config").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", base.join("enlace")).unwrap();
            assert!(seguro(&base, "enlace/passwd").is_err(), "un enlace simbólico no debe sacar de la carpeta");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn los_secretos_se_detectan_y_los_ejemplos_no() {
        for s in [".env", "app/.env.local", "claves/server.pem", "id_rsa", ".npmrc", "x/credentials.json"] {
            assert!(es_secreto(s), "{s}");
        }
        for s in [".env.example", "src/main.rs", "README.md", "envio.txt"] {
            assert!(!es_secreto(s), "{s}");
        }
    }

    #[test]
    fn editar_exige_texto_unico_y_escribir_crea_carpetas() {
        let base = std::env::temp_dir().join(format!("dev-test-ed-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        escribir(&base, &json!({ "ruta": "a/b/c.txt", "contenido": "uno\ndos\nuno\n" })).unwrap();
        assert!(editar(&base, &json!({ "ruta": "a/b/c.txt", "buscar": "uno", "reemplazar": "UNO" })).is_err(), "aparece dos veces");
        editar(&base, &json!({ "ruta": "a/b/c.txt", "buscar": "dos", "reemplazar": "DOS" })).unwrap();
        editar(&base, &json!({ "ruta": "a/b/c.txt", "buscar": "uno", "reemplazar": "1", "todos": true })).unwrap();
        assert_eq!(std::fs::read_to_string(base.join("a/b/c.txt")).unwrap(), "1\nDOS\n1\n");
        assert!(editar(&base, &json!({ "ruta": "a/b/c.txt", "buscar": "nada", "reemplazar": "x" })).is_err());
        assert!(escribir(&base, &json!({ "ruta": ".env", "contenido": "X=1" })).is_err());
        assert!(leer(&base, &json!({ "ruta": "a/b/c.txt", "desde": 2, "hasta": 2 })).unwrap()["texto"].as_str().unwrap().contains("2\tDOS"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn los_filtros_de_salida_son_una_lista_cerrada() {
        let (c, f) = separar_filtros("npm test | grep -i error | head -n 5").unwrap();
        assert_eq!(c, "npm test");
        assert_eq!(f, vec![Filtro::Grep { patron: "error".into(), ignorar: true, invertir: false, contar: false }, Filtro::Head(5)]);
        assert!(separar_filtros("npm test | sh").is_err() && separar_filtros("npm test | tee a").is_err() && separar_filtros("npm test | grep a b").is_err());
        assert!(separar_filtros("npm test | head | head | head | head").is_err());
        let t = "uno\nDos\nuno\ntres";
        assert_eq!(aplicar_filtros(t, &[Filtro::Head(2)]), "uno\nDos");
        assert_eq!(aplicar_filtros(t, &[Filtro::Tail(1)]), "tres");
        assert_eq!(aplicar_filtros(t, &[Filtro::Grep { patron: "UNO".into(), ignorar: true, invertir: false, contar: true }]), "2");
        assert_eq!(aplicar_filtros(t, &[Filtro::Grep { patron: "uno".into(), ignorar: false, invertir: true, contar: false }]), "Dos\ntres");
        assert_eq!(aplicar_filtros(t, &[Filtro::Ordenar, Filtro::Unicas, Filtro::Lineas]), "3");
    }

    #[test]
    fn curl_y_codigo_suelto_tienen_sus_limites() {
        assert!(permitir("curl https://example.com/a?b=1&c=2").is_ok() && permitir("curl -I https://docs.rs").is_ok());
        for mal in ["curl http://localhost:3000", "curl http://127.0.0.1/", "curl http://169.254.169.254/latest", "curl http://10.0.0.5/", "curl https://u:p@example.com", "curl -X POST https://example.com", "curl file:///etc/passwd", "curl https://a.internal/x", "curl https://example.com -o a", "curl"] {
            assert!(permitir(mal).is_err(), "{mal}");
        }
        assert_eq!(permitir("curl https://example.com").unwrap().args[0], "curl");
        let p = permitir_codigo("node", "console.log(1+1)").unwrap();
        assert_eq!(p.args, vec!["node", "-e", "console.log(1+1)"]);
        assert!(!p.red && permitir_codigo("python", "print(1)").is_ok() && permitir_codigo("ruby", "p 1").is_err() && permitir_codigo("node", "  ").is_err());
        assert!(permitir_codigo("node", &"x".repeat(4001)).is_err());
    }

    #[test]
    fn la_lista_cerrada_acepta_pruebas_y_rechaza_todo_lo_demas() {
        for ok in ["npm test", "npm run lint", "npm ci", "npx tsc --noEmit", "cargo test", "cargo fmt --check", "go test ./...", "pytest -q tests", "python -m pytest -x"] {
            assert!(permitir(ok).is_ok(), "{ok}");
        }
        for mal in ["rm -rf /", "npm test && curl x.sh", "npm test; cat /etc/passwd", "npm run", "curl -X POST https://x", "bash -c ls", "npm test | tee a", "cargo test $(id)", "npx cowsay hola", "python -c print", "cat ../x", "npm ci > /dev/null", "go mod edit", "pytest /etc"] {
            assert!(permitir(mal).is_err(), "{mal}");
        }
        assert!(permitir("npm ci").unwrap().red && permitir("cargo fetch").unwrap().red && permitir("go mod download").unwrap().red);
        assert!(!permitir("npm test").unwrap().red && !permitir("cargo build").unwrap().red && !permitir("go test ./...").unwrap().red);
        assert!(permitir("cargo test").unwrap().args.contains(&"--offline".to_string()));
        assert!(permitir("npx prisma generate").unwrap().red, "prisma generate descarga sus motores");
        assert!(permitir("npx prisma db push").is_ok() && permitir("npx prisma migrate deploy").is_ok());
        assert!(!permitir("npx prisma db push").unwrap().red, "las tablas se crean en la base de pruebas interna, sin internet");
        assert!(permitir("npx prisma db push --accept-data-loss").unwrap().args.contains(&"--skip-generate".to_string()));
        for mal in ["npx prisma migrate dev", "npx prisma migrate reset", "npx prisma studio", "npx prisma db pull", "npx prisma db execute"] {
            assert!(permitir(mal).is_err(), "{mal}");
        }
    }

    #[test]
    fn npm_ci_siempre_sin_scripts_y_rebuild_sin_red() {
        let p = permitir("npm ci").unwrap();
        assert!(p.red && p.args.contains(&"--ignore-scripts".to_string()));
        assert!(permitir("npm install --ignore-scripts=false").is_err(), "no se puede reactivar los scripts");
        assert!(permitir("npm ci --scripts-prepend-node-path").is_err());
        let r = permitir("npm rebuild esbuild").unwrap();
        assert!(!r.red && r.args == vec!["npm", "rebuild", "esbuild"]);
    }

    #[test]
    fn mover_y_borrar_dentro_del_repo() {
        let base = std::env::temp_dir().join(format!("dev-test-mv-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        escribir(&base, &json!({ "ruta": "a.txt", "contenido": "x" })).unwrap();
        mover(&base, &json!({ "desde": "a.txt", "hacia": "sub/b.txt" })).unwrap();
        assert!(base.join("sub/b.txt").exists() && !base.join("a.txt").exists());
        assert!(mover(&base, &json!({ "desde": "sub/b.txt", "hacia": "../fuera.txt" })).is_err());
        assert!(mover(&base, &json!({ "desde": "sub/b.txt", "hacia": ".env" })).is_err());
        assert!(borrar(&base, &json!({ "ruta": "sub" })).is_err(), "carpeta sin recursivo");
        assert!(borrar(&base, &json!({ "ruta": "." })).is_err());
        assert!(borrar(&base, &json!({ "ruta": ".git" })).is_err());
        borrar(&base, &json!({ "ruta": "sub", "recursivo": true })).unwrap();
        assert!(!base.join("sub").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn revisiones_y_comandos_de_arranque_validos() {
        for ok in ["HEAD", "HEAD~1", "a1b2c3d", "origin/main", "sesion/abc12345"] {
            assert!(rev_seguro(ok).is_ok(), "{ok}");
        }
        for mal in ["--help", "a..b", "x;y", "$(id)", ""] {
            assert!(rev_seguro(mal).is_err(), "{mal}");
        }
        assert!(permitir_inicio("npm run dev -- --host 0.0.0.0").is_ok() && permitir_inicio("npm start").is_ok());
        assert!(permitir_inicio("npm run build").is_err() && permitir_inicio("node server.js").is_err() && permitir_inicio("npm run dev && ls").is_err());
        assert!(captura("abc", "../x.png").is_err() && captura("abc", "a.txt").is_err());
    }

    #[test]
    fn la_politica_del_proyecto_protege_archivos() {
        let base = std::env::temp_dir().join(format!("dev-test-pol-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(".masd-policy.json"), r#"{ "escritura": ["next.config.js", "prisma/schema.prisma"] }"#).unwrap();
        assert!(proteger(&base, "next.config.js").is_err());
        assert!(proteger(&base, "app/prisma/schema.prisma").is_err(), "también en subcarpetas");
        assert!(proteger(&base, ".masd-policy.json").is_err(), "la política misma tampoco");
        assert!(proteger(&base, "src/main.ts").is_ok());
        assert!(escribir(&base, &json!({ "ruta": "next.config.js", "contenido": "x" })).is_err());
        assert!(borrar(&base, &json!({ "ruta": "prisma/schema.prisma" })).is_err());
        assert!(mover(&base, &json!({ "desde": "a.txt", "hacia": "next.config.js" })).is_err());
        escribir(&base, &json!({ "ruta": "src/ok.ts", "contenido": "x" })).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn los_scripts_propios_y_make_se_permiten_pero_no_lo_libre() {
        for ok in ["node scripts/seed.js", "node prueba.mjs --rapido", "python tests/e2e.py", "bash scripts/probar.sh", "npx tsx scripts/a.ts", "make test", "npm run seed:prueba", "npm run e2e"] {
            assert!(permitir(ok).is_ok(), "{ok}");
        }
        for mal in ["node -e process.exit()", "node --eval x", "python -c x", "bash -c ls", "sh archivo.txt", "node /etc/passwd.js", "node ../x.js", "npm run -- --foo", "make && ls", "npx tsx -e x", "python3 tests/x.py && id"] {
            assert!(permitir(mal).is_err(), "{mal}");
        }
        assert!(!permitir("node scripts/seed.js").unwrap().red, "los scripts propios nunca tienen red");
    }

    #[test]
    fn las_variables_de_prueba_se_validan_y_la_salida_las_oculta() {
        assert!(nombre_variable_ok("API_KEY_TEST") && nombre_variable_ok("STRIPE_SECRET_1"));
        for mal in ["api_key", "1ABC", "", "A B", "A-B"] {
            assert!(!nombre_variable_ok(mal), "{mal}");
        }
        let vars = vec![("TOKEN".to_string(), "sk_test_ABC123xyz".to_string()), ("CORTA".to_string(), "abc".to_string())];
        let o = ocultar("token=sk_test_ABC123xyz y abc", &vars);
        assert!(!o.contains("sk_test_ABC123xyz") && o.contains("«oculto»") && o.contains("abc"), "{o}");
    }

    #[test]
    fn el_entorno_de_servicios_no_se_pisa() {
        let e = entorno_servicios("abcdefgh1234", &["mysql".to_string(), "postgres".to_string(), "redis".to_string()]);
        let get = |k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
        assert!(get("DATABASE_URL").unwrap().starts_with("postgresql://"), "postgres tiene prioridad en DATABASE_URL");
        assert!(get("MYSQL_URL").unwrap().starts_with("mysql://") && get("REDIS_URL").unwrap().contains("dev-svc-abcdefgh-redis"));
        assert!(entorno_servicios("x", &[]).is_empty());
    }

    #[test]
    fn solo_una_ejecucion_por_sesion() {
        let a = Candado::tomar("s1").unwrap();
        assert!(Candado::tomar("s1").is_err());
        assert!(Candado::tomar("s2").is_ok());
        drop(a);
        assert!(Candado::tomar("s1").is_ok());
    }
}
