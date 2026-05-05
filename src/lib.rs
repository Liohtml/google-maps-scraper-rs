//! # google-maps-scraper
//!
//! Apify-style Google Maps scraper for Rust. Drives a headless browser
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
//! **Obscura (recommended):** Download the
//! [Obscura binary](https://github.com/h4ckf0r0day/obscura/releases)
//! and place it on your `PATH`. No Chrome needed.
//!
//! **Chrome (legacy):** Chrome / Chromium installed locally. On macOS the
//! auto-detect finds `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`.
//! On Linux: `apt install chromium`. Override via the `CHROME` env var.
//!
//! ## Quick start
//!
//! ```no_run
//! use google_maps_scraper::{MapsScraper, ScraperConfig};
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let scraper = MapsScraper::launch(ScraperConfig::obscura()).await?;
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
//! Obscura's stealth mode includes fingerprint randomization, TLS mimicry
//! (Chrome 145), tracker blocking (3,520 domains), and `navigator.webdriver`
//! masking. For high-volume scraping use a residential proxy, slow down
//! delays between queries, and don't reuse a single session for hundreds
//! of queries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cdp;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

const SEARCH_URL_BASE: &str = "https://www.google.com/maps";

/// Which browser backend to use.
#[derive(Debug, Clone, Default)]
pub enum BrowserBackend {
    /// Standard headless Chrome (requires Chrome/Chromium installed).
    #[default]
    Chrome,
    /// Obscura headless browser — lightweight, stealthy, no Chrome needed.
    Obscura {
        /// Path to the Obscura binary. Defaults to `"obscura"` (on PATH).
        bin: Option<PathBuf>,
        /// CDP port Obscura listens on. Defaults to 9222.
        port: u16,
    },
}

/// All errors this crate produces.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Chrome failed to launch (often: not installed, or missing libs on Linux).
    #[error("chrome launch failed: {0}")]
    ChromeLaunch(String),

    /// Obscura failed to start.
    #[error("obscura launch failed: {0}")]
    ObscuraLaunch(String),

    /// Driving the page failed (navigation, evaluation, …).
    #[error("page error: {0}")]
    Page(String),

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
    /// 5-digit German postcode if address parses as DE format.
    pub postcode: Option<String>,
    /// City detected in the address.
    pub city: Option<String>,
    /// First phone number listed.
    pub phone: Option<String>,
    /// External website URL listed in the panel.
    pub website: Option<String>,
    /// The Google Maps URL (`/maps/place/...`) of this entry.
    pub maps_url: Option<String>,
    /// The query that produced this hit.
    pub source_query: Option<String>,
}

/// Scraper configuration.
#[derive(Debug, Clone)]
pub struct ScraperConfig {
    /// Which browser backend to use. Default: Chrome.
    pub backend: BrowserBackend,
    /// Run Chrome in headless mode (default: true). Use `false` for debugging.
    /// Ignored when `backend` is `Obscura` (always headless).
    pub headless: bool,
    /// How many `scrollTop = scrollHeight` iterations to perform per search
    /// before assuming the feed is exhausted. Default: 30.
    pub max_scroll_iterations: usize,
    /// If false, skip clicking each result and only return surface data
    /// (name, maps_url) — much faster but no website / phone. Default: true.
    pub enrich: bool,
    /// Delay between consecutive HTTP requests inside Chrome.
    pub between_query_delay: Duration,
    /// Per-place delay after clicking (gives the panel time to render).
    pub place_panel_delay: Duration,
}

impl Default for ScraperConfig {
    fn default() -> Self {
        Self {
            backend: BrowserBackend::default(),
            headless: true,
            max_scroll_iterations: 30,
            enrich: true,
            between_query_delay: Duration::from_secs(2),
            place_panel_delay: Duration::from_millis(1500),
        }
    }
}

impl ScraperConfig {
    /// Create a config that uses Obscura with stealth mode. No Chrome needed.
    pub fn obscura() -> Self {
        Self {
            backend: BrowserBackend::Obscura {
                bin: None,
                port: 9222,
            },
            ..Default::default()
        }
    }

    /// Create a config that uses Obscura at a specific binary path.
    pub fn obscura_at(bin: impl Into<PathBuf>, port: u16) -> Self {
        Self {
            backend: BrowserBackend::Obscura {
                bin: Some(bin.into()),
                port,
            },
            ..Default::default()
        }
    }
}

