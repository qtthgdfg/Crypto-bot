//! # bot_signals
//!
//! Stateless signal-generation engine.  `SignalEngine::evaluate`
//! is a pure function: given a slice of candles and configuration
//! it returns zero or more `Signal` structs — no I/O, no side
//! effects, fully unit-testable.

use bot_indicators as ind;
use bot_smc as smc;
use bot_types::{Candle, Direction, Price, SetupKind, Signal};
use chrono::Utc;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use uuid::Uuid;

// ── Configuration for the engine ─────────────────────────────────

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub min_confidence:  f64,
    pub min_risk_reward: f64,
    pub max_signals:     usize, // per call
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { min_confidence: 0.76, min_risk_reward: 2.0, max_signals: 2 }
    }
}

// ── Public entry-point ────────────────────────────────────────────

/// Evaluate all setups for `symbol` and return qualifying signals.
///
/// `capital` and `kelly_f` are used to compute `size_usd`.
pub fn evaluate(
    symbol:  &str,
    candles: &[Candle],
    capital: f64,
    kelly_f: f64,
    cfg:     &EngineConfig,
) -> Vec<Signal> {
    if candles.len() < 55 { return vec![]; }

    let closes: Vec<f64> = ind::closes(candles);
    let price_f  = *closes.last().unwrap();
    let price    = candles.last().unwrap().close;
    let size_usd = Decimal::try_from((capital * kelly_f).max(1.0).min(capital * 0.10))
        .unwrap_or(Decimal::ONE);

    // ── Indicators ────────────────────────────────────────────────
    let rsi   = ind::rsi(&closes, 14).unwrap_or(50.0);
    let macd  = ind::macd(&closes).ok();
    let atr   = ind::atr(candles, 14).unwrap_or(price_f * 0.01);
    let vwap  = ind::vwap(candles).unwrap_or(price_f);
    let e20   = ind::ema_last(&closes, 20).unwrap_or(price_f);
    let e50   = ind::ema_last(&closes, 50).unwrap_or(price_f);
    let rdiv  = ind::rsi_divergence(candles, 14).ok().flatten();

    // ── SMC context ───────────────────────────────────────────────
    let smc_ctx = smc::analyse(candles, price);

    let mut signals: Vec<Signal> = Vec::new();

    // ─────────────────────────────────────────────────────────────
    // SETUP 1 — Order Block Bounce + RSI Confirmation
    // ─────────────────────────────────────────────────────────────
    for ob in &smc_ctx.order_blocks {
        match ob.dir {
            Direction::Long if ob.contains(price, 0.6) && rsi < 42.0 => {
                let tp  = price.0 + fdec(atr * 3.2);
                let sl  = ob.low.0  - fdec(atr * 0.5);
                let rr  = risk_reward(price.0, tp, sl);
                if rr >= cfg.min_risk_reward {
                    let conf = 0.76 + ob.strength.min(30.0) / 300.0
                        + if smc_ctx.score > 0.4 { 0.08 } else { 0.0 };
                    let mut reasons = smc_ctx.reasons.clone();
                    reasons.push(format!("RSI oversold {rsi:.1}"));
                    reasons.push(format!("VWAP {vwap:.4}"));
                    reasons.push(format!("OB strength {:.0}%", ob.strength));
                    signals.push(build(symbol, Direction::Long, SetupKind::OrderBlockBounce,
                        price.0, tp, sl, rr, conf.min(0.96), reasons, size_usd));
                }
            }
            Direction::Short if ob.contains(price, 0.6) && rsi > 58.0 => {
                let tp  = price.0 - fdec(atr * 3.2);
                let sl  = ob.high.0 + fdec(atr * 0.5);
                let rr  = risk_reward(price.0, tp, sl);
                if rr >= cfg.min_risk_reward {
                    let conf = 0.76 + ob.strength.min(30.0) / 300.0
                        + if smc_ctx.score > 0.4 { 0.08 } else { 0.0 };
                    let mut reasons = smc_ctx.reasons.clone();
                    reasons.push(format!("RSI overbought {rsi:.1}"));
                    signals.push(build(symbol, Direction::Short, SetupKind::OrderBlockBounce,
                        price.0, tp, sl, rr, conf.min(0.95), reasons, size_usd));
                }
            }
            _ => {}
        }
    }

    // ─────────────────────────────────────────────────────────────
    // SETUP 2 — Change of Character + MACD confirmation
    // ─────────────────────────────────────────────────────────────
    if let (Some(dir), Some(m)) = (&smc_ctx.choch, macd) {
        let macd_confirms = match dir {
            Direction::Long  => m.histogram > 0.0,
            Direction::Short => m.histogram < 0.0,
        };
        if macd_confirms {
            let (tp, sl) = tp_sl_symmetric(price.0, atr, 4.5, 1.5, dir);
            let rr = risk_reward(price.0, tp, sl);
            if rr >= cfg.min_risk_reward {
                let conf = 0.82 + if smc_ctx.score > 0.4 { 0.09 } else { 0.0 };
                let reasons = vec![
                    format!("{dir:?} Change of Character"),
                    format!("MACD histogram {:.6}", m.histogram),
                    format!("RSI {rsi:.1}"),
                    format!("SMC score {:.0}%", smc_ctx.score * 100.0),
                ];
                signals.push(build(symbol, dir.clone(), SetupKind::ChangeOfCharacter,
                    price.0, tp, sl, rr, conf.min(0.95), reasons, size_usd));
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // SETUP 3 — Break of Structure + EMA trend alignment
    // ─────────────────────────────────────────────────────────────
    if let Some(dir) = &smc_ctx.bos {
        let ema_aligned = match dir {
            Direction::Long  => e20 > e50 && price_f > e20,
            Direction::Short => e20 < e50 && price_f < e20,
        };
        if ema_aligned {
            let (tp, sl) = tp_sl_symmetric(price.0, atr, 3.5, 1.2, dir);
            let rr = risk_reward(price.0, tp, sl);
            if rr >= cfg.min_risk_reward {
                let conf = 0.79 + if smc_ctx.score > 0.3 { 0.10 } else { 0.0 };
                let reasons = vec![
                    format!("{dir:?} Break of Structure"),
                    format!("EMA20 {e20:.4} {} EMA50 {e50:.4}",
                        if e20 > e50 { ">" } else { "<" }),
                    format!("VWAP {vwap:.4}"),
                ];
                signals.push(build(symbol, dir.clone(), SetupKind::BreakOfStructure,
                    price.0, tp, sl, rr, conf.min(0.95), reasons, size_usd));
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // SETUP 4 — Liquidity Sweep Reversal
    // ─────────────────────────────────────────────────────────────
    if let Some(dir) = &smc_ctx.liq_sweep {
        let rsi_ok = match dir {
            Direction::Long  => rsi < 38.0,
            Direction::Short => rsi > 62.0,
        };
        if rsi_ok || smc_ctx.score > 0.5 {
            let (tp, sl) = tp_sl_symmetric(price.0, atr, 4.0, 1.0, dir);
            let rr = risk_reward(price.0, tp, sl);
            if rr >= cfg.min_risk_reward {
                let conf = 0.83 + if smc_ctx.score > 0.5 { 0.09 } else { 0.0 };
                let mut reasons = smc_ctx.reasons.clone();
                reasons.push(format!("RSI {rsi:.1}"));
                signals.push(build(symbol, dir.clone(), SetupKind::LiquiditySweepReversal,
                    price.0, tp, sl, rr, conf.min(0.95), reasons, size_usd));
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // SETUP 5 — RSI Divergence
    // ─────────────────────────────────────────────────────────────
    if let Some(dir) = rdiv {
        let (tp, sl) = tp_sl_symmetric(price.0, atr, 3.0, 1.5, &dir);
        let rr = risk_reward(price.0, tp, sl);
        if rr >= cfg.min_risk_reward {
            let conf = 0.77 + if smc_ctx.score > 0.3 { 0.08 } else { 0.0 };
            let reasons = vec![
                format!("{dir:?} RSI Divergence"),
                format!("RSI {rsi:.1}"),
                format!("VWAP {vwap:.4}"),
            ];
            signals.push(build(symbol, dir, SetupKind::RsiDivergence,
                price.0, tp, sl, rr, conf.min(0.95), reasons, size_usd));
        }
    }

    // ─────────────────────────────────────────────────────────────
    // SETUP 6 — Volume Breakout
    // ─────────────────────────────────────────────────────────────
    {
        let n = candles.len();
        let avg_vol: f64 = candles[n.saturating_sub(20)..n - 1]
            .iter()
            .map(|c| c.volume.0.to_f64().unwrap_or(0.0))
            .sum::<f64>() / 19.0;
        let cur_vol = candles[n - 1].volume.0.to_f64().unwrap_or(0.0);
        let spike   = if avg_vol > 0.0 { cur_vol / avg_vol } else { 1.0 };

        if spike >= 3.0 {
            let dir = if price_f > e20 { Direction::Long } else { Direction::Short };
            let (tp, sl) = tp_sl_symmetric(price.0, atr, 2.5, 1.0, &dir);
            let rr = risk_reward(price.0, tp, sl);
            if rr >= cfg.min_risk_reward {
                let conf = (0.75 + spike / 25.0).min(0.93);
                let reasons = vec![
                    format!("Volume spike {spike:.1}× average"),
                    format!("Price vs EMA20: {price_f:.4} vs {e20:.4}"),
                    format!("RSI {rsi:.1}"),
                ];
                signals.push(build(symbol, dir, SetupKind::VolumeBreakout,
                    price.0, tp, sl, rr, conf, reasons, size_usd));
            }
        }
    }

    // ── Filter + sort + cap ───────────────────────────────────────
    signals.retain(|s| s.confidence >= cfg.min_confidence && s.risk_reward >= cfg.min_risk_reward);
    signals.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    signals.truncate(cfg.max_signals);
    signals
}

// ── Helpers ───────────────────────────────────────────────────────

#[inline]
fn fdec(f: f64) -> Decimal {
    Decimal::try_from(f).unwrap_or_default()
}

fn tp_sl_symmetric(
    entry: Decimal,
    atr:   f64,
    tp_mult: f64,
    sl_mult: f64,
    dir: &Direction,
) -> (Decimal, Decimal) {
    match dir {
        Direction::Long  => (entry + fdec(atr * tp_mult), entry - fdec(atr * sl_mult)),
        Direction::Short => (entry - fdec(atr * tp_mult), entry + fdec(atr * sl_mult)),
    }
}

fn risk_reward(entry: Decimal, tp: Decimal, sl: Decimal) -> f64 {
    let reward = (tp - entry).abs();
    let risk   = (entry - sl).abs();
    if risk.is_zero() { return 0.0; }
    (reward / risk).to_f64().unwrap_or(0.0)
}

#[allow(clippy::too_many_arguments)]
fn build(
    symbol:  &str,
    dir:     Direction,
    kind:    SetupKind,
    entry:   Decimal,
    tp:      Decimal,
    sl:      Decimal,
    rr:      f64,
    conf:    f64,
    reasons: Vec<String>,
    size:    Decimal,
) -> Signal {
    let rr_val = risk_reward(entry, tp, sl);
    Signal {
        id:          Uuid::new_v4(),
        symbol:      bot_types::Symbol::new(symbol),
        direction:   dir,
        kind,
        entry:       Price::new(entry),
        take_profit: Price::new(tp),
        stop_loss:   Price::new(sl),
        risk_reward: rr.max(rr_val),
        confidence:  conf,
        size_usd:    size,
        reasons,
        created_at:  Utc::now(),
    }
}
