use crate::extract::{normalize_url, parse_page};
use crate::models::{CrawlTask, JobPosting};
use crate::robots::RobotsRules;
use anyhow::Result;
use regex::Regex;
use reqwest::{header, redirect::Policy, Client, StatusCode};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, info, warn};
use url::{Host, Url};

const MAX_REDIRECTS: usize = 5;
const MAX_LINKS_PER_PAGE: usize = 100;
const ATS_DOMAINS: &[&str] = &[
    "greenhouse.io",
    "lever.co",
    "ashbyhq.com",
    "myworkdayjobs.com",
    "myworkdaysite.com",
    "smartrecruiters.com",
    "workable.com",
    "jobvite.com",
    "recruitee.com",
    "teamtailor.com",
    "bamboohr.com",
    "icims.com",
    "successfactors.com",
    "oraclecloud.com",
    "dayforcehcm.com",
    "rippling.com",
];

#[derive(Debug, Clone)]
pub struct CrawlerConfig {
    pub max_pages: usize,
    pub max_depth: usize,
    pub default_delay: Duration,
    pub max_body_bytes: usize,
    pub require_all_keywords: bool,
}

struct HttpPage {
    final_url: Url,
    content_type: String,
    body: Vec<u8>,
}

pub async fn crawl(
    client: &Client,
    seeds: Vec<Url>,
    keywords: &[String],
    config: &CrawlerConfig,
) -> Result<Vec<JobPosting>> {
    let mut queue = VecDeque::new();
    for mut seed in seeds {
        normalize_url(&mut seed);
        if !is_safe_public_url(&seed) {
            warn!(url = %seed, "Skipping unsafe or unsupported seed URL");
            continue;
        }
        let Some(host) = seed.host_str().map(normalize_host) else { continue };
        queue.push_back(CrawlTask {
            url: seed.clone(),
            depth: 0,
            root_host: host,
            discovered_from: "seed/search discovery".to_string(),
        });
    }

    let mut visited = HashSet::new();
    let mut robots_cache: HashMap<String, RobotsRules> = HashMap::new();
    let mut last_request: HashMap<String, Instant> = HashMap::new();
    let mut jobs: HashMap<String, JobPosting> = HashMap::new();
    let mut pages_crawled = 0usize;

    while let Some(task) = queue.pop_front() {
        if pages_crawled >= config.max_pages {
            info!(limit = config.max_pages, "Reached maximum page limit");
            break;
        }
        let mut requested_url = task.url.clone();
        normalize_url(&mut requested_url);
        let url_key = requested_url.as_str().to_string();
        if !visited.insert(url_key) {
            continue;
        }
        if task.depth > config.max_depth || !is_safe_public_url(&requested_url) {
            continue;
        }

        let origin = origin_key(&requested_url);
        if !robots_cache.contains_key(&origin) {
            let rules = fetch_robots(client, &requested_url, config.max_body_bytes).await;
            robots_cache.insert(origin.clone(), rules);
            // The robots.txt request also counts toward per-origin pacing.
            last_request.insert(origin.clone(), Instant::now());
        }
        let rules = robots_cache.get(&origin).cloned().unwrap_or_default();
        if !rules.allows(&requested_url) {
            debug!(url = %requested_url, "Blocked by robots.txt");
            continue;
        }

        // Honor both the user-configured pacing and a site's robots.txt Crawl-delay.
        let delay = rules.crawl_delay.unwrap_or(config.default_delay).max(config.default_delay);
        wait_for_origin(&origin, delay, &mut last_request).await;
        let fetched = fetch_url(client, &requested_url, config.max_body_bytes).await?;
        last_request.insert(origin.clone(), Instant::now());
        let Some(http_page) = fetched else {
            continue;
        };
        if !http_page.content_type.to_ascii_lowercase().contains("text/html")
            && !http_page.content_type.to_ascii_lowercase().contains("application/xhtml")
        {
            continue;
        }
        let html = String::from_utf8_lossy(&http_page.body).to_string();
        pages_crawled += 1;
        info!(pages = pages_crawled, depth = task.depth, url = %http_page.final_url, "Crawled page");

        let page = parse_page(
            http_page.final_url.clone(),
            &html,
            &task.discovered_from,
            keywords,
            config.require_all_keywords,
        );
        for posting in page.postings {
            jobs.entry(posting.url.clone()).and_modify(|existing| {
                if posting.score > existing.score || existing.description.len() < posting.description.len() {
                    *existing = posting.clone();
                }
            }).or_insert(posting);
        }

        if task.depth >= config.max_depth {
            continue;
        }

        let page_host = page.final_url.host_str().unwrap_or_default();
        let current_page_is_career = page.is_career_page
            || page.final_url.path().to_ascii_lowercase().contains("job")
            || page.final_url.path().to_ascii_lowercase().contains("career");
        let mut scheduled = 0usize;
        for (link_score, link) in page.links {
            if !is_safe_public_url(&link) {
                continue;
            }
            let Some(link_host) = link.host_str() else { continue };
            let same_site = same_site_or_subdomain(page_host, link_host)
                && same_site_or_subdomain(&task.root_host, link_host);
            let known_ats = is_known_ats(link_host);
            if !same_site && !known_ats {
                continue;
            }
            // On a career page, job links often have no descriptive anchor text.
            // On ordinary company pages, only follow links that look career-related.
            if !current_page_is_career && link_score <= 0 {
                continue;
            }
            if link_score < 0 {
                continue;
            }
            let mut next = link.clone();
            normalize_url(&mut next);
            if visited.contains(next.as_str()) {
                continue;
            }
            queue.push_back(CrawlTask {
                url: next,
                depth: task.depth + 1,
                root_host: task.root_host.clone(),
                discovered_from: http_page.final_url.to_string(),
            });
            scheduled += 1;
            if scheduled >= MAX_LINKS_PER_PAGE {
                break;
            }
        }

        // A robots.txt sitemap is a useful way to discover job-detail URLs that are
        // not linked from the visible navigation. Only career-looking URLs are queued.
        if task.depth == 0 {
            let sitemap_urls: Vec<String> = if rules.sitemaps.is_empty() {
                vec![format!("{origin}/sitemap.xml")]
            } else {
                rules.sitemaps.iter().take(3).cloned().collect()
            };
            for sitemap in sitemap_urls {
                if let Ok(sitemap_url) = Url::parse(&sitemap) {
                    if !is_safe_public_url(&sitemap_url) || !rules.allows(&sitemap_url) {
                        continue;
                    }
                    let sitemap_origin = origin_key(&sitemap_url);
                    let sitemap_delay = config.default_delay.max(rules.crawl_delay.unwrap_or_default());
                    wait_for_origin(&sitemap_origin, sitemap_delay, &mut last_request).await;
                    let sitemap_links = fetch_sitemap_links(client, &sitemap_url, config.max_body_bytes).await;
                    last_request.insert(sitemap_origin, Instant::now());
                    for sitemap_link in sitemap_links {
                        let Some(host) = sitemap_link.host_str() else { continue };
                        if (same_site_or_subdomain(&task.root_host, host) || is_known_ats(host))
                            && crate::extract::career_link_score(&sitemap_link, "") > 0
                        {
                            queue.push_back(CrawlTask {
                                url: sitemap_link,
                                depth: 1,
                                root_host: task.root_host.clone(),
                                discovered_from: sitemap_url.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    info!(pages_crawled, job_count = jobs.len(), "Crawl finished");
    let mut result: Vec<JobPosting> = jobs.into_values().collect();
    result.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
    Ok(result)
}


async fn wait_for_origin(origin: &str, delay: Duration, last_request: &mut HashMap<String, Instant>) {
    if let Some(previous) = last_request.get(origin) {
        let elapsed = previous.elapsed();
        if elapsed < delay {
            sleep(delay - elapsed).await;
        }
    }
    last_request.insert(origin.to_string(), Instant::now());
}

async fn fetch_robots(client: &Client, page_url: &Url, max_bytes: usize) -> RobotsRules {
    let mut robots_url = page_url.clone();
    robots_url.set_path("/robots.txt");
    robots_url.set_query(None);
    robots_url.set_fragment(None);

    let response = match client.get(robots_url.clone()).header(header::ACCEPT, "text/plain,*/*;q=0.5").send().await {
        Ok(response) => response,
        Err(error) => {
            warn!(url = %robots_url, error = %error, "Could not read robots.txt; continuing with configured delay");
            return RobotsRules::default();
        }
    };
    if response.status() == StatusCode::FORBIDDEN || response.status() == StatusCode::UNAUTHORIZED {
        let mut rules = RobotsRules::default();
        rules.deny_all = true;
        return rules;
    }
    if response.status() == StatusCode::NOT_FOUND || !response.status().is_success() {
        return RobotsRules::default();
    }
    let mut response = response;
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            warn!(url = %robots_url, "robots.txt exceeded response size limit; using default rules");
            return RobotsRules::default();
        }
        body.extend_from_slice(&chunk);
    }
    RobotsRules::parse(&String::from_utf8_lossy(&body))
}

async fn fetch_url(client: &Client, start_url: &Url, max_bytes: usize) -> Result<Option<HttpPage>> {
    let mut current = start_url.clone();
    for _ in 0..=MAX_REDIRECTS {
        if !is_safe_public_url(&current) {
            return Ok(None);
        }
        let response = match client
            .get(current.clone())
            .header(header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,text/plain;q=0.8,*/*;q=0.5")
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                debug!(url = %current, error = %error, "HTTP request failed");
                return Ok(None);
            }
        };

        let status = response.status();
        if is_redirect(status) {
            let Some(location) = response.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()) else {
                return Ok(None);
            };
            current = match current.join(location) {
                Ok(url) => url,
                Err(_) => return Ok(None),
            };
            continue;
        }
        if !status.is_success() {
            return Ok(None);
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if let Some(length) = response.content_length() {
            if length > max_bytes as u64 {
                debug!(url = %current, length, "Skipping response above configured size limit");
                return Ok(None);
            }
        }

        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len().saturating_add(chunk.len()) > max_bytes {
                debug!(url = %current, "Response exceeded configured size limit");
                return Ok(None);
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(Some(HttpPage { final_url: current, content_type, body }));
    }
    Ok(None)
}

async fn fetch_sitemap_links(client: &Client, sitemap_url: &Url, max_bytes: usize) -> Vec<Url> {
    let Ok(Some(page)) = fetch_url(client, sitemap_url, max_bytes).await else { return Vec::new() };
    let xml = String::from_utf8_lossy(&page.body);
    let Ok(loc_regex) = Regex::new(r"(?is)<loc>\s*([^<]+?)\s*</loc>") else { return Vec::new() };
    loc_regex
        .captures_iter(&xml)
        .filter_map(|capture| capture.get(1))
        .filter_map(|value| Url::parse(value.as_str().trim()).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .collect()
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn origin_key(url: &Url) -> String {
    format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default())
}

fn normalize_host(host: &str) -> String {
    let lower = host.to_ascii_lowercase();
    lower.strip_prefix("www.").unwrap_or(&lower).to_string()
}

fn same_site_or_subdomain(root: &str, candidate: &str) -> bool {
    let root = normalize_host(root);
    let candidate = normalize_host(candidate);
    candidate == root || candidate.ends_with(&format!(".{root}")) || root.ends_with(&format!(".{candidate}"))
}

fn is_known_ats(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    ATS_DOMAINS.iter().any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn is_safe_public_url(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match url.host() {
        Some(Host::Domain(host)) => {
            let lower = host.to_ascii_lowercase();
            lower != "localhost"
                && !lower.ends_with(".localhost")
                && !lower.ends_with(".local")
                && !lower.ends_with(".internal")
        }
        Some(Host::Ipv4(ip)) => {
            let octets = ip.octets();
            let shared_address_space = octets[0] == 100 && (64..=127).contains(&octets[1]);
            !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
                || ip.is_broadcast() || ip.is_multicast() || shared_address_space)
        }
        Some(Host::Ipv6(ip)) => {
            !(ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
                || ip.is_unspecified() || ip.is_multicast())
        }
        None => false,
    }
}
