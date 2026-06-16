//! # google-maps-scraper
//!
//! Apify-style Google Maps scraper for Rust. Drives a real headless Chrome
//! via the Chrome DevTools Protocol, searches Google Maps for any query,
//! scrolls the results feed until exhaustion, then clicks each place card
//! and extracts the public details (name, address, phone, website).
//!
//! ## When to use this
//!
//! - You want **lots** of Google Maps results (hundreds per query) without
//!   paying for the official Places API.
//! - You don't have an Apify subscription, or want a self-hosted scraper.
//! - You're comfortable with the brittleness of DOM-based scraping (Google
//!   occasionally changes selectors; this crate keeps them in one place
//!   so updates are localised).
//!
//! ## Requirements
//!
//! - Chrome / Chromium installed locally. On macOS the auto-detect finds
//!   `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`.
//!   On Linux: `apt install chromium`. Override via the `CHROME` env var.
//!
//! ## Quick start
//!
//! ```no_run
//! use google_maps_scraper::{MapsScraper, ScraperConfig};
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let scraper = MapsScraper::launch(ScraperConfig::default()).await?;
//!
//! let places = scraper
//!     .search_many(&["coffee shop Berlin", "bakery Munich"])
//!     .await?;
//!
//! for p in &places {
//!     println!("{} — {:?} — {:?}", p.name, p.website, p.phone);
//! }
//! # Ok(()) }
//! ```
//!
//! ## Anti-detection notes
//!
//! Google's bot-detection adapts. The `--disable-blink-features=AutomationControlled`
//! flag is set by default. For high-volume scraping use a residential proxy /
//! Browserless service, slow down delay between queries, and don't reuse a
//! single browser session for hundreds of queries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

const SEARCH_URL_BASE: &str = "https://www.google.com/maps";

/// All errors this crate produces.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Chrome failed to launch (often: not installed, or missing libs on Linux).
    #[error("chrome launch failed: {0}")]
    ChromeLaunch(String),

    /// Driving the page failed (navigation, evaluation, …).
    #[error("page error: {0}")]
    Page(String),

    /// A `ScraperConfig` value was rejected (e.g. an invalid proxy).
    #[error("invalid config: {0}")]
    Config(String),

    /// Underlying chromiumoxide error.
    #[error("cdp: {0}")]
    Cdp(#[from] chromiumoxide::error::CdpError),
}

/// Result type used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// One Google Maps place that the scraper extracted.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Place {
    /// Display name (from H1 on the detail panel).
    pub name: String,
    /// Full address as shown in the panel.
    pub address: Option<String>,
    /// 5-digit postcode parsed from the address. Populated only when the
    /// address contains a `NNNNN City` segment (German postal format); `None`
    /// otherwise. The match is a heuristic and may also fire on other 5-digit
    /// formats (e.g. a US ZIP + city).
    pub postcode: Option<String>,
    /// City parsed alongside the postcode from a `NNNNN City` segment (German
    /// postal format). `None` when no such segment is present. Same heuristic
    /// caveat as [`Place::postcode`].
    pub city: Option<String>,
    /// First phone number listed.
    pub phone: Option<String>,
    /// External website URL listed in the panel.
    pub website: Option<String>,
    /// The Google Maps URL (`/maps/place/...`) of this entry.
    pub maps_url: Option<String>,
    /// Latitude parsed from the `@lat,lng` segment of `maps_url`, if present.
    pub latitude: Option<f64>,
    /// Longitude parsed from the `@lat,lng` segment of `maps_url`, if present.
    pub longitude: Option<f64>,
    /// The query that produced this hit.
    pub source_query: Option<String>,
}

