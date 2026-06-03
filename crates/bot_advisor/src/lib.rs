//! # bot_advisor
//!
//! Queries the database for per-setup and per-symbol performance,
//! compares against known benchmarks, and sends a prioritised list
//! of **concrete, code-level improvements** to Telegram.
//!
//! Runs every 6 hours so the operator always knows exactly what
//! to change to increase profitability.

use bot_db::{SetupStat, Store, SymbolStat};
use bot_telegram::TelegramClient;
use bot_types::Metrics;
use std::sync::Arc;
use tracing::info;

// ── Priority levels ───────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum P { Critical = 1, Important = 2, Info = 3 }

impl P {
    fn badge(self) -> &'static str {
        match self {
            P::Critical  => "🔴 CRITICAL",
            P::Important => "🟡 IMPORTANT",
            P::Info      => "🟢 ENHANCE",
        }
    }
}

struct Tip {
    priority: P,
    tag:      &'static str,
    body:     String,
}

// ── Advisor ───────────────────────────────────────────────────────

pub struct Advisor {
    db: Arc<dyn Store>,
    tg: Arc<TelegramClient>,
}

impl Advisor {
    pub fn new(db: Arc<dyn Store>, tg: Arc<TelegramClient>) -> Self {
        Self { db, tg }
    }

    pub async fn run(&self) {
        let m      = self.db.metrics().await;
        let setups = self.db.setup_stats().await;
        let syms   = self.db.symbol_stats().await;

        let tips = build_tips(&m, &setups, &syms);
        send_report(&self.tg, &m, &tips).await;
        info!(tips = tips.len(), "Advisor report sent");
    }
}

// ── Tip generation ────────────────────────────────────────────────

