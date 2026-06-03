#!/usr/bin/env bash
# Start the bot in LIVE mode. Safety gate enforced.
set -e
[ -f .env ] && export $(grep -v '^#' .env | xargs)
echo ""; echo "⚠  LIVE TRADING — real money at risk"
echo ""; echo "Confirm ALL of the following:"; echo ""
echo "  [1] Bot sent 🎓 LIVE READY on Telegram"
echo "  [2] BOT_MODE=live is set in .env"
echo "  [3] Binance key has ONLY Spot Trading permission"
echo "  [4] Binance key is IP-restricted to this server"
echo ""; read -rp "  Type YES to continue: " ans
[[ "$ans" == "YES" ]] || { echo "Aborted."; exit 0; }
if [ ! -f target/release/pro_trading_bot ] || \
   [ src/main.rs -nt target/release/pro_trading_bot ]; then
    echo "Building…"; cargo build --release
fi
RUST_LOG=${RUST_LOG:-info} ./target/release/pro_trading_bot