// ───────── unified page driver (enum dispatch) ─────────

enum Driver {
    Chrome {
        page: Page,
        _browser: Browser,
        _handler: tokio::task::JoinHandle<()>,
    },
    Obscura {
        cdp: cdp::CdpPage,
        _child: tokio::process::Child,
    },
}

impl Driver {
    async fn goto(&self, url: &str) -> Result<()> {
        match self {
            Driver::Chrome { page, .. } => {
                page.goto(url).await.map_err(|e| Error::Page(e.to_string()))?;
                Ok(())
            }
            Driver::Obscura { cdp, .. } => {
                cdp.goto(url).await.map_err(Error::Page)
            }
        }
    }

    async fn evaluate_json(&self, js: &str) -> Result<serde_json::Value> {
        match self {
            Driver::Chrome { page, .. } => {
                let val: serde_json::Value = page.evaluate(js).await?.into_value().unwrap_or_default();
                Ok(val)
            }
            Driver::Obscura { cdp, .. } => {
                cdp.evaluate(js).await.map_err(Error::Page)
            }
        }
    }

    async fn find_element_exists(&self, sel: &str) -> Result<bool> {
        match self {
            Driver::Chrome { page, .. } => Ok(page.find_element(sel).await.is_ok()),
            Driver::Obscura { cdp, .. } => cdp.find_element(sel).await.map_err(Error::Page),
        }
    }

    async fn click_selector(&self, sel: &str) -> Result<()> {
        match self {
            Driver::Chrome { page, .. } => {
                if let Ok(el) = page.find_element(sel).await {
                    let _ = el.click().await;
                }
                Ok(())
            }
            Driver::Obscura { cdp, .. } => cdp.click(sel).await.map_err(Error::Page),
        }
    }

    /// Set Google consent cookies to bypass the consent redirect.
    async fn set_consent_cookies(&self) {
        match self {
            Driver::Chrome { page, .. } => {
                // Chrome: set via JS after navigating to google.com first.
                let _ = page.goto("https://www.google.com").await;
                let _ = page.evaluate(
                    "document.cookie = 'CONSENT=YES+; domain=.google.com; path=/; max-age=63072000'"
                ).await;
                let _ = page.evaluate(
                    "document.cookie = 'SOCS=CAESEwgDEgk2MDI1MDUwNA; domain=.google.com; path=/; max-age=63072000'"
                ).await;
            }
            Driver::Obscura { cdp, .. } => {
                let _ = cdp.set_cookies(&[
                    ("CONSENT", "YES+", ".google.com"),
                    ("SOCS", "CAESEwgDEgk2MDI1MDUwNA", ".google.com"),
                ]).await;
            }
        }
    }
}

// ───────── scraper ─────────

/// The scraper. Holds an active browser process (Chrome or Obscura).
///
/// Drop the scraper to close the browser.
pub struct MapsScraper {
    driver: Driver,
    cfg: ScraperConfig,
}

impl MapsScraper {
    /// Launch a browser and return a ready-to-use scraper.
    ///
    /// With `BrowserBackend::Chrome` this launches headless Chrome.
    /// With `BrowserBackend::Obscura` it spawns the Obscura binary in
    /// `serve --stealth` mode and connects via CDP WebSocket.
    pub async fn launch(cfg: ScraperConfig) -> Result<Self> {
        match &cfg.backend {
            BrowserBackend::Chrome => Self::launch_chrome(cfg).await,
            BrowserBackend::Obscura { bin, port } => {
                let bin = bin.clone();
                let port = *port;
                Self::launch_obscura(cfg, bin, port).await
            }
        }
    }

    async fn launch_chrome(cfg: ScraperConfig) -> Result<Self> {
        let mut builder = BrowserConfig::builder()
            .arg("--lang=en-US,en")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-blink-features=AutomationControlled")
            .arg("--window-size=1280,1024")
            .arg("--user-agent=Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36");
        if !cfg.headless {
            builder = builder.with_head();
        }
        let browser_cfg = builder
            .build()
            .map_err(|e| Error::ChromeLaunch(e.to_string()))?;

        let (browser, mut handler) = Browser::launch(browser_cfg)
            .await
            .map_err(|e| Error::ChromeLaunch(e.to_string()))?;

        let handler_task = tokio::spawn(async move { while (handler.next().await).is_some() {} });

        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| Error::Page(e.to_string()))?;

