use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use url::Url;

const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    url: String,
}

/// Discover starting URLs. Brave Search is optional; direct seeds work without it.
pub async fn discover_urls(
    client: &Client,
    api_key: &str,
    queries: &[String],
    country: &str,
    count: usize,
) -> Result<Vec<Url>> {
    let mut urls = Vec::new();

    for query in queries {
        let count_text = count.min(20).to_string();
        let response = client
            .get(BRAVE_ENDPOINT)
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .query(&[
                ("q", query.as_str()),
                ("country", country),
                ("search_lang", "en"),
                ("count", count_text.as_str()),
            ])
            .send()
            .await
            .with_context(|| format!("Brave Search request failed for query: {query}"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Brave Search returned HTTP {status}: {}", body.chars().take(500).collect::<String>());
        }

        let data: BraveResponse = response
            .json()
            .await
            .context("Could not parse Brave Search response")?;

        if let Some(web) = data.web {
            for result in web.results {
                if let Ok(url) = Url::parse(&result.url) {
                    if matches!(url.scheme(), "http" | "https") {
                        urls.push(url);
                    }
                }
            }
        }
    }

    Ok(urls)
}

pub fn make_queries(keywords: &[String], location: &str) -> Vec<String> {
    keywords
        .iter()
        .take(12)
        .map(|keyword| {
            let escaped = keyword.replace('"', "").trim().to_string();
            format!("\"{escaped}\" (jobs OR careers OR vacancies OR openings OR hiring) {location}")
        })
        .collect()
}
