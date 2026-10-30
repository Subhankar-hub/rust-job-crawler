mod crawler;
mod extract;
mod models;
mod robots;
mod search;
mod store;

use anyhow::{bail, Context, Result};
use clap::{ArgAction, Parser};
use reqwest::{redirect::Policy, Client};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};
use url::Url;

#[derive(Debug, Parser)]
#[command(name = "job-crawler", version, about = "Discover and crawl company career sites and job boards for keyword-matched job postings")]
struct Args {
    /// Keyword or phrase to match. Repeat the flag or use comma-separated values.
    #[arg(long = "keyword", value_delimiter = ',', action = ArgAction::Append)]
    keywords: Vec<String>,

    /// Optional file containing one keyword or phrase per line.
    #[arg(long)]
    keywords_file: Option<PathBuf>,

    /// Starting URL or company homepage. May be repeated.
    #[arg(long = "seed", action = ArgAction::Append)]
    seeds: Vec<String>,

    /// Optional file containing one starting URL per line.
    #[arg(long)]
    seed_file: Option<PathBuf>,

    /// Discover candidate company/job pages via Brave Search before crawling.
    #[arg(long, default_value_t = false)]
    discover: bool,

    /// Override generated search queries. May be repeated; used with --discover.
    #[arg(long = "search-query", action = ArgAction::Append)]
    search_queries: Vec<String>,

    /// Brave Search country code (for example IN, US, GB).
    #[arg(long, default_value = "IN")]
    country: String,

    /// Location text included in generated search queries.
    #[arg(long, default_value = "India")]
    location: String,

    /// Maximum number of HTML pages to crawl.
    #[arg(long, default_value_t = 250)]
    max_pages: usize,

    /// Maximum link depth from each starting URL.
    #[arg(long, default_value_t = 3)]
    max_depth: usize,

    /// Minimum delay between requests to the same origin, in milliseconds.
    #[arg(long, default_value_t = 1500)]
    delay_ms: u64,

    /// Maximum bytes downloaded for one response.
    #[arg(long, default_value_t = 2_000_000)]
    max_body_bytes: usize,

    /// Require every configured keyword to match. Default behavior is any keyword.
    #[arg(long, default_value_t = false)]
    require_all: bool,

    /// SQLite database path.
    #[arg(long, default_value = "jobs.sqlite")]
    db: PathBuf,

    /// Optional CSV export path.
    #[arg(long)]
    csv: Option<PathBuf>,

    /// Optional JSON export path.
    #[arg(long)]
    json: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(log_filter)
        .with_target(false)
        .init();

    let args = Args::parse();
    let keywords = load_keywords(&args)?;
    if keywords.is_empty() {
        bail!("Provide at least one --keyword or --keywords-file entry");
    }

    let client = Client::builder()
        .user_agent("rust-job-crawler/0.1 (respects robots.txt)")
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        // Redirects are followed manually so every redirect destination is validated.
        .redirect(Policy::none())
        .build()
        .context("Could not build HTTP client")?;

    let mut seeds = load_seeds(&args)?;
    if args.discover {
        let key = std::env::var("BRAVE_SEARCH_API_KEY")
            .context("--discover requires BRAVE_SEARCH_API_KEY in the environment")?;
        let queries = if args.search_queries.is_empty() {
            search::make_queries(&keywords, &args.location)
        } else {
            args.search_queries.clone()
        };
        info!(query_count = queries.len(), "Discovering candidate job and career pages with Brave Search");
        let discovered = search::discover_urls(&client, &key, &queries, &args.country, 20).await?;
        info!(count = discovered.len(), "Search discovery returned candidate URLs");
        seeds.extend(discovered);
    }

    if seeds.is_empty() {
        bail!("No starting URLs found. Supply --seed/--seed-file or enable --discover.");
    }

    let config = crawler::CrawlerConfig {
        max_pages: args.max_pages.max(1),
        max_depth: args.max_depth,
        default_delay: Duration::from_millis(args.delay_ms),
        max_body_bytes: args.max_body_bytes.max(64_000),
        require_all_keywords: args.require_all,
    };

    info!(keywords = ?keywords, seeds = seeds.len(), "Starting job crawl");
    let postings = crawler::crawl(&client, seeds, &keywords, &config).await?;
    let saved = store::save_postings(&args.db, &postings)?;
    info!(matched = postings.len(), saved, database = %args.db.display(), "Stored matching postings");

    if let Some(path) = args.csv {
        let count = store::export_csv(&args.db, &path)?;
        info!(count, path = %path.display(), "Exported CSV");
    }
    if let Some(path) = args.json {
        let count = store::export_json(&args.db, &path)?;
        info!(count, path = %path.display(), "Exported JSON");
    }

    println!("\nMatched {} job posting(s). Database: {}", postings.len(), args.db.display());
    for (index, job) in postings.iter().take(30).enumerate() {
        let company = job.company.as_deref().unwrap_or(&job.source_domain);
        let location = job.location.as_deref().unwrap_or("Location not detected");
        println!("{}. {} | {} | {} | score {}", index + 1, job.title, company, location, job.score);
        println!("   {}", job.url);
        println!("   Matched: {}", job.matched_keywords.join(", "));
    }
    if postings.is_empty() {
        warn!("No matching job postings found. Increase --max-pages/--max-depth, refine keywords, or add company career-page seeds.");
    }
    Ok(())
}

fn load_keywords(args: &Args) -> Result<Vec<String>> {
    let mut values = args.keywords.clone();
    if let Some(path) = &args.keywords_file {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read keywords file {}", path.display()))?;
        values.extend(content.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_string));
    }
    let mut output = Vec::new();
    for value in values {
        let cleaned = value.trim().to_string();
        if !cleaned.is_empty() && !output.iter().any(|existing: &String| existing.eq_ignore_ascii_case(&cleaned)) {
            output.push(cleaned);
        }
    }
    Ok(output)
}

fn load_seeds(args: &Args) -> Result<Vec<Url>> {
    let mut values = args.seeds.clone();
    if let Some(path) = &args.seed_file {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read seed file {}", path.display()))?;
        values.extend(content.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_string));
    }
    let mut output = Vec::new();
    for value in values {
        let candidate = if value.starts_with("http://") || value.starts_with("https://") {
            value
        } else {
            format!("https://{value}")
        };
        match Url::parse(&candidate) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => output.push(url),
            Ok(_) => warn!(seed = %candidate, "Ignoring non-HTTP seed"),
            Err(error) => warn!(seed = %candidate, error = %error, "Ignoring invalid seed URL"),
        }
    }
    Ok(output)
}
