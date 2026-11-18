//! Infraestructura de las demos (servicio privilegiado): estado del servidor y de los contenedores de cada demo
//! (app + base), y las acciones dormir, despertar, reiniciar y ver logs. Solo toca contenedores que pertenecen a
//! una Solución desplegada (`deployContainerName` y su base `demo-db-<slug>`): nunca el portal ni otros proyectos.

use std::{collections::HashMap, time::Duration};

use serde_json::{json, Value};

use super::repo::{sh, sh_salida, R};
use crate::{
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, B},
};

const LOG_NGINX: &str = "/var/log/nginx";

fn nombre_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 120 && n.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false) && n.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// «41.62MiB», «1.2GiB», «512KiB», «0B» → megabytes.
pub(crate) fn mb_de(t: &str) -> f64 {
    let t = t.trim();
    let i = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let n: f64 = t[..i].parse().unwrap_or(0.0);
    match t[i..].trim().to_ascii_lowercase().as_str() {
        "gib" | "gb" => n * 1024.0,
        "mib" | "mb" => n,
        "kib" | "kb" => n / 1024.0,
        _ => n / (1024.0 * 1024.0),
    }
}

fn kb_de(meminfo: &str, clave: &str) -> f64 {
    meminfo.lines().find(|l| l.starts_with(clave)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
}

fn dias_desde_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// «[05/Nov/2026:10:11:12 +0000]» → segundos desde 1970.
pub(crate) fn epoch_de_nginx(linea: &str) -> Option<i64> {
    let a = linea.find('[')?;
    let b = linea[a..].find(']')? + a;
    let t = &linea[a + 1..b];
    let (fecha, zona) = t.split_once(' ')?;
    let mut p = fecha.split(&['/', ':'][..]);
    let d: i64 = p.next()?.parse().ok()?;
    let mes = p.next()?;
    let m = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|x| *x == mes)? as i64 + 1;
    let y: i64 = p.next()?.parse().ok()?;
    let (h, mi, s): (i64, i64, i64) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    let signo = if zona.starts_with('-') { -1 } else { 1 };
    let z: i64 = zona.trim_start_matches(['+', '-']).parse().unwrap_or(0);
    let desfase = signo * ((z / 100) * 3600 + (z % 100) * 60);
    Some(dias_desde_civil(y, m, d) * 86400 + h * 3600 + mi * 60 + s - desfase)
}

/// «2026-11-07T15:44:00.123456789Z» → segundos desde 1970 (UTC).
pub(crate) fn epoch_de_docker(t: &str) -> Option<i64> {
    let t = t.trim();
    if t.len() < 19 || t.starts_with("0001") {
        return None;
    }
    let n = |a: usize, b: usize| t.get(a..b)?.parse::<i64>().ok();
    Some(dias_desde_civil(n(0, 4)?, n(5, 7)?, n(8, 10)?) * 86400 + n(11, 13)? * 3600 + n(14, 16)? * 60 + n(17, 19)?)
}

/// Desde cuándo existe el registro de visitas de la demo (el archivo de log se creó en ese momento).
fn registro_desde(slug: &str) -> Option<i64> {
    std::fs::metadata(format!("{LOG_NGINX}/demo-{slug}.access.log")).ok()?.created().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

/// Una demo se duerme sola si está activa, no está fijada y lleva más de N días sin visitas ni arranques.
pub(crate) fn debe_dormir(estado: &str, fijada: bool, actividad: i64, dias: i64, ahora: i64) -> bool {
    estado == "activa" && !fijada && actividad > 0 && ahora - actividad > dias * 86400
}

fn ahora_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Última visita REAL a la demo: se ignoran las peticiones del propio servidor (la comprobación de salud del catálogo).
pub(crate) fn ultimo_acceso_de(texto: &str) -> Option<i64> {
    texto.lines().rev().filter(|l| !["177.7.46.87 ", "127.0.0.1 ", "::1 "].iter().any(|ip| l.starts_with(ip))).find_map(epoch_de_nginx)
}

fn ultimo_acceso(slug: &str) -> Option<i64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(format!("{LOG_NGINX}/demo-{slug}.access.log")).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(96 * 1024))).ok()?;
    let mut b = Vec::new();
    f.read_to_end(&mut b).ok()?;
    ultimo_acceso_de(&String::from_utf8_lossy(&b))
}

