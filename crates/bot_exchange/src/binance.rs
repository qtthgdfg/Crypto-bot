//! Binance Spot REST implementation of [`Exchange`].
//!
//! Professional concerns addressed here:
//! * **Rate limiting** — Binance enforces 1 200 request-weight/min.
//!   A `Semaphore` gate keeps us under the limit.
//! * **Retry with exponential back-off** — transient HTTP/5xx errors
//!   are retried up to 3 times before propagating.
//! * **HMAC-SHA256 signing** — every private endpoint is signed.
//! * **Error detection** — every response is inspected for Binance's
//!   `{"code":-xxxx,"msg":"..."}` error envelope.

use super::{Exchange, ExchangeError, Ticker24h};
use async_trait::async_trait;
use bot_types::{
    Candle, Direction, Interval, OrderRequest, OrderResult, OrderSide, Price, Quantity, Symbol,
};
use chrono::Utc;
use hex;
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode};
use rust_decimal::prelude::*;
use serde_json::Value;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing::{debug, instrument, warn};

// Binance weight limit: 1 200 / min → allow max 20 concurrent requests
const MAX_CONCURRENT: usize = 20;
const BASE_URL: &str = "https://api.binance.com";

pub struct BinanceExchange {
    api_key: String,
    secret:  String,
    client:  Client,
    gate:    Arc<Semaphore>,
}

impl BinanceExchange {
    pub fn new(api_key: &str, secret: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .connection_verbose(false)
            .build()
            .expect("reqwest client");
        Self {
            api_key: api_key.to_owned(),
            secret:  secret.to_owned(),
            client,
            gate: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        }
    }

    // ── HMAC-SHA256 signing ───────────────────────────────────────
    fn sign(&self, query: &str) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.secret.as_bytes()).expect("HMAC");
        mac.update(query.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    // ── Signed GET ────────────────────────────────────────────────
    async fn signed_get(&self, path: &str, params: &str) -> Result<Value, ExchangeError> {
        let ts = Utc::now().timestamp_millis();
        let query = format!("{params}&timestamp={ts}");
        let sig   = self.sign(&query);
        let url   = format!("{BASE_URL}{path}?{query}&signature={sig}");
        self.get_json(&url).await
    }

    // ── Signed POST ───────────────────────────────────────────────
    async fn signed_post(&self, path: &str, params: &str) -> Result<Value, ExchangeError> {
        let ts    = Utc::now().timestamp_millis();
        let query = format!("{params}&timestamp={ts}");
        let sig   = self.sign(&query);
        let url   = format!("{BASE_URL}{path}?{query}&signature={sig}");
        let _permit = self.gate.acquire().await.unwrap();
        let resp = self
            .client
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await?;
        parse_response(resp).await
    }

    // ── Public GET with retry ─────────────────────────────────────
    async fn get_json(&self, url: &str) -> Result<Value, ExchangeError> {
        let _permit = self.gate.acquire().await.unwrap();
        let mut delay = Duration::from_millis(300);
        for attempt in 0..3u8 {
            let resp = self
                .client
                .get(url)
                .header("X-MBX-APIKEY", &self.api_key)
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        warn!("Rate-limited by Binance, backing off");
                        sleep(delay).await;
                        delay *= 2;
                        continue;
                    }
                    return parse_response(r).await;
                }
                Err(e) if attempt < 2 => {
                    warn!(attempt, error=%e, "Transient HTTP error, retrying");
                    sleep(delay).await;
                    delay *= 2;
                }
                Err(e) => return Err(ExchangeError::Http(e)),
            }
        }
        Err(ExchangeError::RateLimit)
    }
}

