//! Run with:  cargo run --example scrape_one
//!
//! Scrapes one Google Maps query and prints the results.
//! Requires Chrome installed locally.

use google_maps_scraper::{MapsScraper, ScraperConfig};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let scraper = MapsScraper::launch(ScraperConfig::default()).await?;
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