async fn servidor() -> Value {
    let mi = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let (total, disp, st, sf) = (kb_de(&mi, "MemTotal:"), kb_de(&mi, "MemAvailable:"), kb_de(&mi, "SwapTotal:"), kb_de(&mi, "SwapFree:"));
    let (dt, du) = match sh("df", &["-B1", "--output=size,used", "/"], None, 10).await {
        Ok(s) => {
            let v: Vec<f64> = s.lines().nth(1).unwrap_or("").split_whitespace().filter_map(|x| x.parse().ok()).collect();
            (v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0))
        }
        Err(_) => (0.0, 0.0),
    };
    json!({
        "ramTotalMb": (total / 1024.0).round(), "ramUsadaMb": ((total - disp) / 1024.0).round(),
        "swapTotalMb": (st / 1024.0).round(), "swapUsadoMb": ((st - sf) / 1024.0).round(),
        "discoTotalGb": (dt / 1e9).round(), "discoUsadoGb": (du / 1e9).round(),
    })
}

/// Nombre → (estado de Docker, «Up 2 days»).
async fn estados() -> HashMap<String, (String, String)> {
    let mut m = HashMap::new();
    if let Ok(s) = sh("docker", &["ps", "-a", "--format", "{{.Names}}|{{.State}}|{{.Status}}"], None, 20).await {
        for l in s.lines() {
            let p: Vec<&str> = l.splitn(3, '|').collect();
            if p.len() == 3 {
                m.insert(p[0].to_string(), (p[1].to_string(), p[2].to_string()));
            }
        }
    }
    m
}

/// Nombre → (inicio, tope de memoria en MB; 0 = sin tope).
async fn inspeccion(nombres: &[String]) -> HashMap<String, (Option<i64>, i64)> {
    let mut m = HashMap::new();
    if nombres.is_empty() {
        return m;
    }
    let mut args: Vec<&str> = vec!["inspect", "-f", "{{.Name}}|{{.State.StartedAt}}|{{.HostConfig.Memory}}"];
    args.extend(nombres.iter().map(String::as_str));
    if let Ok((_, o, _)) = sh_salida("docker", &args, None, 20).await {
        for l in o.lines() {
            let p: Vec<&str> = l.splitn(3, '|').collect();
            if p.len() == 3 {
                m.insert(p[0].trim_start_matches('/').to_string(), (epoch_de_docker(p[1]), p[2].trim().parse::<i64>().unwrap_or(0) / (1024 * 1024)));
            }
        }
    }
    m
}

async fn memorias() -> HashMap<String, f64> {
    let mut m = HashMap::new();
    if let Ok(s) = sh("docker", &["stats", "--no-stream", "--format", "{{.Name}}|{{.MemUsage}}"], None, 40).await {
        for l in s.lines() {
            if let Some((n, u)) = l.split_once('|') {
                m.insert(n.to_string(), mb_de(u.split('/').next().unwrap_or("")).round());
            }
        }
    }
    m
}

fn slug_de_url(url: &str) -> String {
    url.trim_start_matches("https://").trim_start_matches("http://").split('.').next().unwrap_or("").to_string()
}

struct Demo {
    id: String,
    nombre: String,
    url: String,
    app: String,
    db: Option<String>,
    puerto: i64,
    dias: i64,
    mem: Option<i64>,
}