fn build_tips(m: &Metrics, setups: &[SetupStat], syms: &[SymbolStat]) -> Vec<Tip> {
    let mut tips: Vec<Tip> = Vec::new();

    // ── A. Data volume ────────────────────────────────────────────
    if m.total < 10 {
        tips.push(Tip {
            priority: P::Critical,
            tag: "DATA",
            body: format!(
                "Only {n} trades recorded. Need ≥50 for reliable statistics. \
                 Keep paper mode running — do not switch to live yet.",
                n = m.total
            ),
        });
    } else if m.total < 30 {
        tips.push(Tip {
            priority: P::Important,
            tag: "DATA",
            body: format!("{n}/50 trades complete. Results will stabilise around 50.", n = m.total),
        });
    }

    // ── B. Win rate ───────────────────────────────────────────────
    if m.total >= 10 {
        if m.win_rate < 0.42 {
            tips.push(Tip {
                priority: P::Critical,
                tag: "WIN RATE",
                body: format!(
                    "Win rate {:.1}% is critically low. \
                     Raise `min_confidence` from 0.76 → 0.83 in your .env. \
                     Also add a 4H EMA trend filter: skip Long signals when \
                     4H EMA20 < EMA50.",
                    m.win_rate * 100.0
                ),
            });
        } else if m.win_rate < 0.50 {
            tips.push(Tip {
                priority: P::Important,
                tag: "WIN RATE",
                body: format!(
                    "Win rate {:.1}% is below breakeven. \
                     Raise `min_risk_reward` from 2.0 → 2.5 in your .env \
                     to filter out lower-quality setups.",
                    m.win_rate * 100.0
                ),
            });
        } else if m.win_rate >= 0.62 {
            tips.push(Tip {
                priority: P::Info,
                tag: "WIN RATE",
                body: format!(
                    "Win rate {:.1}% is excellent. Consider lowering \
                     `min_confidence` by 0.02 to capture more signals \
                     without meaningfully hurting quality.",
                    m.win_rate * 100.0
                ),
            });
        }
    }

    // ── C. Profit factor & exits ──────────────────────────────────
    if m.total >= 10 {
        if m.profit_factor < 1.0 {
            tips.push(Tip {
                priority: P::Critical,
                tag: "EXITS",
                body: format!(
                    "Profit factor {:.2} — bot is net losing. \
                     Avg win ${:.4} vs avg loss ${:.4}. \
                     Widen TP: change `atr * 3.2` → `atr * 4.0` in \
                     bot_signals/src/lib.rs for all Long setups.",
                    m.profit_factor, m.avg_win_usd, m.avg_loss_usd
                ),
            });
        } else if m.profit_factor < 1.4 {
            tips.push(Tip {
                priority: P::Important,
                tag: "EXITS",
                body: format!(
                    "Profit factor {:.2} is marginal (need ≥1.5). \
                     Implement a breakeven stop: once price moves +1R \
                     in your favour, move SL to entry. \
                     Add this logic to `LearningEngine::evaluate_open`.",
                    m.profit_factor
                ),
            });
        }
        if m.avg_loss_usd > m.avg_win_usd * 0.80 {
            tips.push(Tip {
                priority: P::Important,
                tag: "SL SIZE",
                body: format!(
                    "Avg loss ${:.4} is too close to avg win ${:.4}. \
                     Your SL multiplier may be too wide. \
                     Try changing `sl_mult` from 1.5 → 1.0 × ATR \
                     and `tp_mult` from 3.0 → 3.5 × ATR.",
                    m.avg_loss_usd, m.avg_win_usd
                ),
            });
        }
    }

    // ── D. Drawdown ───────────────────────────────────────────────
    if m.max_drawdown > 0.12 {
        tips.push(Tip {
            priority: P::Critical,
            tag: "DRAWDOWN",
            body: format!(
                "Max drawdown {:.1}% is dangerously high. \
                 Set MAX_DAILY_LOSS_PCT=0.03 and MAX_TRADE_SIZE_PCT=0.05 \
                 in your .env immediately.",
                m.max_drawdown * 100.0
            ),
        });
    } else if m.max_drawdown > 0.07 {
        tips.push(Tip {
            priority: P::Important,
            tag: "DRAWDOWN",
            body: format!(
                "Drawdown {:.1}% is elevated. Add a consecutive-loss \
                 breaker: pause 2 hours after 3 losses in a row. \
                 Track `consecutive_losses: u32` in RiskManager.",
                m.max_drawdown * 100.0
            ),
        });
    }

    // ── E. Kelly sizing ───────────────────────────────────────────
    if m.total >= 20 {
        if m.kelly_fraction < 0.02 {
            tips.push(Tip {
                priority: P::Important,
                tag: "KELLY",
                body: "Kelly fraction < 2% — edge is too thin to size up. \
                       Focus on improving win-rate first. Do NOT increase \
                       KELLY_FRACTION manually until WR ≥ 54%.".to_owned(),
            });
        } else if m.kelly_fraction >= 0.08 {
            tips.push(Tip {
                priority: P::Info,
                tag: "KELLY",
                body: format!(
                    "Kelly fraction {:.1}% is strong. You are compounding \
                     near-optimally. Keep KELLY_FRACTION=0.25 (quarter-Kelly) \
                     for safety — this is already the right setting.",
                    m.kelly_fraction * 100.0
                ),
            });
        }
    }

    // ── F. Per-setup analysis ─────────────────────────────────────
    for s in setups {
        if s.total < 5 { continue; }
        let wr = s.win_rate();
        if wr < 0.38 {
            tips.push(Tip {
                priority: P::Important,
                tag: "SETUP",
                body: format!(
                    "'{}' win rate is only {:.0}% ({}/{} trades, PnL ${:.4}). \
                     Disable it: add `&& s.kind != SetupKind::{}` to the \
                     `signals.retain` filter in bot_signals/src/lib.rs.",
                    s.kind, wr * 100.0, s.wins, s.total, s.total_pnl,
                    s.kind.replace(' ', "")
                ),
            });
        } else if wr > 0.68 && s.total >= 8 {
            tips.push(Tip {
                priority: P::Info,
                tag: "SETUP",
                body: format!(
                    "'{}' win rate is {:.0}% — your best setup! \
                     Lower its min_confidence by 0.03 to get more of these.",
                    s.kind, wr * 100.0
                ),
            });
        }
    }

    // ── G. Per-symbol analysis ────────────────────────────────────
    let mut bad: Vec<&SymbolStat> = syms.iter()
        .filter(|s| s.total >= 4 && s.win_rate() < 0.38)
        .collect();
    bad.sort_by(|a, b| a.win_rate().partial_cmp(&b.win_rate()).unwrap());

    if !bad.is_empty() {
        let list = bad.iter().take(4)
            .map(|s| format!("{} ({:.0}%WR)", s.symbol.to_uppercase(), s.win_rate() * 100.0))
            .collect::<Vec<_>>().join(", ");
        tips.push(Tip {
            priority: P::Important,
            tag: "COINS",
            body: format!(
                "Underperformers: {list}. Remove them from watchlist or add a \
                 per-symbol min-confidence override of 0.88 in the scanner."
            ),
        });
    }

    let best: Vec<&SymbolStat> = syms.iter()
        .filter(|s| s.total >= 4 && s.win_rate() >= 0.65)
        .collect();
    if !best.is_empty() {
        let list = best.iter().take(4)
            .map(|s| format!("{} ({:.0}%WR, +${:.3})", s.symbol.to_uppercase(),
                s.win_rate() * 100.0, s.total_pnl))
            .collect::<Vec<_>>().join(", ");
        tips.push(Tip {
            priority: P::Info,
            tag: "COINS",
            body: format!(
                "Top performers: {list}. Prioritise these — lower their \
                 min_composite_score in the scanner by 0.05."
            ),
        });
    }

    // ── H. Structural upgrades (always present) ───────────────────
    tips.push(Tip {
        priority: P::Info,
        tag: "UPGRADE",
        body: "Add a 4H trend gate: only take Long signals when 4H EMA20 > EMA50. \
               Fetch 4H candles for each symbol in the main loop and pass a \
               `trend_bullish_4h: bool` flag into `signals::evaluate()`.".to_owned(),
    });
    tips.push(Tip {
        priority: P::Info,
        tag: "UPGRADE",
        body: "Add session filter: best crypto moves occur during London (08–12 UTC) \
               and New York (13–20 UTC) opens. Add \
               `let h = Utc::now().hour(); if !(8..=20).contains(&h) { continue; }` \
               at the top of the main loop.".to_owned(),
    });
    tips.push(Tip {
        priority: P::Info,
        tag: "UPGRADE",
        body: "Add partial TP: close 40% of position at 1:1 R:R, then move SL to \
               breakeven. This converts near-misses into small wins and boosts WR \
               without changing your signal logic.".to_owned(),
    });
    tips.push(Tip {
        priority: P::Info,
        tag: "UPGRADE",
        body: "Add a correlation guard: never hold Long BTC + Long ETH simultaneously. \
               Track `active_longs: HashSet<Symbol>` in the Orchestrator and skip \
               correlated pairs when one is already open.".to_owned(),
    });

    // Sort: Critical first, then Important, then Info
    tips.sort_by_key(|t| t.priority);
    tips
}

