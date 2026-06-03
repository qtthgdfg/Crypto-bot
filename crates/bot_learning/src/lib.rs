//! # bot_learning
//!
//! Two things live here:
//!
//! * `LearningEngine` — records every signal as a paper trade,
//!   evaluates TP / SL hits against live prices, and fires the
//!   "LIVE READY" Telegram alert when performance targets are met.
//!
//! * `Orchestrator` — the top-level async driver that wires every
//!   crate together and runs the main 30-second tick loop.

use bot_advisor::Advisor;
use bot_config::{Config, Mode};
use bot_db::Store;
use bot_exchange::Exchange;
use bot_risk::RiskManager;
use bot_scanner::CoinScanner;
use bot_signals::{self as signals, EngineConfig};
use bot_telegram::TelegramClient;
use bot_types::{Direction, Interval, Metrics, Outcome, Signal, Symbol, TradeRecord};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum LearnError {
    #[error("Exchange error: {0}")]
    Exchange(#[from] bot_exchange::ExchangeError),
    #[error("Database error: {0}")]
    Db(#[from] bot_db::DbError),
}

// ─────────────────────────────────────────────────────────────────
// Learning Engine
// ─────────────────────────────────────────────────────────────────

pub struct LearningEngine {
    db:           Arc<dyn Store>,
    tg:           Arc<TelegramClient>,
    open_trades:  RwLock<Vec<TradeRecord>>,
    live_notified: RwLock<bool>,
}

impl LearningEngine {
    pub fn new(db: Arc<dyn Store>, tg: Arc<TelegramClient>) -> Self {
        Self {
            db,
            tg,
            open_trades:   RwLock::new(Vec::new()),
            live_notified: RwLock::new(false),
        }
    }

    /// Persist a new signal and add it to the open-trades list.
    pub async fn record(&self, signal: Signal) -> Result<(), LearnError> {
        self.db.save_signal(&signal).await?;
        self.open_trades.write().await.push(TradeRecord {
            signal,
            outcome:    Outcome::Open,
            exit_price: None,
            pnl_usd:    None,
            closed_at:  None,
        });
        Ok(())
    }

    /// Check every open trade against `prices`; close those that
    /// hit TP, SL, or the 24-hour expiry.
    pub async fn evaluate_open(
        &self,
        prices: &HashMap<Symbol, f64>,
    ) {
        let now = Utc::now();
        let mut trades = self.open_trades.write().await;

        for t in trades.iter_mut() {
            if t.outcome != Outcome::Open { continue; }

            let price = match prices.get(&t.signal.symbol) {
                Some(&p) => p,
                None => continue,
            };

            let tp    = t.signal.take_profit.to_f64();
            let sl    = t.signal.stop_loss.to_f64();
            let entry = t.signal.entry.to_f64();

            let tp_hit = match t.signal.direction {
                Direction::Long  => price >= tp,
                Direction::Short => price <= tp,
            };
            let sl_hit = match t.signal.direction {
                Direction::Long  => price <= sl,
                Direction::Short => price >= sl,
            };
            let expired = (now - t.signal.created_at).num_hours() >= 24;

            if !(tp_hit || sl_hit || expired) { continue; }

            let exit = if tp_hit { tp } else if sl_hit { sl } else { price };
            let pnl  = pnl_usd(&t.signal.direction, entry, exit,
                t.signal.size_usd.to_f64().unwrap_or(0.0));

            t.outcome    = if pnl >= 0.0 { Outcome::Win } else { Outcome::Loss };
            t.exit_price = Some(bot_types::Price::new(
                rust_decimal::Decimal::try_from(exit).unwrap_or_default()
            ));
            t.pnl_usd    = Some(pnl);
            t.closed_at  = Some(now);

            // Persist
            if let Err(e) = self.db
                .close_trade(t.signal.id, &t.outcome, exit, pnl)
                .await
            {
                warn!(error=%e, "Failed to persist trade close");
            }

            self.tg.trade_result(&t.signal, &t.outcome, exit, pnl).await;

            info!(
                symbol  = %t.signal.symbol,
                outcome = %t.outcome,
                pnl     = pnl,
                "Paper trade closed"
            );
        }

        drop(trades);
        self.check_live_ready().await;
    }

    async fn check_live_ready(&self) {
        if *self.live_notified.read().await { return; }
        let m = self.db.metrics().await;

        if m.total  >= 50
            && m.win_rate      >= 0.54
            && m.profit_factor >= 1.50
        {
            *self.live_notified.write().await = true;
            self.tg.live_ready(&m).await;
            info!("Live-ready notification sent");
        }
    }

    pub fn open_count(&self) -> usize {
        // Non-blocking best-effort read
        self.open_trades
            .try_read()
            .map(|g| g.iter().filter(|t| t.outcome == Outcome::Open).count())
            .unwrap_or(0)
    }
}

fn pnl_usd(dir: &Direction, entry: f64, exit: f64, size: f64) -> f64 {
    let pct = match dir {
        Direction::Long  => (exit - entry) / entry,
        Direction::Short => (entry - exit) / entry,
    };
    pct * size
}

// ─────────────────────────────────────────────────────────────────
// Orchestrator
// ─────────────────────────────────────────────────────────────────

pub struct Orchestrator {
    config:   Arc<Config>,
    db:       Arc<dyn Store>,
    tg:       Arc<TelegramClient>,
    exchange: Arc<dyn Exchange>,
    risk:     Arc<RiskManager>,
    learner:  Arc<LearningEngine>,
    advisor:  Arc<Advisor>,
    scanner:  CoinScanner,
    watchlist: RwLock<Vec<Symbol>>,
}

impl Orchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config:   Arc<Config>,
        db:       Arc<dyn Store>,
        tg:       Arc<TelegramClient>,
        exchange: Arc<dyn Exchange>,
        risk:     Arc<RiskManager>,
        learner:  Arc<LearningEngine>,
        advisor:  Arc<Advisor>,
        scanner:  CoinScanner,
    ) -> Self {
        Self {
            config,
            db,
            tg,
            exchange,
            risk,
            learner,
            advisor,
            scanner,
            watchlist: RwLock::new(Vec::new()),
        }
    }

    // ── Entry point ───────────────────────────────────────────────

    pub async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        let capital = self.db.capital().await;
        self.tg.startup(
            &self.config.mode.to_string(),
            capital,
            self.config.max_symbols,
        ).await;

        info!(mode=%self.config.mode, capital, "Orchestrator starting");

        // ── Initial watchlist scan ────────────────────────────────
        self.refresh_watchlist().await;

        // ── Spawn background tasks ────────────────────────────────
        self.spawn_daily_reset();
        self.spawn_hourly_dashboard();
        self.spawn_advisor_task();
        self.spawn_rescan_task();

        // ── Main tick loop ────────────────────────────────────────
        self.run_main_loop().await;
        Ok(())
    }

    // ── Main 30-second loop ───────────────────────────────────────

    async fn run_main_loop(self: &Arc<Self>) {
        let sig_cfg = EngineConfig {
            min_confidence:  self.config.min_confidence,
            min_risk_reward: self.config.min_risk_reward,
            max_signals:     2,
        };
        let mut tick = interval(Duration::from_secs(30));

        loop {
            tick.tick().await;

            // ── 1. Risk gate ──────────────────────────────────────
            if let Err(e) = self.risk.pre_trade_check().await {
                warn!(error=%e, "Risk gate blocked");
                self.tg.risk_halt(&e.to_string()).await;
                // Auto-reset circuit breaker after cool-down
                tokio::time::sleep(Duration::from_secs(300)).await;
                self.risk.reset_breaker().await;
                continue;
            }

            let symbols = self.watchlist.read().await.clone();
            if symbols.is_empty() { continue; }

            // ── 2. Batch-fetch current prices ─────────────────────
            let prices: HashMap<Symbol, f64> =
                match self.exchange.prices(&symbols).await {
                    Ok(map) => map.into_iter()
                        .map(|(sym, p)| (sym, p.to_f64()))
                        .collect(),
                    Err(e) => {
                        warn!(error=%e, "Price fetch failed");
                        continue;
                    }
                };

            // ── 3. Evaluate open paper trades ─────────────────────
            self.learner.evaluate_open(&prices).await;

            // ── 4. Per-symbol analysis ────────────────────────────
            let capital = self.db.capital().await;
            let metrics = self.db.metrics().await;
            let kelly   = metrics.kelly_fraction.max(0.02);
            let size    = self.risk.position_size().await;

            for symbol in &symbols {
                // Fetch 15-min candles
                let candles = match self.exchange
                    .klines(symbol, Interval::FifteenMinutes, 100)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => { warn!(error=%e, sym=%symbol, "Klines failed"); continue; }
                };

                // Generate signals
                let sigs = signals::evaluate(
                    symbol.as_str(), &candles, capital, kelly, &sig_cfg,
                );

                for sig in sigs {
                    info!(
                        sym       = %sig.symbol,
                        direction = %sig.direction,
                        kind      = %sig.kind,
                        conf      = sig.confidence,
                        rr        = sig.risk_reward,
                        "Signal generated"
                    );

                    // Telegram signal card
                    self.tg.signal_card(&sig).await;

                    // Record in learning engine
                    if let Err(e) = self.learner.record(sig.clone()).await {
                        warn!(error=%e, "Failed to record signal");
                        continue;
                    }

                    // Live execution
                    if self.config.mode == Mode::Live {
                        self.execute_live(&sig, size).await;
                    }
                }
            }

            // ── 5. Status log ─────────────────────────────────────
            let m = self.db.metrics().await;
            info!(
                capital   = m.capital,
                trades    = m.total,
                wins      = m.wins,
                losses    = m.losses,
                win_rate  = format!("{:.1}%", m.win_rate * 100.0),
                open      = self.learner.open_count(),
                "Tick complete"
            );
        }
    }

    // ── Live order execution ──────────────────────────────────────

    async fn execute_live(&self, sig: &Signal, size: f64) {
        use bot_types::{OrderRequest, OrderSide};
        use rust_decimal::Decimal;

        let side = match sig.direction {
            Direction::Long  => OrderSide::Buy,
            Direction::Short => OrderSide::Sell,
        };
        let req = OrderRequest {
            symbol:    sig.symbol.clone(),
            side,
            quote_qty: Decimal::try_from(size).unwrap_or(Decimal::ONE),
        };
        match self.exchange.place_order(req).await {
            Ok(r)  => info!(order_id=%r.order_id, "Live order placed"),
            Err(e) => error!(error=%e, "Live order failed"),
        }
    }

    // ── Watchlist refresh ─────────────────────────────────────────

    async fn refresh_watchlist(&self) {
        let coins = self.scanner
            .top_coins(self.config.max_symbols, self.config.min_volume_usd)
            .await;

        let syms: Vec<Symbol> = coins.iter().map(|c| c.symbol.clone()).collect();
        self.tg.scan_report(&coins).await;
        info!(symbols = ?syms.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
              "Watchlist refreshed");
        *self.watchlist.write().await = syms;
    }

    // ── Background tasks ──────────────────────────────────────────

    fn spawn_daily_reset(self: &Arc<Self>) {
        let db = self.db.clone();
        tokio::spawn(async move {
            let mut iv = interval(Duration::from_secs(86_400));
            loop {
                iv.tick().await;
                if let Err(e) = db.reset_daily().await {
                    warn!(error=%e, "Daily PnL reset failed");
                } else {
                    info!("Daily PnL reset");
                }
            }
        });
    }

    fn spawn_hourly_dashboard(self: &Arc<Self>) {
        let db = self.db.clone();
        let tg = self.tg.clone();
        tokio::spawn(async move {
            let mut iv = interval(Duration::from_secs(3_600));
            loop {
                iv.tick().await;
                let m = db.metrics().await;
                tg.dashboard(&m).await;
                info!(grade=%m.grade(), capital=m.capital, "Dashboard sent");
            }
        });
    }

    fn spawn_advisor_task(self: &Arc<Self>) {
        let advisor = self.advisor.clone();
        tokio::spawn(async move {
            // First report after 30 minutes (some data needed)
            tokio::time::sleep(Duration::from_secs(1_800)).await;
            let mut iv = interval(Duration::from_secs(21_600));
            loop {
                iv.tick().await;
                advisor.run().await;
            }
        });
    }

    fn spawn_rescan_task(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            // First rescan after 4 hours
            let mut iv = interval(Duration::from_secs(14_400));
            iv.tick().await; // skip first tick (already scanned at startup)
            loop {
                iv.tick().await;
                this.refresh_watchlist().await;
            }
        });
    }
}
