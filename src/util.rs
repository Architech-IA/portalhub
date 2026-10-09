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
    /// Nulos tipados: la caché de sentencias reutiliza el tipo del parámetro de la primera
    /// ejecución, así que un parámetro que a veces es número/booleano nunca debe viajar como
    /// texto nulo (daría "invalid input syntax for type boolean").
    OF(Option<f64>),
    OBo(Option<bool>),
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
            B::OF(x) => q.bind(*x),
            B::OBo(x) => q.bind(*x),
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
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?([+]00(:00)?)?$").expect("regex fecha"));

fn normalizar_fecha(s: &str) -> String {
    // Las columnas `timestamptz` (tablas del consejo) salen con sufijo +00:00; se descarta.
    let s = s.strip_suffix("+00:00").or_else(|| s.strip_suffix("+00")).unwrap_or(s);
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
    log_activity_full(pool, tipo, descripcion, entity_type, entity_id, user_id, lead_id, None, None).await
}

/// Igual que `log_activity` pero con `proposalId` y `projectId` (los usan propuestas e hitos).
#[allow(clippy::too_many_arguments)]
pub async fn log_activity_full(
    pool: &PgPool,
    tipo: &str,
    descripcion: &str,
    entity_type: &str,
    entity_id: &str,
    user_id: Option<&str>,
    lead_id: Option<&str>,
    proposal_id: Option<&str>,
    project_id: Option<&str>,
) {
    let Some(uid) = user_id.filter(|u| !u.is_empty()) else { return };
    let r = exec(
        pool,
        r#"INSERT INTO "Activity" (id, type, description, "entityType", "entityId", "userId", "leadId", "proposalId", "projectId", "createdAt")
           VALUES ($1, $2::"ActivityType", $3, $4, $5, $6, $7, $8, $9, NOW())"#,
        &[
            B::T(new_id()),
            B::T(tipo.to_string()),
            B::T(descripcion.to_string()),
            B::T(entity_type.to_string()),
            B::T(entity_id.to_string()),
            B::T(uid.to_string()),
            B::OT(lead_id.filter(|x| !x.is_empty()).map(|x| x.to_string())),
            B::OT(proposal_id.filter(|x| !x.is_empty()).map(|x| x.to_string())),
            B::OT(project_id.filter(|x| !x.is_empty()).map(|x| x.to_string())),
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

// ── Helpers para rutas CRUD ─────────────────────────────────────────────────────────────────
/// `new Date(x)` de JavaScript sobre un parámetro de texto (ISO o "YYYY-MM-DD" = medianoche UTC)
/// → timestamp UTC sin zona (como guarda Prisma). NULL si el parámetro es NULL.
pub fn ts_js_opt(n: usize) -> String {
    format!("CASE WHEN ${n}::text IS NULL THEN NULL ELSE ${n}::text::timestamptz AT TIME ZONE 'UTC' END")
}

/// `x ? new Date(x) : null` — texto de fecha del cuerpo (vacío o ausente = None).
pub fn fecha_cuerpo(body: &Value, k: &str) -> Option<String> {
    match body.get(k) {
        Some(Value::String(x)) if !x.is_empty() => Some(x.clone()),
        _ => None,
    }
}

/// `x != null && x !== '' ? Number(x) : null`
pub fn num_o_nulo(body: &Value, k: &str) -> Option<f64> {
    match body.get(k) {
        None | Some(Value::Null) => None,
        Some(Value::String(x)) if x.is_empty() => None,
        v => Some(numero(v)),
    }
}

/// Constructor de `UPDATE ... SET col = $n, ... WHERE id = $1 RETURNING *` con columnas
/// opcionales (el `...(x !== undefined ? {x} : {})` de Prisma). `$1` siempre es el id.
pub struct Upd {
    pub sets: Vec<String>,
    pub binds: Vec<B>,
}

impl Upd {
    /// Tabla con columna "updatedAt" (se refresca sola, Prisma lo hace en el cliente).
    pub fn new(id: &str) -> Self {
        Self { sets: vec![r#""updatedAt" = NOW()"#.to_string()], binds: vec![B::T(id.to_string())] }
    }
    /// Tabla sin "updatedAt".
    pub fn sin_updated(id: &str) -> Self {
        Self { sets: vec![], binds: vec![B::T(id.to_string())] }
    }
    pub fn set(&mut self, col: &str, b: B) {
        self.binds.push(b);
        self.sets.push(format!(r#""{col}" = ${}"#, self.binds.len()));
    }
    /// Con una expresión que usa el marcador `{n}` (un cast, `ts_js_opt`...).
    pub fn set_expr(&mut self, col: &str, b: B, expr: &str) {
        self.binds.push(b);
        let n = self.binds.len();
        self.sets.push(format!(r#""{col}" = {}"#, expr.replace("{n}", &format!("${n}"))));
    }
    pub fn sql(&self, tabla: &str) -> String {
        format!(
            r#"WITH up AS (UPDATE "{tabla}" SET {} WHERE id = $1 RETURNING *) SELECT to_jsonb(up) FROM up"#,
            self.sets.join(", ")
        )
    }
}

/// Valor del cuerpo como texto opcional: string → Some, null/ausente → None.
pub fn s_opt(body: &Value, k: &str) -> Option<String> {
    s(body, k)
}

/// `sesion.id` como `Option<&str>` para `log_activity` (vacío = sin usuario).
pub fn uid(sesion: &crate::session::Session) -> Option<&str> {
    if sesion.id.is_empty() {
        None
    } else {
        Some(&sesion.id)
    }
}

impl Upd {
    /// `WITH up AS (UPDATE ... RETURNING *) <resto>` — para devolver la fila con relaciones.
    pub fn con(&self, tabla: &str, resto: &str) -> String {
        format!(r#"WITH up AS (UPDATE "{tabla}" SET {} WHERE id = $1 RETURNING *) {resto}"#, self.sets.join(", "))
    }
}

// ── Caché por huella para lecturas grandes ──────────────────────────────────────────────────
/// Últimas respuestas de lecturas grandes, por clave (p. ej. el usuario), con su huella (md5 del JSON).
pub type CacheJson = std::sync::Mutex<std::collections::HashMap<String, (String, std::sync::Arc<Value>)>>;

/// Para lecturas de decenas o cientos de KB cuyo costo real es TRANSFERIR el JSON desde la base
/// remota (el enlace rinde ~0,5 MB/s: el backlog de 1,3 MB tardaba ~1,4 s; una lista de 150 KB,
/// ~0,3 s). Primero se pide solo la huella (md5 del mismo JSON, calculada en la base): si coincide
/// con la última respuesta de esa `clave`, se devuelve la copia en memoria; si no, se trae todo
/// (JSON + huella en una sola consulta). La respuesta es SIEMPRE el estado actual de la base:
/// cualquier escritura (de Rust, de Next o del Motor) cambia la huella. `sql` debe ser un `SELECT`
/// de UNA columna jsonb.
pub async fn fetch_json_cacheado(pool: &PgPool, cache: &CacheJson, clave: &str, sql: &str, binds: &[B]) -> Result<std::sync::Arc<Value>, sqlx::Error> {
    let hash_sql = format!("SELECT md5(t.j::text) FROM ({sql}) t(j)");
    let actual = fetch_text_opt(pool, &hash_sql, binds).await?;
    if let Some(h) = &actual {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((hc, v)) = guard.get(clave) {
            if hc == h {
                return Ok(v.clone());
            }
        }
    }
    let full_sql = format!("SELECT t.j, md5(t.j::text) FROM ({sql}) t(j)");
    let row = aplicar(sqlx::query(&full_sql), binds).fetch_one(pool).await?;
    let mut v: Value = row.try_get(0)?;
    fix_dates(&mut v);
    let huella: String = row.try_get(1)?;
    let v = std::sync::Arc::new(v);
    let mut g = cache.lock().unwrap_or_else(|e| e.into_inner());
    if g.len() >= 64 {
        g.clear();
    }
    g.insert(clave.to_string(), (huella, v.clone()));
    Ok(v)
}