async fn demos(st: &AppState, solo: Option<&str>, est: &HashMap<String, (String, String)>) -> R<Vec<(Demo, bool)>> {
    let filas = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'nombre', nombre, 'url', "deployUrl", 'app', "deployContainerName", 'puerto', "deployPort", 'fijada', "infraFijada", 'dias', "infraDiasInactiva", 'mem', "infraMemoriaMb") ORDER BY nombre), '[]'::jsonb)
           FROM "Solucion" WHERE "deployContainerName" LIKE 'demo-%' AND "deployUrl" IS NOT NULL"#,
        &[],
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut out = vec![];
    for f in filas.as_array().cloned().unwrap_or_default() {
        let id = f["id"].as_str().unwrap_or("").to_string();
        if solo.map(|s| s != id).unwrap_or(false) {
            continue;
        }
        let (url, app) = (f["url"].as_str().unwrap_or("").to_string(), f["app"].as_str().unwrap_or("").to_string());
        if !nombre_ok(&app) || !est.contains_key(&app) {
            continue;
        }
        let slug = slug_de_url(&url);
        let db = format!("demo-db-{slug}");
        out.push((
            Demo { id, nombre: f["nombre"].as_str().unwrap_or("").to_string(), url, app, db: (nombre_ok(&db) && est.contains_key(&db)).then_some(db), puerto: f["puerto"].as_i64().unwrap_or(0), dias: f["dias"].as_i64().unwrap_or(7).max(1), mem: f["mem"].as_i64() },
            f["fijada"] == true,
        ));
    }
    Ok(out)
}

pub async fn estado(st: &AppState) -> R<Value> {
    let est = estados().await;
    let mem = memorias().await;
    let lista = demos(st, None, &est).await?;
    let nombres: Vec<String> = lista.iter().flat_map(|(d, _)| std::iter::once(d.app.clone()).chain(d.db.clone())).collect();
    let insp = inspeccion(&nombres).await;
    // Cuando el sistema de archivos no informa la fecha de creación del log, se usa la fecha en que se activó el registro de visitas.
    let log_desde = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('v', valor) FROM "InfraConfig" WHERE clave = 'logDesde'"#, &[]).await.ok().flatten().and_then(|v| v["v"].as_str().and_then(|x| x.parse::<i64>().ok()));
    let mut out = vec![];
    for (d, fijada) in lista {
        let mut conts = vec![json!({ "nombre": d.app, "rol": "app" })];
        if let Some(db) = &d.db {
            conts.push(json!({ "nombre": db, "rol": "base" }));
        }
        let mut corriendo = 0;
        for c in conts.iter_mut() {
            let n = c["nombre"].as_str().unwrap_or("").to_string();
            let (e, s) = est.get(&n).cloned().unwrap_or_default();
            if e == "running" {
                corriendo += 1;
            }
            c["estado"] = json!(e);
            c["detalle"] = json!(s);
            c["ramMb"] = json!(if e == "running" { mem.get(&n).copied().unwrap_or(0.0) } else { 0.0 });
        }
        let total = conts.len();
        let slug = slug_de_url(&d.url);
        let ultimo = ultimo_acceso(&slug);
        let registro = registro_desde(&slug).or(log_desde);
        let (inicio, tope) = insp.get(&d.app).copied().unwrap_or((None, 0));
        let inicio = if corriendo > 0 { inicio } else { None };
        let ram: f64 = conts.iter().map(|c| c["ramMb"].as_f64().unwrap_or(0.0)).sum();
        out.push(json!({
            "id": d.id, "nombre": d.nombre, "url": d.url, "slug": slug, "fijada": fijada,
            "estado": if corriendo == total { "activa" } else if corriendo == 0 { "dormida" } else { "parcial" },
            "contenedores": conts, "ramMb": ram, "ultimoAcceso": ultimo, "inicio": inicio, "registroDesde": registro,
            "actividad": ultimo.unwrap_or(0).max(inicio.unwrap_or(0)).max(registro.unwrap_or(0)),
            "diasInactiva": d.dias, "memoriaMb": tope, "memoriaConfigurada": d.mem,
        }));
    }
    Ok(json!({ "servidor": servidor().await, "demos": out, "ahora": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }))
}

