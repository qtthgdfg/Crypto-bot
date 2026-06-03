//! # bot_config
//!
//! Typed, validated configuration loaded from environment variables.
//! Fail-fast at startup: if any required variable is missing or
//! malformed, the process exits before doing any network I/O.

use rust_decimal::Decimal;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Missing environment variable '{0}'")]
    Missing(&'static str),

    #[error("Invalid value for '{var}': {source}")]
    Parse {
        var: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Validation failed: {0}")]
    Invalid(String),
}

// ── Bot mode ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Paper,
    Live,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Paper => write!(f, "paper"),
            Mode::Live  => write!(f, "live"),
        }
    }
}

impl FromStr for Mode {
    type Err = ConfigError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "paper" => Ok(Mode::Paper),
            "live"  => Ok(Mode::Live),
            _       => Err(ConfigError::Invalid(
                format!("BOT_MODE must be 'paper' or 'live', got '{s}'"),
            )),
        }
    }
}

// ── Main config struct ────────────────────────────────────────────

/// All runtime configuration for the bot.
///
/// Loaded from environment variables (or `.env`) with strict
/// validation. The constructor returns an error if any required
/// field is absent or cannot be parsed — there are no panics here.
#[derive(Debug, Clone)]
pub struct Config {
    // Exchange credentials
    pub binance_api_key: String,
    pub binance_secret:  String,

    // Telegram
    pub telegram_token:   String,
    pub telegram_chat_id: String,

    // Runtime mode
    pub mode: Mode,

    // Capital management
    pub starting_capital:  Decimal,
    pub kelly_fraction:    f64,
    pub max_trade_size_pct: f64, // max fraction of capital per trade

    // Scanner
    pub max_symbols:    usize,
    pub min_volume_usd: f64, // 24-h quote volume filter

    // Learning / live-ready thresholds
    pub min_trades_for_live:       usize,
    pub live_ready_win_rate:       f64,
    pub live_ready_profit_factor:  f64,

    // Risk management
    pub max_daily_loss_pct: f64,
    pub max_drawdown_pct:   f64,

    // Signal quality filters
    pub min_confidence:  f64,
    pub min_risk_reward: f64,

    // Database
    pub db_path: String,
}

impl Config {
    /// Build a `Config` from the current environment.
    ///
    /// # Errors
    /// Returns [`ConfigError`] if any required variable is missing
    /// or fails to parse / validate.
    pub fn from_env() -> Result<Self, ConfigError> {
        let c = Self {
            binance_api_key: require("BINANCE_API_KEY")?,
            binance_secret:  require("BINANCE_SECRET")?,
            telegram_token:   require("TELEGRAM_TOKEN")?,
            telegram_chat_id: require("TELEGRAM_CHAT_ID")?,
            mode:             parse("BOT_MODE")?,
            starting_capital: parse("STARTING_CAPITAL_USD")
                .unwrap_or(Decimal::ONE),
            kelly_fraction:   parse("KELLY_FRACTION")
                .unwrap_or(0.25_f64),
            max_trade_size_pct: parse("MAX_TRADE_SIZE_PCT")
                .unwrap_or(0.10_f64),
            max_symbols:    parse("MAX_ACTIVE_SYMBOLS")
                .unwrap_or(15_usize),
            min_volume_usd: parse("MIN_VOLUME_USD")
                .unwrap_or(2_000_000_f64),
            min_trades_for_live:      parse("MIN_TRADES_FOR_LIVE")
                .unwrap_or(50_usize),
            live_ready_win_rate:      parse("LIVE_READY_WIN_RATE")
                .unwrap_or(0.54_f64),
            live_ready_profit_factor: parse("LIVE_READY_PROFIT_FACTOR")
                .unwrap_or(1.5_f64),
            max_daily_loss_pct: parse("MAX_DAILY_LOSS_PCT")
                .unwrap_or(0.05_f64),
            max_drawdown_pct:   parse("MAX_DRAWDOWN_PCT")
                .unwrap_or(0.15_f64),
            min_confidence:  parse("MIN_CONFIDENCE")
                .unwrap_or(0.76_f64),
            min_risk_reward: parse("MIN_RISK_REWARD")
                .unwrap_or(2.0_f64),
            db_path: std::env::var("DB_PATH")
                .unwrap_or_else(|_| "pro_bot.db".to_string()),
        };
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.kelly_fraction <= 0.0 || self.kelly_fraction > 1.0 {
            return Err(ConfigError::Invalid(
                "KELLY_FRACTION must be in (0, 1]".into(),
            ));
        }
        if self.max_daily_loss_pct <= 0.0 || self.max_daily_loss_pct > 0.5 {
            return Err(ConfigError::Invalid(
                "MAX_DAILY_LOSS_PCT must be in (0, 0.5]".into(),
            ));
        }
        if self.live_ready_win_rate < 0.5 || self.live_ready_win_rate > 1.0 {
            return Err(ConfigError::Invalid(
                "LIVE_READY_WIN_RATE must be in [0.5, 1.0]".into(),
            ));
        }
        if self.mode == Mode::Live
            && (self.binance_api_key.starts_with("YOUR")
                || self.binance_secret.starts_with("YOUR"))
        {
            return Err(ConfigError::Invalid(
                "Live mode requires real Binance credentials".into(),
            ));
        }
        Ok(())
    }
}

// ── Private helpers ───────────────────────────────────────────────

fn require(key: &'static str) -> Result<String, ConfigError> {
    std::env::var(key).map_err(|_| ConfigError::Missing(key))
}

fn parse<T>(key: &'static str) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let raw = std::env::var(key).map_err(|_| ConfigError::Missing(key))?;
    raw.parse::<T>().map_err(|e| ConfigError::Parse {
        var:    key,
        source: Box::new(e),
    })
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses_paper() {
        assert_eq!("paper".parse::<Mode>().unwrap(), Mode::Paper);
        assert_eq!("PAPER".parse::<Mode>().unwrap(), Mode::Paper);
    }

    #[test]
    fn mode_parses_live() {
        assert_eq!("live".parse::<Mode>().unwrap(), Mode::Live);
    }

    #[test]
    fn mode_rejects_unknown() {
        assert!("staging".parse::<Mode>().is_err());
    }

    #[test]
    fn mode_display() {
        assert_eq!(Mode::Paper.to_string(), "paper");
        assert_eq!(Mode::Live.to_string(),  "live");
    }
}
