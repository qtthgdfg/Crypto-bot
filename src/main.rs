//! # Pro Trading Bot v3.0
//!
//! Entry point: initialises structured logging, loads validated config,
//! wires the dependency graph, and runs a graceful-shutdown handler.
//!
//! **Every** business-logic concern lives in a dedicated workspace crate.
//! This file is intentionally thin — it is only glue.

use anyhow::Result;
use tracing::{error, info};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    // ── 1. Structured logging ─────────────────────────────────────
    // Set RUST_LOG=debug for verbose output, info is the default.
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(fmt::layer().with_target(true).with_line_number(true))
        .init();

    info!(version = "3.0.0", "Pro Trading Bot starting");

    // ── 2. Load .env (silently ignored if absent in production) ───
    dotenvy::dotenv().ok();

    // ── 3. Validated configuration ────────────────────────────────
    let config = match bot_config::Config::from_env() {
        Ok(c) => {
            info!(mode = %c.mode, max_symbols = c.max_symbols, "Config loaded");
            c
        }
        Err(e) => {
            error!(error = %e, "Invalid configuration — check your .env file");
            std::process::exit(1);
        }
    };

    // ── 4. Build shared infrastructure ────────────────────────────
    let db       = bot_db::SqliteStore::connect(&config.db_path).await?;
    let telegram = bot_telegram::TelegramClient::new(&config.telegram_token, &config.telegram_chat_id);
    let exchange = build_exchange(&config);

    // ── 5. Compose top-level orchestrator ─────────────────────────
    let bot = build_bot(config, db, telegram, exchange).await?;

    // ── 6. Run until signal or fatal error ───────────────────────
    tokio::select! {
        result = bot.run() => {
            if let Err(ref e) = result {
                error!(error = %e, "Bot exited with error");
            }
            result
        }
        _ = shutdown_signal() => {
            info!("Shutdown signal received — stopping gracefully");
            Ok(())
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Factory helpers (keep main() readable)
// ─────────────────────────────────────────────────────────────────

fn build_exchange(
    config: &bot_config::Config,
) -> std::sync::Arc<dyn bot_exchange::Exchange> {
    use bot_exchange::{binance::BinanceExchange, paper::PaperExchange};
    use bot_config::Mode;
    use std::sync::Arc;

    match config.mode {
        Mode::Live => Arc::new(BinanceExchange::new(
            &config.binance_api_key,
            &config.binance_secret,
        )),
        Mode::Paper => Arc::new(PaperExchange::new(config.starting_capital)),
    }
}

async fn build_bot(
    config: bot_config::Config,
    db: bot_db::SqliteStore,
    telegram: bot_telegram::TelegramClient,
    exchange: std::sync::Arc<dyn bot_exchange::Exchange>,
) -> Result<bot_learning::Orchestrator> {
    use std::sync::Arc;

    let config   = Arc::new(config);
    let db       = Arc::new(db);
    let telegram = Arc::new(telegram);

    let risk     = bot_risk::RiskManager::new(config.clone(), db.clone());
    let learner  = bot_learning::LearningEngine::new(db.clone(), telegram.clone());
    let advisor  = bot_advisor::Advisor::new(db.clone(), telegram.clone());
    let scanner  = bot_scanner::CoinScanner::new(exchange.clone());

    Ok(bot_learning::Orchestrator::new(
        config, db, telegram, exchange, risk, learner, advisor, scanner,
    ))
}

// ── OS signal handler ─────────────────────────────────────────────
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c   => { info!("Received Ctrl+C");  }
        _ = sigterm  => { info!("Received SIGTERM"); }
    }
}