/// Scraper configuration.
#[derive(Debug, Clone)]
pub struct ScraperConfig {
    /// Run Chrome in headless mode (default: true). Use `false` for debugging.
    pub headless: bool,
    /// How many `scrollTop = scrollHeight` iterations to perform per search
    /// before assuming the feed is exhausted. Default: 30.
    pub max_scroll_iterations: usize,
    /// If false, skip clicking each result and only return surface data
    /// (name, maps_url) — much faster but no website / phone. Default: true.
    pub enrich: bool,
    /// Delay between consecutive HTTP requests inside Chrome.
    pub between_query_delay: Duration,
    /// Per-place delay after clicking (gives the panel time to render). The
    /// effective wait is this plus a random `0..=place_panel_jitter` — see
    /// [`ScraperConfig::place_panel_jitter`].
    pub place_panel_delay: Duration,
    /// Upper bound on the random extra delay added *on top of*
    /// [`ScraperConfig::place_panel_delay`] before each place visit. A fresh
    /// value in `0..=place_panel_jitter` is drawn per place to de-regularise the
    /// request cadence (mild bot-detection mitigation, not a guarantee). The
    /// jitter is purely additive, so `place_panel_delay` is always the minimum;
    /// set this to `Duration::ZERO` to disable it. Default: 750 ms.
    pub place_panel_jitter: Duration,
    /// Maximum number of unique places to return per query.
    /// `None` (default) means unlimited.
    pub max_places: Option<usize>,
    /// Timeout for each page navigation / JS evaluation step. Default: 30s.
    pub nav_timeout: Duration,
    /// Optional proxy passed to Chrome as `--proxy-server=<value>`.
    /// If `None`, falls back to the `PROXY_URL` environment variable.
    /// The value must not contain whitespace (rejected at launch).
    pub proxy: Option<String>,
    /// Optional `User-Agent` override.
    ///
    /// `None` (default) leaves Chrome to report its own, always-current UA,
    /// which stays consistent with the real TLS/HTTP2 fingerprint. Set this
    /// only if you need to pin a specific UA string.
    pub user_agent: Option<String>,
    /// Optional WebSocket URL of a remote Chrome (e.g. a Browserless instance)
    /// to connect to instead of launching a local browser. If `None`, falls
    /// back to the `BROWSERLESS_URL` environment variable.
    ///
    /// When set, the browser is reached via `Browser::connect`, so local
    /// launch arguments (`headless`, `proxy`, window size, user agent) are
    /// controlled by the remote endpoint and ignored here.
    pub browserless_url: Option<String>,
}

impl Default for ScraperConfig {
    fn default() -> Self {
        Self {
            headless: true,
            max_scroll_iterations: 30,
            enrich: true,
            between_query_delay: Duration::from_secs(2),
            place_panel_delay: Duration::from_millis(1500),
            place_panel_jitter: Duration::from_millis(750),
            max_places: None,
            nav_timeout: Duration::from_secs(30),
            proxy: None,
            user_agent: None,
            browserless_url: None,
        }
    }
}

/// The scraper. Holds an active Chrome browser process.
///
/// Drop the scraper to close Chrome.
pub struct MapsScraper {
    browser: Browser,
    handler_task: tokio::task::JoinHandle<()>,
    cfg: ScraperConfig,
}

