//! # bot_smc
//!
//! Smart Money Concept detectors.  All functions are pure
//! (stateless, no I/O) and fully unit-tested.

use bot_types::{Candle, Direction, Price};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;

// ─────────────────────────────────────────────────────────────────
// Order Block
// ─────────────────────────────────────────────────────────────────

/// The last opposing candle before a significant impulsive move.
///
/// * **Bullish OB**: bearish candle immediately before a rally of ≥ 1.5 %.
/// * **Bearish OB**: bullish candle immediately before a sell-off of ≥ 1.5 %.
#[derive(Debug, Clone)]
pub struct OrderBlock {
    pub high:     Price,
    pub low:      Price,
    pub dir:      Direction,
    /// Body size as % of candle range (0–100). Higher = stronger.
    pub strength: f64,
}

impl OrderBlock {
    /// Is `price` currently inside (or within `tolerance_pct` of) this block?
    pub fn contains(&self, price: Price, tolerance_pct: f64) -> bool {
        let tol = Decimal::try_from(tolerance_pct / 100.0).unwrap_or_default();
        let low  = self.low.0  * (Decimal::ONE - tol);
        let high = self.high.0 * (Decimal::ONE + tol);
        price.0 >= low && price.0 <= high
    }
}

pub fn find_order_blocks(candles: &[Candle]) -> Vec<OrderBlock> {
    if candles.len() < 6 { return vec![]; }
    let mut out = Vec::new();
    let n = candles.len();

    for i in 1..n.saturating_sub(3) {
        let c   = &candles[i];
        let rng = c.high.0 - c.low.0;
        if rng.is_zero() { continue; }
        let str_pct = ((c.close.0 - c.open.0).abs() / rng * Decimal::from(100))
            .to_f64()
            .unwrap_or(0.0);

        // Bullish OB: bearish candle before a ≥ 1.5 % up-move
        if c.close.0 < c.open.0 {
            let threshold = c.open.0 * Decimal::try_from(1.015).unwrap();
            if candles[i + 1..].iter().take(4).any(|k| k.close.0 > threshold) {
                out.push(OrderBlock { high: c.open, low: c.low, dir: Direction::Long, strength: str_pct });
            }
        }
        // Bearish OB: bullish candle before a ≥ 1.5 % down-move
        if c.close.0 > c.open.0 {
            let threshold = c.open.0 * Decimal::try_from(0.985).unwrap();
            if candles[i + 1..].iter().take(4).any(|k| k.close.0 < threshold) {
                out.push(OrderBlock { high: c.high, low: c.open, dir: Direction::Short, strength: str_pct });
            }
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────
// Fair Value Gap
// ─────────────────────────────────────────────────────────────────

/// A 3-candle price imbalance where the market moved too fast to
/// leave overlapping wicks.
#[derive(Debug, Clone)]
pub struct FairValueGap {
    pub high: Price,
    pub low:  Price,
    pub dir:  Direction,
}

impl FairValueGap {
    pub fn contains(&self, price: Price) -> bool {
        price.0 >= self.low.0 && price.0 <= self.high.0
    }
}

pub fn find_fvgs(candles: &[Candle]) -> Vec<FairValueGap> {
    if candles.len() < 3 { return vec![]; }
    candles
        .windows(3)
        .filter_map(|w| {
            // Bullish FVG: candle[2].low > candle[0].high
            if w[2].low.0 > w[0].high.0 {
                return Some(FairValueGap {
                    high: w[2].low,
                    low:  w[0].high,
                    dir:  Direction::Long,
                });
            }
            // Bearish FVG: candle[2].high < candle[0].low
            if w[2].high.0 < w[0].low.0 {
                return Some(FairValueGap {
                    high: w[0].low,
                    low:  w[2].high,
                    dir:  Direction::Short,
                });
            }
            None
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────
// Break of Structure
// ─────────────────────────────────────────────────────────────────

/// A confirmed close above the previous swing high (bullish BOS) or
/// below the previous swing low (bearish BOS).
pub fn detect_bos(candles: &[Candle]) -> Option<Direction> {
    if candles.len() < 20 { return None; }
    let n       = candles.len();
    let lookback = &candles[n - 20..n - 1]; // exclude current candle
    let current  = &candles[n - 1];

    let swing_high = lookback.iter().map(|c| c.high.0).max()?;
    let swing_low  = lookback.iter().map(|c| c.low.0).min()?;

    if current.close.0 > swing_high { return Some(Direction::Long);  }
    if current.close.0 < swing_low  { return Some(Direction::Short); }
    None
}

// ─────────────────────────────────────────────────────────────────
// Change of Character
// ─────────────────────────────────────────────────────────────────

/// The *first* BOS that contradicts the previous 20-candle trend.
/// This signals a potential trend reversal rather than continuation.
pub fn detect_choch(candles: &[Candle]) -> Option<Direction> {
    if candles.len() < 40 { return None; }
    let n    = candles.len();
    let old  = &candles[n - 40..n - 20];
    let new  = &candles[n - 20..];

    let old_open  = old.first()?.open.0;
    let old_close = old.last()?.close.0;

    // Previous trend bearish → look for bullish CHoCH
    if old_close < old_open {
        let prev_high = old.iter().map(|c| c.high.0).max()?;
        if new.last()?.close.0 > prev_high { return Some(Direction::Long); }
    }
    // Previous trend bullish → look for bearish CHoCH
    if old_close > old_open {
        let prev_low = old.iter().map(|c| c.low.0).min()?;
        if new.last()?.close.0 < prev_low { return Some(Direction::Short); }
    }
    None
}

// ─────────────────────────────────────────────────────────────────
// Liquidity Sweep
// ─────────────────────────────────────────────────────────────────

/// A wick through a key level followed by a close back inside.
/// Indicates stop-loss hunting by institutional players, usually
/// followed by a sharp reversal.
pub fn detect_liq_sweep(candles: &[Candle]) -> Option<Direction> {
    if candles.len() < 15 { return None; }
    let n        = candles.len();
    let lookback = &candles[n - 15..n - 2];
    let last     = &candles[n - 1];

    let key_high = lookback.iter().map(|c| c.high.0).max()?;
    let key_low  = lookback.iter().map(|c| c.low.0).min()?;

    // Wick above key high, close back below → bearish sweep of buy-stops
    if last.high.0 > key_high && last.close.0 < key_high {
        return Some(Direction::Short);
    }
    // Wick below key low, close back above → bullish sweep of sell-stops
    if last.low.0 < key_low && last.close.0 > key_low {
        return Some(Direction::Long);
    }
    None
}

// ─────────────────────────────────────────────────────────────────
// Composite SMC score
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SmcContext {
    pub score:         f64,          // 0.0–1.0
    pub reasons:       Vec<String>,
    pub order_blocks:  Vec<OrderBlock>,
    pub fvgs:          Vec<FairValueGap>,
    pub bos:           Option<Direction>,
    pub choch:         Option<Direction>,
    pub liq_sweep:     Option<Direction>,
}

/// Runs all SMC detectors and returns a composite context.
pub fn analyse(candles: &[Candle], price: Price) -> SmcContext {
    if candles.len() < 40 {
        return SmcContext::default();
    }
    let mut ctx = SmcContext {
        order_blocks: find_order_blocks(candles),
        fvgs:         find_fvgs(candles),
        bos:          detect_bos(candles),
        choch:        detect_choch(candles),
        liq_sweep:    detect_liq_sweep(candles),
        ..Default::default()
    };

    let mut score = 0.0_f64;

    // OB within 2.5 % of price
    for ob in &ctx.order_blocks {
        if ob.contains(price, 2.5) {
            score += 0.25;
            ctx.reasons.push(format!(
                "{:?} Order Block ${:.4}–${:.4} (strength {:.0}%)",
                ob.dir, ob.low, ob.high, ob.strength
            ));
        }
    }
    // FVG containing price
    for fvg in &ctx.fvgs {
        if fvg.contains(price) {
            score += 0.20;
            ctx.reasons.push(format!(
                "{:?} FVG ${:.4}–${:.4}",
                fvg.dir, fvg.low, fvg.high
            ));
        }
    }
    if let Some(ref dir) = ctx.bos {
        score += 0.20;
        ctx.reasons.push(format!("{dir:?} Break of Structure"));
    }
    if let Some(ref dir) = ctx.choch {
        score += 0.25;
        ctx.reasons.push(format!("{dir:?} Change of Character"));
    }
    if let Some(ref dir) = ctx.liq_sweep {
        score += 0.30;
        ctx.reasons.push(format!("{dir:?} Liquidity Sweep"));
    }
    ctx.score = score.min(1.0);
    ctx
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bot_types::{Quantity, Symbol};
    use rust_decimal::Decimal;

    fn candle(open: f64, high: f64, low: f64, close: f64) -> Candle {
        let p = |v: f64| Price::new(Decimal::try_from(v).unwrap());
        Candle {
            symbol:      Symbol::new("TEST"),
            open_time:   chrono::Utc::now(),
            open:        p(open),
            high:        p(high),
            low:         p(low),
            close:       p(close),
            volume:      Quantity::new(Decimal::ONE),
            quote_volume: Decimal::ONE,
            taker_buy_volume: Decimal::ONE,
            is_closed:   true,
        }
    }

    #[test]
    fn bullish_fvg_detected() {
        // c0.high=100, c2.low=105 → gap [100,105]
        let candles = vec![
            candle(95.0, 100.0, 90.0, 98.0),
            candle(101.0, 103.0, 99.0, 102.0),
            candle(104.0, 110.0, 105.0, 108.0),
        ];
        let fvgs = find_fvgs(&candles);
        assert_eq!(fvgs.len(), 1);
        assert_eq!(fvgs[0].dir, Direction::Long);
        assert_eq!(fvgs[0].low.0,  Decimal::try_from(100.0).unwrap());
        assert_eq!(fvgs[0].high.0, Decimal::try_from(105.0).unwrap());
    }

    #[test]
    fn bearish_fvg_detected() {
        let candles = vec![
            candle(110.0, 115.0, 105.0, 106.0),
            candle(104.0, 105.0, 100.0, 101.0),
            candle(99.0,  100.0,  90.0,  92.0),
        ];
        let fvgs = find_fvgs(&candles);
        assert_eq!(fvgs.len(), 1);
        assert_eq!(fvgs[0].dir, Direction::Short);
    }

    #[test]
    fn fvg_no_gap_returns_empty() {
        // Overlapping wicks → no FVG
        let candles = vec![
            candle(95.0, 102.0, 90.0, 100.0),
            candle(99.0, 105.0, 97.0, 103.0),
            candle(100.0, 108.0, 98.0, 106.0),
        ];
        assert!(find_fvgs(&candles).is_empty());
    }

    #[test]
    fn order_block_contains() {
        let ob = OrderBlock {
            high:     Price::new(Decimal::try_from(105.0).unwrap()),
            low:      Price::new(Decimal::try_from(100.0).unwrap()),
            dir:      Direction::Long,
            strength: 70.0,
        };
        assert!(ob.contains(Price::new(Decimal::try_from(102.0).unwrap()), 0.0));
        assert!(!ob.contains(Price::new(Decimal::try_from(110.0).unwrap()), 0.0));
        // With tolerance
        assert!(ob.contains(Price::new(Decimal::try_from(107.0).unwrap()), 2.0));
    }

    #[test]
    fn bos_returns_none_on_short_input() {
        let candles: Vec<Candle> = (0..10).map(|i| candle(i as f64, i as f64 + 1.0, i as f64 - 1.0, i as f64)).collect();
        assert!(detect_bos(&candles).is_none());
    }

    #[test]
    fn liq_sweep_bullish() {
        // Build 15 flat candles then a wick-below-then-close-above candle
        let mut candles: Vec<Candle> = (0..14).map(|_| candle(100.0, 102.0, 98.0, 100.0)).collect();
        // Last candle: wick to 95 (below key_low=98), close at 101
        candles.push(candle(100.0, 102.0, 95.0, 101.0));
        assert_eq!(detect_liq_sweep(&candles), Some(Direction::Long));
    }
}
