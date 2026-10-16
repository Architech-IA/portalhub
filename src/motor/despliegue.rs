//! Despliegue de un proyecto a una URL real, base de datos por proyecto, migraciones y variables de
//! entorno (`lib/executor/deploy.ts` y `secrets.ts`). Corre en el servicio privilegiado: usa Docker,
//! Nginx y archivos de `/root/deploys`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::repo::{resolver_repo, sh, slug_texto, R};
use crate::{
    state::AppState,
    util::{exec, fetch_json_opt, B},
};

const DEPLOYS: &str = "/root/deploys";
const DOMINIO: &str = "demos.architechia.co";
const PUERTO_INI: i64 = 4100;
const PUERTO_FIN: i64 = 4199;
const ENV_DIR: &str = "/root/deploys/env";

pub fn slug_deploy(texto: &str) -> String {
    slug_texto(texto, 40, "proyecto")
}

fn dockerfile_generico() -> PathBuf {
    Path::new(DEPLOYS).join("_Dockerfile.generic-nextjs")
}

// ── Variables de entorno por proyecto ────────────────────────────────────────────────────────
fn ruta_env(nombre: &str) -> PathBuf {
    let _ = std::fs::create_dir_all(ENV_DIR);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(ENV_DIR, std::fs::Permissions::from_mode(0o700));
    }
    Path::new(ENV_DIR).join(format!("{}.env", slug_deploy(nombre)))
}

fn leer_env(p: &Path) -> BTreeMap<String, String> {
    // JS conserva el orden de inserción; aquí se conserva el de lectura vía `Vec` para no reordenar.
    let mut out = BTreeMap::new();
    for linea in std::fs::read_to_string(p).unwrap_or_default().lines() {
        let t = linea.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(i) = t.find('=') {
            out.insert(t[..i].trim().to_string(), t[i + 1..].to_string());
        }
    }
    out
}

fn leer_env_ordenado(p: &Path) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![];
    for linea in std::fs::read_to_string(p).unwrap_or_default().lines() {
        let t = linea.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(i) = t.find('=') {
            let (k, val) = (t[..i].trim().to_string(), t[i + 1..].to_string());
            match v.iter_mut().find(|(x, _)| *x == k) {
                Some(e) => e.1 = val,
                None => v.push((k, val)),
            }
        }
    }
    v
}

fn escribir_env(p: &Path, vars: &[(String, String)]) -> R<()> {
    let texto = vars.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n") + "\n";
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p).map_err(|e| e.to_string())?;
        f.write_all(texto.as_bytes()).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(p, texto).map_err(|e| e.to_string())
    }
}

/// Solo los NOMBRES de variable — nunca los valores.
pub fn listar_variables(proyecto: &str) -> Vec<String> {
    let p = ruta_env(proyecto);
    if !p.exists() {
        return vec![];
    }
    leer_env_ordenado(&p).into_iter().map(|(k, _)| k).collect()
}

pub fn guardar_variable(proyecto: &str, nombre: &str, valor: &str) -> R<()> {
    let valido = !nombre.is_empty() && nombre.chars().enumerate().all(|(i, c)| c == '_' || c.is_ascii_uppercase() || (i > 0 && c.is_ascii_digit()));
    if !valido {
        return Err(format!("Nombre de variable inválido: \"{nombre}\" — solo MAYÚSCULAS, números y guión bajo, sin empezar con número."));
    }
    let p = ruta_env(proyecto);
    let mut vars = if p.exists() { leer_env_ordenado(&p) } else { vec![] };
    match vars.iter_mut().find(|(k, _)| k == nombre) {
        Some(e) => e.1 = valor.to_string(),
        None => vars.push((nombre.to_string(), valor.to_string())),
    }
    escribir_env(&p, &vars)
}

pub fn borrar_variable(proyecto: &str, nombre: &str) -> R<()> {
    let p = ruta_env(proyecto);
    if !p.exists() {
        return Ok(());
    }
    let vars: Vec<(String, String)> = leer_env_ordenado(&p).into_iter().filter(|(k, _)| k != nombre).collect();
    let _ = leer_env; // (se conserva el lector simple por si hace falta)
    escribir_env(&p, &vars)
}

fn env_si_existe(proyecto: &str) -> Option<String> {
    let p = ruta_env(proyecto);
    p.exists().then(|| p.to_string_lossy().to_string())
}

// ── Despliegue ───────────────────────────────────────────────────────────────────────────────
async fn puerto_libre(st: &AppState) -> R<i64> {
    let usados = crate::util::fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg("deployPort"), '[]'::jsonb) FROM "Solucion" WHERE "deployPort" IS NOT NULL"#, &[]).await.map_err(|e| e.to_string())?;
    let ocupados: Vec<i64> = usados.as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_i64()).collect();
    (PUERTO_INI..=PUERTO_FIN).find(|p| !ocupados.contains(p)).ok_or_else(|| "No hay puertos libres en el rango de despliegues (4100-4199).".to_string())
}

