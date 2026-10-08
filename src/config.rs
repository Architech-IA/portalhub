use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub nextauth_secret: String,
    pub internal_api_key: Option<String>,
    pub opencode_api_key: Option<String>,
    pub opencode_url: String,
    pub opencode_model: String,
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub bind: String,
    pub port: u16,
}

/// Prisma acepta parámetros en la URL que sqlx no entiende (`schema`, `connection_limit`,
/// `pool_timeout`, `pgbouncer`) — se sacan para reusar exactamente la misma DATABASE_URL que ya
/// usa el portal, sin duplicarla ni mantener una segunda.
fn limpiar_url_prisma(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    const IGNORAR: [&str; 4] = ["schema", "connection_limit", "pool_timeout", "pgbouncer"];
    let resto: Vec<&str> = query
        .split('&')
        .filter(|p| {
            let k = p.split('=').next().unwrap_or("");
            !IGNORAR.contains(&k)
        })
        .collect();
    if resto.is_empty() {
        base.to_string()
    } else {
        format!("{}?{}", base, resto.join("&"))
    }
}

fn opcional(nombre: &str) -> Option<String> {
    env::var(nombre).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let database_url = env::var("DATABASE_URL").map_err(|_| "falta DATABASE_URL".to_string())?;
        let nextauth_secret = env::var("NEXTAUTH_SECRET").map_err(|_| "falta NEXTAUTH_SECRET".to_string())?;
        Ok(Self {
            database_url: limpiar_url_prisma(&database_url),
            nextauth_secret,
            internal_api_key: opcional("INTERNAL_API_KEY"),
            opencode_api_key: opcional("OPENCODE_API_KEY"),
            opencode_url: opcional("OPENCODE_URL")
                .unwrap_or_else(|| "https://opencode.ai/zen/go/v1/chat/completions".to_string()),
            opencode_model: opcional("OPENCODE_MODEL").unwrap_or_else(|| "qwen3.7-max".to_string()),
            google_client_id: opcional("GOOGLE_CLIENT_ID"),
            google_client_secret: opcional("GOOGLE_CLIENT_SECRET"),
            bind: opcional("BIND").unwrap_or_else(|| "127.0.0.1".to_string()),
            port: opcional("PORT").and_then(|p| p.parse().ok()).unwrap_or(3100),
        })
    }
}
