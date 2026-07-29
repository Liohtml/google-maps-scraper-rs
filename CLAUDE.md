# CLAUDE.md — maintainer guide for google-maps-scraper-rs

Apify-style Google Maps scraper for Rust (headless Chrome via CDP,
`chromiumoxide`). Published on crates.io as `google-maps-scraper` since 0.3.0.
Single-crate library, all code in `src/lib.rs`.

## Commands

```bash
cargo test                             # unit + doc tests (no browser needed)
cargo fmt --all -- --check             # formatting (CI-enforced)
cargo clippy --all-targets -- -D warnings
cargo audit                            # security advisories (CI-enforced)
cargo run --example scrape_one         # live smoke test (needs Chrome; set CHROME=<path>)
```

The unit tests cover only the pure parsing helpers. The browser-driving
pipeline (`launch`/`search_many`/scroll/extract) has **no automated tests**
(issue #27) — a live run is the only end-to-end verification.

## CI / release automation

- `ci.yml` — build+test, fmt+clippy, cargo-audit on every PR and push to main.
- `release.yml` — on a `v*` tag push: verifies the tag matches
  `Cargo.toml` version, runs the full check suite + `cargo publish --dry-run`,
  publishes to crates.io (`CARGO_REGISTRY_TOKEN` secret, `release`
  environment), creates the GitHub release from the CHANGELOG section.
  Publish and release-creation steps are **idempotent** (skip if already done).

## Release process

1. Move the `## [Unreleased]` CHANGELOG entries into a new `## [X.Y.Z] - date`
   section; update the compare links at the bottom.
2. Bump `version` in `Cargo.toml`. Update the README install snippet if the
   minor changed.
3. PR → CI green → merge.
4. Tag the merge commit `vX.Y.Z` and push the tag — the workflow does the rest.
   (Remote Claude sessions cannot push tags; the maintainer pushes the tag, or
   creates the GitHub release in the UI, which also creates the tag.)

## SemVer caveats (important)

`ScraperConfig`, `Place`, and `Error` are **not** `#[non_exhaustive]` and have
all-public fields. Adding a field/variant is therefore a breaking change for
exhaustive constructors/matches → requires a **minor** bump while in 0.x.
`Place` gained `rating`/`reviews_count`/`category` after 0.3.0, so the next
release must be **0.4.0**. Consider `#[non_exhaustive]` before 1.0.

## Conventions

- Keep all DOM selectors in `src/lib.rs` close together; they are the brittle
  part and Google shifts them occasionally (README documents this promise).
- Every user-visible change gets a CHANGELOG entry under `[Unreleased]`.
- New `Place`/`ScraperConfig` fields are `Option<T>` and must degrade to
  `None`, never fail extraction.
- Selector changes should be validated against live Google Maps when possible
  (local Chrome or `BROWSERLESS_URL`); if not validated, say so in the PR.

## Team (subagents in `.claude/agents/`)

- `improvement-researcher` — read-only scout: researches concrete improvement
  ideas (ecosystem, advisories, competitor feature gaps, open issues) and
  reports prioritized proposals.
- `release-engineer` — executes the release process above end-to-end.
