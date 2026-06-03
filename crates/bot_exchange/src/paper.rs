//! Paper-trading exchange — fulfils the `Exchange` contract using
//! live Binance public market data but never submits real orders.

use super::{Exchange, ExchangeError, Ticker24h};
use async_trait::async_trait;
use bot_types::{
    Candle, Interval, OrderRequest, OrderResult, OrderSide, Price, Quantity, Symbol,
};
use reqwest::Client;
use rust_decimal::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::info;

pub struct PaperExchange {
    client:  Client,
    balance: Arc<RwLock<Decimal>>, // paper USDT balance
}

impl PaperExchange {
    pub fn new(starting_usdt: Decimal) -> Self {
        info!(balance = %starting_usdt, "Paper exchange initialised");
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            balance: Arc::new(RwLock::new(starting_usdt)),
        }
    }

    async fn public_get(&self, url: &str) -> Result<Value, ExchangeError> {
        let resp = self.client.get(url).send().await?;
        let text = resp.text().await?;
        serde_json::from_str(&text)
            .map_err(|e| ExchangeError::Parse(format!("{e}: {text}")))
    }
}

#[async_trait]
impl Exchange for PaperExchange {
    fn name(&self) -> &'static str { "Paper" }

    async fn price(&self, symbol: &Symbol) -> Result<Price, ExchangeError> {
        let url  = format!(
            "https://api.binance.com/api/v3/ticker/price?symbol={symbol}"
        );
        let data = self.public_get(&url).await?;
        let s    = data["price"]
            .as_str()
            .ok_or_else(|| ExchangeError::Parse("no price".into()))?;
        Decimal::from_str(s)
            .map(Price::new)
            .map_err(|e| ExchangeError::Parse(e.to_string()))
    }

    async fn prices(
        &self,
        symbols: &[Symbol],
    ) -> Result<HashMap<Symbol, Price>, ExchangeError> {
        let url  = "https://api.binance.com/api/v3/ticker/price";
        let data = self.public_get(url).await?;
        let arr  = data
            .as_array()
            .ok_or_else(|| ExchangeError::Parse("expected array".into()))?;

        let want: std::collections::HashSet<&Symbol> = symbols.iter().collect();
        let mut map = HashMap::new();
        for item in arr {
            let sym = Symbol::new(item["symbol"].as_str().unwrap_or(""));
            if want.contains(&sym) {
                if let Some(Ok(d)) =
                    item["price"].as_str().map(Decimal::from_str)
                {
                    map.insert(sym, Price::new(d));
                }
            }
        }
        Ok(map)
    }

    async fn klines(
        &self,
        symbol:   &Symbol,
        interval: Interval,
        limit:    u32,
    ) -> Result<Vec<Candle>, ExchangeError> {
        let url = format!(
            "https://api.binance.com/api/v3/klines?symbol={}&interval={}&limit={}",
            symbol, interval, limit
        );
        let data = self.public_get(&url).await?;
        let arr  = data
            .as_array()
            .ok_or_else(|| ExchangeError::Parse("expected array".into()))?;

        let parse = |v: &Value| {
            v.as_str()
                .and_then(|s| Decimal::from_str(s).ok())
                .map(Price::new)
                .ok_or_else(|| ExchangeError::Parse("bad price".into()))
        };
        let dec = |v: &Value| {
            v.as_str()
                .and_then(|s| Decimal::from_str(s).ok())
                .unwrap_or_default()
        };

        arr.iter()
            .map(|k| {
                let k = k.as_array().ok_or_else(|| {
                    ExchangeError::Parse("bad kline row".into())
                })?;
                Ok(Candle {
                    symbol:   symbol.clone(),
                    open_time: chrono::DateTime::from_timestamp_millis(
                        k[0].as_i64().unwrap_or(0),
                    )
                    .unwrap_or_default(),
                    open:  parse(&k[1])?,
                    high:  parse(&k[2])?,
                    low:   parse(&k[3])?,
                    close: parse(&k[4])?,
                    volume:           Quantity::new(dec(&k[5])),
                    quote_volume:     dec(&k[7]),
                    taker_buy_volume: dec(&k[9]),
                    is_closed:        true,
                })
            })
            .collect()
    }

    /// Simulate a fill at the current live price.
    async fn place_order(
        &self,
        req: OrderRequest,
    ) -> Result<OrderResult, ExchangeError> {
        let current = self.price(&req.symbol).await?;
        let qty     = req.quote_qty / current.0;

        // Deduct from paper balance on buys
        if matches!(req.side, OrderSide::Buy) {
            let mut bal = self.balance.write().await;
            if *bal < req.quote_qty {
                return Err(ExchangeError::InsufficientBalance);
            }
            *bal -= req.quote_qty;
        }

        info!(
            symbol  = %req.symbol,
            side    = ?req.side,
            qty     = %qty.round_dp(6),
            price   = %current,
            "PAPER order filled"
        );

        Ok(OrderResult {
            order_id: format!("PAPER-{}", chrono::Utc::now().timestamp_millis()),
            symbol:   req.symbol,
            side:     req.side,
            qty:      qty.round_dp(6),
            price:    current,
            status:   "FILLED".to_owned(),
        })
    }

    async fn cancel_all(&self, _symbol: &Symbol) -> Result<(), ExchangeError> {
        Ok(()) // no open orders in paper mode
    }

    async fn balance(&self, asset: &str) -> Result<Decimal, ExchangeError> {
        if asset.eq_ignore_ascii_case("USDT") {
            Ok(*self.balance.read().await)
        } else {
            Ok(Decimal::ZERO)
        }
    }

    async fn usdt_symbols(&self) -> Result<Vec<Symbol>, ExchangeError> {
        let url  = "https://api.binance.com/api/v3/exchangeInfo";
        let data = self.public_get(url).await?;
        Ok(data["symbols"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter(|s| {
                s["quoteAsset"].as_str()          == Some("USDT")
                    && s["status"].as_str()        == Some("TRADING")
                    && s["isSpotTradingAllowed"].as_bool() == Some(true)
            })
            .filter_map(|s| s["symbol"].as_str().map(Symbol::new))
            .collect())
    }

    async fn tickers_24h(&self) -> Result<Vec<Ticker24h>, ExchangeError> {
        let url  = "https://api.binance.com/api/v3/ticker/24hr";
        let data = self.public_get(url).await?;
        let arr  = data.as_array().unwrap_or(&vec![]).clone();
        Ok(arr
            .iter()
            .filter_map(|t| {
                let sym = Symbol::new(t["symbol"].as_str()?);
                if !sym.as_str().ends_with("USDT") {
                    return None;
                }
                let p = |k: &str| {
                    t[k].as_str()
                        .and_then(|s| Decimal::from_str(s).ok())
                        .map(Price::new)
                };
                let d = |k: &str| {
                    t[k].as_str()
                        .and_then(|s| Decimal::from_str(s).ok())
                        .unwrap_or_default()
                };
                Some(Ticker24h {
                    symbol:           sym,
                    price_change_pct: t["priceChangePercent"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0.0),
                    last_price:  p("lastPrice")?,
                    volume:      d("volume"),
                    quote_volume: d("quoteVolume"),
                    high: p("highPrice")?,
                    low:  p("lowPrice")?,
                    count: t["count"].as_u64().unwrap_or(0),
                })
            })
            .collect())
    }
}
