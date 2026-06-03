#!/usr/bin/env bash
# ================================================================
#  setup.sh  —  One-command environment bootstrap
#  Run once after cloning: bash setup.sh
# ================================================================
set -euo pipefail

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'

banner() { echo -e "${GREEN}[setup]${NC} $*"; }
warn()   { echo -e "${YELLOW}[warn] ${NC} $*"; }
fail()   { echo -e "${RED}[fail] ${NC} $*"; exit 1; }

echo -e "${GREEN}"
cat << 'ART'
╔═══════════════════════════════════════════════════╗
║   PRO TRADING BOT v3.0 — Setup Wizard             ║
║   Rust Workspace · 13 Crates · Production-grade   ║
╚═══════════════════════════════════════════════════╝
ART
echo -e "${NC}"

# ── Step 1: Rust toolchain ────────────────────────────────────────
banner "Step 1/5: Rust toolchain"
if command -v cargo &>/dev/null; then
    banner "  Already installed: $(rustc --version)"
    rustup update stable --quiet
else
    warn "  Rust not found — installing via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable
    source "$HOME/.cargo/env"
fi
rustup component add clippy rustfmt

# ── Step 2: .env file ─────────────────────────────────────────────
banner "Step 2/5: Environment file"
if [ ! -f .env ]; then
    cp .env.example .env
    warn "  .env created — EDIT IT before running the bot!"
else
    banner "  .env already exists — skipping"
fi

# ── Step 3: Check .env is filled in ──────────────────────────────
banner "Step 3/5: Validating .env"
for KEY in BINANCE_API_KEY BINANCE_SECRET TELEGRAM_TOKEN TELEGRAM_CHAT_ID; do
    VAL=$(grep "^${KEY}=" .env | cut -d= -f2-)
    if [[ "$VAL" == YOUR_* ]] || [[ -z "$VAL" ]]; then
        warn "  ${KEY} is not set in .env"
    else
        banner "  ${KEY} ✓"
    fi
done

# ── Step 4: Debug build (fast feedback) ──────────────────────────
banner "Step 4/5: Debug build"
cargo build 2>&1 | tail -3
banner "  Debug build OK"

# ── Step 5: Release build ─────────────────────────────────────────
banner "Step 5/5: Release build"
cargo build --release 2>&1 | tail -3
banner "  Binary: ./target/release/pro_trading_bot"

chmod +x run_paper.sh run_live.sh

echo ""
echo -e "${GREEN}═══════════════════════════════════════════════════${NC}"
echo -e "${GREEN} Setup complete!  Next steps:${NC}"
echo ""
echo -e "  1. ${YELLOW}nano .env${NC}                  — add your API keys"
echo -e "  2. ${YELLOW}bash run_paper.sh${NC}          — start learning (paper)"
echo -e "  3. Wait for ${YELLOW}🎓 LIVE READY${NC} on Telegram"
echo -e "  4. ${YELLOW}nano .env${NC} → BOT_MODE=live  — go live"
echo -e "  5. ${YELLOW}bash run_live.sh${NC}           — start live trading"
echo ""
echo -e "${RED}  ⚠  Never trade more than you can afford to lose.${NC}"
echo -e "${GREEN}═══════════════════════════════════════════════════${NC}"
