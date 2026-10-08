//! Utilidades compartidas: acceso a Postgres devolviendo JSON, ids y fechas compatibles con Prisma.
//!
//! Estrategia de datos: las rutas devuelven el resultado de consultas que arman el JSON en el
//! propio Postgres (`to_jsonb`, `jsonb_build_object`, `jsonb_agg`), así la forma de la respuesta
//! es la misma que arma Prisma hoy (mismos nombres de campo, relaciones anidadas) sin tener que
//! declarar una struct por cada consulta. Las escrituras usan SQL parametrizado.

use std::sync::LazyLock;

use rand::Rng;
use regex::Regex;
use serde_json::Value;
use sqlx::{postgres::PgArguments, query::Query, PgPool, Postgres, Row};

/// Parámetro enlazable a una consulta.
#[derive(Debug, Clone)]
pub enum B {
    T(String),
    OT(Option<String>),
    I(i64),
    F(f64),
    J(Value),
    Bo(bool),
}

impl From<&str> for B {
    fn from(s: &str) -> Self {
        B::T(s.to_string())
    }
}
impl From<String> for B {
    fn from(s: String) -> Self {
        B::T(s)
    }
}
impl From<Option<String>> for B {
    fn from(s: Option<String>) -> Self {
        B::OT(s)
    }
}

fn aplicar<'q>(mut q: Query<'q, Postgres, PgArguments>, binds: &'q [B]) -> Query<'q, Postgres, PgArguments> {
    for b in binds {
        q = match b {
            B::T(s) => q.bind(s.as_str()),
            B::OT(s) => q.bind(s.as_deref()),
            B::I(i) => q.bind(*i),
            B::F(f) => q.bind(*f),
            B::J(v) => q.bind(v),
            B::Bo(x) => q.bind(*x),
        };
    }
    q
}

/// Ejecuta una consulta que devuelve UNA columna jsonb en UNA fila.
pub async fn fetch_json(pool: &PgPool, sql: &str, binds: &[B]) -> Result<Value, sqlx::Error> {
    let row = aplicar(sqlx::query(sql), binds).fetch_one(pool).await?;
    let mut v: Value = row.try_get(0)?;
    fix_dates(&mut v);
    Ok(v)
}