const DOCKERFILE: &str = r#"FROM node:20-alpine AS deps
WORKDIR /app
RUN apk add --no-cache openssl
COPY package.json package-lock.json* ./
COPY prisma ./prisma
RUN npm ci || npm install

FROM node:20-alpine AS builder
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
RUN mkdir -p public
# El servidor no tiene swap y comparte RAM con el portal: se limita el heap de Node para que una compilación grande falle sola en vez de arrastrar el host.
ENV NODE_OPTIONS=--max-old-space-size=1536
RUN npm run build
RUN mkdir -p /salida && \
    if [ -d .next/standalone ]; then \
      cp -r .next/standalone/. /salida/ && \
      mkdir -p /salida/.next && \
      cp -r .next/static /salida/.next/static && \
      cp -r public /salida/public && \
      echo 'node server.js' > /salida/iniciar.sh; \
    else \
      cp -r node_modules /salida/node_modules && \
      cp -r .next /salida/.next && \
      cp -r public /salida/public && \
      cp package.json /salida/package.json && \
      echo 'npm start' > /salida/iniciar.sh; \
    fi && \
    cp -r prisma /salida/prisma && \
    if [ -d node_modules/.prisma ]; then mkdir -p /salida/node_modules/.prisma && cp -r node_modules/.prisma/. /salida/node_modules/.prisma/; fi && \
    if [ -d node_modules/@prisma ]; then mkdir -p /salida/node_modules/@prisma && cp -r node_modules/@prisma/. /salida/node_modules/@prisma/; fi

FROM node:20-alpine AS runner
WORKDIR /app
ENV NODE_ENV=production
ENV HOSTNAME=0.0.0.0
RUN apk add --no-cache openssl
RUN addgroup -g 1001 -S nodejs && adduser -S nextjs -u 1001
COPY --from=builder --chown=nextjs:nodejs /salida ./
USER nextjs
EXPOSE 3000
CMD ["sh", "iniciar.sh"]
"#;

