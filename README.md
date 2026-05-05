# google-maps-scraper

[![crates.io](https://img.shields.io/crates/v/google-maps-scraper.svg)](https://crates.io/crates/google-maps-scraper)
[![docs.rs](https://docs.rs/google-maps-scraper/badge.svg)](https://docs.rs/google-maps-scraper)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Apify-style Google Maps scraper for Rust. Drives a headless browser via the Chrome DevTools Protocol — no API key required. Ships with [Obscura](https://github.com/h4ckf0r0day/obscura) support for built-in stealth (fingerprint randomization, TLS mimicry, tracker blocking) to avoid Google detection.

## Why

Until now there has been **no production-quality Rust crate** for scraping Google Maps. The official Places API is paid, and Apify is JavaScript-only. This crate fills the gap with the same approach used by Apify's `compass~crawler-google-places` actor, written natively in Rust. With the Obscura backend, it also solves the detection problem that plagues Chrome-based scrapers.

## Features

- 🌍 Search Google Maps with arbitrary text queries.
- 🔄 Auto-scrolls the results feed until exhausted.
- 🪟 Clicks each result to grab address + phone + website from the panel.
- 🚪 Auto-dismisses the cookie consent banner on first visit.
- 🛡️ **Obscura backend** — built-in stealth mode with fingerprint randomization, TLS mimicry, and tracker blocking to avoid Google detection.
- 🔀 Chrome fallback — still works with standard headless Chrome if needed.
- 📦 Returns clean `Place` structs with German address parsing built-in.

## Requirements

**Option A — Obscura (recommended):**

No Chrome needed. Download the [Obscura binary](https://github.com/h4ckf0r0day/obscura/releases) and place it on your `PATH`, or pass the path directly via `ScraperConfig::obscura_at()`.

**Option B — Chrome (legacy):**

- Chrome / Chromium installed locally.
  - macOS: detected at `/Applications/Google Chrome.app/...`.
  - Linux: `apt install chromium-browser`.
  - Windows: detected in `Program Files`.
- Override the binary location with the `CHROME` env var if needed.

## Installation

```toml
[dependencies]
google-maps-scraper = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Quick start

### With Obscura (recommended)

```rust,no_run
use google_maps_scraper::{MapsScraper, ScraperConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Stealth mode enabled automatically — no Chrome needed
    let scraper = MapsScraper::launch(ScraperConfig::obscura()).await?;

    let places = scraper.search("coffee shop Berlin").await?;
    for p in &places {
        println!("{} – {:?} – {:?}", p.name, p.website, p.phone);
    }
    Ok(())
}
```

### With Chrome (legacy)

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
# use google_maps_scraper::{MapsScraper, ScraperConfig, BrowserBackend};
# use std::time::Duration;
# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let cfg = ScraperConfig {
    backend: BrowserBackend::Obscura {     // or BrowserBackend::Chrome
        bin: None,                         // finds "obscura" on PATH
        port: 9222,
    },
    headless: true,                        // ignored for Obscura (always headless)
    max_scroll_iterations: 50,             // load more results
    enrich: true,                          // click each place for website/phone
    between_query_delay: Duration::from_secs(3),
    place_panel_delay: Duration::from_millis(2000),
};
let scraper = MapsScraper::launch(cfg).await?;
# Ok(()) }
```

Or use the convenience constructors:

```rust,no_run
# use google_maps_scraper::ScraperConfig;
// Obscura on PATH, default port 9222
let cfg = ScraperConfig::obscura();

// Obscura at a specific path and port
let cfg = ScraperConfig::obscura_at("/opt/obscura/obscura", 9333);
```

Set `enrich: false` for a 5–10× speedup if you only need names + maps URLs (no website / phone).

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
    pub source_query: Option<String>,
}
```

`Place` derives `Serialize` + `Deserialize` so you can write straight to JSONL.

## Examples

```bash
# With Obscura (recommended)
cargo run --example scrape_obscura

# With Chrome (legacy)
cargo run --example scrape_one
```

## Known Limitations

**Google Maps requires Chrome.** Google Maps is an extremely complex SPA (Angular + WebComponents + WebGL) that requires a full browser engine to render any DOM content. Obscura can navigate to the correct URL and bypass the consent banner, but the Maps app renders zero interactive DOM elements without full CSS layout, Custom Elements, and WebGL support.

**Use `ScraperConfig::default()` (Chrome) for Google Maps scraping.** The Obscura backend is ready for when Obscura adds fuller Web API support, and works well for simpler scraping targets.

## Anti-detection

### Obscura stealth (default)

Obscura's `--stealth` mode is enabled automatically and provides:

| Protection | Detail |
|---|---|
| TLS fingerprint | Mimics Chrome 145 via `wreq` — defeats JA3/JA4 detection |
| `navigator.webdriver` | Set to `undefined` (real Chrome value) |
| Canvas/Audio/GPU | Per-session randomized fingerprints |
| Tracker blocking | 3,520 analytics/ads domains blocked at compile time |
| Function masking | `Function.prototype.toString()` returns `[native code]` |
| User-Agent | Chrome 145 with high-entropy `userAgentData` |
| Memory footprint | ~30 MB vs 200+ MB — run more parallel sessions |

### Rate limits

Google still applies IP-based rate limiting regardless of stealth:

- 🟢 1–30 queries per session: usually fine.
- 🟡 30–100 queries: feed may start returning empty. Restart the browser between batches.
- 🔴 100+ queries from one IP: expect to be soft-banned (search returns 0 results). Use a residential proxy.

The crate ships with the most reliable selectors at the time of writing. Google's DOM shifts occasionally — file an issue if the scraper stops returning data.

## Comparison with alternatives

| Tool | Lang | Pricing | Stealth | Notes |
|---|---|---|---|---|
| Google Places API | any | $32/1k + $17/1k details | N/A | Official; paid. Full coverage. |
| Apify `compass~crawler-google-places` | JS | $5/1k results | Basic | Battle-tested. Requires Apify account. |
| **google-maps-scraper + Obscura** | Rust | **$0** | **Built-in** | Self-hosted; stealth by default; this crate. |
| **google-maps-scraper + Chrome** | Rust | **$0** | Minimal | Fallback mode; detectable. |
| `googlescraper` (Python) | Python | $0 | None | Less maintained. |

## Roadmap

- Headed-mode debugging helper that opens DevTools.
- Built-in residential-proxy support via `BROWSERLESS_URL`.
- Concurrent multi-page scraping inside one browser.
- ~~Stealth plugin (`puppeteer-extra-plugin-stealth`-equivalent).~~ **Done** — use `ScraperConfig::obscura()` for built-in stealth via [Obscura](https://github.com/h4ckf0r0day/obscura).

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