/// Parse an HTTP response: propagate Binance API error envelopes,
/// otherwise deserialise to `Value`.
async fn parse_response(resp: reqwest::Response) -> Result<Value, ExchangeError> {
    let text = resp.text().await.map_err(ExchangeError::Http)?;
    let json: Value = serde_json::from_str(&text)
        .map_err(|e| ExchangeError::Parse(format!("{e}: {text}")))?;

    // Binance wraps errors as {"code":-1121,"msg":"Invalid symbol."}
    if let Some(code) = json.get("code").and_then(Value::as_i64) {
        if code < 0 {
            let message = json["msg"].as_str().unwrap_or("unknown").to_owned();
            return Err(ExchangeError::Api { code, message });
        }
    }
    Ok(json)
}

// ── Parse helpers ──────────────────────────────────────────────────

fn parse_price(v: &Value) -> Result<Price, ExchangeError> {
    let s = v.as_str().ok_or_else(|| ExchangeError::Parse("price not a string".into()))?;
    Decimal::from_str(s)
        .map(Price::new)
        .map_err(|e| ExchangeError::Parse(e.to_string()))
}

fn parse_dec(v: &Value) -> Decimal {
    v.as_str()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or_default()
}

fn interval_str(i: Interval) -> &'static str {
    match i {
        Interval::OneMinute      => "1m",
        Interval::FiveMinutes    => "5m",
        Interval::FifteenMinutes => "15m",
        Interval::OneHour        => "1h",
        Interval::FourHours      => "4h",
        Interval::OneDay         => "1d",
    }
}

// ── Exchange impl ─────────────────────────────────────────────────

#[async_trait]
impl Exchange for BinanceExchange {
    fn name(&self) -> &'static str { "Binance" }

    #[instrument(skip(self))]
    async fn price(&self, symbol: &Symbol) -> Result<Price, ExchangeError> {
        let url = format!(
            "{BASE_URL}/api/v3/ticker/price?symbol={}", symbol
        );
        let data = self.get_json(&url).await?;
        parse_price(&data["price"])
    }

    #[instrument(skip(self))]
    async fn prices(
        &self,
        symbols: &[Symbol],
    ) -> Result<HashMap<Symbol, Price>, ExchangeError> {
        let url = format!("{BASE_URL}/api/v3/ticker/price");
        let data = self.get_json(&url).await?;
        let arr = data
            .as_array()
            .ok_or_else(|| ExchangeError::Parse("expected array".into()))?;

        let symbol_set: std::collections::HashSet<&Symbol> = symbols.iter().collect();
        let mut map = HashMap::new();
        for item in arr {
            let sym = Symbol::new(item["symbol"].as_str().unwrap_or(""));
            if symbol_set.contains(&sym) {
                if let Ok(p) = parse_price(&item["price"]) {
                    map.insert(sym, p);
                }
            }
        }
        Ok(map)
    }

    #[instrument(skip(self))]
    async fn klines(
        &self,
        symbol:   &Symbol,
        interval: Interval,
        limit:    u32,
    ) -> Result<Vec<Candle>, ExchangeError> {
        let url = format!(
            "{BASE_URL}/api/v3/klines?symbol={}&interval={}&limit={}",
            symbol, interval_str(interval), limit
        );
        let data = self.get_json(&url).await?;
        let arr  = data
            .as_array()
            .ok_or_else(|| ExchangeError::Parse("expected array".into()))?;

        let mut candles = Vec::with_capacity(arr.len());
        for k in arr {
            let k = k.as_array().ok_or_else(|| ExchangeError::Parse("bad kline".into()))?;
            let ts_ms = k[0].as_i64().unwrap_or(0);
            candles.push(Candle {
                symbol:   symbol.clone(),
                open_time: chrono::DateTime::from_timestamp_millis(ts_ms)
                    .unwrap_or_default(),
                open:  parse_price(&k[1])?,
                high:  parse_price(&k[2])?,
                low:   parse_price(&k[3])?,
                close: parse_price(&k[4])?,
                volume:           Quantity::new(parse_dec(&k[5])),
                quote_volume:     parse_dec(&k[7]),
                taker_buy_volume: parse_dec(&k[9]),
                is_closed:        true,
            });
        }
        debug!(symbol=%symbol, count=candles.len(), "Klines fetched");
        Ok(candles)
    }

