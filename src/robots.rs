use regex::Regex;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Default)]
pub struct RobotsRules {
    rules: Vec<(bool, String)>, // (is_allow, path pattern)
    pub crawl_delay: Option<Duration>,
    pub sitemaps: Vec<String>,
    pub deny_all: bool,
}

impl RobotsRules {
    pub fn parse(body: &str) -> Self {
        let mut output = Self::default();
        let mut agents: Vec<String> = Vec::new();
        let mut selected_group = false;
        let mut group_has_directive = false;
        let token = "rust-job-crawler";

        for raw_line in body.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();

            match name.as_str() {
                "user-agent" => {
                    if group_has_directive {
                        agents.clear();
                        selected_group = false;
                        group_has_directive = false;
                    }
                    agents.push(value.to_ascii_lowercase());
                    selected_group = agents.iter().any(|agent| {
                        agent == "*" || token.starts_with(agent.as_str()) || agent.starts_with(token)
                    });
                }
                "allow" | "disallow" => {
                    group_has_directive = true;
                    if selected_group && !value.is_empty() {
                        output.rules.push((name == "allow", value.to_string()));
                    }
                }
                "crawl-delay" => {
                    group_has_directive = true;
                    if selected_group {
                        if let Ok(seconds) = value.parse::<f64>() {
                            if seconds.is_finite() && seconds >= 0.0 {
                                output.crawl_delay = Some(Duration::from_secs_f64(seconds.min(30.0)));
                            }
                        }
                    }
                }
                "sitemap" => output.sitemaps.push(value.to_string()),
                _ => {}
            }
        }
        output
    }

    pub fn allows(&self, url: &Url) -> bool {
        if self.deny_all {
            return false;
        }
        let path = match url.query() {
            Some(query) => format!("{}?{}", url.path(), query),
            None => url.path().to_string(),
        };
        let mut best: Option<(usize, bool)> = None;
        for (is_allow, pattern) in &self.rules {
            if robots_pattern_matches(pattern, &path) {
                let specificity = pattern.trim_end_matches('$').replace('*', "").len();
                if best.map(|(length, _)| specificity >= length).unwrap_or(true) {
                    best = Some((specificity, *is_allow));
                }
            }
        }
        best.map(|(_, is_allow)| is_allow).unwrap_or(true)
    }
}

fn robots_pattern_matches(pattern: &str, path: &str) -> bool {
    let anchored_end = pattern.ends_with('$');
    let raw = pattern.trim_end_matches('$');
    let escaped = regex::escape(raw).replace(r"\*", ".*");
    let expression = if anchored_end {
        format!("^{escaped}$")
    } else {
        format!("^{escaped}")
    };
    Regex::new(&expression)
        .map(|regex| regex.is_match(path))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_disallow_and_more_specific_allow_rules() {
        let rules = RobotsRules::parse(
            "User-agent: *\nDisallow: /private\nAllow: /private/job-posting\nCrawl-delay: 2\n",
        );
        let blocked = Url::parse("https://example.com/private/profile").unwrap();
        let allowed = Url::parse("https://example.com/private/job-posting/123").unwrap();
        assert!(!rules.allows(&blocked));
        assert!(rules.allows(&allowed));
        assert_eq!(rules.crawl_delay, Some(Duration::from_secs(2)));
    }
}
