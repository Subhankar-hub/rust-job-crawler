use crate::models::{JobPosting, PageData};
use chrono::Utc;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;
use std::collections::HashSet;
use url::Url;

const JOB_SIGNALS: &[&str] = &[
    "apply now",
    "apply for this job",
    "job description",
    "responsibilities",
    "qualifications",
    "requirements",
    "what you will do",
    "what we're looking for",
    "what we are looking for",
    "employment type",
    "full-time",
    "full time",
    "part-time",
    "salary",
    "compensation",
];

const CAREER_TERMS: &[&str] = &[
    "career", "careers", "job", "jobs", "vacan", "opening", "position", "requisition",
    "opportunit", "recruit", "talent", "hiring", "role", "work-with-us", "join-our-team", "join-us", "join us", "employment",
];

pub fn parse_page(
    final_url: Url,
    html_text: &str,
    discovered_from: &str,
    keywords: &[String],
    require_all: bool,
) -> PageData {
    let document = Html::parse_document(html_text);
    let title_selector = Selector::parse("title").expect("valid selector");
    let h1_selector = Selector::parse("h1").expect("valid selector");
    let body_selector = Selector::parse("body").expect("valid selector");
    let anchor_selector = Selector::parse("a[href]").expect("valid selector");
    let script_selector = Selector::parse("script[type='application/ld+json']").expect("valid selector");
    let canonical_selector = Selector::parse("link[rel='canonical']").expect("valid selector");

    let html_title = document
        .select(&title_selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .trim()
        .to_string();
    let h1 = document
        .select(&h1_selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .trim()
        .to_string();
    let title = if !h1.is_empty() { h1.clone() } else { html_title.clone() };
    let body_text = document
        .select(&body_selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let page_text = normalize_text(&body_text);
    let page_text = truncate_chars(&page_text, 20_000);

    let body_lower = page_text.to_lowercase();
    let body_career_signal = [
        "current openings", "open positions", "open roles", "career opportunities",
        "join our team", "work with us", "we are hiring", "we're hiring",
    ].iter().any(|phrase| body_lower.contains(phrase));
    let is_career_page = looks_like_career_page(&final_url, &html_title, &h1) || body_career_signal;
    let canonical_url = document
        .select(&canonical_selector)
        .next()
        .and_then(|node| node.value().attr("href"))
        .and_then(|href| final_url.join(href).ok())
        .filter(|candidate| candidate.scheme() == "http" || candidate.scheme() == "https")
        .unwrap_or_else(|| final_url.clone());

    let mut structured_jobs = Vec::new();
    for script in document.select(&script_selector) {
        let json = script.text().collect::<String>();
        if let Ok(value) = serde_json::from_str::<Value>(&json) {
            collect_job_postings(&value, &mut structured_jobs);
        }
    }

    let mut postings = Vec::new();
    let mut seen_job_urls = HashSet::new();
    for value in structured_jobs {
        if let Some(mut posting) = posting_from_json_ld(
            &value,
            &canonical_url,
            discovered_from,
            &page_text,
        ) {
            posting.apply_url = find_apply_url(&document, &canonical_url);
            if let Some(posting) = match_keywords(posting, keywords, require_all) {
                if seen_job_urls.insert(posting.url.clone()) {
                    postings.push(posting);
                }
            }
        }
    }

    // Fallback for sites that do not publish schema.org JobPosting JSON-LD.
    if postings.is_empty() && looks_like_job_detail(&final_url, &html_title, &h1, &page_text) {
        let fallback = JobPosting {
            url: canonical_url.to_string(),
            apply_url: find_apply_url(&document, &final_url),
            title: if title.is_empty() { "Untitled job posting".to_string() } else { title },
            company: None,
            location: find_location_hint(&page_text),
            employment_type: find_employment_type(&page_text),
            description: truncate_chars(&page_text, 8_000),
            date_posted: None,
            valid_through: None,
            source_domain: domain_of(&final_url),
            discovered_from: discovered_from.to_string(),
            matched_keywords: Vec::new(),
            score: 0,
            found_at: Utc::now().to_rfc3339(),
        };
        if let Some(posting) = match_keywords(fallback, keywords, require_all) {
            postings.push(posting);
        }
    }

    let mut links = Vec::new();
    let mut link_seen = HashSet::new();
    for anchor in document.select(&anchor_selector) {
        let Some(href) = anchor.value().attr("href") else { continue };
        let Ok(mut target) = final_url.join(href) else { continue };
        if !matches!(target.scheme(), "http" | "https") {
            continue;
        }
        target.set_fragment(None);
        normalize_tracking_params(&mut target);
        if target == final_url || !link_seen.insert(target.as_str().to_string()) {
            continue;
        }
        if is_probably_asset(&target) {
            continue;
        }
        let anchor_text = normalize_text(&anchor.text().collect::<Vec<_>>().join(" "));
        let score = career_link_score(&target, &anchor_text);
        links.push((score, target));
    }
    links.sort_by(|a, b| b.0.cmp(&a.0));

    PageData {
        final_url,
        title: html_title,
        text: page_text,
        postings,
        links,
        is_career_page,
    }
}

fn collect_job_postings(value: &Value, output: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_job_postings(item, output);
            }
        }
        Value::Object(map) => {
            let is_job = match map.get("@type") {
                Some(Value::String(value)) => value.eq_ignore_ascii_case("JobPosting"),
                Some(Value::Array(values)) => values.iter().any(|value| {
                    value.as_str().map(|s| s.eq_ignore_ascii_case("JobPosting")).unwrap_or(false)
                }),
                _ => false,
            };
            if is_job {
                output.push(value.clone());
                return;
            }
            for child in map.values() {
                collect_job_postings(child, output);
            }
        }
        _ => {}
    }
}

