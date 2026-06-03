# 🤖 Pro Trading Bot v3.0 — Binance Edition

> Production-grade Rust workspace · 13 crates · trait-based abstractions · full test suite · CI/CD

---

## Architecture

This is a **Cargo workspace** — each concern is its own crate with a single responsibility.
Nothing is a monolith. Swapping Binance for Bybit requires one new `impl Exchange`.

```
┌─────────────────────────────────────────────────────────────┐
│                        main.rs  (thin entry point)          │
│         load config → wire deps → run Orchestrator          │
└────────────────────────┬────────────────────────────────────┘
                         │
         ┌───────────────▼───────────────┐
         │         bot_learning          │
         │   LearningEngine + Orchestrator│
         └──┬────┬────┬────┬────┬────────┘
            │    │    │    │    │
      ┌─────▼┐ ┌─▼──┐ │  ┌─▼──┐ │
      │risk  │ │scan│ │  │adv │ │
      └──────┘ └────┘ │  └────┘ │
                    ┌─▼──────────▼─┐
                    │  bot_signals  │
                    └──────┬───────┘
                    ┌──────▼───────┐
             ┌──────┤  bot_smc     ├──────┐
             │      └──────────────┘      │
      ┌──────▼──────┐              ┌──────▼──────┐
      │bot_indicators│             │ bot_exchange │
      └─────────────┘             │ (trait +     │
                                  │  Binance +   │
                                  │  Paper impl) │
                                  └─────────────┘
```

---

## Workspace Crates

| Crate | Responsibility |
|---|---|
| `bot_config` | Validated config loaded from env vars; fails fast on bad input |
| `bot_types` | Newtype wrappers (`Symbol`, `Price`, `Quantity`) + all shared domain types |
| `bot_db` | `Store` trait + SQLite implementation (WAL mode, typed queries) |
| `bot_exchange` | `Exchange` trait + `BinanceExchange` (rate-limit, retry, HMAC) + `PaperExchange` |
| `bot_indicators` | RSI, EMA, MACD, ATR, VWAP, StochRSI, RSI Divergence — **pure functions, fully tested** |
| `bot_smc` | Order Blocks, FVG, BOS, CHoCH, Liquidity Sweeps — **pure functions, fully tested** |
| `bot_signals` | Stateless signal engine: 6 setups, confidence filter, R:R filter |
| `bot_scanner` | Concurrent coin scorer: fetches all USDT pairs, scores by volume/volatility/SMC |
| `bot_risk` | Kelly criterion, daily-loss cap, drawdown circuit-breaker |
| `bot_learning` | Paper-trade tracking, TP/SL evaluation, live-ready detection |
| `bot_advisor` | Per-setup + per-symbol analysis → prioritised Telegram improvement report |
| `bot_telegram` | Typed Telegram client — signal cards, dashboard, trade results |
| `bot_learning` | `Orchestrator` — main 30-second tick loop wiring all crates |

---

## Quick Start

### Prerequisites

```bash
# Rust stable (1.78+)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 1 — Clone & setup

```bash
git clone https://github.com/your-org/pro_trading_bot
cd pro_trading_bot
bash setup.sh          # installs Rust, builds release binary, validates .env
```

### 2 — Configure

```bash
cp .env.example .env
nano .env              # fill in Binance keys + Telegram token
```

### 3 — Run in paper mode *(always start here)*

```bash
bash run_paper.sh
```

The bot will:
1. Scan all Binance USDT pairs and send a ranked watchlist to Telegram
2. Generate signals every 30 s — paper-trade every one
3. Track every TP/SL hit in `pro_bot.db`
4. Send an improvement report every 6 h
5. Send **🎓 LIVE READY** once win-rate ≥ 54 % and profit-factor ≥ 1.5× over ≥ 50 trades

### 4 — Go live *(only after 🎓 LIVE READY)*

```bash
# In .env:
BOT_MODE=live