impl MapsScraper {
    /// Launch (or connect to) a Chrome browser and return a ready-to-use scraper.
    ///
    /// If [`ScraperConfig::browserless_url`] is set (or the `BROWSERLESS_URL`
    /// environment variable is present), this connects to that remote Chrome
    /// over the DevTools WebSocket instead of launching a local browser — handy
    /// when no local Chrome is available or for high-volume scraping through a
    /// managed Chrome service.
    pub async fn launch(cfg: ScraperConfig) -> Result<Self> {
        // Remote Chrome (Browserless): explicit config field wins, else env var.
        let remote = cfg
            .browserless_url
            .clone()
            .or_else(|| std::env::var("BROWSERLESS_URL").ok())
            .filter(|u| !u.is_empty());

        let (browser, mut handler) = if let Some(ws_url) = remote {
            info!(endpoint = %redact_url(&ws_url), "connecting to remote Chrome");
            let proxy_configured = cfg.proxy.as_deref().is_some_and(|p| !p.is_empty())
                || std::env::var("PROXY_URL").is_ok_and(|p| !p.is_empty());
            if proxy_configured {
                warn!(
                    "proxy config is ignored when connecting to a remote Chrome; \
                     configure the proxy on the remote endpoint instead"
                );
            }
            Browser::connect(ws_url)
                .await
                .map_err(|e| Error::ChromeLaunch(e.to_string()))?
        } else {
            let mut builder = BrowserConfig::builder()
                .arg("--lang=en-US,en")
                .arg("--no-first-run")
                .arg("--no-default-browser-check")
                .arg("--disable-blink-features=AutomationControlled")
                .arg("--window-size=1280,1024");
            // Only override the UA when explicitly configured (see field docs).
            if let Some(ua) = cfg.user_agent.as_deref().filter(|u| !u.is_empty()) {
                builder = builder.arg(format!("--user-agent={ua}"));
            }
            // Use Chrome's *new* headless mode when headless: unlike the old
            // `--headless`, it reports a normal, current user-agent with no
            // `HeadlessChrome` token — so the default `user_agent: None` does not
            // leak that bot-detection signal.
            builder = if cfg.headless {
                builder.new_headless_mode()
            } else {
                builder.with_head()
            };
            // Proxy: explicit config field wins, otherwise fall back to PROXY_URL.
            if let Some(proxy) = cfg
                .proxy
                .clone()
                .or_else(|| std::env::var("PROXY_URL").ok())
                .filter(|p| !p.is_empty())
            {
                check_proxy(&proxy)?;
                info!(server = %redact_url(&proxy), "using proxy server");
                builder = builder.arg(format!("--proxy-server={proxy}"));
            }
            let browser_cfg = builder
                .build()
                .map_err(|e| Error::ChromeLaunch(e.to_string()))?;

            Browser::launch(browser_cfg)
                .await
                .map_err(|e| Error::ChromeLaunch(e.to_string()))?
        };

        let handler_task = tokio::spawn(async move { while (handler.next().await).is_some() {} });

        Ok(Self {
            browser,
            handler_task,
            cfg,
        })
    }

    /// Run a single Google Maps text search and return the extracted places.
    pub async fn search(&self, query: &str) -> Result<Vec<Place>> {
        self.search_many(&[query]).await
    }

    /// Run several queries through one browser session.
    /// Results are deduped by website domain (and by maps_url when no website).
    pub async fn search_many(&self, queries: &[&str]) -> Result<Vec<Place>> {
        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|e| Error::Page(e.to_string()))?;