fn asegurar_dockerfile() -> R<()> {
    std::fs::create_dir_all(DEPLOYS).map_err(|e| e.to_string())?;
    let p = dockerfile_generico();
    // Se reescribe si cambió la plantilla (antes solo se creaba si faltaba y los cambios nunca llegaban).
    if std::fs::read_to_string(&p).map(|c| c != DOCKERFILE).unwrap_or(true) {
        std::fs::write(&p, DOCKERFILE).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn asegurar_carpeta_prisma(repo: &Path) {
    let _ = std::fs::create_dir_all(repo.join("prisma"));
}

fn ruta_nginx(slug: &str) -> String {
    format!("/etc/nginx/sites-enabled/demo-{slug}")
}

const NGINX_PLANTILLA: &str = r#"server {
    listen 80;
    server_name @HOST@;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    server_name @HOST@;

    ssl_certificate /etc/letsencrypt/live/@DOMINIO@/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/@DOMINIO@/privkey.pem;

    client_max_body_size 20M;

    location / {
        proxy_pass http://localhost:@PUERTO@;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_cache_bypass $http_upgrade;

        add_header X-Frame-Options "SAMEORIGIN" always;
        add_header X-Content-Type-Options "nosniff" always;
        add_header Referrer-Policy "strict-origin-when-cross-origin" always;
    }
}
"#;

fn nginx_https(slug: &str, puerto: i64) -> String {
    NGINX_PLANTILLA.replace("@HOST@", &format!("{slug}.{DOMINIO}")).replace("@DOMINIO@", DOMINIO).replace("@PUERTO@", &puerto.to_string())
}

async fn esperar_contenedor(contenedor: &str, puerto: i64, intentos: u32) -> R<()> {
    for _ in 0..intentos {
        if sh("curl", &["-sf", "-o", "/dev/null", "-m", "3", &format!("http://localhost:{puerto}/")], None, 30).await.is_ok() {
            return Ok(());
        }
        let estado = sh("docker", &["inspect", "-f", "{{.State.Status}}", contenedor], None, 30).await.unwrap_or_else(|_| "?".into());
        if estado.trim() == "exited" {
            let logs = sh("docker", &["logs", "--tail", "30", contenedor], None, 30).await.unwrap_or_default();
            let cola: String = logs.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            return Err(format!("El contenedor se cerró solo antes de responder.\n{cola}"));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Err(format!("El contenedor no respondió en el puerto {puerto} después de {}s.", intentos * 2))
}

async fn sol_campos(st: &AppState, id: &str) -> R<Value> {
    fetch_json_opt(&st.pool, r#"SELECT to_jsonb(s) FROM "Solucion" s WHERE s.id = $1"#, &[B::T(id.into())]).await.map_err(|e| e.to_string())?.ok_or_else(|| "La Solución no existe.".to_string())
}

/// Pone en marcha un contenedor de la imagen indicada, espera a que responda, recién entonces apunta Nginx a él y retira el viejo.
async fn levantar(st: &AppState, sol: &Value, solucion_id: &str, nombre: &str, slug: &str, imagen: &str, viejo: &Option<String>) -> R<Value> {
        let nuevo = format!("demo-{slug}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
        let puerto = puerto_libre(st).await?;
        // El puerto se publica SOLO en localhost: Nginx es quien atiende a internet (con HTTPS). Antes quedaba en 0.0.0.0 y la app respondía por HTTP plano.
        let mut args: Vec<String> = ["run", "-d", "--name", &nuevo, "-p", &format!("127.0.0.1:{puerto}:3000"), "--restart", "unless-stopped"].iter().map(|s| s.to_string()).collect();
        if let Some(red) = sol["dbNetworkName"].as_str().filter(|x| !x.is_empty()) {
            args.extend(["--network".into(), red.to_string()]);
        }
        if let Some(env) = env_si_existe(&nombre) {
            args.extend(["--env-file".into(), env]);
        }
        args.push(imagen.to_string());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        sh("docker", &a, None, 300).await?;
        if let Err(e) = esperar_contenedor(&nuevo, puerto, 15).await {
            let _ = sh("docker", &["rm", "-f", &nuevo], None, 60).await;
            return Err(e);
        }
        // Recién ahora Nginx pasa a apuntar al nuevo.
        let nginx: R<()> = async {
            std::fs::write(ruta_nginx(&slug), nginx_https(&slug, puerto)).map_err(|e| e.to_string())?;
            sh("nginx", &["-t"], None, 60).await?;
            sh("systemctl", &["reload", "nginx"], None, 60).await?;
            Ok(())
        }
        .await;
        if let Err(e) = nginx {
            let _ = sh("docker", &["rm", "-f", &nuevo], None, 60).await;
            let _ = std::fs::remove_file(ruta_nginx(&slug));
            return Err(e);
        }
        if let Some(v) = &viejo {
            let _ = sh("docker", &["rm", "-f", v], None, 60).await;
        }
        let url = format!("https://{slug}.{DOMINIO}");
        exec(
            &st.pool,
            r#"UPDATE "Solucion" SET "deployUrl"=$1, "deployPort"=$2::int, "deployContainerName"=$3, "deployStatus"='LIVE', "deployedAt"=NOW(), "updatedAt"=NOW() WHERE id=$4"#,
            &[B::T(url.clone()), B::I(puerto), B::T(nuevo.clone()), B::T(solucion_id.into())],
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(json!({ "url": url, "puerto": puerto, "contenedor": nuevo, "slug": slug }))
}

/// Vuelve a la versión anterior de producción (la que había antes del último despliegue), sin reconstruir nada.
pub async fn revertir(st: &AppState, solucion_id: &str) -> R<Value> {
    let sol = sol_campos(st, solucion_id).await?;
    let nombre = sol["nombre"].as_str().unwrap_or("").to_string();
    let slug = slug_deploy(&nombre);
    let imagen = format!("demo-image-{slug}");
    if sh("docker", &["image", "inspect", &format!("{imagen}:anterior")], None, 60).await.is_err() {
        return Err("No hay una versión anterior guardada: solo existe después de un segundo despliegue.".into());
    }
    let viejo = sol["deployContainerName"].as_str().filter(|x| !x.is_empty()).map(String::from);
    if viejo.is_none() {
        return Err("El proyecto no está desplegado: no hay nada que revertir.".into());
    }
    exec(&st.pool, r#"UPDATE "Solucion" SET "deployStatus"='DEPLOYING', "updatedAt"=NOW() WHERE id=$1"#, &[B::T(solucion_id.into())]).await.map_err(|e| e.to_string())?;
    let r = levantar(st, &sol, solucion_id, &nombre, &slug, &format!("{imagen}:anterior"), &viejo).await;
    match &r {
        Ok(_) => {
            // La que acaba de salir queda como «anterior»: volver a revertir la recupera.
            let _ = sh("docker", &["tag", &format!("{imagen}:latest"), &format!("{imagen}:reemplazada")], None, 60).await;
            let _ = sh("docker", &["tag", &format!("{imagen}:anterior"), &format!("{imagen}:latest")], None, 60).await;
            let _ = sh("docker", &["tag", &format!("{imagen}:reemplazada"), &format!("{imagen}:anterior")], None, 60).await;
        }
        Err(_) => {
            let _ = exec(&st.pool, r#"UPDATE "Solucion" SET "deployStatus"='LIVE', "updatedAt"=NOW() WHERE id=$1"#, &[B::T(solucion_id.into())]).await;
        }
    }
    r
}

fn rama_valida(r: &str) -> bool {
    !r.is_empty() && r.len() <= 80 && !r.starts_with('-') && !r.contains("..") && r.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

/// Horas que vive un entorno de pruebas antes de retirarse solo.
const HORAS_PRUEBAS: u64 = 72;

fn ahora_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn archivo_pruebas(slug: &str, ext: &str) -> PathBuf {
    Path::new(DEPLOYS).join("pruebas").join(format!("{slug}.{ext}"))
}

fn guardar_privado(p: &Path, texto: &str) -> R<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, texto).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn cola_txt(t: &str, n: usize) -> String {
    let c: Vec<char> = t.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

async fn quitar_base(slug: &str) {
    let n = format!("demo-db-{slug}");
    let _ = sh("docker", &["rm", "-f", &n], None, 60).await;
    let _ = sh("docker", &["volume", "rm", &n], None, 60).await;
}

/// Un Postgres desechable por entorno de pruebas (red propia, sin puerto publicado). Devuelve la URL y si es nueva.
async fn crear_base_pruebas(slug: &str, reutilizar: bool) -> R<(String, bool)> {
    let (red, cont) = (format!("demo-net-{slug}"), format!("demo-db-{slug}"));
    let archivo = archivo_pruebas(slug, "env");
    if reutilizar {
        let url = std::fs::read_to_string(&archivo).unwrap_or_default().trim().to_string();
        let viva = sh("docker", &["inspect", "-f", "{{.State.Running}}", &cont], None, 30).await.map(|o| o.trim() == "true").unwrap_or(false);
        if !url.is_empty() && viva {
            return Ok((url, false));
        }
    }
    quitar_base(slug).await;
    red_asegurar(&red).await?;
    let clave = clave_aleatoria();
    let db = slug.replace('-', "_");
    let etiqueta = format!("demo-pruebas={slug}");
    sh(
        "docker",
        &[
            "run", "-d", "--name", &cont, "--network", &red, "--label", &etiqueta, "--label", "demo-rol=db", "--memory=512m", "--restart", "no", "-e", &format!("POSTGRES_PASSWORD={clave}"), "-e", &format!("POSTGRES_DB={db}"), "-e",
            "POSTGRES_USER=app", "-v", &format!("{cont}:/var/lib/postgresql/data"), "postgres:16-alpine",
        ],
        None,
        300,
    )
    .await?;
    esperar_postgres(&cont, "app", 20).await?;
    let url = format!("postgresql://app:{clave}@{cont}:5432/{db}");
    guardar_privado(&archivo, &url)?;
    Ok((url, true))
}

/// Crea las tablas en la base de pruebas: migrate deploy si el proyecto trae migraciones; si no, db push desde schema.prisma.
async fn preparar_esquema(dir: &Path, imagen: &str, red: &str, url: &str, vars: Option<&str>) -> String {
    if !dir.join("prisma").join("schema.prisma").exists() {
        return "Sin Prisma: la app recibe DATABASE_URL y debe crear sus propias tablas.".into();
    }
    let con_migraciones = std::fs::read_dir(dir.join("prisma").join("migrations")).map(|d| d.flatten().any(|e| e.path().is_dir())).unwrap_or(false);
    let (nombre, orden): (&str, Vec<&str>) = if con_migraciones { ("migrate deploy", vec!["migrate", "deploy"]) } else { ("db push (el proyecto no tiene migraciones)", vec!["db", "push", "--skip-generate"]) };
    let paquete = version_prisma(dir).map(|v| format!("prisma@{v}")).unwrap_or_else(|| "prisma".into());
    let mut a: Vec<String> = ["run", "--rm", "--user", "root", "--network", red].iter().map(|s| s.to_string()).collect();
    if let Some(f) = vars {
        a.extend(["--env-file".to_string(), f.to_string()]);
    }
    a.extend(["-e".to_string(), format!("DATABASE_URL={url}"), imagen.to_string(), "npx".to_string(), "--yes".to_string(), paquete]);
    a.extend(orden.iter().map(|s| s.to_string()));
    let r: Vec<&str> = a.iter().map(String::as_str).collect();
    match sh("docker", &r, None, 300).await {
        Ok(_) => format!("Esquema: {nombre} aplicado."),
        Err(e) => format!("Esquema: {nombre} FALLÓ — {}", cola_txt(&e, 300)),
    }
}

/// Corre el seed que el repositorio ya trae (prisma db seed o npm run seed), una sola vez y solo sobre una base nueva.
async fn sembrar(dir: &Path, red: &str, url: &str, vars: Option<&str>) -> Option<String> {
    let pkg: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).ok()?).ok()?;
    let comando = if pkg["prisma"]["seed"].is_string() {
        "npx prisma db seed"
    } else if pkg["scripts"]["seed"].is_string() {
        "npm run seed"
    } else {
        return None;
    };
    let script = format!("(npm ci --ignore-scripts --no-audit --no-fund || npm install --ignore-scripts --no-audit --no-fund) && (npx prisma generate || true) && {comando}");
    let montaje = format!("{}:/work", dir.to_string_lossy());
    let mut a: Vec<String> = [
        "run", "--rm", "--network", red, "--memory=2g", "--memory-swap=2g", "--cpus=2", "--pids-limit=512", "--cap-drop=ALL", "--security-opt=no-new-privileges", "-v", &montaje, "-w", "/work", "-e", "HOME=/tmp", "-e", "npm_config_cache=/tmp/.npm", "-e", "CI=1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(f) = vars {
        a.extend(["--env-file".to_string(), f.to_string()]);
    }
    a.extend(["-e".to_string(), format!("DATABASE_URL={url}"), "node:22-bookworm".to_string(), "sh".to_string(), "-c".to_string(), script]);
    let r: Vec<&str> = a.iter().map(String::as_str).collect();
    Some(match sh("docker", &r, None, 900).await {
        Ok(_) => format!("Semilla: se corrió «{comando}» del repositorio."),
        Err(e) => format!("Semilla: «{comando}» FALLÓ — {}", cola_txt(&e, 300)),
    })
}

/// Entorno de PRUEBAS: una copia aparte (otra URL) construida desde una rama que una persona ya subió, con su PROPIA base de datos desechable y solo las
/// variables de prueba del proyecto (nada de producción). Vence a las 72 h.
pub async fn desplegar_pruebas(st: &AppState, solucion_id: &str, rama: &str, base: &str, semilla: bool) -> R<Value> {
    if !rama_valida(rama) {
        return Err("Nombre de rama no válido.".into());
    }
    let sol = sol_campos(st, solucion_id).await?;
    let nombre = sol["nombre"].as_str().unwrap_or("").to_string();
    if sol["repositorio"].as_str().map(|r| r.is_empty()).unwrap_or(true) {
        return Err("Esta Solución no tiene repositorio asociado — no hay nada que desplegar.".into());
    }
    let slug = format!("{}-pruebas", slug_deploy(&nombre));
    let repo = resolver_repo(st, Some(solucion_id)).await?;
    super::repo::git_con_credenciales(st, &["fetch", "origin", rama], &repo).await.map_err(|_| format!("La rama «{rama}» no existe en el repositorio: una persona debe subirla primero."))?;
    let dir = PathBuf::from(format!("{DEPLOYS}/pruebas/{slug}"));
    let _ = sh("git", &["worktree", "remove", "--force", &dir.to_string_lossy()], Some(&repo), 60).await;
    let _ = std::fs::remove_dir_all(&dir);
    let _ = sh("git", &["worktree", "prune"], Some(&repo), 60).await;
    std::fs::create_dir_all(format!("{DEPLOYS}/pruebas")).map_err(|e| e.to_string())?;
    sh("git", &["worktree", "add", "--detach", &dir.to_string_lossy(), "FETCH_HEAD"], Some(&repo), 300).await?;
    let base_nueva = std::sync::atomic::AtomicBool::new(false);
    let r: R<Value> = async {
        asegurar_dockerfile()?;
        asegurar_carpeta_prisma(&dir);
        let imagen = format!("demo-image-{slug}");
        sh("docker", &["build", "-f", &dockerfile_generico().to_string_lossy(), "-t", &imagen, &dir.to_string_lossy()], None, 600).await?;
        let vars = super::dev::archivo_variables(solucion_id);
        let red = format!("demo-net-{slug}");
        let (url_db, nueva) = crear_base_pruebas(&slug, base == "conservar").await?;
        base_nueva.store(nueva, std::sync::atomic::Ordering::SeqCst);
        let mut notas: Vec<String> = vec![format!("Base de datos: {}.", if nueva { "nueva y vacía" } else { "la del entorno de pruebas anterior (conservada)" })];
        notas.push(preparar_esquema(&dir, &imagen, &red, &url_db, vars.as_deref()).await);
        if semilla && nueva {
            match sembrar(&dir, &red, &url_db, vars.as_deref()).await {
                Some(t) => notas.push(t),
                None => notas.push("Semilla: el repositorio no define ninguna (prisma db seed o el script «seed»).".into()),
            }
        } else if semilla {
            notas.push("Semilla omitida: la base se conservó y volver a sembrar duplicaría datos.".into());
        }
        notas.push(if vars.is_some() { "Variables de prueba del proyecto: inyectadas.".into() } else { "Variables de prueba: el proyecto no tiene ninguna.".to_string() });
        let puerto = (4200u16..4250).find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()).ok_or_else(|| "No hay puertos libres para pruebas (4200-4249).".to_string())?;
        let nuevo = format!("demo-{slug}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
        let etiqueta = format!("demo-pruebas={slug}");
        let publicar = format!("127.0.0.1:{puerto}:3000");
        let mut a: Vec<String> = ["run", "-d", "--name", &nuevo, "--label", &etiqueta, "--label", "demo-rol=app", "-p", &publicar, "--network", &red, "--memory=1g", "--restart", "no"].iter().map(|s| s.to_string()).collect();
        if let Some(f) = &vars {
            a.extend(["--env-file".to_string(), f.clone()]);
        }
        a.extend(["-e".to_string(), format!("DATABASE_URL={url_db}"), imagen.clone()]);
        let args: Vec<&str> = a.iter().map(String::as_str).collect();
        sh("docker", &args, None, 300).await?;
        if let Err(e) = esperar_contenedor(&nuevo, puerto as i64, 15).await {
            let _ = sh("docker", &["rm", "-f", &nuevo], None, 60).await;
            return Err(e);
        }
        let nginx: R<()> = async {
            std::fs::write(ruta_nginx(&slug), nginx_https(&slug, puerto as i64)).map_err(|e| e.to_string())?;
            sh("nginx", &["-t"], None, 60).await?;
            sh("systemctl", &["reload", "nginx"], None, 60).await?;
            Ok(())
        }
        .await;
        if let Err(e) = nginx {
            let _ = sh("docker", &["rm", "-f", &nuevo], None, 60).await;
            let _ = std::fs::remove_file(ruta_nginx(&slug));
            return Err(e);
        }
        let previos = sh("docker", &["ps", "-a", "--filter", &format!("label={etiqueta}"), "--filter", "label=demo-rol=app", "--format", "{{.Names}}"], None, 60).await.unwrap_or_default();
        for p in previos.lines().map(str::trim).filter(|p| !p.is_empty() && *p != nuevo) {
            let _ = sh("docker", &["rm", "-f", p], None, 60).await;
        }
        let expira = ahora_s() + HORAS_PRUEBAS * 3600;
        let _ = guardar_privado(&archivo_pruebas(&slug, "expira"), &expira.to_string());
        Ok(json!({ "url": format!("https://{slug}.{DOMINIO}"), "rama": rama, "puerto": puerto, "notas": notas, "expira": expira, "horas": HORAS_PRUEBAS }))
    }
    .await;
    if r.is_err() && base_nueva.load(std::sync::atomic::Ordering::SeqCst) {
        quitar_base(&slug).await;
    }
    let _ = sh("git", &["worktree", "remove", "--force", &dir.to_string_lossy()], Some(&repo), 60).await;
    r
}

/// Retira un entorno de pruebas completo: contenedores (app y base), volumen, red, Nginx y sus archivos.
async fn retirar_slug(slug: &str) {
    let previos = sh("docker", &["ps", "-a", "--filter", &format!("label=demo-pruebas={slug}"), "--format", "{{.Names}}"], None, 60).await.unwrap_or_default();
    for p in previos.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let _ = sh("docker", &["rm", "-f", p], None, 60).await;
    }
    quitar_base(slug).await;
    let _ = sh("docker", &["network", "rm", &format!("demo-net-{slug}")], None, 60).await;
    let _ = std::fs::remove_file(ruta_nginx(slug));
    let _ = std::fs::remove_file(archivo_pruebas(slug, "env"));
    let _ = std::fs::remove_file(archivo_pruebas(slug, "expira"));
    let _ = sh("systemctl", &["reload", "nginx"], None, 60).await;
}

/// Retira el entorno de pruebas de un proyecto (a mano).
pub async fn retirar_pruebas(st: &AppState, solucion_id: &str) -> R<Value> {
    let sol = sol_campos(st, solucion_id).await?;
    let slug = format!("{}-pruebas", slug_deploy(sol["nombre"].as_str().unwrap_or("")));
    retirar_slug(&slug).await;
    Ok(json!({ "retirado": true }))
}

/// Retira los entornos de pruebas cuyo plazo (72 h) ya venció. La llama la limpieza periódica del motor.
pub async fn limpiar_pruebas_vencidas() {
    let Ok(rd) = std::fs::read_dir(Path::new(DEPLOYS).join("pruebas")) else { return };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let Some(slug) = n.strip_suffix(".expira") else { continue };
        if std::fs::read_to_string(e.path()).ok().and_then(|t| t.trim().parse::<u64>().ok()).map(|x| x < ahora_s()).unwrap_or(false) {
            tracing::info!("[pruebas] entorno vencido retirado: {slug}");
            retirar_slug(slug).await;
        }
    }
}

/// Redeploy sin downtime: el contenedor nuevo se levanta en un puerto aparte y se confirma sano antes de
/// que Nginx deje de apuntar al viejo.
pub async fn desplegar(st: &AppState, solucion_id: &str) -> R<Value> {
    let sol = sol_campos(st, solucion_id).await?;
    let nombre = sol["nombre"].as_str().unwrap_or("").to_string();
    if sol["repositorio"].as_str().map(|r| r.is_empty()).unwrap_or(true) {
        return Err("Esta Solución no tiene repositorio asociado — no hay nada que desplegar.".into());
    }
    let viejo = sol["deployContainerName"].as_str().filter(|x| !x.is_empty()).map(String::from);
    exec(&st.pool, r#"UPDATE "Solucion" SET "deployStatus"='DEPLOYING', "updatedAt"=NOW() WHERE id=$1"#, &[B::T(solucion_id.into())]).await.map_err(|e| e.to_string())?;
    let r: R<Value> = async {
        let repo = resolver_repo(st, Some(solucion_id)).await?;
        // El despliegue siempre parte de lo ya mergeado y revisado en main.
        super::repo::git_con_credenciales(st, &["fetch", "origin", "main"], &repo).await?;
        sh("git", &["checkout", "main"], Some(&repo), 300).await?;
        sh("git", &["reset", "--hard", "origin/main"], Some(&repo), 300).await?;
        asegurar_dockerfile()?;
        asegurar_carpeta_prisma(&repo);
        let slug = slug_deploy(&nombre);
        let imagen = format!("demo-image-{slug}");
        // Se construye con una etiqueta aparte: la versión que está en producción sigue intacta hasta que la nueva responda.
        sh("docker", &["build", "-f", &dockerfile_generico().to_string_lossy(), "-t", &format!("{imagen}:nueva"), &repo.to_string_lossy()], None, 600).await?;
        let r = levantar(st, &sol, solucion_id, &nombre, &slug, &format!("{imagen}:nueva"), &viejo).await?;
        // Recién ahora la versión que estaba pasa a ser la «anterior» (a la que se puede volver) y la nueva, la vigente.
        let _ = sh("docker", &["tag", &format!("{imagen}:latest"), &format!("{imagen}:anterior")], None, 60).await;
        let _ = sh("docker", &["tag", &format!("{imagen}:nueva"), &format!("{imagen}:latest")], None, 60).await;
        Ok(r)
    }
    .await;
    if r.is_err() {
        // Si ya había un despliegue previo sano, el estado real sigue siendo LIVE.
        let _ = exec(&st.pool, r#"UPDATE "Solucion" SET "deployStatus"=$1, "updatedAt"=NOW() WHERE id=$2"#, &[B::T(if viejo.is_some() { "LIVE" } else { "FAILED" }.into()), B::T(solucion_id.into())]).await;
    }
    r
}

// ── Base de datos por proyecto ───────────────────────────────────────────────────────────────
async fn red_asegurar(nombre: &str) -> R<()> {
    let existe = sh("docker", &["network", "ls", "--filter", &format!("name=^{nombre}$"), "--format", "{{.Name}}"], None, 60).await.unwrap_or_default();
    if existe.trim().is_empty() {
        sh("docker", &["network", "create", nombre], None, 60).await?;
    }
    Ok(())
}

async fn esperar_postgres(contenedor: &str, usuario: &str, intentos: u32) -> R<()> {
    for _ in 0..intentos {
        if sh("docker", &["exec", contenedor, "pg_isready", "-U", usuario], None, 30).await.is_ok() {
            return Ok(());
        }
        let estado = sh("docker", &["inspect", "-f", "{{.State.Status}}", contenedor], None, 30).await.unwrap_or_else(|_| "?".into());
        if estado.trim() == "exited" {
            let logs = sh("docker", &["logs", "--tail", "30", contenedor], None, 30).await.unwrap_or_default();
            let cola: String = logs.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            return Err(format!("El contenedor de la base de datos se cerró solo antes de estar listo.\n{cola}"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    Err(format!("La base de datos no respondió después de {}s.", intentos as f64 * 1.5))
}

fn clave_aleatoria() -> String {
    use rand::Rng;
    const ALF: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut r = rand::thread_rng();
    (0..32).map(|_| ALF[r.gen_range(0..ALF.len())] as char).collect()
}

/// Un Postgres dedicado por proyecto, sin puerto publicado: solo alcanzable desde la app, por una red privada.
pub async fn aprovisionar_base(st: &AppState, solucion_id: &str) -> R<Value> {
    let sol = sol_campos(st, solucion_id).await?;
    if sol["dbContainerName"].as_str().map(|x| !x.is_empty()).unwrap_or(false) {
        return Err("Este proyecto ya tiene una base de datos aprovisionada.".into());
    }
    let nombre = sol["nombre"].as_str().unwrap_or("").to_string();
    let slug = slug_deploy(&nombre);
    let red = format!("demo-net-{slug}");
    let volumen = format!("demo-db-{slug}");
    let contenedor = format!("demo-db-{slug}");
    let db = slug.replace('-', "_");
    let usuario = "app";
    let clave = clave_aleatoria();
    exec(&st.pool, r#"UPDATE "Solucion" SET "dbStatus"='PROVISIONING', "updatedAt"=NOW() WHERE id=$1"#, &[B::T(solucion_id.into())]).await.map_err(|e| e.to_string())?;
    let r: R<Value> = async {
        red_asegurar(&red).await?;
        let _ = sh("docker", &["volume", "create", &volumen], None, 60).await;
        sh(
            "docker",
            &[
                "run", "-d", "--name", &contenedor, "--network", &red, "-e", &format!("POSTGRES_PASSWORD={clave}"), "-e", &format!("POSTGRES_DB={db}"), "-e", &format!("POSTGRES_USER={usuario}"), "-v",
                &format!("{volumen}:/var/lib/postgresql/data"), "--restart", "unless-stopped", "postgres:16-alpine",
            ],
            None,
            300,
        )
        .await?;
        esperar_postgres(&contenedor, usuario, 20).await?;
        guardar_variable(&nombre, "DATABASE_URL", &format!("postgresql://{usuario}:{clave}@{contenedor}:5432/{db}"))?;
        exec(
            &st.pool,
            r#"UPDATE "Solucion" SET "dbContainerName"=$1, "dbNetworkName"=$2, "dbVolumeName"=$3, "dbStatus"='READY', "dbProvisionedAt"=NOW(), "updatedAt"=NOW() WHERE id=$4"#,
            &[B::T(contenedor.clone()), B::T(red.clone()), B::T(volumen.clone()), B::T(solucion_id.into())],
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(json!({ "contenedor": contenedor, "red": red, "volumen": volumen }))
    }
    .await;
    if r.is_err() {
        let _ = exec(&st.pool, r#"UPDATE "Solucion" SET "dbStatus"='FAILED', "updatedAt"=NOW() WHERE id=$1"#, &[B::T(solucion_id.into())]).await;
    }
    r
}

/// Versión de Prisma que fija el proyecto (devDependencies/dependencies `prisma`, o `@prisma/client`), sin el prefijo ^ o ~.
pub(crate) fn version_prisma(repo: &Path) -> Option<String> {
    let pkg: Value = serde_json::from_str(&std::fs::read_to_string(repo.join("package.json")).ok()?).ok()?;
    let crudo = ["devDependencies", "dependencies"].iter().find_map(|k| pkg[k]["prisma"].as_str()).or_else(|| pkg["dependencies"]["@prisma/client"].as_str())?;
    let v = crudo.trim_start_matches(|c: char| !c.is_ascii_digit());
    let ok = v.split('.').count() == 3 && v.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    ok.then(|| v.to_string())
}

/// `prisma migrate deploy` dentro de un contenedor descartable en la red privada de la base. Manual a propósito.
pub async fn aplicar_migraciones(st: &AppState, solucion_id: &str) -> R<Value> {
    let sol = sol_campos(st, solucion_id).await?;
    if sol["repositorio"].as_str().map(|r| r.is_empty()).unwrap_or(true) {
        return Err("Esta Solución no tiene repositorio asociado.".into());
    }
    let Some(red) = sol["dbNetworkName"].as_str().filter(|x| !x.is_empty()).map(String::from) else {
        return Err("Este proyecto todavía no tiene una base de datos aprovisionada — agregala primero.".into());
    };
    let nombre = sol["nombre"].as_str().unwrap_or("").to_string();
    let repo = resolver_repo(st, Some(solucion_id)).await?;
    super::repo::git_con_credenciales(st, &["fetch", "origin", "main"], &repo).await?;
    sh("git", &["checkout", "main"], Some(&repo), 300).await?;
    sh("git", &["reset", "--hard", "origin/main"], Some(&repo), 300).await?;
    if !repo.join("prisma").join("schema.prisma").exists() {
        return Err("Este proyecto no tiene prisma/schema.prisma — no hay migraciones de Prisma que aplicar.".into());
    }
    let slug = slug_deploy(&nombre);
    let imagen = format!("demo-image-{slug}");
    let hay_imagen = sh("docker", &["image", "inspect", &imagen], None, 60).await.is_ok();
    if !hay_imagen {
        asegurar_dockerfile()?;
        asegurar_carpeta_prisma(&repo);
        sh("docker", &["build", "-f", &dockerfile_generico().to_string_lossy(), "-t", &imagen, &repo.to_string_lossy()], None, 600).await?;
    }
    // "--user root": prisma CLI necesita escribir binarios de motor en node_modules/@prisma/engines.
    let mut args: Vec<String> = ["run", "--rm", "--user", "root", "--network", &red].iter().map(|s| s.to_string()).collect();
    if let Some(env) = env_si_existe(&nombre) {
        args.extend(["--env-file".into(), env]);
    }
    // La imagen final no trae el CLI de Prisma: sin fijar la versión, npx baja la más nueva (7.x), que rechaza esquemas de Prisma 5.
    let paquete = version_prisma(&repo).map(|v| format!("prisma@{v}")).unwrap_or_else(|| "prisma".into());
    args.extend([imagen, "npx".into(), "--yes".into(), paquete, "migrate".into(), "deploy".into()]);
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let cola = |t: String| -> String {
        let c: Vec<char> = t.chars().collect();
        c[c.len().saturating_sub(4000)..].iter().collect()
    };
    match sh("docker", &a, None, 180).await {
        Ok(salida) => Ok(json!({ "ok": true, "salida": cola(salida) })),
        Err(e) => Ok(json!({ "ok": false, "salida": cola(e) })),
    }
}

#[cfg(test)]
mod pruebas_prisma {
    use super::*;

    fn con_paquete(json: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pv-{}", std::process::id() as u128 * 1000 + json.len() as u128));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("package.json"), json).unwrap();
        d
    }

    #[test]
    fn toma_la_version_que_fija_el_proyecto() {
        assert_eq!(version_prisma(&con_paquete(r#"{"devDependencies":{"prisma":"5.22.0"}}"#)).as_deref(), Some("5.22.0"));
        assert_eq!(version_prisma(&con_paquete(r#"{"devDependencies":{"prisma":"^5.10.2"},"dependencies":{"@prisma/client":"^5.10.2"}}"#)).as_deref(), Some("5.10.2"));
        assert_eq!(version_prisma(&con_paquete(r#"{"dependencies":{"@prisma/client":"~6.1.0"}}"#)).as_deref(), Some("6.1.0"));
        assert_eq!(version_prisma(&con_paquete(r#"{"dependencies":{"next":"14.2.15"}}"#)), None);
        assert_eq!(version_prisma(&con_paquete(r#"{"devDependencies":{"prisma":"latest"}}"#)), None);
    }
}