        Ok(Self {
            driver: Driver::Chrome {
                page,
                _browser: browser,
                _handler: handler_task,
            },
            cfg,
        })
    }

    async fn launch_obscura(
        cfg: ScraperConfig,
        bin: Option<PathBuf>,
        port: u16,
    ) -> Result<Self> {
        let bin_path = bin.unwrap_or_else(|| PathBuf::from("obscura"));

        info!(bin = %bin_path.display(), port, "starting Obscura");

        let child = tokio::process::Command::new(&bin_path)
            .args(["serve", "--port", &port.to_string(), "--stealth"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Error::ObscuraLaunch(format!(
                    "failed to spawn {}: {e}. Is Obscura installed?",
                    bin_path.display()
                ))
            })?;

        // Wait for Obscura to bind.
        let mut connected = false;
        for attempt in 1..=20 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            debug!(attempt, "probing Obscura CDP endpoint");
            if tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .is_ok()
            {
                connected = true;
                break;
            }
        }
        if !connected {
            return Err(Error::ObscuraLaunch(format!(
                "Obscura did not start within 5 s on port {port}"
            )));
        }

        let ws_url = format!("ws://127.0.0.1:{port}");
        info!("Obscura ready, connecting via {ws_url}");

        let cdp_page = cdp::CdpPage::connect(&ws_url)
            .await
            .map_err(Error::ObscuraLaunch)?;

        Ok(Self {
            driver: Driver::Obscura {
                cdp: cdp_page,
                _child: child,
            },
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
        // Navigate to google.com first, set consent cookies, then go to maps.
        self.driver.goto("https://www.google.com").await?;
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Set consent cookies via JS on the google.com domain.
        self.driver
            .evaluate_json("document.cookie = 'CONSENT=YES+; domain=.google.com; path=/; max-age=63072000'")
            .await
            .ok();
        self.driver
            .evaluate_json("document.cookie = 'SOCS=CAESEwgDEgk2MDI1MDUwNQ; domain=.google.com; path=/; max-age=63072000'")
            .await
            .ok();

        // Also set via CDP if available.
        self.driver.set_consent_cookies().await;

        // Now navigate to Maps.
        self.driver.goto("https://www.google.com/maps").await?;
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Check if we still landed on consent, handle it.
        dismiss_consent(&self.driver).await;

        let mut out: Vec<Place> = Vec::new();
        let mut seen_keys: HashSet<String> = HashSet::new();

        for (i, q) in queries.iter().enumerate() {
            info!(progress = i + 1, total = queries.len(), query = %q, "scanning");
            let url = format!(
                "{}/search/{}/",
                SEARCH_URL_BASE,
                urlencoding::encode(q)
            );
            if let Err(e) = self.driver.goto(&url).await {
                warn!("goto error: {e}");
                continue;
            }
            tokio::time::sleep(self.cfg.between_query_delay).await;

            // Wait for results panel to appear.
            let _ = self.driver.find_element_exists("div[role='feed']").await;

            // Scroll until stable.
            let _ = scroll_feed(&self.driver, self.cfg.max_scroll_iterations).await;

            // Collect place URLs in the feed.
            let urls = collect_place_urls(&self.driver).await.unwrap_or_default();
            info!(found = urls.len(), "feed collected");

            if !self.cfg.enrich {
                for u in urls {
                    if seen_keys.insert(u.clone()) {
                        out.push(Place {
                            name: String::new(),
                            maps_url: Some(u),
                            source_query: Some((*q).to_string()),
                            ..Default::default()
                        });
                    }
                }
                continue;
            }

            for place_url in urls {
                if let Err(e) = self.driver.goto(&place_url).await {
                    warn!("place goto: {e}");
                    continue;
                }
                tokio::time::sleep(self.cfg.place_panel_delay).await;
                let detail = match extract_place_details(&self.driver).await {
                    Ok(d) => d,
                    Err(e) => {
                        warn!("extract: {e}");
                        continue;
                    }
                };

                // Dedup key: prefer website domain, else maps_url.
                let key = detail
                    .website_domain()
                    .unwrap_or_else(|| place_url.clone());
                if !seen_keys.insert(key) {
                    continue;
                }

                let (postcode, city) =
                    parse_german_address(detail.address.as_deref().unwrap_or(""));
                out.push(Place {
                    name: detail.name.unwrap_or_default(),
                    address: detail.address,
                    postcode,
                    city,
                    phone: detail.phone,
                    website: detail.website,
                    maps_url: Some(place_url),
                    source_query: Some((*q).to_string()),
                });
            }
        }

        Ok(out)
    }

    /// Close the browser cleanly. (Drop also works.)
    pub async fn close(self) -> Result<()> {
        match self.driver {
            Driver::Chrome { mut _browser, _handler, .. } => {
                let _ = _browser.close().await;
                _handler.abort();
            }
            Driver::Obscura { mut _child, .. } => {
                let _ = _child.kill().await;
            }
        }
        Ok(())
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

async fn dismiss_consent(driver: &Driver) {
    // Check if we landed on the consent page.
    let on_consent = driver
        .evaluate_json("window.location.href.includes('consent.google')")
        .await
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !on_consent {
        // Try the in-page consent overlay (Chrome usually shows this).
        let selectors = [
            "button[aria-label*='Alle akzeptieren']",
            "button[aria-label*='Accept all']",
            "button[aria-label*='Reject all']",
            "button[aria-label*='Alle ablehnen']",
        ];
        for sel in selectors {
            if driver.find_element_exists(sel).await.unwrap_or(false) {
                let _ = driver.click_selector(sel).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
                break;
            }
        }
        return;
    }

    // On the consent.google.com redirect page, submit the form via JS.
    info!("consent page detected, accepting cookies");
    let _ = driver
        .evaluate_json(
            r#"(() => {
                const btn = document.querySelector("button[aria-label*='akzeptieren']")
                    || document.querySelector("button[aria-label*='Accept']");
                if (btn) { btn.click(); return 'clicked'; }
                // Fallback: submit the first form that looks like consent.
                const form = document.querySelector("form[action*='consent']");
                if (form) { form.submit(); return 'submitted'; }
                return 'not_found';
            })()"#,
        )
        .await;

    // Wait for the consent redirect to complete.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // After consent, Obscura may still be on the consent page (form POST
    // redirect). Navigate back to maps explicitly.
    let _ = driver.goto("https://www.google.com/maps").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
}