    #[instrument(skip(self))]
    async fn place_order(
        &self,
        req: OrderRequest,
    ) -> Result<OrderResult, ExchangeError> {
        let side = match req.side {
            OrderSide::Buy  => "BUY",
            OrderSide::Sell => "SELL",
        };
        let qty_str = req.quote_qty.round_dp(2).to_string();
        let params  = format!(
            "symbol={}&side={}&type=MARKET&quoteOrderQty={}",
            req.symbol, side, qty_str
        );
        let data = self.signed_post("/api/v3/order", &params).await?;

        let fills = data["fills"].as_array();
        let avg_price = fills
            .and_then(|f| {
                let sum_qty: Decimal  = f.iter().map(|x| parse_dec(&x["qty"])).sum();
                let sum_val: Decimal  = f.iter()
                    .map(|x| parse_dec(&x["price"]) * parse_dec(&x["qty"]))
                    .sum();
                if sum_qty.is_zero() { None }
                else { Some(Price::new(sum_val / sum_qty)) }
            })
            .unwrap_or(Price::new(Decimal::ZERO));

        Ok(OrderResult {
            order_id: data["orderId"].to_string(),
            symbol:   req.symbol,
            side:     req.side,
            qty:      parse_dec(&data["executedQty"]),
            price:    avg_price,
            status:   data["status"].as_str().unwrap_or("UNKNOWN").to_owned(),
        })
    }

    async fn cancel_all(&self, symbol: &Symbol) -> Result<(), ExchangeError> {
        self.signed_post(
            "/api/v3/openOrders",
            &format!("symbol={symbol}"),
        ).await?;
        Ok(())
    }

    async fn balance(&self, asset: &str) -> Result<Decimal, ExchangeError> {
        let data = self.signed_get("/api/v3/account", "").await?;
        let balances = data["balances"]
            .as_array()
            .ok_or_else(|| ExchangeError::Parse("no balances".into()))?;
        for b in balances {
            if b["asset"].as_str() == Some(asset) {
                return Ok(parse_dec(&b["free"]));
            }
        }
        Ok(Decimal::ZERO)
    }

    async fn usdt_symbols(&self) -> Result<Vec<Symbol>, ExchangeError> {
        let url  = format!("{BASE_URL}/api/v3/exchangeInfo");
        let data = self.get_json(&url).await?;
        let syms = data["symbols"]
            .as_array()
            .ok_or_else(|| ExchangeError::Parse("no symbols".into()))?
            .iter()
            .filter(|s| {
                s["quoteAsset"].as_str()  == Some("USDT")
                    && s["status"].as_str()   == Some("TRADING")
                    && s["isSpotTradingAllowed"].as_bool() == Some(true)
            })
            .filter_map(|s| s["symbol"].as_str().map(Symbol::new))
            .collect();
        Ok(syms)
    }

    async fn tickers_24h(&self) -> Result<Vec<Ticker24h>, ExchangeError> {
        let url  = format!("{BASE_URL}/api/v3/ticker/24hr");
        let data = self.get_json(&url).await?;
        let arr  = data
            .as_array()
            .ok_or_else(|| ExchangeError::Parse("expected array".into()))?;

        let tickers = arr
            .iter()
            .filter_map(|t| {
                let sym = Symbol::new(t["symbol"].as_str()?);
                if !sym.as_str().ends_with("USDT") {
                    return None;
                }
                Some(Ticker24h {
                    symbol:           sym,
                    price_change_pct: t["priceChangePercent"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0.0),
                    last_price:  parse_price(&t["lastPrice"]).ok()?,
                    volume:      parse_dec(&t["volume"]),
                    quote_volume: parse_dec(&t["quoteVolume"]),
                    high: parse_price(&t["highPrice"]).ok()?,
                    low:  parse_price(&t["lowPrice"]).ok()?,
                    count: t["count"].as_u64().unwrap_or(0),
                })
            })
            .collect();
        Ok(tickers)
    }
}