        // Run the scrape on a dedicated tab and always close it afterwards —
        // even if the body returns early with an error — so a failed run never
        // leaks an open Chrome tab for the lifetime of the scraper.
        let result = self.search_many_on_page(&page, queries).await;
        let _ = page.close().await;
        result
    }

    async fn search_many_on_page(&self, page: &Page, queries: &[&str]) -> Result<Vec<Place>> {
        // Visit the maps homepage once to handle the consent banner.
        goto_with_timeout(page, "https://www.google.com/maps", self.cfg.nav_timeout).await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let _ = dismiss_consent(page).await;

        let out: Arc<Mutex<Vec<Place>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_keys: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        for (i, q) in queries.iter().enumerate() {
            info!(progress = i + 1, total = queries.len(), query = %q, "scanning");
            let url = format!("{}/search/{}/", SEARCH_URL_BASE, urlencoding::encode(q));
            if let Err(e) = goto_with_timeout(page, &url, self.cfg.nav_timeout).await {
                warn!("goto error: {e}");
                continue;
            }
            tokio::time::sleep(self.cfg.between_query_delay).await;

            // Wait for results panel (bounded so a stalled render can't hang us).
            let _ =
                tokio::time::timeout(self.cfg.nav_timeout, page.find_element("div[role='feed']"))
                    .await;

            // Scroll until stable
            let _ = scroll_feed(page, self.cfg.max_scroll_iterations).await;

            // Collect place URLs in the feed. Only keep https:// links so a
            // tampered DOM can't feed us a `javascript:` / `data:` URL to navigate to.
            let urls: Vec<String> = collect_place_urls(page)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|u| u.starts_with("https://"))
                .collect();
            info!(found = urls.len(), "feed collected");

            // Number of unique places added for *this* query, used to honor max_places.
            let mut added_this_query = 0usize;

            if !self.cfg.enrich {
                let mut o = out.lock().await;
                let mut sk = seen_keys.lock().await;
                for u in urls {
                    if self.cfg.max_places.is_some_and(|m| added_this_query >= m) {
                        break;
                    }
                    if sk.insert(u.clone()) {
                        let (latitude, longitude) = parse_coords_from_maps_url(&u);
                        o.push(Place {
                            name: String::new(),
                            maps_url: Some(u),
                            latitude,
                            longitude,
                            source_query: Some((*q).to_string()),
                            ..Default::default()
                        });
                        added_this_query += 1;
                    }
                }
                continue;
            }

            for place_url in urls {
                if self.cfg.max_places.is_some_and(|m| added_this_query >= m) {
                    break;
                }
                // Fast-path: skip a place URL we have already visited (within this
                // query or a previous one) before paying for a full navigation.
                {
                    let sk = seen_keys.lock().await;
                    if sk.contains(&place_url) {
                        continue;
                    }
                }
                if let Err(e) = goto_with_timeout(page, &place_url, self.cfg.nav_timeout).await {
                    warn!("place goto: {e}");
                    continue;
                }
                // Give the detail panel a chance to render before extracting:
                // wait (bounded) for the title `<h1>` that `extract_place_details`
                // reads, then let the rest settle. This reduces — but cannot fully
                // prevent — empty results on a slow render.
                let _ = tokio::time::timeout(self.cfg.nav_timeout, page.find_element("h1")).await;
                // Settle delay + random jitter so consecutive place visits don't
                // form a fixed-interval (easily-flagged) pattern.
                let max_jitter_ms =
                    u64::try_from(self.cfg.place_panel_jitter.as_millis()).unwrap_or(u64::MAX);
                let jitter = Duration::from_millis(jitter_ms(time_seed(), max_jitter_ms));
                tokio::time::sleep(self.cfg.place_panel_delay + jitter).await;
                let detail = match extract_place_details(page).await {
                    Ok(d) => d,
                    Err(e) => {
                        warn!("extract: {e}");
                        // Still register the URL so a consistently-failing place
                        // is not re-navigated on every subsequent query.
                        seen_keys.lock().await.insert(place_url.clone());
                        continue;
                    }
                };

                // Dedup by website domain (falling back to the maps URL when there
                // is no website), and register the place. See `register_place`.
                let key = detail.website_domain().unwrap_or_else(|| place_url.clone());
                let is_new = register_place(&mut *seen_keys.lock().await, &key, &place_url);
                if !is_new {
                    continue;
                }

                let (postcode, city) =
                    parse_german_address(detail.address.as_deref().unwrap_or(""));
                let (latitude, longitude) = parse_coords_from_maps_url(&place_url);
                let mut o = out.lock().await;
                o.push(Place {
                    name: detail.name.unwrap_or_default(),
                    address: detail.address,
                    postcode,
                    city,
                    phone: detail.phone,
                    website: detail.website,
                    maps_url: Some(place_url),
                    latitude,
                    longitude,
                    source_query: Some((*q).to_string()),
                });
                added_this_query += 1;
            }
        }

        let final_out = Arc::try_unwrap(out)
            .map_err(|_| Error::Page("output Arc still held".into()))?
            .into_inner();
        Ok(final_out)
    }

    /// Close Chrome cleanly. (Drop also works.)
    pub async fn close(mut self) -> Result<()> {
        let _ = self.browser.close().await;
        self.handler_task.abort();
        Ok(())
    }
}

impl Drop for MapsScraper {
    /// Abort the CDP handler task if the scraper is dropped without an explicit
    /// `close()` (e.g. on panic or early return). `chromiumoxide::Browser` has
    /// its own `Drop` that signals Chrome to shut down, so this just makes sure
    /// the background polling task does not outlive the scraper.
    fn drop(&mut self) {
        self.handler_task.abort();
    }
}

