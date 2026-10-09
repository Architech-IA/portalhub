//! Autenticación: protocolo de NextAuth v4 (`/api/auth/**`) con las mismas cookies y el mismo JWE, de modo
//! que las sesiones ya emitidas siguen valiendo y el frontend (`next-auth/react`) no cambia. Proveedores:
//! credenciales (correo + contraseña con bcrypt) y Google OAuth (con PKCE y `state`). Además, la conexión
//! de la cuenta de Microsoft de cada persona (`/api/auth/microsoft/**`).
//!
//! Equivale a `src/lib/auth.ts` + `src/app/api/auth/**` de Next.

use std::collections::HashMap;

use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    session::{cifrar_token, descifrar_token, token_de_cabecera},
    state::AppState,
    util::{exec, fetch_json_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/session", get(sesion))
        .route("/api/auth/csrf", get(csrf))
        .route("/api/auth/providers", get(proveedores))
        .route("/api/auth/signin", get(pagina_ingreso))
        .route("/api/auth/signin/{proveedor}", post(ingresar).get(pagina_ingreso_proveedor))
        .route("/api/auth/callback/{proveedor}", get(callback_get).post(callback_post))
        .route("/api/auth/signout", post(salir).get(pagina_salida))
        .route("/api/auth/error", get(pagina_error))
        .route("/api/auth/_log", post(registro_cliente))
        .route("/api/auth/microsoft", get(microsoft_inicio))
        .route("/api/auth/microsoft/callback", get(microsoft_callback))
        .route("/api/auth/microsoft/disconnect", post(microsoft_desconectar))
}

const MAX_EDAD: i64 = 30 * 24 * 60 * 60;
const TROZO_COOKIE: usize = 3933;

// ── Configuración y cookies ──────────────────────────────────────────────────────────────────
fn url_base() -> String {
    std::env::var("NEXTAUTH_URL").unwrap_or_else(|_| "http://localhost:3000".to_string()).trim_end_matches('/').to_string()
}

fn seguras() -> bool {
    url_base().starts_with("https://")
}

fn pref() -> &'static str {
    if seguras() {
        "__Secure-"
    } else {
        ""
    }
}

fn nombre_sesion() -> String {
    format!("{}next-auth.session-token", pref())
}
fn nombre_callback() -> String {
    format!("{}next-auth.callback-url", pref())
}
fn nombre_csrf() -> String {
    format!("{}next-auth.csrf-token", if seguras() { "__Host-" } else { "" })
}
fn nombre_state() -> String {
    format!("{}next-auth.state", pref())
}
fn nombre_pkce() -> String {
    format!("{}next-auth.pkce.code_verifier", pref())
}

/// `encodeURIComponent`.
fn codificar(t: &str) -> String {
    let mut s = String::new();
    for b in t.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => s.push(b as char),
            otro => s.push_str(&format!("%{otro:02X}")),
        }
    }
    s
}

