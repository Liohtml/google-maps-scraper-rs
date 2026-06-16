# google-maps-scraper

[![CI](https://github.com/Liohtml/google-maps-scraper-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Liohtml/google-maps-scraper-rs/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

> **Note:** This crate is not yet published on crates.io. Install it from git (see below).

Apify-style Google Maps scraper for Rust. Drives a real headless Chrome via the Chrome DevTools Protocol — no API key required.

## Why

Until now there has been **no production-quality Rust crate** for scraping Google Maps. The official Places API is paid, and Apify is JavaScript-only. This crate fills the gap with the same approach used by Apify's `compass~crawler-google-places` actor, written natively in Rust.

## Features

- 🌍 Search Google Maps with arbitrary text queries.
- 🔄 Auto-scrolls the results feed until exhausted.
- 🪟 Clicks each result to grab address + phone + website from the panel.
- 🚪 Auto-dismisses the cookie consent banner on first visit.
- 🛡️ Sets `--disable-blink-features=AutomationControlled` to reduce detection.
- 📦 Returns clean `Place` structs with German address parsing built-in.

## Requirements

- Chrome / Chromium installed locally — **unless** you connect to a remote Chrome
  via `browserless_url` / `BROWSERLESS_URL` (see [Remote Chrome](#remote-chrome-browserless--no-local-chrome-required)),
  in which case no local browser is needed.
  - macOS: detected at `/Applications/Google Chrome.app/...`.
  - Linux: `apt install chromium-browser`.
  - Windows: detected in `Program Files`.
- Override the binary location with the `CHROME` env var if needed.

## Installation

```toml
[dependencies]
google-maps-scraper = { git = "https://github.com/Liohtml/google-maps-scraper-rs" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Quick start

```rust,no_run
use google_maps_scraper::{MapsScraper, ScraperConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scraper = MapsScraper::launch(ScraperConfig::default()).await?;

    let places = scraper.search("coffee shop Berlin").await?;
    for p in &places {
        println!("{} – {:?} – {:?}", p.name, p.website, p.phone);
    }
    Ok(())
}
```

## Multiple queries in one session

```rust,no_run
# use google_maps_scraper::{MapsScraper, ScraperConfig};
# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let scraper = MapsScraper::launch(ScraperConfig::default()).await?;
let places = scraper.search_many(&[
    "bakery Munich",
    "bakery Hamburg",
    "bakery Frankfurt",
]).await?;
println!("{} unique places across all 3 queries", places.len());
# Ok(()) }
```

Results are automatically deduplicated by website domain (or maps URL when no website).

## Configuration

```rust,no_run
# use google_maps_scraper::{MapsScraper, ScraperConfig};
# use std::time::Duration;
# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let cfg = ScraperConfig {
    headless: false,                       // see Chrome window for debugging
    max_scroll_iterations: 50,             // load more results
    enrich: true,                          // click each place for website/phone
    between_query_delay: Duration::from_secs(3),
    place_panel_delay: Duration::from_millis(2000),
    max_places: Some(50),                  // cap unique places per query (None = unlimited)
    nav_timeout: Duration::from_secs(30),  // fail instead of hanging on a stalled page
    proxy: Some("http://user:pass@host:port".into()), // or set the PROXY_URL env var
    user_agent: None,                      // None = Chrome's own current UA (recommended)
    browserless_url: None,                 // or set BROWSERLESS_URL to use a remote Chrome
};
let scraper = MapsScraper::launch(cfg).await?;
# Ok(()) }
```

Set `enrich: false` for a 5–10× speedup if you only need names + maps URLs (no website / phone).

For high-volume scraping, set `proxy` (or the `PROXY_URL` environment variable) to route
Chrome through a residential proxy and reduce the chance of being soft-banned.

## Remote Chrome (Browserless) — no local Chrome required

If you don't have Chrome installed locally, point the scraper at a remote Chrome over
the DevTools WebSocket. Set `browserless_url` (or the `BROWSERLESS_URL` environment
variable) and `MapsScraper::launch` will `connect` to it instead of launching a browser:

```rust,no_run
# use google_maps_scraper::{MapsScraper, ScraperConfig};
# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let cfg = ScraperConfig {
    browserless_url: Some("ws://localhost:3000".into()), // or wss://chrome.browserless.io?token=...
    ..Default::default()
};
let scraper = MapsScraper::launch(cfg).await?;
# Ok(()) }
```

When connecting to a remote Chrome, local launch arguments (`headless`, `proxy`, window
size, user agent) are controlled by the remote endpoint — configure those there.

## What you get back

```rust
pub struct Place {
    pub name: String,
    pub address: Option<String>,
    pub postcode: Option<String>,        // German format detection
    pub city: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub maps_url: Option<String>,
    pub latitude: Option<f64>,           // parsed from the maps_url @lat,lng segment
    pub longitude: Option<f64>,
    pub source_query: Option<String>,
}
```

`Place` derives `Serialize` + `Deserialize` so you can write straight to JSONL.

## Examples

```bash
cargo run --example scrape_one
```

## Anti-detection caveats

Google detects bot traffic. Rough survival guide:

- 🟢 1–30 queries per session: usually fine.
- 🟡 30–100 queries: feed may start returning empty. Restart the browser between batches.
- 🔴 100+ queries from one IP: expect to be soft-banned (search returns 0 results). Use a residential proxy or [Browserless](https://www.browserless.io/) cloud Chrome.

The crate ships with the most reliable selectors at the time of writing. Google's DOM shifts occasionally — file an issue if the scraper stops returning data.

## Comparison with alternatives

| Tool | Lang | Pricing | Notes |
|---|---|---|---|
| Google Places API | any | $32/1000 + $17/1000 details | Official; paid. Full coverage. |
| Apify `compass~crawler-google-places` | JS | $5/1000 results | Battle-tested. Requires Apify account. |
| **google-maps-scraper** | Rust | **$0** | Self-hosted; brittler; this crate. |
| `googlescraper` (Python) | Python | $0 | Less maintained. |

## Roadmap

- ✅ Proxy support via `ScraperConfig::proxy` / `PROXY_URL`.
- ✅ Coordinates (`latitude` / `longitude`) parsed from the maps URL.
- ✅ Remote Chrome (Browserless) support via `ScraperConfig::browserless_url` / `BROWSERLESS_URL`.
- Headed-mode debugging helper that opens DevTools.
- Richer place data: rating, review count, category, opening hours ([#11](https://github.com/Liohtml/google-maps-scraper-rs/issues/11)).
- Concurrent multi-page scraping inside one browser.
- Stealth plugin (`puppeteer-extra-plugin-stealth`-equivalent).

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
