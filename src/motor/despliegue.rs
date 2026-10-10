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
        let nuevo = format!("demo-{slug}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
        let puerto = puerto_libre(st).await?;
        sh("docker", &["build", "-f", &dockerfile_generico().to_string_lossy(), "-t", &imagen, &repo.to_string_lossy()], None, 600).await?;
        // El puerto se publica SOLO en localhost: Nginx es quien atiende a internet (con HTTPS). Antes quedaba en 0.0.0.0 y la app respondía por HTTP plano.
        let mut args: Vec<String> = ["run", "-d", "--name", &nuevo, "-p", &format!("127.0.0.1:{puerto}:3000"), "--restart", "unless-stopped"].iter().map(|s| s.to_string()).collect();
        if let Some(red) = sol["dbNetworkName"].as_str().filter(|x| !x.is_empty()) {
            args.extend(["--network".into(), red.to_string()]);
        }
        if let Some(env) = env_si_existe(&nombre) {
            args.extend(["--env-file".into(), env]);
        }
        args.push(imagen.clone());
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
