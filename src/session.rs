//! Sesión compartida con NextAuth v4: el portal sigue siendo quien emite la cookie
//! (`next-auth.session-token`); este servicio solo la VALIDA, así nadie se loguea dos veces.
//!
//! NextAuth v4.24 guarda la sesión como un JWE compacto (alg `dir`, enc `A256GCM`) cuya clave
//! se deriva con HKDF-SHA256 desde NEXTAUTH_SECRET (salt vacío, info
//! "NextAuth.js Generated Encryption Key", 32 bytes). Se descifra a mano con aes-gcm.
//!
//! Hardening respecto del portal actual: `proxy.ts` de Next solo comprueba que la cookie EXISTA
//! (no que sea válida), así que hoy cualquier valor inventado pasa el filtro. Acá toda ruta
//! exige una sesión realmente descifrable y vigente, o la API key interna.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use axum::{extract::FromRequestParts, http::request::Parts};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hkdf::Hkdf;
use serde_json::Value;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{error::ApiError, state::AppState};

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    /// Llamada servidor-a-servidor con la API key interna (sin usuario).
    pub is_service: bool,
}

impl Session {
    pub fn is_admin(&self) -> bool {
        self.role == "ADMIN" || self.role == "SUPERADMIN"
    }

    /// Equivalente a `requireAdmin` de las rutas de Next: 403 si no es ADMIN/SUPERADMIN.
    pub fn require_admin(&self) -> Result<(), ApiError> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(ApiError::forbidden("No autorizado"))
        }
    }

    /// Equivalente a `if (!token?.sub) return 401`: exige un usuario real (no la API key).
    pub fn require_user(&self) -> Result<&str, ApiError> {
        if self.is_service || self.id.is_empty() {
            Err(ApiError::unauthorized())
        } else {
            Ok(&self.id)
        }
    }
}

const COOKIE_BASES: [&str; 2] = ["__Secure-next-auth.session-token", "next-auth.session-token"];

fn leer_cookie(header: &str, nombre: &str) -> Option<String> {
    for par in header.split(';') {
        let par = par.trim();
        if let Some((k, v)) = par.split_once('=') {
            if k == nombre {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// NextAuth parte la cookie en trozos (`.0`, `.1`, …) cuando supera ~4 KB.
fn token_de_cookies(header: &str) -> Option<String> {
    for base in COOKIE_BASES {
        if let Some(v) = leer_cookie(header, base) {
            return Some(v);
        }
        let mut partes = String::new();
        let mut i = 0;
        while let Some(trozo) = leer_cookie(header, &format!("{base}.{i}")) {
            partes.push_str(&trozo);
            i += 1;
        }
        if !partes.is_empty() {
            return Some(partes);
        }
    }
    None
}

pub fn descifrar_token(token: &str, secret: &str) -> Option<Value> {
    let partes: Vec<&str> = token.split('.').collect();
    if partes.len() != 5 {
        return None;
    }
    let (protegido, iv, cifrado, tag) = (partes[0], partes[2], partes[3], partes[4]);

    let hk = Hkdf::<Sha256>::new(Some(b""), secret.as_bytes());
    let mut clave = [0u8; 32];
    hk.expand(b"NextAuth.js Generated Encryption Key", &mut clave).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&clave).ok()?;

    let iv = URL_SAFE_NO_PAD.decode(iv).ok()?;
    if iv.len() != 12 {
        return None;
    }
    let mut datos = URL_SAFE_NO_PAD.decode(cifrado).ok()?;
    datos.extend(URL_SAFE_NO_PAD.decode(tag).ok()?);

    let plano = cipher
        .decrypt(Nonce::from_slice(&iv), Payload { msg: &datos, aad: protegido.as_bytes() })
        .ok()?;
    let claims: Value = serde_json::from_slice(&plano).ok()?;

    let ahora = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    if let Some(exp) = claims.get("exp").and_then(|e| e.as_i64()) {
        if exp <= ahora {
            return None;
        }
    }
    Some(claims)
}

fn texto(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

impl FromRequestParts<AppState> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // Servidor-a-servidor: misma API key interna que ya acepta proxy.ts.
        if let (Some(esperada), Some(recibida)) = (
            state.cfg.internal_api_key.as_deref(),
            parts.headers.get("x-api-key").and_then(|h| h.to_str().ok()),
        ) {
            if !esperada.is_empty() && esperada == recibida {
                return Ok(Session {
                    id: String::new(),
                    email: String::new(),
                    name: "service".into(),
                    role: "SERVICE".into(),
                    is_service: true,
                });
            }
        }

        let cookies = parts
            .headers
            .get("cookie")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let token = token_de_cookies(cookies).ok_or_else(ApiError::unauthorized)?;
        let claims = descifrar_token(&token, &state.cfg.nextauth_secret).ok_or_else(ApiError::unauthorized)?;

        let id = {
            let sub = texto(&claims, "sub");
            if sub.is_empty() { texto(&claims, "id") } else { sub }
        };
        if id.is_empty() {
            return Err(ApiError::unauthorized());
        }
        Ok(Session {
            id,
            email: texto(&claims, "email"),
            name: texto(&claims, "name"),
            role: texto(&claims, "role"),
            is_service: false,
        })
    }
}

/// Sesión opcional: `None` si no hay una válida (para rutas que aceptan anónimos, como el chat
/// público de Orión, pero que usan el usuario cuando lo hay).
pub struct Opcional(pub Option<Session>);

impl FromRequestParts<AppState> for Opcional {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(Opcional(Session::from_request_parts(parts, state).await.ok()))
    }
}

/// Token de sesión (JWE) dentro de una cabecera `Cookie`, ya unido si vino partido en trozos.
pub fn token_de_cabecera(header: &str) -> Option<String> {
    token_de_cookies(header)
}

fn clave_derivada(secret: &str) -> Option<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(b""), secret.as_bytes());
    let mut clave = [0u8; 32];
    hk.expand(b"NextAuth.js Generated Encryption Key", &mut clave).ok()?;
    Some(clave)
}

