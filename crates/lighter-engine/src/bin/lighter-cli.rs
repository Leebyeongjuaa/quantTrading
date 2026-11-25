//! Command-line front end for the Lighter engine.
//!
//! Secrets are read from the environment / `.env`:
//! `LIGHTER_API_KEY_PRIVATE`, `LIGHTER_ACCOUNT_INDEX`, `LIGHTER_API_KEY_INDEX`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use lighter_engine::exec::{
    IcebergConfig, IcebergExecutor, Leg, PairConfig, PairExecutor, PairSizer, Pricing,
};
use lighter_engine::ws::{self, WsConfig, WsEvent};
use lighter_engine::{
    candles, Config, LightersSigner, MarketCatalog, Network, NewOrder, OrderGateway, OrderKind,
    RestClient, Side, Signer, TimeInForce,
};
use rust_decimal::Decimal;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "lighter-cli", version, about = "Lighter DEX trading engine")]
struct Cli {
    #[arg(long, value_enum, default_value = "mainnet", global = true)]
    network: Network,
    /// Sign everything but send nothing.
    #[arg(long, global = true)]
    dry_run: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List markets with decimals, minimums and last prices.
    Markets,
    /// Show the live top of book for a market (via WebSocket).
    Book {
        symbol: String,
        #[arg(long, default_value_t = 10)]
        depth: usize,
    },
    /// Download candles and optionally resample (e.g. 5m -> 10m) to CSV.
    Candles {
        symbol: String,
        /// Native Lighter resolution: 1m 5m 15m 30m 1h 4h 12h 1d
        #[arg(long, default_value = "5m")]
        resolution: String,
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Target bar size, e.g. 10m. Must be a multiple of `resolution`.
        #[arg(long)]
        resample: Option<String>,
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Download candles for many markets into a directory (`<SYMBOL>.csv`), for `wq-bt`.
    CandlesUniverse {
        /// Symbols to fetch; default = the top N perp markets by daily quote volume.
        #[arg(long, value_delimiter = ',')]
        symbols: Vec<String>,
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Restrict the top-N ranking to one asset class.
        #[arg(long, value_enum)]
        asset_class: Option<AssetClass>,
        #[arg(long, default_value = "1h")]
        resolution: String,
        /// How far back to go. Lighter history starts 2025-01-17; earlier bars need --fallback.
        #[arg(long, default_value_t = 90)]
        days: u32,
        #[arg(long)]
        resample: Option<String>,
        /// Fill the period before a Lighter market existed from another venue's archive.
        #[arg(long, value_enum, default_value = "none")]
        fallback: Fallback,
        #[arg(long)]
        out_dir: std::path::PathBuf,
    },
    /// Dump raw WebSocket frames for a market (debugging).
    WsDump {
        symbol: String,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Generate a new API key pair (register the public key with ChangePubKey before use).
    GenKey,
    /// Verify the local key matches the one registered for (account, api_key_index).
    CheckKey,
    /// Print the next nonce for the configured key.
    Nonce,
    /// Print an auth token for private REST/WS endpoints.
    AuthToken {
        #[arg(long, default_value_t = 3600)]
        ttl_secs: u64,
    },
    /// Open orders for a market.
    Orders { symbol: String },
    /// Place one order.
    Place {
        #[arg(long)]
        symbol: String,
        #[arg(long, value_enum)]
        side: Side,
        #[arg(long)]
        size: Decimal,
        /// Limit price, or worst acceptable price with --market.
        #[arg(long)]
        price: Decimal,
        #[arg(long)]
        market: bool,
        #[arg(long, value_enum, default_value = "gtt")]
        tif: TimeInForce,
        #[arg(long, default_value_t = 3600)]
        expiry_secs: u64,
        #[arg(long)]
        reduce_only: bool,
    },
    /// Cancel one order by its exchange order index.
    Cancel {
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        order_index: i64,
    },
    /// Cancel every open order on the account.
    CancelAll,
    /// Work a large order as an iceberg: only `display` is visible at a time.
    Iceberg {
        #[arg(long)]
        symbol: String,
        #[arg(long, value_enum)]
        side: Side,
        #[arg(long)]
        total: Decimal,
        #[arg(long)]
        display: Decimal,
        /// Fixed limit price for every slice.
        #[arg(long, conflicts_with = "peg_ticks")]
        price: Option<Decimal>,
        /// Peg slices to the best quote, improved by N ticks (0 = join).
        #[arg(long)]
        peg_ticks: Option<i64>,
        #[arg(long, value_enum, default_value = "post-only")]
        tif: TimeInForce,
        /// Fraction of a slice that must fill before the next is placed (0,1].
        #[arg(long, default_value = "1.0")]
        refill_at: Decimal,
        #[arg(long, default_value_t = 500)]
        min_interval_ms: u64,
        #[arg(long, default_value_t = 2)]
        reprice_ticks: i64,
        #[arg(long, default_value_t = 0)]
        jitter_pct: u32,
        #[arg(long)]
        reduce_only: bool,
        #[arg(long)]
        deadline_secs: Option<u64>,
    },
    /// Enter a two-leg spread: buy `--long`, sell `--short`, both IOC, submitted as one batch.
    Pair {
        #[arg(long)]
        long: String,
        #[arg(long)]
        short: String,
        /// USD notional of the long leg; the short leg gets notional * beta.
        #[arg(long, conflicts_with_all = ["size_long", "size_short"])]
        notional: Option<Decimal>,
        #[arg(long, default_value = "1.0")]
        beta: Decimal,
        #[arg(long, requires = "size_short")]
        size_long: Option<Decimal>,
        #[arg(long, requires = "size_long")]
        size_short: Option<Decimal>,
        #[arg(long, default_value_t = 10)]
        slippage_bps: u32,
        #[arg(long, default_value_t = 2)]
        retries: u32,
        #[arg(long, default_value_t = 5)]
        fill_timeout_secs: u64,
        #[arg(long)]
        reduce_only: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Fallback {
    None,
    /// Binance USDT-M futures klines from data.binance.vision, spliced before Lighter data.
    Binance,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum AssetClass {
    Crypto,
    Commodity,
    Fx,
    UsEquity,
    IntlEquity,
    Other,
}

impl AssetClass {
    /// Lighter's `strategy_index` code for the class.
    fn code(self) -> i64 {
        match self {
            AssetClass::Crypto => 2,
            AssetClass::Commodity => 3,
            AssetClass::Fx => 4,
            AssetClass::UsEquity => 5,
            AssetClass::IntlEquity => 6,
            AssetClass::Other => 7,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let cfg = Config::from_env(cli.network)?;
    let rest = RestClient::new(&cfg.rest_url)?;

    match cli.cmd {
        Cmd::Markets => {
            let mut markets = rest.order_book_details(None).await?;
            markets.sort_by_key(|m| m.market_id);
            println!(
                "{:<10} {:>4} {:<5} {:>5} {:>5} {:>14} {:>12} {:>14} {:>8}",
                "symbol", "id", "type", "szdp", "pxdp", "min_base", "min_quote", "last", "status"
            );
            for m in markets {
                println!(
                    "{:<10} {:>4} {:<5} {:>5} {:>5} {:>14} {:>12} {:>14} {:>8}",
                    m.symbol,
                    m.market_id,
                    m.market_type,
                    m.size_decimals,
                    m.price_decimals,
                    m.min_base_amount,
                    m.min_quote_amount,
                    m.last_trade_price
                        .map(|p| p.to_string())
                        .unwrap_or_default(),
                    m.status
                );
            }
        }
        Cmd::Book { symbol, depth } => {
            let catalog = MarketCatalog::load(&rest).await?;
            let m = catalog.get(&symbol)?;
            let mut ws_cfg = WsConfig::new(&cfg.ws_url);
            ws_cfg.order_books = vec![m.market_id];
            let mut h = ws::spawn(ws_cfg);
            let wait = async {
                while let Some(ev) = h.events.recv().await {
                    if let WsEvent::Book { snapshot: true, .. } = ev {
                        break;
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(15), wait)
                .await
                .context("no snapshot")?;
            let book = h.book(m.market_id).await.context("book missing")?;
            h.shutdown();
            let (bids, asks) = book.depth(depth);
            println!(
                "{} (market {})  mid={}  spread={}",
                m.symbol,
                m.market_id,
                book.mid().map(|v| v.to_string()).unwrap_or_default(),
                book.spread().map(|v| v.to_string()).unwrap_or_default()
            );
            println!(
                "{:>16} {:>14} | {:<14} {:<16}",
                "bid_size", "bid", "ask", "ask_size"
            );
            for i in 0..depth {
                let b = bids
                    .get(i)
                    .map(|(p, s)| (p.to_string(), s.to_string()))
                    .unwrap_or_default();
                let a = asks
                    .get(i)
                    .map(|(p, s)| (p.to_string(), s.to_string()))
                    .unwrap_or_default();
                println!("{:>16} {:>14} | {:<14} {:<16}", b.1, b.0, a.0, a.1);
            }
        }
        Cmd::Candles {
            symbol,
            resolution,
            days,
            resample,
            out,
        } => {
            let catalog = MarketCatalog::load(&rest).await?;
            let m = catalog.get(&symbol)?;
            if !lighter_engine::rest::CANDLE_RESOLUTIONS.contains(&resolution.as_str()) {
                bail!(
                    "Lighter only serves {:?}; use --resample for others",
                    lighter_engine::rest::CANDLE_RESOLUTIONS
                );
            }
            let end = chrono::Utc::now().timestamp_millis();
            let start = end - days as i64 * 86_400_000;
            let mut bars = rest
                .candles_range(m.market_id, &resolution, start, end)
                .await?;
            eprintln!(
                "fetched {} x {resolution} candles for {}",
                bars.len(),
                m.symbol
            );
            if let Some(target) = resample {
                let src_ms =
                    lighter_engine::rest::resolution_ms(&resolution).context("bad resolution")?;
                let tgt_ms =
                    lighter_engine::rest::resolution_ms(&target).context("bad --resample")?;
                if tgt_ms % src_ms != 0 || tgt_ms <= src_ms {
                    bail!("{target} is not a multiple of {resolution}");
                }
                bars = candles::resample(&bars, tgt_ms);
                eprintln!("resampled to {} x {target} bars", bars.len());
            }
            let f =
                std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
            candles::write_csv(std::io::BufWriter::new(f), &bars)?;
            println!("wrote {} bars to {}", bars.len(), out.display());
        }
        Cmd::CandlesUniverse {
            symbols,
            top,
            asset_class,
            resolution,
            days,
            resample,
            fallback,
            out_dir,
        } => {
            if !lighter_engine::rest::CANDLE_RESOLUTIONS.contains(&resolution.as_str()) {
                bail!(
                    "Lighter only serves {:?}; use --resample for others",
                    lighter_engine::rest::CANDLE_RESOLUTIONS
                );
            }
            let mut markets = rest.order_book_details(None).await?;
            markets.retain(|m| m.status == "active" && !m.is_frozen);
            if let Some(c) = asset_class {
                markets.retain(|m| m.strategy_index == c.code());
            }
            let chosen: Vec<_> = if symbols.is_empty() {
                markets.sort_by(|a, b| {
                    b.daily_quote_volume
                        .partial_cmp(&a.daily_quote_volume)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                markets
                    .into_iter()
                    .filter(|m| m.market_type == "perp")
                    .take(top)
                    .collect()
            } else {
                let catalog = MarketCatalog::from_markets(markets);
                symbols
                    .iter()
                    .map(|s| catalog.get(s).cloned())
                    .collect::<Result<_, _>>()?
            };
            let src_ms =
                lighter_engine::rest::resolution_ms(&resolution).context("bad resolution")?;
            let tgt_ms = resample
                .as_deref()
                .map(lighter_engine::rest::resolution_ms)
                .map(|o| o.context("bad --resample"))
                .transpose()?;
            if let Some(t) = tgt_ms {
                if t % src_ms != 0 || t <= src_ms {
                    bail!("--resample must be a multiple of {resolution}");
                }
            }
            std::fs::create_dir_all(&out_dir)?;
            let end = chrono::Utc::now().timestamp_millis();
            let start = end - days as i64 * 86_400_000;
            let binance = match fallback {
                Fallback::Binance => Some(lighter_engine::binance::BinanceSource::new()?),
                Fallback::None => None,
            };
            let mut coverage = Vec::new();
            for m in chosen {
                // Skip existing files so an interrupted download can be resumed.
                let path = out_dir.join(format!("{}.csv", m.symbol));
                if path.exists() {
                    println!("{:<10} exists, skipping", m.symbol);
                    continue;
                }
                let lighter = match rest
                    .candles_range(m.market_id, &resolution, start, end)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("{}: candle download failed, skipping: {e}", m.symbol);
                        continue;
                    }
                };
                let first = lighter.first().map(|c| c.open_time_ms);
                let mut bars = lighter.clone();
                let mut note = String::new();
                let mut bn_used = 0usize;
                let mut ratio = 1.0;
                if let Some(bn) = &binance {
                    let gap = first.map(|f| f - start > src_ms).unwrap_or(true);
                    if gap {
                        let sym = lighter_engine::binance::futures_symbol(&m.symbol);
                        let bn_end = first.map(|f| f + 48 * src_ms).unwrap_or(end).min(end);
                        match bn.klines(&sym, &resolution, start, bn_end).await {
                            Ok(b) if !b.is_empty() => {
                                let (spliced, r, used) =
                                    lighter_engine::binance::splice(&b, &lighter);
                                bars = spliced;
                                bn_used = used;
                                ratio = r;
                                note = format!("+{used} binance bars ({sym}, ratio {r:.5})");
                            }
                            Ok(_) => note = format!("no binance archive for {sym}"),
                            Err(e) => note = format!("binance fetch failed for {sym}: {e}"),
                        }
                    }
                }
                let raw_first = bars.first().map(|c| c.open_time_ms);
                let raw_last = bars.last().map(|c| c.open_time_ms);
                if let Some(t) = tgt_ms {
                    bars = candles::resample(&bars, t);
                }
                let f = std::fs::File::create(&path)
                    .with_context(|| format!("create {}", path.display()))?;
                candles::write_csv(std::io::BufWriter::new(f), &bars)?;
                let fmt = |ms: Option<i64>| {
                    ms.and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "-".into())
                };
                println!(
                    "{:<10} {:>6} bars  {} .. {}  lighter from {}  {}",
                    m.symbol,
                    bars.len(),
                    fmt(raw_first),
                    fmt(raw_last),
                    fmt(first),
                    note
                );
                coverage.push(serde_json::json!({
                    "symbol": m.symbol,
                    "market_id": m.market_id,
                    "bars": bars.len(),
                    "from": raw_first,
                    "to": raw_last,
                    "lighter_from": first,
                    "binance_bars": bn_used,
                    "binance_ratio": ratio,
                }));
            }
            std::fs::write(
                out_dir.join("coverage.json"),
                serde_json::to_string_pretty(&coverage)?,
            )?;
        }
        Cmd::WsDump { symbol, seconds } => {
            let catalog = MarketCatalog::load(&rest).await?;
            let m = catalog.get(&symbol)?;
            let mut ws_cfg = WsConfig::new(&cfg.ws_url);
            ws_cfg.order_books = vec![m.market_id];
            ws_cfg.trades = vec![m.market_id];
            ws_cfg.emit_raw = true;
            let mut h = ws::spawn(ws_cfg);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
            loop {
                tokio::select! {
                    ev = h.events.recv() => match ev {
                        Some(WsEvent::Raw(v)) => { let s = v.to_string(); println!("{}", &s[..s.len().min(600)]); }
                        Some(_) => {}
                        None => break,
                    },
                    _ = tokio::time::sleep_until(deadline) => break,
                }
            }
            h.shutdown();
        }
        Cmd::GenKey => {
            let (private, public) = LightersSigner::generate_key();
            println!("LIGHTER_API_KEY_PRIVATE={private}");
            println!("public_key={public}");
            println!("Register the public key with a ChangePubKey transaction (needs your L1 wallet) before trading.");
        }
        Cmd::CheckKey => {
            let signer = build_signer(&cfg)?;
            let registered = rest
                .api_key_public(signer.account_index(), signer.api_key_index())
                .await?;
            let local = signer.public_key_hex();
            match registered {
                Some(r)
                    if r.trim_start_matches("0x")
                        .eq_ignore_ascii_case(local.trim_start_matches("0x")) =>
                {
                    println!(
                        "OK: local key matches registered key for account {} / api key {}",
                        signer.account_index(),
                        signer.api_key_index()
                    )
                }
                Some(r) => bail!("MISMATCH: registered={r} local={local}"),
                None => bail!(
                    "no key registered at api_key_index {}",
                    signer.api_key_index()
                ),
            }
        }
        Cmd::Nonce => {
            let acct = cfg.require_account()?;
            println!("{}", rest.next_nonce(acct, cfg.api_key_index).await?);
        }
        Cmd::AuthToken { ttl_secs } => {
            let signer = build_signer(&cfg)?;
            println!(
                "{}",
                signer.auth_token(chrono::Utc::now().timestamp() + ttl_secs as i64)?
            );
        }
        Cmd::Orders { symbol } => {
            let gw = build_gateway(&cfg, &rest, cli.dry_run).await?;
            let orders = gw.active_orders(&symbol).await?;
            println!(
                "{:<18} {:<18} {:>5} {:>14} {:>14} {:>14} {:<10}",
                "order_index", "client_idx", "side", "price", "initial", "remaining", "status"
            );
            for o in orders {
                println!(
                    "{:<18} {:<18} {:>5} {:>14} {:>14} {:>14} {:<10}",
                    o.order_index,
                    o.client_order_index,
                    o.side(),
                    o.price,
                    o.initial_base_amount,
                    o.remaining_base_amount,
                    o.status
                );
            }
        }
        Cmd::Place {
            symbol,
            side,
            size,
            price,
            market,
            tif,
            expiry_secs,
            reduce_only,
        } => {
            let gw = build_gateway(&cfg, &rest, cli.dry_run).await?;
            let kind = if market {
                OrderKind::Market
            } else {
                OrderKind::Limit {
                    tif,
                    expiry: Duration::from_secs(expiry_secs),
                }
            };
            let s = gw
                .submit(&NewOrder {
                    symbol,
                    side,
                    size,
                    price,
                    kind,
                    reduce_only,
                    client_order_index: None,
                })
                .await?;
            println!(
                "tx_hash={} client_order_index={} size={} price={}",
                s.tx_hash, s.client_order_index, s.size, s.price
            );
        }
        Cmd::Cancel {
            symbol,
            order_index,
        } => {
            let gw = build_gateway(&cfg, &rest, cli.dry_run).await?;
            println!("tx_hash={}", gw.cancel(&symbol, order_index).await?);
        }
        Cmd::CancelAll => {
            let gw = build_gateway(&cfg, &rest, cli.dry_run).await?;
            println!("tx_hash={}", gw.cancel_all().await?);
        }
        Cmd::Iceberg {
            symbol,
            side,
            total,
            display,
            price,
            peg_ticks,
            tif,
            refill_at,
            min_interval_ms,
            reprice_ticks,
            jitter_pct,
            reduce_only,
            deadline_secs,
        } => {
            let gw = build_gateway(&cfg, &rest, cli.dry_run).await?;
            let pricing = match (price, peg_ticks) {
                (Some(p), _) => Pricing::Fixed(p),
                (None, Some(t)) => Pricing::Peg { offset_ticks: t },
                (None, None) => bail!("give --price or --peg-ticks"),
            };
            let report = IcebergExecutor::new(&gw, &cfg.ws_url)
                .run(
                    IcebergConfig {
                        symbol,
                        side,
                        total_size: total,
                        display_size: display,
                        pricing,
                        tif,
                        refill_at,
                        min_interval: Duration::from_millis(min_interval_ms),
                        reprice_ticks,
                        jitter_pct,
                        reduce_only,
                    },
                    deadline_secs.map(Duration::from_secs),
                )
                .await?;
            println!(
                "filled={} remaining={} avg_price={} slices={}",
                report.filled,
                report.remaining,
                report
                    .avg_price
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into()),
                report.slices_sent
            );
        }
        Cmd::Pair {
            long,
            short,
            notional,
            beta,
            size_long,
            size_short,
            slippage_bps,
            retries,
            fill_timeout_secs,
            reduce_only,
        } => {
            let gw = build_gateway(&cfg, &rest, cli.dry_run).await?;
            let (sz_long, sz_short) = match (notional, size_long, size_short) {
                (Some(n), _, _) => {
                    let pa = gw
                        .markets()
                        .get(&long)?
                        .last_trade_price
                        .context("no last price for long leg")?;
                    let pb = gw
                        .markets()
                        .get(&short)?
                        .last_trade_price
                        .context("no last price for short leg")?;
                    PairSizer::from_notional(n, pa, pb, beta)?
                }
                (None, Some(a), Some(b)) => (a, b),
                _ => bail!("give --notional or both --size-long and --size-short"),
            };
            let report = PairExecutor::new(&gw, &cfg.ws_url)
                .run(PairConfig {
                    legs: vec![
                        Leg {
                            symbol: long,
                            side: Side::Buy,
                            size: sz_long,
                        },
                        Leg {
                            symbol: short,
                            side: Side::Sell,
                            size: sz_short,
                        },
                    ],
                    slippage_bps,
                    max_retries: retries,
                    fill_timeout: Duration::from_secs(fill_timeout_secs),
                    reduce_only,
                })
                .await?;
            for l in &report.legs {
                println!(
                    "{:<8} {:<4} requested={} filled={} avg={}",
                    l.symbol,
                    l.side.map(|s| s.to_string()).unwrap_or_default(),
                    l.requested,
                    l.filled,
                    l.avg_price()
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "-".into())
                );
            }
            println!(
                "rounds={} net_notional={}",
                report.rounds,
                report.net_notional()
            );
        }
    }
    Ok(())
}

fn build_signer(cfg: &Config) -> anyhow::Result<LightersSigner> {
    Ok(LightersSigner::new(
        cfg.require_key()?,
        cfg.require_account()?,
        cfg.api_key_index,
        cfg.chain_id,
    )?)
}

async fn build_gateway(
    cfg: &Config,
    rest: &RestClient,
    dry_run: bool,
) -> anyhow::Result<OrderGateway> {
    let signer = build_signer(cfg)?;
    let catalog = MarketCatalog::load(rest).await?;
    Ok(OrderGateway::new(
        rest.clone(),
        Arc::new(signer),
        catalog,
        dry_run,
    ))
}