// ── Report formatting ─────────────────────────────────────────────

async fn send_report(tg: &TelegramClient, m: &Metrics, tips: &[Tip]) {
    let header = format!(
        "🤖 <b>SELF-IMPROVEMENT REPORT</b>\n\
         Grade: <b>{grade}</b> | Trades: {total} | Capital: ${cap:.4}\n\
         ══════════════════════════════════════",
        grade = m.grade(),
        total = m.total,
        cap   = m.capital,
    );

    let body = tips.iter()
        .take(12)
        .enumerate()
        .map(|(i, t)| format!(
            "\n{badge} [<b>{tag}</b>] #{n}\n  ➤ {body}",
            badge = t.priority.badge(),
            tag   = t.tag,
            n     = i + 1,
            body  = t.body,
        ))
        .collect::<Vec<_>>()
        .join("\n");

    let footer = format!(
        "\n══════════════════════════════════════\n\
         📈 WR:{wr:.1}%  PF:{pf:.2}×  DD:{dd:.1}%  Kelly:{k:.1}%\n\
         🔄 Next analysis in 6 hours",
        wr = m.win_rate     * 100.0,
        pf = m.profit_factor,
        dd = m.max_drawdown * 100.0,
        k  = m.kelly_fraction * 100.0,
    );

    tg.send(&format!("{header}{body}{footer}")).await;
}
