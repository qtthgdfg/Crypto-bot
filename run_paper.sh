#!/usr/bin/env bash
# Start the bot in PAPER (learning) mode — no real money touched.
set -e
[ -f .env ] && export $(grep -v '^#' .env | xargs)
if [ ! -f target/release/pro_trading_bot ] || \
   [ src/main.rs -nt target/release/pro_trading_bot ]; then
    echo "Building…"; cargo build --release
fi
echo "▶  Starting in PAPER mode"
RUST_LOG=${RUST_LOG:-info} ./target/release/pro_trading_bot
