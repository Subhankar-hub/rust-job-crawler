# Rust Job Crawler

A Rust command-line crawler that discovers keyword-matched job postings from both job boards and ordinary company websites, including consultancy, startup, enterprise, government, and institution career pages.

It is designed as a **discovery + crawl + extraction** pipeline:

1. **Discover** candidate pages through the Brave Search API (optional), or accept company homepages/career URLs supplied by you.
2. **Crawl** the candidate pages and follow relevant same-site links such as `Careers`, `Jobs`, `Openings`, `Vacancies`, and `Join our team`.
3. **Follow common ATS links** when a company points to Greenhouse, Lever, Ashby, Workday, SmartRecruiters, Workable, Jobvite, Recruitee, Teamtailor, BambooHR, iCIMS, SuccessFactors, Oracle Cloud, Dayforce, or Rippling.
4. **Extract** Schema.org `JobPosting` JSON-LD first, then use page-content heuristics for sites without structured job data.
5. **Match** job title and description against configured keywords.
6. **Deduplicate and store** the results in SQLite, with optional CSV and JSON exports.

## Current capabilities and boundaries

- Works with job boards **and** ordinary company/consultancy websites. A company homepage can be supplied as a seed; the crawler looks for career links and recognized applicant-tracking-system destinations.
- Search discovery requires a Brave Search API key. This is an external search index, not a guarantee that every company's website or every unindexed vacancy will be found.
- Direct seed crawling does not require an API key.
- Respects `robots.txt` allow/disallow rules and `Crawl-delay` where present; also applies a configurable minimum per-origin delay.
- Checks `robots.txt` sitemaps and falls back to `/sitemap.xml` when no sitemap is declared; it queues career/job-looking URLs from those sitemap documents.
- Limits page count, crawl depth, and downloaded response size.
- Only follows HTTP/HTTPS URLs and validates redirect destinations. Literal local/private IP URLs are skipped.
- Does **not** execute JavaScript. Some Workday and custom careers portals render listings client-side; those may need a later browser-rendering/API adapter.
- Matching is lexical, not semantic. A keyword such as `Rust` matches the word `Rust`; it does not infer that a posting mentioning systems programming is relevant unless that phrase is also configured.
- The fallback extractor is heuristic. Structured JSON-LD is more reliable than generic HTML extraction. Results should be reviewed before applying.

## Requirements

- Rust toolchain with Cargo (stable Rust; edition 2021). The current `reqwest` release line requires Rust 1.85 or newer; using the latest stable toolchain is recommended.
- Optional: a Brave Search API key for search-based domain discovery.

## Build

```bash
cargo build --release
cargo test
```

The executable will be at `target/release/job-crawler`. The tests cover Schema.org job extraction, keyword filtering, career-link recognition, and robots rules.

## Quick start: crawl supplied company websites

Create a seed list. Include company homepages, consulting firms, startups, public-sector organizations, and career pages:

```text
https://www.example-company.com
https://careers.example-consultancy.in
https://www.example-research-lab.org
```

Run:

```bash
cargo run --release -- \
  --keyword Rust \
  --keyword "Backend Engineer" \
  --keyword PostgreSQL \
  --keyword "Distributed Systems" \
  --seed-file examples/company-seeds.txt \
  --max-pages 300 \
  --max-depth 4 \
  --delay-ms 1500 \
  --db data/jobs.sqlite \
  --csv data/jobs.csv \
  --json data/jobs.json
```

You can also pass a comma-separated keyword value:

```bash
--keyword 'Rust,PostgreSQL,systems programming,backend engineer'
```

Or use a file with one phrase per line:

```bash
cargo run --release -- \
  --keywords-file examples/keywords.txt \
  --seed https://www.example-company.com \
  --csv output/jobs.csv
```

By default, a posting is kept when **any** configured keyword appears in its title or description. Use `--require-all` to require every configured keyword to appear. That can be restrictive for real-world job descriptions.

## Search the wider web first

Get a Brave Search API key from the [Brave Search API dashboard](https://api-dashboard.search.brave.com/), then set it as an environment variable. Do not hard-code or commit the key.

```bash
export BRAVE_SEARCH_API_KEY='your-api-key'

cargo run --release -- \
  --discover \
  --keywords-file examples/keywords.txt \
  --country IN \
  --location India \
  --max-pages 400 \
  --max-depth 3 \
  --db data/jobs.sqlite \
  --csv data/jobs.csv \
  --json data/jobs.json
```

The crawler generates one search query per keyword (up to 12 keywords per discovery run), using terms such as jobs, careers, vacancies, openings, and hiring. All configured keywords are still used during posting matching. You can supply your own queries instead:

```bash
cargo run --release -- \
  --discover \
  --keyword 'Rust systems engineer' \
  --search-query 'Rust systems engineer careers India' \
  --search-query 'site:company-domain.example Rust jobs' \
  --csv output/jobs.csv
```

Search discovery and seed crawling can be combined. The URLs found by search are starting points; the crawler then fetches the pages itself and follows career links.

## Useful options

| Option | Default | Purpose |
|---|---:|---|
| `--keyword` | none | Keyword or phrase; repeat or comma-separate |
| `--keywords-file` | none | File with one keyword per line |
| `--seed` | none | Starting company or career URL; repeatable |
| `--seed-file` | none | File with one URL per line |
| `--discover` | false | Enable Brave Search discovery |
| `--country` | `IN` | Search country code |
| `--location` | `India` | Location in generated search queries |
| `--max-pages` | `250` | Maximum HTML pages per run |
| `--max-depth` | `3` | Maximum link depth from each seed |
| `--delay-ms` | `1500` | Minimum delay per origin |
| `--max-body-bytes` | `2000000` | Maximum response size |
| `--require-all` | false | Require all keywords instead of any keyword |
| `--db` | `jobs.sqlite` | SQLite database path |
| `--csv` | none | Export database to CSV |
| `--json` | none | Export database to JSON |

Set `RUST_LOG=debug` for crawler diagnostics:

```bash
RUST_LOG=debug cargo run --release -- --keyword Rust --seed https://www.example.com
```

## Database schema

The `job_postings` table stores:

- `url` (unique key), `apply_url`, `title`, `company`, `location`
- `employment_type`, `description`, `date_posted`, `valid_through`
- `source_domain`, `discovered_from`, `matched_keywords_json`, `score`, `found_at`

Repeated runs upsert by job URL. Existing descriptions are retained unless a longer description is extracted.

## Suggested next engineering steps

1. Add per-ATS adapters for Greenhouse, Lever, Ashby, and Workday APIs/feeds for better listing coverage.
2. Add a browser-rendering worker (for example, Playwright over a separate browser service) for JavaScript-only career sites.
3. Add semantic ranking (embeddings or a local model) after lexical filtering, not as a replacement for transparent keyword matching.
4. Add scheduled runs, expiry detection, and notifications for newly discovered postings.
5. Add a review UI and filters for location, experience, remote policy, salary, employment type, and publication date.

## Responsible crawling

Use this tool only for publicly accessible pages and in accordance with each site's terms, `robots.txt`, and applicable law. Keep the request delay conservative. Do not bypass authentication, CAPTCHAs, access controls, or anti-bot protections. The crawler intentionally does not attempt to log in or submit applications.

## License

MIT. See `LICENSE`.