fn decodificar(t: &str) -> String {
    let b = t.as_bytes();
    let mut out: Vec<u8> = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 && t.is_char_boundary(i + 1) && t.is_char_boundary((i + 3).min(t.len())) && i + 3 <= t.len() {
            if let Ok(v) = u8::from_str_radix(&t[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn leer_cookie(cab: &HeaderMap, nombre: &str) -> Option<String> {
    let h = cab.get(header::COOKIE)?.to_str().ok()?;
    h.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == nombre).map(|(_, v)| decodificar(v))
}

/// Fecha HTTP (RFC 1123) de un instante en segundos Unix.
fn fecha_http(s: i64) -> String {
    const DIAS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MESES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let dias = s.div_euclid(86400);
    let (y, m, d) = crate::routes::misc::civil_desde_dias(dias);
    let r = s.rem_euclid(86400);
    format!("{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT", DIAS[dias.rem_euclid(7) as usize], MESES[(m - 1) as usize], r / 3600, (r % 3600) / 60, r % 60)
}

/// Como `cookie.serialize` de NextAuth: `Expires` (no `Max-Age`) cuando la cookie vence.
fn set_cookie(nombre: &str, valor: &str, edad: Option<i64>) -> String {
    let mut c = format!("{nombre}={}; Path=/; HttpOnly; SameSite=Lax", codificar(valor));
    if let Some(e) = edad {
        c.push_str(&format!("; Expires={}", fecha_http(ahora() + e)));
    }
    if seguras() {
        c.push_str("; Secure");
    }
    c
}

fn borrar_cookie(nombre: &str) -> String {
    let mut c = format!("{nombre}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT");
    if seguras() {
        c.push_str("; Secure");
    }
    c
}

/// Cookies de sesión (partidas en trozos si el JWE es muy grande, como hace NextAuth).
fn cookies_sesion(jwe: &str, cab: &HeaderMap) -> Vec<String> {
    let nombre = nombre_sesion();
    let mut out: Vec<String> = vec![];
    if jwe.len() <= TROZO_COOKIE {
        out.push(set_cookie(&nombre, jwe, Some(MAX_EDAD)));
    } else {
        for (i, trozo) in jwe.as_bytes().chunks(TROZO_COOKIE).enumerate() {
            out.push(set_cookie(&format!("{nombre}.{i}"), &String::from_utf8_lossy(trozo), Some(MAX_EDAD)));
        }
    }
    // Se borran trozos sobrantes de una sesión anterior más grande.
    let usados = if jwe.len() <= TROZO_COOKIE { 0 } else { jwe.len().div_ceil(TROZO_COOKIE) };
    for i in usados..20 {
        if leer_cookie(cab, &format!("{nombre}.{i}")).is_some() {
            out.push(borrar_cookie(&format!("{nombre}.{i}")));
        }
    }
    if usados > 0 && leer_cookie(cab, &nombre).is_some() {
        out.push(borrar_cookie(&nombre));
    }
    out
}

fn borrar_sesion(cab: &HeaderMap) -> Vec<String> {
    let nombre = nombre_sesion();
    let mut out = vec![borrar_cookie(&nombre)];
    for i in 0..20 {
        if leer_cookie(cab, &format!("{nombre}.{i}")).is_some() {
            out.push(borrar_cookie(&format!("{nombre}.{i}")));
        }
    }
    out
}

fn con_cookies(mut r: Response, cookies: &[String]) -> Response {
    for c in cookies {
        if let Ok(v) = header::HeaderValue::from_str(c) {
            r.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    r
}

fn json_con(estado: StatusCode, cookies: &[String], v: Value) -> Response {
    let mut r = (estado, Json(v)).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store, max-age=0"));
    con_cookies(r, cookies)
}

fn redirigir(url: &str, cookies: &[String]) -> Response {
    let r = Response::builder().status(StatusCode::FOUND).header(header::LOCATION, url).body(axum::body::Body::empty()).unwrap_or_default();
    con_cookies(r, cookies)
}

/// Si el cliente pidió JSON (`json=true`), la redirección se devuelve como `{url}`; si no, como 302.
fn salida(json_modo: bool, url: &str, cookies: &[String]) -> Response {
    if json_modo {
        json_con(StatusCode::OK, cookies, json!({ "url": url }))
    } else {
        redirigir(url, cookies)
    }
}

// ── CSRF ─────────────────────────────────────────────────────────────────────────────────────
fn hash_csrf(token: &str, secreto: &str) -> String {
    Sha256::digest(format!("{token}{secreto}").as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn aleatorio_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn aleatorio_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// `(token, cookie_nueva?, verificado)` — token de la cookie si es válida; `verificado` si el del cuerpo coincide.
fn csrf_estado(cab: &HeaderMap, secreto: &str, enviado: Option<&str>) -> (String, Option<String>, bool) {
    if let Some(c) = leer_cookie(cab, &nombre_csrf()) {
        if let Some((token, hash)) = c.split_once('|') {
            if hash_csrf(token, secreto) == hash {
                return (token.to_string(), None, enviado == Some(token));
            }
        }
    }
    let token = aleatorio_hex(32);
    let valor = format!("{token}|{}", hash_csrf(&token, secreto));
    (token, Some(set_cookie(&nombre_csrf(), &valor, None)), false)
}

async fn csrf(State(st): State<AppState>, cab: HeaderMap) -> Response {
    let (token, nueva, _) = csrf_estado(&cab, &st.cfg.nextauth_secret, None);
    let mut cookies: Vec<String> = nueva.into_iter().collect();
    if leer_cookie(&cab, &nombre_callback()).is_none() {
        cookies.push(set_cookie(&nombre_callback(), &url_base(), None));
    }
    json_con(StatusCode::OK, &cookies, json!({ "csrfToken": token }))
}

async fn proveedores() -> Response {
    let b = url_base();
    json_con(
        StatusCode::OK,
        &[],
        json!({
            "google": { "id": "google", "name": "Google", "type": "oauth", "signinUrl": format!("{b}/api/auth/signin/google"), "callbackUrl": format!("{b}/api/auth/callback/google") },
            "credentials": { "id": "credentials", "name": "credentials", "type": "credentials", "signinUrl": format!("{b}/api/auth/signin/credentials"), "callbackUrl": format!("{b}/api/auth/callback/credentials") },
        }),
    )
}

// ── Sesión ───────────────────────────────────────────────────────────────────────────────────
fn ahora() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn iso(segundos: i64) -> String {
    let (y, m, d) = crate::routes::misc::civil_desde_dias(segundos.div_euclid(86400));
    let r = segundos.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", r / 3600, (r % 3600) / 60, r % 60)
}

fn claims_de(cab: &HeaderMap, secreto: &str) -> Option<Value> {
    let h = cab.get(header::COOKIE)?.to_str().ok()?;
    let token = token_de_cabecera(h)?;
    descifrar_token(&token, secreto)
}

async fn sesion(State(st): State<AppState>, cab: HeaderMap) -> Response {
    let Some(claims) = claims_de(&cab, &st.cfg.nextauth_secret) else {
        // Una cookie ilegible o vencida se borra para que el navegador no la siga mandando.
        let borrar = if leer_cookie(&cab, &nombre_sesion()).is_some() { borrar_sesion(&cab) } else { vec![] };
        return json_con(StatusCode::OK, &borrar, json!({}));
    };
    // La sesión se renueva con cada consulta (NextAuth reescribe la cookie con una nueva expiración).
    let mut nuevo = claims.clone();
    if let Some(o) = nuevo.as_object_mut() {
        for k in ["iat", "exp", "jti"] {
            o.remove(k);
        }
    }
    let cookies = cifrar_token(&nuevo, &st.cfg.nextauth_secret, MAX_EDAD).map(|j| cookies_sesion(&j, &cab)).unwrap_or_default();
    let mut user = serde_json::Map::new();
    for (k, de) in [("name", "name"), ("email", "email"), ("image", "picture")] {
        if let Some(v) = claims.get(de).filter(|v| !v.is_null()) {
            user.insert(k.into(), v.clone());
        }
    }
    user.insert("id".into(), claims.get("id").cloned().unwrap_or(Value::Null));
    user.insert("role".into(), claims.get("role").cloned().unwrap_or(Value::Null));
    user.insert("avatar".into(), claims.get("avatar").cloned().unwrap_or(Value::Null));
    user.insert("googleConnected".into(), json!(claims["googleConnected"].as_bool().unwrap_or(false)));
    user.insert("microsoftConnected".into(), json!(claims["microsoftConnected"].as_bool().unwrap_or(false)));
    json_con(StatusCode::OK, &cookies, json!({ "user": Value::Object(user), "expires": iso(ahora() + MAX_EDAD) }))
}

// ── Utilidades de inicio de sesión ───────────────────────────────────────────────────────────
async fn registrar(st: &AppState, cab: &HeaderMap, usuario: Option<&str>, correo: Option<&str>, accion: &str, ok: bool, detalle: Option<&str>) {
    let ip = cab
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| cab.get("x-real-ip").and_then(|v| v.to_str().ok()).map(String::from))
        .unwrap_or_else(|| "unknown".into());
    let ua = cab.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("unknown").to_string();
    let r = exec(
        &st.pool,
        r#"INSERT INTO "SessionLog" (id, "userId", email, action, ip, "userAgent", success, details, "createdAt") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())"#,
        &[B::T(new_id()), B::OT(usuario.map(String::from)), B::OT(correo.map(String::from)), B::T(accion.into()), B::T(ip), B::T(ua), B::Bo(ok), B::OT(detalle.map(String::from))],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("SessionLog error: {e}");
    }
}

/// `redirect` por defecto de NextAuth: rutas relativas y mismo origen; cualquier otra cosa va a la base.
fn url_segura(url: &str) -> String {
    let base = url_base();
    if url.starts_with('/') && !url.starts_with("//") {
        return format!("{base}{url}");
    }
    if url == base || url.starts_with(&format!("{base}/")) || url.starts_with(&format!("{base}?")) {
        return url.to_string();
    }
    base
}

fn destino(cab: &HeaderMap, enviado: Option<&str>) -> String {
    let crudo = enviado.filter(|u| !u.is_empty()).map(String::from).or_else(|| leer_cookie(cab, &nombre_callback())).unwrap_or_else(url_base);
    url_segura(&crudo)
}

fn es_json(cuerpo: &HashMap<String, String>) -> bool {
    cuerpo.get("json").map(|j| j == "true").unwrap_or(false)
}

// ── Ingreso con credenciales ─────────────────────────────────────────────────────────────────
async fn autorizar(st: &AppState, cab: &HeaderMap, correo: &str, clave: &str) -> Option<Value> {
    let u = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', id, 'name', name, 'email', email, 'role', role, 'avatar', avatar, 'password', password,
             'googleConnected', "googleAccessToken" IS NOT NULL AND "googleAccessToken" <> '', 'microsoftConnected', "microsoftAccessToken" IS NOT NULL AND "microsoftAccessToken" <> '')
           FROM "User" WHERE email = $1"#,
        &[B::T(correo.into())],
    )
    .await
    .ok()
    .flatten();
    let Some(u) = u else {
        registrar(st, cab, None, Some(correo), "FAILED_LOGIN", false, Some("Usuario no encontrado")).await;
        return None;
    };
    let hash = u["password"].as_str().unwrap_or("").to_string();
    let clave = clave.to_string();
    let ok = tokio::task::spawn_blocking(move || bcrypt::verify(clave, &hash).unwrap_or(false)).await.unwrap_or(false);
    if !ok {
        registrar(st, cab, u["id"].as_str(), u["email"].as_str(), "FAILED_LOGIN", false, Some("Contraseña incorrecta")).await;
        return None;
    }
    Some(u)
}

async fn callback_post(State(st): State<AppState>, Path(proveedor): Path<String>, cab: HeaderMap, Form(cuerpo): Form<HashMap<String, String>>) -> Response {
    if proveedor != "credentials" {
        return redirigir(&format!("{}/login?error=Callback", url_base()), &[]);
    }
    let json_modo = es_json(&cuerpo);
    let (_, nueva, verificado) = csrf_estado(&cab, &st.cfg.nextauth_secret, cuerpo.get("csrfToken").map(String::as_str));
    let mut cookies: Vec<String> = nueva.into_iter().collect();
    if !verificado {
        return salida(json_modo, &format!("{}/api/auth/signin?csrf=true", url_base()), &cookies);
    }
    let (correo, clave) = (cuerpo.get("email").cloned().unwrap_or_default(), cuerpo.get("password").cloned().unwrap_or_default());
    if correo.is_empty() || clave.is_empty() {
        return error_credenciales(json_modo, &cookies);
    }
    let Some(u) = autorizar(&st, &cab, &correo, &clave).await else { return error_credenciales(json_modo, &cookies) };
    registrar(&st, &cab, u["id"].as_str(), Some(&correo), "LOGIN", true, None).await;
    let token = json!({
        "name": u["name"], "email": u["email"], "sub": u["id"], "id": u["id"], "role": u["role"], "avatar": u["avatar"],
        "googleConnected": u["googleConnected"], "microsoftConnected": u["microsoftConnected"],
    });
    let Some(jwe) = cifrar_token(&token, &st.cfg.nextauth_secret, MAX_EDAD) else {
        return redirigir(&format!("{}/login?error=Callback", url_base()), &cookies);
    };
    let destino = destino(&cab, cuerpo.get("callbackUrl").map(String::as_str));
    cookies.push(set_cookie(&nombre_callback(), &destino, None));
    cookies.extend(cookies_sesion(&jwe, &cab));
    salida(json_modo, &destino, &cookies)
}

fn error_credenciales(json_modo: bool, cookies: &[String]) -> Response {
    let url = format!("{}/api/auth/error?error=CredentialsSignin&provider=credentials", url_base());
    if json_modo {
        json_con(StatusCode::UNAUTHORIZED, cookies, json!({ "url": url }))
    } else {
        redirigir(&url, cookies)
    }
}

// ── Google OAuth ─────────────────────────────────────────────────────────────────────────────
const SCOPE_GOOGLE: &str = "openid email profile https://www.googleapis.com/auth/calendar.events";

async fn ingresar(State(st): State<AppState>, Path(proveedor): Path<String>, cab: HeaderMap, Form(cuerpo): Form<HashMap<String, String>>) -> Response {
    let json_modo = es_json(&cuerpo);
    let (_, nueva, verificado) = csrf_estado(&cab, &st.cfg.nextauth_secret, cuerpo.get("csrfToken").map(String::as_str));
    let mut cookies: Vec<String> = nueva.into_iter().collect();
    if !verificado {
        return salida(json_modo, &format!("{}/api/auth/signin?csrf=true", url_base()), &cookies);
    }
    if proveedor != "google" {
        return redirigir(&format!("{}/login", url_base()), &cookies);
    }
    let (Some(cliente), Some(_)) = (st.cfg.google_client_id.clone(), st.cfg.google_client_secret.clone()) else {
        return redirigir(&format!("{}/login?error=Configuration", url_base()), &cookies);
    };
    let callback = destino(&cab, cuerpo.get("callbackUrl").map(String::as_str));
    let estado = aleatorio_b64(32);
    let verificador = aleatorio_b64(32);
    let desafio = URL_SAFE_NO_PAD.encode(Sha256::digest(verificador.as_bytes()));
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&scope={}&response_type=code&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        codificar(&cliente),
        codificar(SCOPE_GOOGLE),
        codificar(&format!("{}/api/auth/callback/google", url_base())),
        codificar(&estado),
        codificar(&desafio)
    );
    cookies.push(set_cookie(&nombre_callback(), &callback, Some(900)));
    if let Some(j) = cifrar_token(&json!({ "value": estado }), &st.cfg.nextauth_secret, 900) {
        cookies.push(set_cookie(&nombre_state(), &j, Some(900)));
    }
    if let Some(j) = cifrar_token(&json!({ "value": verificador }), &st.cfg.nextauth_secret, 900) {
        cookies.push(set_cookie(&nombre_pkce(), &j, Some(900)));
    }
    salida(json_modo, &url, &cookies)
}

async fn callback_get(State(st): State<AppState>, Path(proveedor): Path<String>, cab: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let limpiar = vec![borrar_cookie(&nombre_state()), borrar_cookie(&nombre_pkce())];
    let falla = |codigo: &str| redirigir(&format!("{}/login?error={codigo}", url_base()), &limpiar);
    if proveedor != "google" {
        return falla("Callback");
    }
    if q.contains_key("error") {
        return falla("OAuthCallback");
    }
    let (Some(codigo), Some(estado_q)) = (q.get("code"), q.get("state")) else { return falla("OAuthCallback") };
    let secreto = &st.cfg.nextauth_secret;
    let leer_valor = |nombre: String| leer_cookie(&cab, &nombre).and_then(|c| descifrar_token(&c, secreto)).and_then(|v| v["value"].as_str().map(String::from));
    let (Some(estado_c), Some(verificador)) = (leer_valor(nombre_state()), leer_valor(nombre_pkce())) else { return falla("OAuthCallback") };
    if &estado_c != estado_q {
        return falla("OAuthCallback");
    }
    let (Some(cliente), Some(clave)) = (st.cfg.google_client_id.clone(), st.cfg.google_client_secret.clone()) else { return falla("Configuration") };
    let tokens = st
        .http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", codigo.as_str()),
            ("redirect_uri", &format!("{}/api/auth/callback/google", url_base())),
            ("client_id", &cliente),
            ("client_secret", &clave),
            ("code_verifier", &verificador),
        ])
        .send()
        .await;
    let tokens: Value = match tokens {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
        Ok(r) => {
            tracing::error!("Google rechazó el intercambio de código: {}", r.status());
            return falla("OAuthCallback");
        }
        Err(e) => {
            tracing::error!("Google token: {e}");
            return falla("OAuthCallback");
        }
    };
    let Some(acceso) = tokens["access_token"].as_str().map(String::from) else { return falla("OAuthCallback") };
    // El id_token llega directo del endpoint de Google por TLS: se lee su contenido sin verificar la firma.
    let perfil: Value = tokens["id_token"]
        .as_str()
        .and_then(|t| t.split('.').nth(1))
        .and_then(|p| URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let (Some(sub), Some(correo)) = (perfil["sub"].as_str().map(String::from), perfil["email"].as_str().map(String::from)) else { return falla("OAuthCallback") };
    let nombre = perfil["name"].as_str().map(String::from);
    let imagen = perfil["picture"].as_str().map(String::from);
    let refresco = tokens["refresh_token"].as_str().map(String::from);
    let expira = tokens["expires_in"].as_i64().map(|e| ahora() + e);

    // Callback `signIn`: vincula los tokens de Google al usuario (o lo crea como PARTNER).
    let existe = crate::util::fetch_text_opt(&st.pool, r#"SELECT id FROM "User" WHERE email = $1"#, &[B::T(correo.clone())]).await.ok().flatten();
    let r: Result<(), String> = async {
        if existe.is_some() {
            exec(
                &st.pool,
                r#"UPDATE "User" SET "googleAccessToken" = $2, "googleRefreshToken" = $3, "googleTokenExpiry" = CASE WHEN $4::float8 IS NULL THEN NULL ELSE to_timestamp($4::float8) AT TIME ZONE 'UTC' END, "updatedAt" = NOW() WHERE email = $1"#,
                &[B::T(correo.clone()), B::T(acceso.clone()), B::OT(refresco.clone()), B::OF(expira.map(|e| e as f64))],
            )
            .await
            .map_err(|e| e.to_string())?;
        } else {
            let clave_azar = format!("{}{}", aleatorio_b64(8), ahora());
            let hash = tokio::task::spawn_blocking(move || bcrypt::hash(clave_azar, 12)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
            exec(
                &st.pool,
                r#"INSERT INTO "User" (id, name, email, password, role, avatar, "googleAccessToken", "googleRefreshToken", "googleTokenExpiry", "createdAt", "updatedAt")
                   VALUES ($1, $2, $3, $4, 'PARTNER', $5, $6, $7, CASE WHEN $8::float8 IS NULL THEN NULL ELSE to_timestamp($8::float8) AT TIME ZONE 'UTC' END, NOW(), NOW())"#,
                &[
                    B::T(new_id()),
                    B::T(nombre.clone().unwrap_or_else(|| correo.split('@').next().unwrap_or("").to_string())),
                    B::T(correo.clone()),
                    B::T(hash),
                    B::OT(imagen.clone()),
                    B::T(acceso.clone()),
                    B::OT(refresco.clone()),
                    B::OF(expira.map(|e| e as f64)),
                ],
            )
            .await
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::error!("Google signIn: {e}");
        return falla("OAuthCallback");
    }
    registrar(&st, &cab, Some(&sub), Some(&correo), "LOGIN", true, None).await;

    // Callback `jwt`: Google no trae role/avatar, se buscan en la base por correo.
    let db = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object('id', id, 'role', role, 'avatar', avatar, 'microsoftConnected', "microsoftAccessToken" IS NOT NULL AND "microsoftAccessToken" <> '') FROM "User" WHERE email = $1"#,
        &[B::T(correo.clone())],
    )
    .await
    .ok()
    .flatten()
    .unwrap_or(Value::Null);
    let mut token = serde_json::Map::new();
    token.insert("name".into(), nombre.clone().map(Value::from).unwrap_or(Value::Null));
    token.insert("email".into(), json!(correo));
    if let Some(i) = &imagen {
        token.insert("picture".into(), json!(i));
    }
    token.insert("sub".into(), json!(sub));
    token.insert("id".into(), if db["id"].is_string() { db["id"].clone() } else { json!(sub) });
    if db["role"].is_string() {
        token.insert("role".into(), db["role"].clone());
    }
    if !db["avatar"].is_null() {
        token.insert("avatar".into(), db["avatar"].clone());
    }
    token.insert("googleConnected".into(), json!(true));
    token.insert("microsoftConnected".into(), json!(db["microsoftConnected"].as_bool().unwrap_or(false)));
    token.insert("googleAccessToken".into(), json!(acceso));
    if let Some(r) = refresco {
        token.insert("googleRefreshToken".into(), json!(r));
    }
    if let Some(e) = expira {
        token.insert("googleTokenExpiry".into(), json!(e));
    }
    let Some(jwe) = cifrar_token(&Value::Object(token), secreto, MAX_EDAD) else { return falla("Callback") };
    let mut cookies = limpiar;
    cookies.extend(cookies_sesion(&jwe, &cab));
    let destino = destino(&cab, None);
    cookies.push(borrar_cookie(&nombre_callback()));
    redirigir(&destino, &cookies)
}

// ── Salida ───────────────────────────────────────────────────────────────────────────────────
async fn salir(State(st): State<AppState>, cab: HeaderMap, Form(cuerpo): Form<HashMap<String, String>>) -> Response {
    let json_modo = es_json(&cuerpo);
    let (_, nueva, verificado) = csrf_estado(&cab, &st.cfg.nextauth_secret, cuerpo.get("csrfToken").map(String::as_str));
    let mut cookies: Vec<String> = nueva.into_iter().collect();
    if !verificado {
        return salida(json_modo, &format!("{}/api/auth/signin?csrf=true", url_base()), &cookies);
    }
    if let Some(claims) = claims_de(&cab, &st.cfg.nextauth_secret) {
        let id = claims["id"].as_str().map(String::from);
        let correo = claims["email"].as_str().map(String::from);
        let _ = exec(
            &st.pool,
            r#"INSERT INTO "SessionLog" (id, "userId", email, action, ip, "userAgent", success, "createdAt") VALUES ($1, $2, $3, 'LOGOUT', 'unknown', 'unknown', true, NOW())"#,
            &[B::T(new_id()), B::OT(id), B::OT(correo)],
        )
        .await;
    }
    cookies.extend(borrar_sesion(&cab));
    let destino = destino(&cab, cuerpo.get("callbackUrl").map(String::as_str));
    salida(json_modo, &destino, &cookies)
}

async fn pagina_salida() -> Response {
    redirigir(&format!("{}/login", url_base()), &[])
}

async fn pagina_ingreso(Query(q): Query<HashMap<String, String>>) -> Response {
    // pages.signIn = '/login': NextAuth redirige con la callbackUrl ya absoluta y el error si lo hubo.
    let mut partes: Vec<String> = vec![];
    if let Some(v) = q.get("callbackUrl").filter(|v| !v.is_empty()) {
        partes.push(format!("callbackUrl={}", codificar(&url_segura(v))));
    }
    if let Some(v) = q.get("error").filter(|v| !v.is_empty()) {
        partes.push(format!("error={}", codificar(v)));
    } else if q.get("csrf").map(|c| c == "true").unwrap_or(false) {
        partes.push("error=Configuration".into());
    }
    let mut url = "/login".to_string();
    if !partes.is_empty() {
        url.push('?');
        url.push_str(&partes.join("&"));
    }
    redirigir(&url, &[])
}

async fn pagina_ingreso_proveedor(Path(_p): Path<String>, q: Query<HashMap<String, String>>) -> Response {
    pagina_ingreso(q).await
}

async fn pagina_error(Query(q): Query<HashMap<String, String>>) -> Response {
    // Sin pages.error, NextAuth manda el error a la página de ingreso.
    let error = q.get("error").cloned().unwrap_or_else(|| "Default".into());
    redirigir(&format!("{}/api/auth/signin?error={}", url_base(), codificar(&error)), &[])
}

async fn registro_cliente() -> Response {
    json_con(StatusCode::OK, &[], json!({}))
}

// ═══════════════════════════════ MICROSOFT ═══════════════════════════════
const SCOPES_MS: &str = "openid email profile offline_access Mail.Read Calendars.Read User.Read";
const TOKEN_MS: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

fn clave_cifrado_ms() -> Option<[u8; 32]> {
    let raw = std::env::var("MICROSOFT_TOKEN_ENCRYPTION_KEY").ok().filter(|r| !r.is_empty())?;
    Some(Sha256::digest(raw.as_bytes()).into())
}

/// AES-256-GCM con IV de 16 bytes, formato `iv:tag:cifrado` en hexadecimal (igual que `encryptToken` de Next).
pub fn cifrar_ms(texto: &str) -> Option<String> {
    use aes_gcm::{
        aead::{consts::U16, generic_array::GenericArray, Aead, KeyInit},
        aes::Aes256,
        AesGcm,
    };
    type Gcm16 = AesGcm<Aes256, U16>;
    let clave = clave_cifrado_ms()?;
    let cifra = Gcm16::new(GenericArray::from_slice(&clave));
    let mut iv = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut iv);
    let mut salida = cifra.encrypt(GenericArray::from_slice(&iv), texto.as_bytes()).ok()?;
    let tag = salida.split_off(salida.len() - 16);
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    Some(format!("{}:{}:{}", hex(&iv), hex(&tag), hex(&salida)))
}

fn usuario_de_sesion(st: &AppState, cab: &HeaderMap) -> Option<String> {
    let c = claims_de(cab, &st.cfg.nextauth_secret)?;
    c["id"].as_str().filter(|x| !x.is_empty()).map(String::from)
}

async fn microsoft_inicio(State(st): State<AppState>, cab: HeaderMap) -> Response {
    if usuario_de_sesion(&st, &cab).is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "No autenticado" }))).into_response();
    }
    let (Ok(cliente), Ok(redirect)) = (std::env::var("MICROSOFT_CLIENT_ID"), std::env::var("MICROSOFT_REDIRECT_URI")) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "Error interno" }))).into_response();
    };
    let estado = aleatorio_hex(32);
    let url = format!(
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id={}&scope={}&redirect_uri={}&response_mode=query&response_type=code&state={}&prompt=consent",
        codificar(&cliente),
        codificar(SCOPES_MS),
        codificar(&redirect),
        estado
    );
    let mut c = format!("microsoft_oauth_state={estado}; Max-Age=600; Path=/; HttpOnly; SameSite=Lax");
    if seguras() {
        c.push_str("; Secure");
    }
    redirigir(&url, &[c])
}

