//! # bot_scanner
//!
//! Fetches every actively-trading USDT pair, scores each one
//! using volume, volatility, trend alignment, and SMC quality,
//! and returns the top N candidates for signal evaluation.
//!
//! Scoring runs concurrently via a `Semaphore`-gated task pool
//! so the full scan completes in seconds not minutes.

use bot_exchange::Exchange;
use bot_indicators as ind;
use bot_smc as smc;
use bot_types::{CoinScore, Interval, Symbol};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};

const CONCURRENT_KLINE_FETCHES: usize = 12;
const MIN_CANDLES_FOR_SCORE: usize = 55;

pub struct CoinScanner {
    exchange: Arc<dyn Exchange>,
}

impl CoinScanner {
    pub fn new(exchange: Arc<dyn Exchange>) -> Self {
        Self { exchange }
    }

    /// Scan all USDT pairs and return the top `n` scored coins.
    ///
    /// Coins are filtered by `min_volume_usd` (24-h quote volume)
    /// before scoring to avoid wasting API calls on micro-caps.
    pub async fn top_coins(
        &self,
        n: usize,
        min_volume_usd: f64,
    ) -> Vec<CoinScore> {
        info!("Scanner: fetching 24-h tickers …");
        let tickers = match self.exchange.tickers_24h().await {
            Ok(t) => t,
            Err(e) => { warn!(error=%e, "Failed to fetch tickers"); return vec![]; }
        };

        // Pre-filter
        let candidates: Vec<_> = tickers
            .into_iter()
            .filter(|t| {
                use rust_decimal::prelude::ToPrimitive;
                t.quote_volume.to_f64().unwrap_or(0.0) >= min_volume_usd
                    && !t.symbol.as_str().contains("UP")
                    && !t.symbol.as_str().contains("DOWN")
                    && !t.symbol.as_str().contains("BULL")
                    && !t.symbol.as_str().contains("BEAR")
            })
            .collect();

        info!("Scanner: {} candidates after volume filter", candidates.len());

        let sem      = Arc::new(Semaphore::new(CONCURRENT_KLINE_FETCHES));
        let exchange = self.exchange.clone();
        let mut handles = Vec::with_capacity(candidates.len());

        for ticker in candidates {
            let sem      = sem.clone();
            let exchange = exchange.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.ok()?;
                let candles = exchange
                    .klines(&ticker.symbol, Interval::FifteenMinutes, 100)
                    .await
                    .ok()?;

                if candles.len() < MIN_CANDLES_FOR_SCORE {
                    return None;
                }

                let score = score_coin(&ticker.symbol, &candles, &ticker);
                Some(score)
            }));
        }

        let mut scores: Vec<CoinScore> = Vec::new();
        for h in handles {
            if let Ok(Some(s)) = h.await {
                scores.push(s);
            }
        }

        scores.sort_by(|a, b| {
            b.composite.partial_cmp(&a.composite).unwrap_or(std::cmp::Ordering::Equal)
        });
        let top: Vec<CoinScore> = scores.into_iter().take(n).collect();

        info!(
            "Scanner: top {} → {}",
            top.len(),
            top.iter()
                .map(|c| c.symbol.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        top
    }
}

// ── Scoring logic ─────────────────────────────────────────────────

fn score_coin(
    symbol:  &Symbol,
    candles: &[bot_types::Candle],
    ticker:  &bot_exchange::Ticker24h,
) -> CoinScore {
    use rust_decimal::prelude::ToPrimitive;

    let closes: Vec<f64> = ind::closes(candles);
    let price_f  = *closes.last().unwrap_or(&0.0);

    // ATR-based volatility (% of price)
    let atr_v     = ind::atr(candles, 14).unwrap_or(price_f * 0.01);
    let volatility = if price_f > 0.0 { atr_v / price_f * 100.0 } else { 0.0 };

    // EMA trend alignment
    let e20 = ind::ema_last(&closes, 20).unwrap_or(price_f);
    let e50 = ind::ema_last(&closes, 50).unwrap_or(price_f);
    let trend_bullish = e20 > e50 && price_f > e20;

    // SMC quality
    let current_price = candles.last().map(|c| c.close).unwrap_or_default();
    let smc_ctx = smc::analyse(candles, current_price);

    // Volume score: log-normalised against a $1B benchmark
    let vol_24h = ticker.quote_volume.to_f64().unwrap_or(0.0);
    let vol_score = (vol_24h.log10().max(0.0) / 9.0).min(1.0);

    // Volatility score: 5 % ATR = maximum useful volatility
    let vola_score = (volatility / 5.0).min(1.0);

    // Trend score: binary
    let trend_score = if trend_bullish { 1.0 } else { 0.0 };

    // Composite: volume 30% + volatility 20% + trend 20% + SMC 30%
    let composite = vol_score  * 0.30
                  + vola_score * 0.20
                  + trend_score * 0.20
                  + smc_ctx.score * 0.30;

    CoinScore {
        symbol:        symbol.clone(),
        composite,
        volume_24h:    vol_24h,
        change_24h:    ticker.price_change_pct,
        volatility,
        smc_score:     smc_ctx.score,
        trend_bullish,
    }
}
