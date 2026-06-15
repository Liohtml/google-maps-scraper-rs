# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `Drop` implementation for `MapsScraper` so the CDP handler task is aborted
  even when `close()` is not called (panic / early return).
- `ScraperConfig::max_places` — cap the number of unique places returned per query.
- `ScraperConfig::nav_timeout` — bound every page navigation / feed-wait step.
- `ScraperConfig::proxy` (and `PROXY_URL` env fallback) — launch Chrome behind a proxy.
- `ScraperConfig::browserless_url` (and `BROWSERLESS_URL` env fallback) — connect to a
  remote Chrome over the DevTools WebSocket instead of launching a local browser.
- `Place::latitude` / `Place::longitude` — parsed from the `@lat,lng` segment of `maps_url`.
- `ScraperConfig::user_agent` — optional `User-Agent` override.
- GitHub Actions CI: build, test, and clippy on push / pull request.

### Changed
- The hardcoded (and stale, macOS-specific) Chrome user-agent is no longer set
  by default. Chrome now reports its own current UA unless `user_agent` is set,
  avoiding a UA/TLS-fingerprint and UA/host-OS mismatch.
- Headless runs now use Chrome's **new** headless mode (`--headless=new`)
  instead of the old `--headless`, so the reported user-agent no longer contains
  the `HeadlessChrome` bot-detection token.
- The proxy value (`proxy` / `PROXY_URL`) is now rejected at launch if it
  contains whitespace (a malformed value Chrome would silently ignore, falling
  back to a direct connection and leaking the real IP).
- Page navigations are wrapped in `tokio::time::timeout` and fail with a clear
  error instead of hanging indefinitely.
- Collected feed URLs are filtered to the `https://` scheme before navigation,
  preventing `javascript:` / `data:` URL execution.
- The German address regex is compiled once via `LazyLock` instead of on every call.
- Upgraded `chromiumoxide` 0.7 → 0.9 and `thiserror` 1 → 2.

### Fixed
- The working tab opened in `search_many` is now closed before returning,
  fixing a tab/memory leak when a scraper is reused for many searches.

## [0.1.0] - 2026-05-02

### Added
- Initial release of `google-maps-scraper`.
- `MapsScraper::launch` — start a headless Chrome and return a scraper instance.
- `MapsScraper::search` — scrape Google Maps for a single query.
- `MapsScraper::search_many` — run multiple queries in one browser session with
  deduplication by website domain (or maps URL when no website).
- `ScraperConfig` — configure headless mode, scroll iterations, enrichment, and delays.
- `Place` — structured output with name, address, postcode, city, phone, website, maps_url.
- German postcode/city parsing via `parse_german_address`.
- Cookie consent auto-dismiss for German and English Google interfaces.