// ───────── internals ─────────

#[derive(Debug, Default)]
struct PlaceDetailRaw {
    name: Option<String>,
    address: Option<String>,
    phone: Option<String>,
    website: Option<String>,
}

impl PlaceDetailRaw {
    fn website_domain(&self) -> Option<String> {
        let w = self.website.as_deref()?;
        let parsed = url::Url::parse(w).ok()?;
        Some(
            parsed
                .host_str()
                .unwrap_or("")
                .trim_start_matches("www.")
                .to_string(),
        )
    }
}

/// Reject a proxy value that Chrome cannot use as a single `--proxy-server`
/// argument. A value with whitespace is malformed: Chrome receives the whole
/// `--proxy-server=<value>` as one token, silently fails to apply the proxy, and
/// falls back to a **direct** connection — leaking the real IP. Failing loudly
/// here surfaces the misconfiguration instead.
fn check_proxy(proxy: &str) -> Result<()> {
    if proxy.contains(char::is_whitespace) {
        return Err(Error::Config("proxy must not contain whitespace".into()));
    }
    Ok(())
}

/// Strip credentials from a URL so it is safe to log: removes any userinfo
/// (`user:pass@`) and the query string (which may carry a `?token=...`).
/// Falls back to `"<redacted>"` when the value does not parse as a URL.
fn redact_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.to_string()
        }
        Err(_) => "<redacted>".to_string(),
    }
}

/// Navigate `page` to `url`, failing with [`Error::Page`] if it does not
/// complete within `timeout`. Prevents a stalled network/render from blocking
/// the async task indefinitely.
async fn goto_with_timeout(page: &Page, url: &str, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, page.goto(url))
        .await
        .map_err(|_| Error::Page(format!("navigation timed out after {timeout:?}: {url}")))??;
    Ok(())
}

async fn dismiss_consent(page: &Page) {
    let selectors = [
        "button[aria-label*='Alle akzeptieren']",
        "button[aria-label*='Alle ablehnen']",
        "button[aria-label*='Accept all']",
        "button[aria-label*='Reject all']",
        "form[action*='consent.google.com'] button",
    ];
    for sel in selectors {
        if let Ok(el) = page.find_element(sel).await {
            let _ = el.click().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            break;
        }
    }
}

async fn scroll_feed(page: &Page, max_iters: usize) -> Result<()> {
    let mut last_height = -1.0_f64;
    let mut stable = 0;
    for _ in 0..max_iters {
        let new_height: f64 = page
            .evaluate(
                "(() => { const f = document.querySelector(\"div[role='feed']\"); if (!f) return -1; f.scrollTop = f.scrollHeight; return f.scrollHeight; })()",
            )
            .await?
            .into_value()
            .unwrap_or(-1.0);
        if new_height < 0.0 {
            break;
        }
        if (new_height - last_height).abs() < 1.0 {
            stable += 1;
            if stable >= 3 {
                break;
            }
        } else {
            stable = 0;
        }
        last_height = new_height;
        tokio::time::sleep(Duration::from_millis(900)).await;
    }
    Ok(())
}

async fn collect_place_urls(page: &Page) -> Result<Vec<String>> {
    let raw: Vec<String> = page
        .evaluate(
            "Array.from(document.querySelectorAll(\"div[role='feed'] a[href*='/maps/place/']\")).map(a => a.href)",
        )
        .await?
        .into_value()
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for u in raw {
        if seen.insert(u.clone()) {
            out.push(u);
        }
    }
    Ok(out)
}