/// `encode` de `next-auth/jwt`: JWE compacto (`dir` + `A256GCM`) con `iat`, `exp` y `jti`.
pub fn cifrar_token(claims: &Value, secret: &str, max_edad_s: i64) -> Option<String> {
    use rand::RngCore;
    let ahora = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let mut carga = claims.as_object()?.clone();
    carga.insert("iat".into(), ahora.into());
    carga.insert("exp".into(), (ahora + max_edad_s).into());
    let mut jti = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut jti);
    jti[6] = (jti[6] & 0x0f) | 0x40;
    jti[8] = (jti[8] & 0x3f) | 0x80;
    let h: Vec<String> = jti.iter().map(|x| format!("{x:02x}")).collect();
    carga.insert("jti".into(), format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat()).into());

    let protegido = URL_SAFE_NO_PAD.encode(br#"{"alg":"dir","enc":"A256GCM"}"#);
    let cipher = Aes256Gcm::new_from_slice(&clave_derivada(secret)?).ok()?;
    let mut iv = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let plano = serde_json::to_vec(&Value::Object(carga)).ok()?;
    let mut cifrado = cipher.encrypt(Nonce::from_slice(&iv), Payload { msg: &plano, aad: protegido.as_bytes() }).ok()?;
    let tag = cifrado.split_off(cifrado.len() - 16);
    Some(format!("{protegido}..{}.{}.{}", URL_SAFE_NO_PAD.encode(iv), URL_SAFE_NO_PAD.encode(cifrado), URL_SAFE_NO_PAD.encode(tag)))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn jwe_ida_y_vuelta() {
        let claims = serde_json::json!({ "sub": "u1", "id": "u1", "role": "ADMIN", "name": "Ñandú", "email": "a@b.co" });
        let t = cifrar_token(&claims, "secreto-de-prueba", 3600).expect("cifra");
        assert_eq!(t.split('.').count(), 5);
        let c = descifrar_token(&t, "secreto-de-prueba").expect("descifra");
        assert_eq!(c["id"], "u1");
        assert_eq!(c["name"], "Ñandú");
        assert!(c["exp"].as_i64().unwrap() > c["iat"].as_i64().unwrap());
        assert!(descifrar_token(&t, "otro-secreto").is_none());
        // vencido
        let v = cifrar_token(&claims, "s", -10).expect("cifra");
        assert!(descifrar_token(&v, "s").is_none());
    }
}
