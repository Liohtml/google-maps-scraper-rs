//! Run with:  cargo run --example scrape_obscura
//!
//! Same as scrape_one but uses Obscura instead of Chrome.
//! Requires the `obscura` binary on PATH or set via OBSCURA_BIN env var.

use google_maps_scraper::{MapsScraper, ScraperConfig};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = match env::var("OBSCURA_BIN") {
        Ok(path) => ScraperConfig::obscura_at(path, 9222),
        Err(_) => ScraperConfig::obscura(),
    };
    let scraper = MapsScraper::launch(cfg).await?;
    let places = scraper.search("coffee shop Berlin").await?;

    println!("Got {} places", places.len());
    for p in places.iter().take(20) {
        println!(
            " - {:50} | {:?} | {:?}",
            p.name,
            p.website.as_deref().unwrap_or(""),
            p.phone.as_deref().unwrap_or("")
        );
    }

    scraper.close().await?;
    Ok(())
}