async fn extract_place_details(page: &Page) -> Result<PlaceDetailRaw> {
    let js = r#"
        (() => {
            const out = {};
            const h1 = document.querySelector('h1');
            out.name = h1 ? h1.textContent.trim() : null;

            const addr = document.querySelector('[data-item-id="address"]');
            out.address = addr ? addr.getAttribute('aria-label') || addr.textContent.trim() : null;

            const phone = document.querySelector('[data-item-id^="phone"]');
            out.phone = phone ? (phone.getAttribute('aria-label') || phone.textContent.trim()) : null;

            const authority = document.querySelector('a[data-item-id="authority"]')
                || document.querySelector('[data-item-id="authority"] a')
                || document.querySelector('a[aria-label*="Website"]');
            out.website = authority ? authority.href : null;
            return out;
        })()
    "#;
    let raw: serde_json::Value = page.evaluate(js).await?.into_value().unwrap_or_default();
    let mut d = PlaceDetailRaw::default();
    if let Some(s) = raw.get("name").and_then(|v| v.as_str()) {
        d.name = Some(s.to_string());
    }
    if let Some(s) = raw.get("address").and_then(|v| v.as_str()) {
        d.address = Some(
            s.trim_start_matches("Adresse: ")
                .trim_start_matches("Address: ")
                .to_string(),
        );
    }
    if let Some(s) = raw.get("phone").and_then(|v| v.as_str()) {
        d.phone = Some(
            s.trim_start_matches("Telefon: ")
                .trim_start_matches("Phone: ")
                .trim()
                .to_string(),
        );
    }
    if let Some(s) = raw.get("website").and_then(|v| v.as_str()) {
        d.website = Some(s.to_string());
    }
    Ok(d)
}

static GERMAN_ADDRESS_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\d{5})\s+([A-ZÄÖÜ][A-Za-zÄÖÜäöüß\-/. ]{1,40})").unwrap());

fn parse_german_address(addr: &str) -> (Option<String>, Option<String>) {
    if let Some(cap) = GERMAN_ADDRESS_RE.captures(addr) {
        return (
            cap.get(1).map(|m| m.as_str().to_string()),
            cap.get(2).map(|m| m.as_str().trim().to_string()),
        );
    }
    (None, None)
}

/// Record a freshly-extracted place in the `seen` set and report whether it is
/// new (i.e. should be kept). `domain_key` is the dedup key — the website domain
/// when the place has one, otherwise the maps URL. `raw_url` is the place's maps
/// URL, which is *always* registered so the navigation fast-path can skip exact
/// repeats later, even for a place whose dedup key is its domain.
///
/// Note: `seen` mixes two key namespaces — bare website domains (`example.de`)
/// and full `https://…` URLs. They can never collide because a domain has no
/// URL scheme.
fn register_place(seen: &mut HashSet<String>, domain_key: &str, raw_url: &str) -> bool {
    let is_new = seen.insert(domain_key.to_string());
    seen.insert(raw_url.to_string());
    is_new
}

/// Map a seed to a value in `0..=max_ms` (inclusive). Non-cryptographic — used
/// only to spread out the delay between place visits, so a weak seed is fine.
fn jitter_ms(seed: u64, max_ms: u64) -> u64 {
    if max_ms == 0 { 0 } else { seed % (max_ms + 1) }
}

/// A cheap, std-only seed for [`jitter_ms`]. Mixes the wall-clock nanoseconds
/// with a process-wide call counter through `DefaultHasher` (SipHash), so the
/// result is well-distributed across the `u64` range and — thanks to the
/// counter — never repeats nor correlates between successive calls, even if the
/// clock is coarse. Not cryptographic; only used to de-regularise delays.
fn time_seed() -> u64 {
    use std::hash::Hasher;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_u64(nanos);
    hasher.write_u64(count);
    hasher.finish()
}

