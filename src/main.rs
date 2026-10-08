mod config;
mod error;
mod extract;
mod google;
mod llm;
mod routes;
mod session;
mod state;
mod texto;
mod util;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{extract::DefaultBodyLimit, extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::Connection;
use std::str::FromStr;

use crate::{config::Config, state::AppState};

async fn health(State(st): State<AppState>) -> Json<Value> {
    let db = sqlx::query("SELECT 1").fetch_one(&st.pool).await.is_ok();
    Json(json!({ "status": if db { "ok" } else { "degraded" }, "db": db, "service": "portalhub" }))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = match Config::from_env() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!("configuración inválida: {e}");
            std::process::exit(1);
        }
    };

    // Las fechas de Prisma se guardan en UTC (timestamp(3) sin zona): la sesión de Postgres se
    // fija en UTC para que los casts `::timestamptz` de las rutas den lo mismo que en Next.
    let mut opts = PgConnectOptions::from_str(&cfg.database_url)
        .unwrap_or_else(|e| {
            tracing::error!("DATABASE_URL inválida: {e}");
            std::process::exit(1);
        })
        .options([("timezone", "UTC")]);

    // La base del portal es remota (Supabase): la conexión DEBE ir cifrada. `Require` cifra
    // siempre (y falla si el servidor no ofrece TLS) en vez de caer a texto plano en silencio;
    // contra una base local (desarrollo) no se exige. No se verifica el certificado porque
    // Supabase firma con su propia CA, que no está entre las raíces públicas.
    let es_local = ["@localhost", "@127.0.0.1", "@[::1]"].iter().any(|h| cfg.database_url.contains(h));
    if !es_local {
        opts = opts.ssl_mode(PgSslMode::Require);
    }

    // El arranque (scripts/start.sh) apunta al pooler en modo SESIÓN, donde cada conexión es
    // exclusiva de este servicio: las sentencias preparadas con nombre y su caché funcionan, y
    // cada consulta repetida cuesta UN viaje a la base en vez de dos (importante con la base
    // remota). En modo transacción (puerto 6543) esto NO funcionaría.

    // Medido (2026-10-08): la red pura al servidor de la base son ~85 ms, pero un `SELECT 1`
    // tardaba ~190 ms. La diferencia era el "ping" de comprobación que sqlx hace ANTES de cada
    // consulta al sacar una conexión del pool (`test_before_acquire`, activado por defecto): un
    // viaje de ida y vuelta extra por petición. Se reemplaza por una comprobación condicional:
    // solo se hace el ping si la conexión estuvo inactiva más de 30 s (cuando el pooler pudo
    // haberla cerrado); en uso normal, la consulta sale directo.
    let pool = PgPoolOptions::new()
        .min_connections(2)
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(10))
        .test_before_acquire(false)
        .before_acquire(|conn, meta| {
            Box::pin(async move {
                if meta.idle_for < Duration::from_secs(30) {
                    return Ok(true);
                }
                Ok(conn.ping().await.is_ok())
            })
        })
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .connect_with(opts)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("no se pudo conectar a Postgres: {e}");
            std::process::exit(1);
        });

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .expect("cliente http");

    let state = AppState { pool, cfg: cfg.clone(), http };

    let app = Router::new()
        .route("/health", get(health))
        .merge(routes::router())
        // Los archivos del hub de leads viajan en base64 dentro del JSON.
        .layer(DefaultBodyLimit::max(40 * 1024 * 1024))
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port).parse().unwrap_or_else(|e| {
        tracing::error!("dirección inválida: {e}");
        std::process::exit(1);
    });
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
        tracing::error!("no se pudo abrir {addr}: {e}");
        std::process::exit(1);
    });
    tracing::info!("portalhub escuchando en http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("servidor");
}