fn posting_from_json_ld(
    value: &Value,
    page_url: &Url,
    discovered_from: &str,
    page_text: &str,
) -> Option<JobPosting> {
    let title = value.get("title")?.as_str()?.trim();
    if title.is_empty() {
        return None;
    }
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .map(clean_html_text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| page_text.to_string());
    let company = value
        .get("hiringOrganization")
        .and_then(|org| org.get("name").or(Some(org)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let location = extract_schema_location(value)
        .or_else(|| value.get("jobLocationType").and_then(Value::as_str).map(str::to_string))
        .or_else(|| find_location_hint(page_text));
    let employment_type = value
        .get("employmentType")
        .map(value_to_string)
        .filter(|s| !s.is_empty());
    let url = value
        .get("url")
        .and_then(Value::as_str)
        .and_then(|s| page_url.join(s).ok())
        .unwrap_or_else(|| page_url.clone());

    Some(JobPosting {
        url: url.to_string(),
        apply_url: None,
        title: title.to_string(),
        company,
        location,
        employment_type,
        description: truncate_chars(&description, 8_000),
        date_posted: value.get("datePosted").map(value_to_string).filter(|s| !s.is_empty()),
        valid_through: value.get("validThrough").map(value_to_string).filter(|s| !s.is_empty()),
        source_domain: domain_of(page_url),
        discovered_from: discovered_from.to_string(),
        matched_keywords: Vec::new(),
        score: 0,
        found_at: Utc::now().to_rfc3339(),
    })
}

fn extract_schema_location(value: &Value) -> Option<String> {
    let locations = value.get("jobLocation")?;
    let first = locations.as_array().and_then(|v| v.first()).unwrap_or(locations);
    let address = first.get("address").unwrap_or(first);
    let mut pieces = Vec::new();
    for key in ["addressLocality", "addressRegion", "addressCountry"] {
        if let Some(value) = address.get(key).map(value_to_string).filter(|s| !s.is_empty()) {
            pieces.push(value);
        }
    }
    if pieces.is_empty() { None } else { Some(pieces.join(", ")) }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(value_to_string).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        Value::Object(map) => map.get("name").map(value_to_string).unwrap_or_default(),
        _ => String::new(),
    }
}

fn match_keywords(mut posting: JobPosting, keywords: &[String], require_all: bool) -> Option<JobPosting> {
    let title = posting.title.to_lowercase();
    let searchable = format!("{} {}", title, posting.description.to_lowercase());
    let mut matched = Vec::new();
    let mut score = 0;
    for keyword in keywords {
        let needle = keyword.to_lowercase();
        if needle.is_empty() || !searchable.contains(&needle) {
            continue;
        }
        if title.contains(&needle) {
            score += 5;
        } else {
            score += 1;
        }
        matched.push(keyword.clone());
    }
    if matched.is_empty() || (require_all && matched.len() != keywords.len()) {
        return None;
    }
    posting.matched_keywords = matched;
    posting.score = score;
    Some(posting)
}

fn looks_like_career_page(url: &Url, title: &str, h1: &str) -> bool {
    let haystack = format!("{} {} {}", url.path(), title, h1).to_lowercase();
    CAREER_TERMS.iter().any(|term| haystack.contains(term))
}

fn looks_like_job_detail(url: &Url, title: &str, h1: &str, body: &str) -> bool {
    let path = url.path().to_lowercase();
    let path_signal = ["job", "jobs", "vacan", "opening", "position", "requisition", "opportunit"]
        .iter().any(|term| path.contains(term));
    let body_lower = body.to_lowercase();
    let signal_count = JOB_SIGNALS.iter().filter(|signal| body_lower.contains(**signal)).count();
    let title_lower = format!("{title} {h1}").to_lowercase();
    let generic_title = ["careers", "career opportunities", "open positions", "join our team", "jobs at"]
        .iter().any(|term| title_lower.trim() == *term);
    body.chars().count() > 350 && signal_count >= 2 && (path_signal || (!generic_title && !h1.is_empty()))
}

pub fn career_link_score(url: &Url, anchor_text: &str) -> i32 {
    let haystack = format!("{} {}", url.path(), anchor_text).to_lowercase();
    let mut score = 0;
    for term in CAREER_TERMS {
        if haystack.contains(term) {
            score += if ["job", "jobs", "vacan", "opening", "position"].contains(term) { 3 } else { 2 };
        }
    }
    if anchor_text.to_lowercase().contains("apply") { score += 4; }
    score
}

fn find_apply_url(document: &Html, base: &Url) -> Option<String> {
    let selector = Selector::parse("a[href]").ok()?;
    for anchor in document.select(&selector) {
        let text = normalize_text(&anchor.text().collect::<Vec<_>>().join(" ")).to_lowercase();
        if text.contains("apply") {
            if let Some(href) = anchor.value().attr("href") {
                if let Ok(url) = base.join(href) {
                    return Some(url.to_string());
                }
            }
        }
    }
    None
}

fn find_location_hint(text: &str) -> Option<String> {
    let re = Regex::new(r"(?i)(?:location|based in|job location)\s*[:\-]?\s*([A-Z][A-Za-z .,'()\-/]{2,60})").ok()?;
    re.captures(text).and_then(|captures| captures.get(1)).map(|m| m.as_str().trim().to_string())
}

fn find_employment_type(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    ["full-time", "full time", "part-time", "part time", "contract", "internship", "temporary"]
        .iter()
        .find(|term| lower.contains(**term))
        .map(|term| term.to_string())
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_html_text(html: &str) -> String {
    let document = Html::parse_fragment(html);
    normalize_text(&document.root_element().text().collect::<Vec<_>>().join(" "))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn domain_of(url: &Url) -> String {
    url.host_str().unwrap_or_default().to_string()
}

fn is_probably_asset(url: &Url) -> bool {
    let path = url.path().to_lowercase();
    [".pdf", ".jpg", ".jpeg", ".png", ".gif", ".svg", ".webp", ".zip", ".css", ".js", ".woff", ".woff2", ".ico"]
        .iter().any(|extension| path.ends_with(extension))
}

fn normalize_tracking_params(url: &mut Url) {
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key_lower = key.to_ascii_lowercase();
            !key_lower.starts_with("utm_")
                && !["source", "ref", "gh_src", "lever-source", "trk", "fbclid", "gclid"].contains(&key_lower.as_str())
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if url.query().is_some() {
        url.set_query(None);
        if !retained.is_empty() {
            url.query_pairs_mut().extend_pairs(retained);
        }
    }
}

pub fn normalize_url(url: &mut Url) {
    url.set_fragment(None);
    normalize_tracking_params(url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_schema_org_job_posting_and_matches_keyword() {
        let html = r#"
        <html><head><title>Backend Engineer - Example Corp</title>
        <script type="application/ld+json">
        {
          "@context": "https://schema.org",
          "@type": "JobPosting",
          "title": "Rust Backend Engineer",
          "description": "Build backend services using Rust and PostgreSQL.",
          "datePosted": "2026-10-20",
          "hiringOrganization": {"@type": "Organization", "name": "Example Corp"},
          "jobLocation": {"address": {"addressLocality": "Bengaluru", "addressRegion": "Karnataka", "addressCountry": "India"}},
          "employmentType": "FULL_TIME"
        }
        </script></head><body><h1>Rust Backend Engineer</h1></body></html>
        "#;
        let url = Url::parse("https://example.com/jobs/rust-backend-engineer").unwrap();
        let page = parse_page(url, html, "test", &["Rust".to_string()], false);
        assert_eq!(page.postings.len(), 1);
        assert_eq!(page.postings[0].title, "Rust Backend Engineer");
        assert_eq!(page.postings[0].company.as_deref(), Some("Example Corp"));
        assert_eq!(page.postings[0].matched_keywords, vec!["Rust".to_string()]);
    }

    #[test]
    fn does_not_return_posting_when_keyword_is_absent() {
        let html = r#"<html><head><title>Careers</title></head><body>
            <h1>Senior Java Developer</h1>
            <p>Responsibilities include building Java services and reviewing code.</p>
            <p>Qualifications: experience with Java and Spring Boot.</p>
            <p>Apply now.</p>
        </body></html>"#;
        let url = Url::parse("https://example.com/jobs/senior-java-developer").unwrap();
        let page = parse_page(url, html, "test", &["Rust".to_string()], false);
        assert!(page.postings.is_empty());
    }

    #[test]
    fn recognizes_company_career_links() {
        let url = Url::parse("https://example.com/careers").unwrap();
        assert!(career_link_score(&url, "Open positions") > 0);
    }
}