async fn microsoft_callback(State(st): State<AppState>, cab: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let base = url_base();
    let Some(usuario) = usuario_de_sesion(&st, &cab) else { return redirigir(&format!("{base}/login?error=microsoft_unauthorized"), &[]) };
    let guardado = leer_cookie(&cab, "microsoft_oauth_state");
    let borrar = vec![format!("microsoft_oauth_state=; Max-Age=0; Path=/{}", if seguras() { "; Secure" } else { "" })];
    if let Some(e) = q.get("error") {
        let msg = q.get("error_description").filter(|m| !m.is_empty()).unwrap_or(e);
        return redirigir(&format!("{base}/profile?microsoft=error&message={}", codificar(msg)), &borrar);
    }
    let (Some(codigo), Some(estado), Some(guardado)) = (q.get("code").filter(|c| !c.is_empty()), q.get("state").filter(|c| !c.is_empty()), guardado.filter(|c| !c.is_empty())) else {
        return redirigir(&format!("{base}/profile?microsoft=error&message=invalid_state"), &borrar);
    };
    if *estado != guardado {
        return redirigir(&format!("{base}/profile?microsoft=error&message=invalid_state"), &borrar);
    }
    let falla = || redirigir(&format!("{base}/profile?microsoft=error&message=token_exchange_failed"), &borrar);
    let (Ok(cliente), Ok(secreto), Ok(redirect)) = (std::env::var("MICROSOFT_CLIENT_ID"), std::env::var("MICROSOFT_CLIENT_SECRET"), std::env::var("MICROSOFT_REDIRECT_URI")) else { return falla() };
    let r = st
        .http
        .post(TOKEN_MS)
        .form(&[("client_id", cliente.as_str()), ("client_secret", &secreto), ("code", codigo), ("redirect_uri", &redirect), ("grant_type", "authorization_code"), ("scope", SCOPES_MS)])
        .send()
        .await;
    let datos: Value = match r {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
        _ => return falla(),
    };
    let Some(acceso) = datos["access_token"].as_str() else { return falla() };
    let correo = datos["id_token"]
        .as_str()
        .and_then(|t| t.split('.').nth(1))
        .and_then(|p| URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|c| ["preferred_username", "email", "upn"].iter().find_map(|k| c[*k].as_str().filter(|x| !x.is_empty()).map(String::from)));
    let expira = ahora() + datos["expires_in"].as_i64().unwrap_or(3600);
    let (Some(acceso_c), refresco_c) = (cifrar_ms(acceso), datos["refresh_token"].as_str().and_then(cifrar_ms)) else { return falla() };
    let r = exec(
        &st.pool,
        r#"UPDATE "User" SET "microsoftAccessToken" = $2, "microsoftRefreshToken" = COALESCE($3, "microsoftRefreshToken"), "microsoftTokenExpiry" = to_timestamp($4::float8) AT TIME ZONE 'UTC',
             "microsoftAccountEmail" = $5, "updatedAt" = NOW() WHERE id = $1"#,
        &[B::T(usuario), B::T(acceso_c), B::OT(refresco_c), B::F(expira as f64), B::OT(correo)],
    )
    .await;
    match r {
        Ok(_) => redirigir(&format!("{base}/profile?microsoft=connected"), &borrar),
        Err(e) => {
            tracing::error!("Microsoft OAuth callback error: {e}");
            falla()
        }
    }
}

async fn microsoft_desconectar(State(st): State<AppState>, cab: HeaderMap) -> Response {
    let Some(usuario) = usuario_de_sesion(&st, &cab) else { return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "No autenticado" }))).into_response() };
    let r = exec(
        &st.pool,
        r#"UPDATE "User" SET "microsoftAccessToken" = NULL, "microsoftRefreshToken" = NULL, "microsoftTokenExpiry" = NULL, "microsoftAccountEmail" = NULL, "updatedAt" = NOW() WHERE id = $1"#,
        &[B::T(usuario)],
    )
    .await;
    match r {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => {
            tracing::error!("Microsoft disconnect: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "Error interno" }))).into_response()
        }
    }
}