/// Parse the `@<lat>,<lng>` segment that Google Maps embeds in place URLs,
/// e.g. `https://www.google.com/maps/place/.../@52.5200,13.4050,17z/...`.
/// Returns `(None, None)` if the URL has no coordinate segment.
fn parse_coords_from_maps_url(url: &str) -> (Option<f64>, Option<f64>) {
    static COORDS_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"@(-?\d+\.\d+),(-?\d+\.\d+)").unwrap());
    if let Some(cap) = COORDS_RE.captures(url) {
        let lat = cap.get(1).and_then(|m| m.as_str().parse::<f64>().ok());
        let lng = cap.get(2).and_then(|m| m.as_str().parse::<f64>().ok());
        return (lat, lng);
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_de_address() {
        let (pc, city) = parse_german_address("Hauptstr. 12, 10115 Berlin, Deutschland");
        assert_eq!(pc.as_deref(), Some("10115"));
        assert!(city.unwrap().starts_with("Berlin"));
    }

    #[test]
    fn website_domain_from_url() {
        let p = PlaceDetailRaw {
            website: Some("https://www.example.de/path?x=1".to_string()),
            ..Default::default()
        };
        assert_eq!(p.website_domain().as_deref(), Some("example.de"));
    }

    #[test]
    fn register_place_dedup() {
        let mut seen = HashSet::new();
        // (a) New domain → kept, both keys registered.
        assert!(register_place(
            &mut seen,
            "example.de",
            "https://maps.google.com/place/A"
        ));
        // (b) Same domain via a *different* URL → discarded.
        assert!(!register_place(
            &mut seen,
            "example.de",
            "https://maps.google.com/place/B"
        ));
        // (c) New website-less place (key == raw URL) → kept.
        assert!(register_place(
            &mut seen,
            "https://maps.google.com/place/C",
            "https://maps.google.com/place/C"
        ));
        // (d) The same website-less place again → discarded.
        assert!(!register_place(
            &mut seen,
            "https://maps.google.com/place/C",
            "https://maps.google.com/place/C"
        ));
        // Every visited raw URL is registered for the fast-path.
        assert!(seen.contains("https://maps.google.com/place/A"));
        assert!(seen.contains("https://maps.google.com/place/B"));
    }

    #[test]
    fn config_defaults() {
        let c = ScraperConfig::default();
        assert!(c.headless);
        assert!(c.enrich);
        assert_eq!(c.max_scroll_iterations, 30);
        assert_eq!(c.max_places, None);
        assert!(c.proxy.is_none());
        assert!(c.user_agent.is_none());
        assert!(c.browserless_url.is_none());
        assert_eq!(c.place_panel_jitter, Duration::from_millis(750));
    }

    #[test]
    fn jitter_within_bounds() {
        // Zero jitter is disabled regardless of seed.
        assert_eq!(jitter_ms(123_456, 0), 0);
        // Any seed maps into 0..=max inclusive.
        for seed in [0u64, 1, 750, 751, u64::MAX] {
            assert!(jitter_ms(seed, 750) <= 750);
        }
        assert_eq!(jitter_ms(750, 750), 750);
        assert_eq!(jitter_ms(751, 750), 0);
    }

    #[test]
    fn check_proxy_rejects_whitespace() {
        assert!(check_proxy("http://user:pass@host:8080").is_ok());
        assert!(check_proxy("socks5://10.0.0.1:1080").is_ok());
        // Whitespace makes a malformed proxy Chrome would silently ignore.
        assert!(check_proxy("http://h:1 --disable-web-security").is_err());
        assert!(check_proxy("http://h:1\t--foo").is_err());
    }

    #[test]
    fn redact_url_strips_credentials_and_token() {
        assert_eq!(
            redact_url("http://user:pass@proxy.example:8080"),
            "http://proxy.example:8080/"
        );
        assert_eq!(
            redact_url("wss://chrome.browserless.io?token=secret123"),
            "wss://chrome.browserless.io/"
        );
        assert_eq!(redact_url("not a url"), "<redacted>");
    }

    #[test]
    fn parses_coords_from_maps_url() {
        let url = "https://www.google.com/maps/place/Cafe/@52.5200066,13.404954,17z/data=abc";
        let (lat, lng) = parse_coords_from_maps_url(url);
        assert_eq!(lat, Some(52.5200066));
        assert_eq!(lng, Some(13.404954));
    }

    #[test]
    fn coords_negative_and_missing() {
        let (lat, lng) =
            parse_coords_from_maps_url("https://maps.google.com/.../@-33.8688,151.2093,15z");
        assert_eq!(lat, Some(-33.8688));
        assert_eq!(lng, Some(151.2093));
        assert_eq!(
            parse_coords_from_maps_url("https://example.com/no-coords"),
            (None, None)
        );
    }
}