bash run_live.sh       # safety confirmation prompt before any order
```

---

## Configuration Reference

| Variable | Default | Description |
|---|---|---|
| `BINANCE_API_KEY` | — | Binance REST key |
| `BINANCE_SECRET` | — | Binance secret |
| `TELEGRAM_TOKEN` | — | Bot token from @BotFather |
| `TELEGRAM_CHAT_ID` | — | Your Telegram chat ID |
| `BOT_MODE` | `paper` | `paper` or `live` |
| `STARTING_CAPITAL_USD` | `1.0` | Paper starting balance |
| `KELLY_FRACTION` | `0.25` | Quarter-Kelly multiplier |
| `MAX_TRADE_SIZE_PCT` | `0.10` | Hard cap per trade (10% of capital) |
| `MAX_ACTIVE_SYMBOLS` | `15` | Concurrent watchlist size |
| `MIN_VOLUME_USD` | `2000000` | 24h volume filter (USD) |
| `MIN_TRADES_FOR_LIVE` | `50` | Paper trades before live-check |
| `LIVE_READY_WIN_RATE` | `0.54` | Win-rate threshold |
| `LIVE_READY_PROFIT_FACTOR` | `1.5` | Profit-factor threshold |
| `MAX_DAILY_LOSS_PCT` | `0.05` | Daily loss cap (5%) |
| `MAX_DRAWDOWN_PCT` | `0.15` | Drawdown circuit-breaker (15%) |
| `MIN_CONFIDENCE` | `0.76` | Signal quality floor |
| `MIN_RISK_REWARD` | `2.0` | Minimum R:R ratio |
| `RUST_LOG` | `info` | Log level: error/warn/info/debug |

---

## Signal Setups

| # | Setup | Entry condition | Avg confidence |
|---|---|---|---|
| 1 | **Order Block Bounce** | Price inside OB zone + RSI < 42 (long) or > 58 (short) | 0.80–0.90 |
| 2 | **Change of Character** | CHoCH detected + MACD histogram confirms | 0.82–0.92 |
| 3 | **Break of Structure** | BOS close + EMA20/50 trend aligned | 0.79–0.89 |
| 4 | **Liquidity Sweep** | Wick through key level, close back inside | 0.83–0.93 |
| 5 | **RSI Divergence** | Price/RSI diverge across two swing points | 0.77–0.87 |
| 6 | **Volume Breakout** | Current volume ≥ 3× 20-bar average | 0.75–0.88 |

All signals require **R:R ≥ 2.0** and **confidence ≥ 0.76** before being sent.
At most **2 signals** are emitted per symbol per tick.

---

## Risk Management

```
Position size = capital × Kelly_fraction × 0.25  (quarter-Kelly)
              = clamped to [1.00 USD, 10% of capital]

Daily loss cap    → 5% of capital  → circuit-breaker fires
Drawdown cap      → 15% from peak  → circuit-breaker fires
Cool-down period  → 5 minutes      → auto-reset
Daily PnL reset   → midnight UTC
```

---

## Telegram Messages

| Message | When |
|---|---|
| 🤖 BOT STARTED | Startup |
| 🔭 WATCHLIST SCAN | Startup + every 4 h |
| ⚡/🔥 SIGNAL | Every qualifying signal (entry, TP, SL, R:R, reasons) |
| ✅/❌ TRADE RESULT | When paper trade closes |
| 📊 DASHBOARD | Every hour |
| 🤖 IMPROVEMENT REPORT | Every 6 h (prioritised action list) |
| 🎓 LIVE READY | Once, when targets are met |
| ⚠️ RISK HALT | Daily loss / drawdown limit hit |

---

## Development

```bash
# Run all tests
cargo test --all

# Run tests with output
cargo test --all -- --nocapture

# Check formatting
cargo fmt --all -- --check

# Lint
cargo clippy --all-targets -- -D warnings

# Build optimised binary
cargo build --release

# Run with debug logging
RUST_LOG=debug ./target/release/pro_trading_bot
```

---

## Deploying 24/7 on a VPS

```bash
# Copy binary and config
scp target/release/pro_trading_bot user@vps:~/pro_trading_bot/
scp .env pro_trading_bot.service user@vps:~/pro_trading_bot/

# On the VPS
sudo cp pro_trading_bot.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable pro_trading_bot
sudo systemctl start  pro_trading_bot
sudo journalctl -u pro_trading_bot -f   # live logs
```

---

## Adding a New Exchange

1. Add a new file `crates/bot_exchange/src/bybit.rs`
2. Implement `Exchange` for `BybitExchange`
3. In `main.rs` `build_exchange()`, add a new match arm for `Mode::Bybit`
4. Zero other changes needed

---

## ⚠️ Disclaimer

This software is provided for **educational purposes only**.  
Cryptocurrency trading carries **significant financial risk**.  
Past paper-trade performance does **not** guarantee future live results.  
You are solely responsible for all trading decisions made using this software.  
Never trade more than you can afford to lose entirely.
