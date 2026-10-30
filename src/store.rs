use crate::models::JobPosting;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

pub fn save_postings(db_path: &Path, postings: &[JobPosting]) -> Result<usize> {
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| format!("Could not create {}", parent.display()))?;
        }
    }
    let connection = Connection::open(db_path)
        .with_context(|| format!("Could not open database {}", db_path.display()))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS job_postings (
             url TEXT PRIMARY KEY,
             apply_url TEXT,
             title TEXT NOT NULL,
             company TEXT,
             location TEXT,
             employment_type TEXT,
             description TEXT NOT NULL,
             date_posted TEXT,
             valid_through TEXT,
             source_domain TEXT NOT NULL,
             discovered_from TEXT NOT NULL,
             matched_keywords_json TEXT NOT NULL,
             score INTEGER NOT NULL,
             found_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_job_postings_score ON job_postings(score DESC);
         CREATE INDEX IF NOT EXISTS idx_job_postings_company ON job_postings(company);"
    )?;

    let transaction = connection.unchecked_transaction()?;
    let mut saved = 0usize;
    {
        let mut statement = transaction.prepare(
            "INSERT INTO job_postings (
                 url, apply_url, title, company, location, employment_type,
                 description, date_posted, valid_through, source_domain,
                 discovered_from, matched_keywords_json, score, found_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(url) DO UPDATE SET
                 apply_url = COALESCE(excluded.apply_url, job_postings.apply_url),
                 title = excluded.title,
                 company = COALESCE(excluded.company, job_postings.company),
                 location = COALESCE(excluded.location, job_postings.location),
                 employment_type = COALESCE(excluded.employment_type, job_postings.employment_type),
                 description = CASE WHEN length(excluded.description) > length(job_postings.description)
                                    THEN excluded.description ELSE job_postings.description END,
                 date_posted = COALESCE(excluded.date_posted, job_postings.date_posted),
                 valid_through = COALESCE(excluded.valid_through, job_postings.valid_through),
                 discovered_from = excluded.discovered_from,
                 matched_keywords_json = excluded.matched_keywords_json,
                 score = MAX(excluded.score, job_postings.score),
                 found_at = excluded.found_at"
        )?;
        for posting in postings {
            let matched = serde_json::to_string(&posting.matched_keywords)?;
            statement.execute(params![
                posting.url,
                posting.apply_url,
                posting.title,
                posting.company,
                posting.location,
                posting.employment_type,
                posting.description,
                posting.date_posted,
                posting.valid_through,
                posting.source_domain,
                posting.discovered_from,
                matched,
                posting.score,
                posting.found_at,
            ])?;
            saved += 1;
        }
    }
    transaction.commit()?;
    Ok(saved)
}

pub fn load_postings(db_path: &Path) -> Result<Vec<JobPosting>> {
    let connection = Connection::open(db_path)
        .with_context(|| format!("Could not open database {}", db_path.display()))?;
    let mut statement = connection.prepare(
        "SELECT url, apply_url, title, company, location, employment_type,
                description, date_posted, valid_through, source_domain,
                discovered_from, matched_keywords_json, score, found_at
         FROM job_postings
         ORDER BY score DESC, COALESCE(date_posted, '') DESC, title ASC"
    )?;
    let rows = statement.query_map([], |row| {
        let matched_json: String = row.get(11)?;
        let matched_keywords = serde_json::from_str(&matched_json).unwrap_or_default();
        Ok(JobPosting {
            url: row.get(0)?,
            apply_url: row.get(1)?,
            title: row.get(2)?,
            company: row.get(3)?,
            location: row.get(4)?,
            employment_type: row.get(5)?,
            description: row.get(6)?,
            date_posted: row.get(7)?,
            valid_through: row.get(8)?,
            source_domain: row.get(9)?,
            discovered_from: row.get(10)?,
            matched_keywords,
            score: row.get(12)?,
            found_at: row.get(13)?,
        })
    })?;

    let mut postings = Vec::new();
    for row in rows {
        postings.push(row?);
    }
    Ok(postings)
}

pub fn export_csv(db_path: &Path, output_path: &Path) -> Result<usize> {
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let postings = load_postings(db_path)?;
    let mut writer = csv::Writer::from_path(output_path)
        .with_context(|| format!("Could not create CSV {}", output_path.display()))?;
    #[derive(serde::Serialize)]
    struct CsvJobRow<'a> {
        url: &'a str,
        apply_url: &'a Option<String>,
        title: &'a str,
        company: &'a Option<String>,
        location: &'a Option<String>,
        employment_type: &'a Option<String>,
        description: &'a str,
        date_posted: &'a Option<String>,
        valid_through: &'a Option<String>,
        source_domain: &'a str,
        discovered_from: &'a str,
        matched_keywords: String,
        score: i32,
        found_at: &'a str,
    }
    for posting in &postings {
        writer.serialize(CsvJobRow {
            url: &posting.url,
            apply_url: &posting.apply_url,
            title: &posting.title,
            company: &posting.company,
            location: &posting.location,
            employment_type: &posting.employment_type,
            description: &posting.description,
            date_posted: &posting.date_posted,
            valid_through: &posting.valid_through,
            source_domain: &posting.source_domain,
            discovered_from: &posting.discovered_from,
            matched_keywords: posting.matched_keywords.join("; "),
            score: posting.score,
            found_at: &posting.found_at,
        })?;
    }
    writer.flush()?;
    Ok(postings.len())
}

pub fn export_json(db_path: &Path, output_path: &Path) -> Result<usize> {
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let postings = load_postings(db_path)?;
    let file = std::fs::File::create(output_path)
        .with_context(|| format!("Could not create JSON {}", output_path.display()))?;
    serde_json::to_writer_pretty(file, &postings)?;
    Ok(postings.len())
}
