use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPosting {
    pub url: String,
    pub apply_url: Option<String>,
    pub title: String,
    pub company: Option<String>,
    pub location: Option<String>,
    pub employment_type: Option<String>,
    pub description: String,
    pub date_posted: Option<String>,
    pub valid_through: Option<String>,
    pub source_domain: String,
    pub discovered_from: String,
    pub matched_keywords: Vec<String>,
    pub score: i32,
    pub found_at: String,
}

#[derive(Debug, Clone)]
pub struct CrawlTask {
    pub url: url::Url,
    pub depth: usize,
    pub root_host: String,
    pub discovered_from: String,
}

#[derive(Debug, Clone)]
pub struct PageData {
    pub final_url: url::Url,
    pub title: String,
    pub text: String,
    pub postings: Vec<JobPosting>,
    pub links: Vec<(i32, url::Url)>,
    pub is_career_page: bool,
}
