//! # bot_telegram
//!
//! Typed Telegram Bot API client.
//! Every message type has its own method so the rest of the
//! codebase never deals with raw HTML strings.

use bot_types::{Direction, Metrics, Outcome, Signal};
use reqwest::Client;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, warn};

const TELEGRAM_API: &str = "https://api.telegram.org";
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum TelegramError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Telegram API error: {0}")]
    Api(String),
}

// ── Client ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TelegramClient {
    token:   String,
    chat_id: String,
    client:  Client,
    enabled: bool,
}

impl TelegramClient {
    pub fn new(token: &str, chat_id: &str) -> Self {
        let enabled = !token.starts_with("YOUR") && !token.is_empty();
        Self {
            token:   token.to_owned(),
            chat_id: chat_id.to_owned(),
            client:  Client::builder()
                .timeout(SEND_TIMEOUT)
                .build()
                .expect("reqwest client"),
            enabled,
        }
    }

    // ── Raw send ──────────────────────────────────────────────────

    pub async fn send(&self, html: &str) {
        if !self.enabled { return; }
        let url = format!("{TELEGRAM_API}/bot{}/sendMessage", self.token);
        match self.client
            .post(&url)
            .form(&[
                ("chat_id",    self.chat_id.as_str()),
                ("text",       html),
                ("parse_mode", "HTML"),
            ])
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                debug!("Telegram message sent ({} chars)", html.len());
            }
            Ok(r) => warn!(status=%r.status(), "Telegram API rejected message"),
            Err(e) => warn!(error=%e, "Telegram send failed"),
        }
    }

    // ── Signal card ───────────────────────────────────────────────

    pub async fn signal_card(&self, s: &Signal) {
        let dir_label = match s.direction {
            Direction::Long  => "📈 LONG",
            Direction::Short => "📉 SHORT",
        };
        let fire = if s.confidence >= 0.88 { "🔥" } else { "⚡" };
        let msg = format!(
            "{fire} <b>SIGNAL — {id}</b>\n\
             ══════════════════════════\n\
             📌 Pair:        <b>{sym}</b>\n\
             🎯 Direction:   {dir}\n\
             🔎 Setup:       {kind}\n\
             ══════════════════════════\n\
             💰 Entry:       <code>${entry:.6}</code>\n\
             ✅ Take Profit: <code>${tp:.6}</code>  <i>+{reward:.2}%</i>\n\
             ❌ Stop Loss:   <code>${sl:.6}</code>  <i>-{risk:.2}%</i>\n\
             ⚖️  R:R:         <b>{rr:.1}×</b>\n\
             🎲 Confidence:  <b>{conf:.0}%</b>\n\
             ══════════════════════════\n\
             📋 Reasons:\n{reasons}\n\
             ══════════════════════════\n\
             💵 Size: <b>${size:.2}</b>",
            id      = &s.id.to_string()[..8],
            sym     = s.symbol,
            dir     = dir_label,
            kind    = s.kind,
            entry   = s.entry.to_f64(),
            tp      = s.take_profit.to_f64(),
            reward  = s.reward_pct(),
            sl      = s.stop_loss.to_f64(),
            risk    = s.risk_pct(),
            rr      = s.risk_reward,
            conf    = s.confidence * 100.0,
            reasons = s.reasons.iter()
                .enumerate()
                .map(|(i, r)| format!("  {}. {r}", i + 1))
                .collect::<Vec<_>>()
                .join("\n"),
            size    = s.size_usd,
        );
        self.send(&msg).await;
    }

    // ── Trade result ──────────────────────────────────────────────

    pub async fn trade_result(
        &self,
        s:       &Signal,
        outcome: &Outcome,
        exit:    f64,
        pnl:     f64,
    ) {
        let (emoji, label) = match outcome {
            Outcome::Win  => ("✅", "WIN"),
            Outcome::Loss => ("❌", "LOSS"),
            Outcome::Open => ("🔄", "STILL OPEN"),
        };
        let msg = format!(
            "{emoji} <b>TRADE {label}</b>\n\
             {sym} | {dir}\n\
             Entry: <code>${entry:.6}</code> → Exit: <code>${exit:.6}</code>\n\
             PnL: <b>${pnl:+.4}</b>",
            sym   = s.symbol,
            dir   = s.direction,
            entry = s.entry.to_f64(),
            exit  = exit,
            pnl   = pnl,
        );
        self.send(&msg).await;
    }

    // ── Hourly dashboard ──────────────────────────────────────────

    pub async fn dashboard(&self, m: &Metrics) {
        let grade = m.grade();
        let msg = format!(
            "📊 <b>PERFORMANCE DASHBOARD</b>\n\
             ══════════════════════════\n\
             💰 Capital:      <b>${cap:.4}</b>\n\
             📈 Total PnL:    <b>${pnl:+.4}</b>\n\
             ══════════════════════════\n\
             🎯 Trades:       <b>{total}</b>\n\
             ✅ Wins:         {wins} ({wr:.1}%)\n\
             ❌ Losses:       {losses} ({lr:.1}%)\n\
             ══════════════════════════\n\
             💵 Avg Win:      +${avg_w:.4}\n\
             💸 Avg Loss:     −${avg_l:.4}\n\
             ⚖️  Profit Factor: <b>{pf:.2}×</b>\n\
             📉 Max Drawdown: {dd:.1}%\n\
             ══════════════════════════\n\
             🔑 Kelly f:      {kelly:.1}%\n\
             🎓 Grade:        <b>{grade}</b>",
            cap   = m.capital,
            pnl   = m.total_pnl,
            total = m.total,
            wins  = m.wins,
            wr    = m.win_rate * 100.0,
            losses = m.losses,
            lr    = (1.0 - m.win_rate) * 100.0,
            avg_w = m.avg_win_usd,
            avg_l = m.avg_loss_usd,
            pf    = m.profit_factor,
            dd    = m.max_drawdown * 100.0,
            kelly = m.kelly_fraction * 100.0,
            grade = grade,
        );
        self.send(&msg).await;
    }

    // ── Scan report ───────────────────────────────────────────────

    pub async fn scan_report(&self, coins: &[bot_types::CoinScore]) {
        if coins.is_empty() { return; }
        let rows = coins
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{rank}. {sym}  score:{score:.2}  vol:${vol:.0}M  \
                     chg:{chg:+.1}%  smc:{smc:.0}%  {trend}",
                    rank  = i + 1,
                    sym   = c.symbol,
                    score = c.composite,
                    vol   = c.volume_24h / 1_000_000.0,
                    chg   = c.change_24h,
                    smc   = c.smc_score * 100.0,
                    trend = if c.trend_bullish { "↑" } else { "↓" },
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.send(&format!(
            "🔭 <b>WATCHLIST SCAN — TOP {n}</b>\n<pre>{rows}</pre>",
            n = coins.len(),
        ))
        .await;
    }

    // ── Live-ready alert ──────────────────────────────────────────

    pub async fn live_ready(&self, m: &Metrics) {
        let msg = format!(
            "🎓 <b>TRAINING COMPLETE — BOT IS LIVE-READY!</b>\n\
             ══════════════════════════════════\n\
             After <b>{total}</b> paper trades:\n\n\
             ✅ Win Rate:      <b>{wr:.1}%</b>\n\
             ✅ Profit Factor: <b>{pf:.2}×</b>\n\
             💰 Paper PnL:    <b>${pnl:+.4}</b>\n\
             💵 Capital:      <b>${cap:.4}</b>\n\
             ══════════════════════════════════\n\
             🚀 Set <code>BOT_MODE=live</code> in .env to go live.\n\
             ⚠️  Only risk what you can afford to lose.",
            total = m.total,
            wr    = m.win_rate * 100.0,
            pf    = m.profit_factor,
            pnl   = m.total_pnl,
            cap   = m.capital,
        );
        self.send(&msg).await;
    }

    // ── Risk halt alert ───────────────────────────────────────────

    pub async fn risk_halt(&self, reason: &str) {
        self.send(&format!(
            "⚠️ <b>RISK HALT</b>\n{reason}\nTrading paused."
        ))
        .await;
    }

    // ── System startup ────────────────────────────────────────────

    pub async fn startup(&self, mode: &str, capital: f64, symbols: usize) {
        self.send(&format!(
            "🤖 <b>PRO TRADING BOT v3.0 STARTED</b>\n\
             Mode:    <b>{mode}</b>\n\
             Capital: <b>${capital:.4}</b>\n\
             Watching up to <b>{symbols}</b> coins",
        ))
        .await;
    }
}