/// Igual que `fetch_json` pero `None` si la consulta no devuelve fila.
pub async fn fetch_json_opt(pool: &PgPool, sql: &str, binds: &[B]) -> Result<Option<Value>, sqlx::Error> {
    let row = aplicar(sqlx::query(sql), binds).fetch_optional(pool).await?;
    match row {
        Some(r) => {
            let mut v: Value = r.try_get(0)?;
            fix_dates(&mut v);
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/// Ejecuta una sentencia y devuelve las filas afectadas.
pub async fn exec(pool: &PgPool, sql: &str, binds: &[B]) -> Result<u64, sqlx::Error> {
    Ok(aplicar(sqlx::query(sql), binds).execute(pool).await?.rows_affected())
}

/// Una columna de texto en una fila (o None).
pub async fn fetch_text_opt(pool: &PgPool, sql: &str, binds: &[B]) -> Result<Option<String>, sqlx::Error> {
    let row = aplicar(sqlx::query(sql), binds).fetch_optional(pool).await?;
    Ok(match row {
        Some(r) => r.try_get::<Option<String>, _>(0)?,
        None => None,
    })
}

/// Un entero (COUNT, etc.).
pub async fn fetch_i64(pool: &PgPool, sql: &str, binds: &[B]) -> Result<i64, sqlx::Error> {
    let row = aplicar(sqlx::query(sql), binds).fetch_one(pool).await?;
    row.try_get::<i64, _>(0)
}

// ── Fechas ──────────────────────────────────────────────────────────────────────────────────
// Prisma devuelve las fechas como ISO-8601 UTC con milisegundos ("2026-10-07T18:49:59.123Z");
// `to_jsonb` de un timestamp(3) las da sin zona y con fracción de largo variable. Se normalizan
// al formato de JavaScript para que el frontend no note ninguna diferencia.
static RE_FECHA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?$").expect("regex fecha"));

fn normalizar_fecha(s: &str) -> String {
    let (base, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut f: String = frac.chars().take(3).collect();
    while f.len() < 3 {
        f.push('0');
    }
    format!("{base}.{f}Z")
}

pub fn fix_dates(v: &mut Value) {
    match v {
        Value::String(s) => {
            if RE_FECHA.is_match(s) {
                *s = normalizar_fecha(s);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(fix_dates),
        Value::Object(o) => o.values_mut().for_each(fix_dates),
        _ => {}
    }
}

/// Igual que `fetch_json_opt` pero SIN normalizar fechas: para columnas `Json` cuyo contenido
/// es del usuario (arquitectura, diagramas) y no debe tocarse.
pub async fn fetch_json_raw(pool: &PgPool, sql: &str, binds: &[B]) -> Result<Option<Value>, sqlx::Error> {
    let row = aplicar(sqlx::query(sql), binds).fetch_optional(pool).await?;
    match row {
        Some(r) => Ok(Some(r.try_get::<Value, _>(0)?)),
        None => Ok(None),
    }
}

// ── Fechas en SQL ───────────────────────────────────────────────────────────────────────────
/// `parseUTC5(x)` de `lib/timezone.ts`: `new Date(x + "-05:00")` → timestamp UTC (sin zona).
pub fn ts_utc5(n: usize) -> String {
    format!("(${n}::text || '-05:00')::timestamptz AT TIME ZONE 'UTC'")
}

/// `parseUTC5Nullable`: NULL si el parámetro es NULL.
pub fn ts_utc5_opt(n: usize) -> String {
    format!("CASE WHEN ${n}::text IS NULL THEN NULL ELSE (${n}::text || '-05:00')::timestamptz AT TIME ZONE 'UTC' END")
}

/// `getDateStrUTC5` (fr-CA): "YYYY-MM-DD" en hora de Bogotá a partir de un timestamp UTC.
pub fn fecha_utc5(col: &str) -> String {
    format!("to_char(({col}) AT TIME ZONE 'UTC' AT TIME ZONE 'America/Bogota', 'YYYY-MM-DD')")
}

// ── Actividad ───────────────────────────────────────────────────────────────────────────────
/// Equivalente a `logActivity` de `lib/activity.ts`: no hace nada sin usuario y nunca falla
/// hacia afuera (un error al registrar actividad no debe romper la operación principal).
pub async fn log_activity(
    pool: &PgPool,
    tipo: &str,
    descripcion: &str,
    entity_type: &str,
    entity_id: &str,
    user_id: Option<&str>,
    lead_id: Option<&str>,
) {
    let Some(uid) = user_id.filter(|u| !u.is_empty()) else { return };
    let r = exec(
        pool,
        r#"INSERT INTO "Activity" (id, type, description, "entityType", "entityId", "userId", "leadId", "createdAt")
           VALUES ($1, $2::"ActivityType", $3, $4, $5, $6, $7, NOW())"#,
        &[
            B::T(new_id()),
            B::T(tipo.to_string()),
            B::T(descripcion.to_string()),
            B::T(entity_type.to_string()),
            B::T(entity_id.to_string()),
            B::T(uid.to_string()),
            B::OT(lead_id.map(|x| x.to_string())),
        ],
    )
    .await;
    if let Err(e) = r {
        tracing::error!("logActivity error: {e}");
    }
}

/// `body.k` es "truthy" en JavaScript (existe y no es null/false/0/"").
pub fn truthy(body: &Value, k: &str) -> bool {
    match body.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `body.k !== undefined` (la clave viene en el JSON, aunque sea null).
pub fn presente(body: &Value, k: &str) -> bool {
    body.get(k).is_some()
}

/// `body.k || null`: string no vacío, o None.
pub fn s_o_nulo(body: &Value, k: &str) -> Option<String> {
    s(body, k).filter(|x| !x.is_empty())
}

// ── Ids ─────────────────────────────────────────────────────────────────────────────────────
fn base36(mut n: u128) -> String {
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(D[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Id con la misma forma que `cuid()` de Prisma ("c" + marca de tiempo + aleatorio, base 36).
pub fn new_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut rng = rand::thread_rng();
    let azar: u128 = rng.gen_range(36u128.pow(11)..36u128.pow(12));
    let contador: u128 = rng.gen_range(0..36u128.pow(4));
    format!("c{}{:0>4}{}", base36(ms), base36(contador), base36(azar))
}

// ── Cuerpo JSON ─────────────────────────────────────────────────────────────────────────────
pub fn s(body: &Value, k: &str) -> Option<String> {
    body.get(k).and_then(|v| v.as_str()).map(|x| x.to_string())
}

pub fn s_no_vacio(body: &Value, k: &str) -> Option<String> {
    s(body, k).filter(|x| !x.trim().is_empty())
}

// ── Números como los ve JavaScript ──────────────────────────────────────────────────────────
/// `parseFloat(x) || 0`: toma el prefijo numérico de un string (o el número tal cual).
pub fn parse_float(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0),
        Some(Value::String(s)) => {
            let t = s.trim_start();
            for fin in (1..=t.len()).rev() {
                if !t.is_char_boundary(fin) {
                    continue;
                }
                if let Ok(x) = t[..fin].parse::<f64>() {
                    if x.is_finite() {
                        return x;
                    }
                }
            }
            0.0
        }
        _ => 0.0,
    }
}

/// `Number(x) || 0`: conversión completa (un string con basura da 0).
pub fn numero(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|x| x.is_finite()).unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => 0.0,
    }
}