async fn scroll_feed(driver: &Driver, max_iters: usize) -> Result<()> {
    let mut last_height = -1.0_f64;
    let mut stable = 0;
    for _ in 0..max_iters {
        let new_height: f64 = driver
            .evaluate_json(
                "(() => { const f = document.querySelector(\"div[role='feed']\"); if (!f) return -1; f.scrollTop = f.scrollHeight; return f.scrollHeight; })()",
            )
            .await?
            .as_f64()
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

async fn collect_place_urls(driver: &Driver) -> Result<Vec<String>> {
    let raw = driver
        .evaluate_json(
            "Array.from(document.querySelectorAll(\"div[role='feed'] a[href*='/maps/place/']\")).map(a => a.href)",
        )
        .await?;
    let urls: Vec<String> = serde_json::from_value(raw).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for u in urls {
        if seen.insert(u.clone()) {
            out.push(u);
        }
    }
    Ok(out)
}

async fn extract_place_details(driver: &Driver) -> Result<PlaceDetailRaw> {
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
    let raw = driver.evaluate_json(js).await?;
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

fn parse_german_address(addr: &str) -> (Option<String>, Option<String>) {
    let re = regex::Regex::new(r"(\d{5})\s+([A-ZÄÖÜ][A-Za-zÄÖÜäöüß\-/. ]{1,40})").unwrap();
    if let Some(cap) = re.captures(addr) {
        return (
            cap.get(1).map(|m| m.as_str().to_string()),
            cap.get(2).map(|m| m.as_str().trim().to_string()),
        );
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
    fn config_defaults() {
        let c = ScraperConfig::default();
        assert!(c.headless);
        assert!(c.enrich);
        assert_eq!(c.max_scroll_iterations, 30);
    }
}