/// Espera a que Nginx (no solo el puerto local) deje de dar 502/503/504 para esa demo.
async fn esperar_nginx(url: &str, intentos: u32) -> bool {
    let host = url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_string();
    if host.is_empty() {
        return true;
    }
    for _ in 0..intentos {
        let r = sh("curl", &["-sk", "-o", "/dev/null", "-m", "4", "-w", "%{http_code}", "--resolve", &format!("{host}:443:127.0.0.1"), &format!("https://{host}/")], None, 10).await;
        if let Ok(c) = r {
            let c = c.trim();
            if c != "000" && !["502", "503", "504"].contains(&c) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

async fn esperar_http(puerto: i64, intentos: u32) -> bool {
    for _ in 0..intentos {
        if sh("curl", &["-s", "-o", "/dev/null", "-m", "3", "-w", "%{http_code}", &format!("http://127.0.0.1:{puerto}/")], None, 8).await.map(|c| c.trim().starts_with(['2', '3', '4'])).unwrap_or(false) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    false
}

pub async fn accion(st: &AppState, id: &str, accion: &str, cuerpo: &Value) -> R<Value> {
    let est = estados().await;
    let lista = demos(st, Some(id), &est).await?;
    let Some((d, _)) = lista.into_iter().next() else { return Err("Esa Solución no tiene una demo con contenedores.".into()) };
    let ejecutar = |args: Vec<String>| async move {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        sh_salida("docker", &a, None, 90).await
    };
    match accion {
        "detener" => {
            let (ok, _, e) = ejecutar(vec!["stop".into(), "-t".into(), "20".into(), d.app.clone()]).await?;
            if !ok {
                return Err(format!("No se pudo detener la app: {}", e.trim().chars().take(200).collect::<String>()));
            }
            if let Some(db) = &d.db {
                let _ = ejecutar(vec!["stop".into(), "-t".into(), "20".into(), db.clone()]).await;
            }
            let _ = exec(&st.pool, r#"UPDATE "Solucion" SET "deployStatus"='STOPPED', "updatedAt"=NOW() WHERE id=$1"#, &[B::T(d.id.clone())]).await;
            Ok(json!({ "ok": true, "mensaje": format!("{} quedó dormida.", d.nombre) }))
        }
        "iniciar" => {
            if let Some(db) = &d.db {
                let _ = ejecutar(vec!["start".into(), db.clone()]).await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
            let (ok, _, e) = ejecutar(vec!["start".into(), d.app.clone()]).await?;
            if !ok {
                return Err(format!("No se pudo iniciar la app: {}", e.trim().chars().take(200).collect::<String>()));
            }
            let vivo = d.puerto > 0 && esperar_http(d.puerto, 15).await && esperar_nginx(&d.url, 15).await;
            if vivo {
                let _ = exec(&st.pool, r#"UPDATE "Solucion" SET "deployStatus"='LIVE', "updatedAt"=NOW() WHERE id=$1"#, &[B::T(d.id.clone())]).await;
            }
            Ok(json!({ "ok": true, "responde": vivo, "mensaje": if vivo { format!("{} está despierta.", d.nombre) } else { format!("{} arrancó pero aún no responde; revisa los logs.", d.nombre) } }))
        }
        "reiniciar" => {
            let (ok, _, e) = ejecutar(vec!["restart".into(), "-t".into(), "20".into(), d.app.clone()]).await?;
            if !ok {
                return Err(format!("No se pudo reiniciar: {}", e.trim().chars().take(200).collect::<String>()));
            }
            let vivo = d.puerto > 0 && esperar_http(d.puerto, 15).await && esperar_nginx(&d.url, 15).await;
            Ok(json!({ "ok": true, "responde": vivo, "mensaje": if vivo { format!("{} se reinició.", d.nombre) } else { format!("{} se reinició pero aún no responde.", d.nombre) } }))
        }
        "limite" => {
            let mb = cuerpo["mb"].as_i64().unwrap_or(0);
            if !(64..=4096).contains(&mb) {
                return Err("El tope debe estar entre 64 y 4096 MB.".into());
            }
            let m = format!("{mb}m");
            let (ok, _, e) = ejecutar(vec!["update".into(), "--memory".into(), m.clone(), "--memory-swap".into(), m, d.app.clone()]).await?;
            if !ok {
                return Err(format!("No se pudo aplicar el tope: {}", e.trim().chars().take(200).collect::<String>()));
            }
            Ok(json!({ "ok": true, "mensaje": format!("Tope de {mb} MB aplicado a {}.", d.nombre) }))
        }
        "logs" => {
            let pedido = cuerpo["contenedor"].as_str().unwrap_or(&d.app).to_string();
            if pedido != d.app && Some(&pedido) != d.db.as_ref() {
                return Err("Ese contenedor no pertenece a esta demo.".into());
            }
            let lineas = cuerpo["lineas"].as_u64().unwrap_or(80).clamp(10, 300).to_string();
            let (_, o, e) = ejecutar(vec!["logs".into(), "--tail".into(), lineas, pedido.clone()]).await?;
            let texto = format!("{o}{e}");
            let t: String = texto.chars().rev().take(20000).collect::<Vec<_>>().into_iter().rev().collect();
            Ok(json!({ "ok": true, "contenedor": pedido, "texto": t }))
        }
        _ => Err("Acción desconocida.".into()),
    }
}

/// Pone un tope de memoria a todos los contenedores de demos que no lo tienen (apps 512 MB, bases 256 MB).
pub async fn topes_todas(st: &AppState) -> R<Value> {
    let est = estados().await;
    let lista = demos(st, None, &est).await?;
    let nombres: Vec<String> = lista.iter().flat_map(|(d, _)| std::iter::once(d.app.clone()).chain(d.db.clone())).collect();
    let insp = inspeccion(&nombres).await;
    let (mut hechos, mut fallos) = (0, vec![]);
    for (d, _) in &lista {
        let mut objetivos = vec![(d.app.clone(), d.mem.unwrap_or(512))];
        if let Some(db) = &d.db {
            objetivos.push((db.clone(), 256));
        }
        for (n, mb) in objetivos {
            if insp.get(&n).map(|x| x.1).unwrap_or(0) > 0 {
                continue;
            }
            let m = format!("{mb}m");
            match sh_salida("docker", &["update", "--memory", &m, "--memory-swap", &m, &n], None, 30).await {
                Ok((true, _, _)) => hechos += 1,
                Ok((false, _, e)) => fallos.push(format!("{n}: {}", e.trim().chars().take(120).collect::<String>())),
                Err(e) => fallos.push(format!("{n}: {e}")),
            }
        }
    }
    Ok(json!({ "ok": fallos.is_empty(), "aplicados": hechos, "fallos": fallos }))
}

async fn registrar_auto(st: &AppState, sol: &str, accion: &str, ok: bool, detalle: &str) {
    let _ = exec(&st.pool, r#"INSERT INTO "InfraAccion" (id, "solucionId", "usuarioId", accion, ok, detalle) VALUES (gen_random_uuid()::text, $1, NULL, $2, $3, $4)"#, &[B::T(sol.into()), B::T(accion.into()), B::Bo(ok), B::T(detalle.chars().take(300).collect())]).await;
}

async fn modo_auto(st: &AppState) -> String {
    fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('v', valor) FROM "InfraConfig" WHERE clave = 'autoDormir'"#, &[]).await.ok().flatten().and_then(|v| v["v"].as_str().map(String::from)).unwrap_or_else(|| "simulacro".into())
}

/// Duerme (o, en simulacro, solo anota) las demos activas, no fijadas, sin visitas ni arranques en más de N días.
async fn auto_dormir(st: &AppState) {
    let modo = modo_auto(st).await;
    if modo == "apagado" {
        return;
    }
    let Ok(e) = estado(st).await else { return };
    let ahora = e["ahora"].as_i64().unwrap_or_else(ahora_s);
    for d in e["demos"].as_array().cloned().unwrap_or_default() {
        let (id, nombre) = (d["id"].as_str().unwrap_or(""), d["nombre"].as_str().unwrap_or(""));
        let (dias, act) = (d["diasInactiva"].as_i64().unwrap_or(7), d["actividad"].as_i64().unwrap_or(0));
        if id.is_empty() || !debe_dormir(d["estado"].as_str().unwrap_or(""), d["fijada"] == true, act, dias, ahora) {
            continue;
        }
        let sin = (ahora - act) / 86400;
        if modo == "activo" {
            match accion(st, id, "detener", &json!({})).await {
                Ok(_) => registrar_auto(st, id, "dormir-auto", true, &format!("Sin visitas hace {sin} días (regla: {dias})")).await,
                Err(er) => registrar_auto(st, id, "dormir-auto", false, &er).await,
            }
        } else {
            let ya = fetch_json(&st.pool, r#"SELECT to_jsonb(COUNT(*)::int) FROM "InfraAccion" WHERE "solucionId" = $1 AND accion = 'simulacro' AND "createdAt" > NOW() - INTERVAL '24 hours'"#, &[B::T(id.into())]).await.ok().and_then(|v| v.as_i64()).unwrap_or(0);
            if ya == 0 {
                registrar_auto(st, id, "simulacro", true, &format!("Habría dormido «{nombre}»: sin visitas hace {sin} días (regla: {dias})")).await;
            }
        }
    }
}

async fn avisar(st: &AppState, titulo: &str, mensaje: &str) {
    let ya = fetch_json(&st.pool, r#"SELECT to_jsonb(COUNT(*)::int) FROM "Notification" WHERE type = 'INFRA_ALERT' AND title = $1 AND "createdAt" > NOW() - INTERVAL '12 hours'"#, &[B::T(titulo.into())]).await.ok().and_then(|v| v.as_i64()).unwrap_or(0);
    if ya == 0 {
        let _ = exec(&st.pool, r#"INSERT INTO "Notification" (id, "userId", type, title, message, link, "createdAt") VALUES (gen_random_uuid()::text, 'system', 'INFRA_ALERT', $1, $2, '/catalogo-demos', NOW())"#, &[B::T(titulo.into()), B::T(mensaje.into())]).await;
    }
}

/// Avisa (una vez cada 12 h por nivel) cuando RAM, swap o disco pasan de 80 % y de 90 %.
async fn revisar_alertas(st: &AppState) {
    let s = servidor().await;
    let medidas = [
        ("Memoria del servidor", s["ramUsadaMb"].as_f64().unwrap_or(0.0), s["ramTotalMb"].as_f64().unwrap_or(0.0)),
        ("Swap del servidor", s["swapUsadoMb"].as_f64().unwrap_or(0.0), s["swapTotalMb"].as_f64().unwrap_or(0.0)),
        ("Disco del servidor", s["discoUsadoGb"].as_f64().unwrap_or(0.0), s["discoTotalGb"].as_f64().unwrap_or(0.0)),
    ];
    for (nombre, usado, total) in medidas {
        if total <= 0.0 {
            continue;
        }
        let p = (usado / total * 100.0).round() as i64;
        let nivel = if p >= 90 { 90 } else if p >= 80 { 80 } else { continue };
        avisar(st, &format!("{nombre} sobre el {nivel} %"), &format!("Está al {p} %. Revisa Catálogo de demos › Infraestructura y duerme las demos que no se usen.")).await;
    }
}

/// Guarda una muestra de memoria (servidor y por demo) para el histórico; conserva 30 días.
async fn registrar_muestra(st: &AppState) {
    let Ok(e) = estado(st).await else { return };
    let mut por_demo = serde_json::Map::new();
    for d in e["demos"].as_array().cloned().unwrap_or_default() {
        if let Some(s) = d["slug"].as_str() {
            por_demo.insert(s.to_string(), json!(d["ramMb"].as_f64().unwrap_or(0.0).round()));
        }
    }
    let s = &e["servidor"];
    let _ = exec(&st.pool, r#"INSERT INTO "InfraMuestra" (ts, "ramMb", "swapMb", demos) VALUES ($1, $2, $3, $4)"#,
        &[B::I(ahora_s()), B::F(s["ramUsadaMb"].as_f64().unwrap_or(0.0)), B::F(s["swapUsadoMb"].as_f64().unwrap_or(0.0)), B::J(Value::Object(por_demo))]).await;
    let _ = exec(&st.pool, r#"DELETE FROM "InfraMuestra" WHERE ts < $1"#, &[B::I(ahora_s() - 30 * 86400)]).await;
}

/// Tareas automáticas del motor: avisos de capacidad (cada 10 min) y dormir demos frías (cada 30 min).
pub fn iniciar_automatico(st: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(150)).await;
        let mut vuelta: u32 = 0;
        loop {
            revisar_alertas(&st).await;
            registrar_muestra(&st).await;
            if vuelta % 3 == 0 {
                auto_dormir(&st).await;
            }
            vuelta = vuelta.wrapping_add(1);
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
    });
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn convierte_memoria_de_docker() {
        assert_eq!(mb_de("41.62MiB").round(), 42.0);
        assert_eq!(mb_de("1.5GiB"), 1536.0);
        assert_eq!(mb_de("2048KiB"), 2.0);
        assert_eq!(mb_de("0B"), 0.0);
    }

    #[test]
    fn lee_la_fecha_del_log() {
        assert_eq!(epoch_de_nginx(r#"1.2.3.4 - - [05/Nov/2026:10:11:12 +0000] "GET / HTTP/1.1" 200 5"#), Some(1793873472));
        assert_eq!(epoch_de_nginx(r#"1.2.3.4 - - [05/Nov/2026:05:11:12 -0500] "GET / HTTP/1.1" 200 5"#), Some(1793873472));
    }

    #[test]
    fn ignora_la_comprobacion_de_salud_del_servidor() {
        let log = "9.9.9.9 - - [05/Nov/2026:08:00:00 +0000] \"GET / HTTP/1.1\" 200 5\n177.7.46.87 - - [05/Nov/2026:12:00:00 +0000] \"GET / HTTP/1.1\" 200 5\n";
        assert_eq!(ultimo_acceso_de(log), epoch_de_nginx("[05/Nov/2026:08:00:00 +0000]"));
        assert_eq!(ultimo_acceso_de("177.7.46.87 - - [05/Nov/2026:12:00:00 +0000] x"), None);
    }

    #[test]
    fn decide_cuando_dormir_una_demo() {
        let dia = 86400;
        assert!(debe_dormir("activa", false, 100, 7, 100 + 8 * dia));
        assert!(!debe_dormir("activa", false, 100, 7, 100 + 7 * dia));
        assert!(!debe_dormir("activa", true, 100, 7, 100 + 30 * dia));
        assert!(!debe_dormir("dormida", false, 100, 7, 100 + 30 * dia));
        assert!(!debe_dormir("activa", false, 0, 7, 100 + 30 * dia));
        assert!(debe_dormir("activa", false, 100, 1, 100 + 2 * dia));
    }

    #[test]
    fn lee_la_fecha_de_docker() {
        assert_eq!(epoch_de_docker("2026-11-05T10:11:12.123456789Z"), Some(1793873472));
        assert_eq!(epoch_de_docker("0001-01-01T00:00:00Z"), None);
        assert_eq!(epoch_de_docker(""), None);
    }

    #[test]
    fn valida_nombres_de_contenedor() {
        assert!(nombre_ok("demo-db-radar-finops-ia"));
        assert!(!nombre_ok("demo; rm -rf /"));
        assert!(!nombre_ok("-x"));
    }
}
