//! `cargo xtask <command>`.
//!
//! - `meta-docs [--out DIR] [--delay-secs N] [--missing-only] [--force]`:
//!   mirror Meta's WhatsApp Business Platform docs as Markdown (Meta serves
//!   `<page>.md`) so agents and humans can grep the real spec. The output is
//!   gitignored: the docs are Meta's copyrighted material, fetched for local
//!   reference only.
//!
//! The crawl is sequential: one request at a time, `--delay-secs` (15)
//! apart. An HTTP 429 waits before asking for the same page again: as long
//! as Meta's `Retry-After` says when it sends one, otherwise 60 s, doubled
//! on each refusal up to 30 min; never less than `--delay-secs`. A page
//! still refused after the 30 min wait, or a `Retry-After` longer than
//! that, stops the crawl: the rate limit is Meta's for the whole client,
//! not for one page. Six parallel workers
//! without a pause drew 429s on 179 of about 390 pages on 2026-09-24; one
//! request every 15 s, backing off as above, drew none on 2026-09-26.
//!
//! The mirror's `README.md` lists the pages still missing (failed, gated,
//! or not reached before a stop), rewritten at the end of every run;
//! `--missing-only` retries just those.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use ureq::config::ConfigBuilder;
use ureq::http::{StatusCode, header::RETRY_AFTER};
use ureq::typestate::AgentScope;

const HOST: &str = "https://developers.facebook.com";
const PREFIX: &str = "/documentation/business-messaging/whatsapp";
/// Pages whose HTML embeds the documentation navigation tree.
const SEEDS: &[&str] = &["/overview/", "/flows/"];
/// The mirror's index, and its list of the pages still missing.
const README: &str = "README.md";
/// Opens the README's list of missing pages (and ends its introduction).
const MISSING_HEADER: &str = "Unavailable pages";

/// The pause between two requests, unless `--delay-secs` says otherwise.
const DEFAULT_DELAY: Duration = Duration::from_secs(15);
/// The waits after an HTTP 429: 60 s, 120 s, … 960 s, 1800 s, then stop.
const BACKOFF: Backoff = Backoff {
    first: Duration::from_secs(60),
    cap: Duration::from_mins(30),
};

const USAGE: &str =
    "usage: cargo xtask meta-docs [--out DIR] [--delay-secs N] [--missing-only] [--force]

  --out DIR         mirror into DIR (default .meta-docs)
  --delay-secs N    seconds between two requests (default 15)
  --missing-only    retry only the pages DIR/README.md lists as unavailable
  --force           fetch pages that are already mirrored too";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("meta-docs") {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let Some(options) = parse_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    meta_docs(Http::new(), &options)
}

/// What `meta-docs` was asked to do.
#[derive(Debug, PartialEq, Eq)]
struct Options {
    out: PathBuf,
    force: bool,
    missing_only: bool,
    delay: Duration,
}

/// The options after `meta-docs`, or `None` for `--help`.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Options>> {
    let mut options = Options {
        out: PathBuf::from(".meta-docs"),
        force: false,
        missing_only: false,
        delay: DEFAULT_DELAY,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => options.out = PathBuf::from(args.next().context("--out needs a value")?),
            "--delay-secs" => {
                let value = args.next().context("--delay-secs needs a value")?;
                let secs = value
                    .parse()
                    .with_context(|| format!("--delay-secs takes whole seconds, not `{value}`"))?;
                options.delay = Duration::from_secs(secs);
            }
            "--missing-only" => options.missing_only = true,
            "--force" => options.force = true,
            "-h" | "--help" => return Ok(None),
            other => bail!("unknown argument `{other}`\n{USAGE}"),
        }
    }
    Ok(Some(options))
}

/// How long to wait after an HTTP 429 before asking again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Backoff {
    /// The first wait when Meta sends no `Retry-After`; doubled on each
    /// further refusal.
    first: Duration,
    /// The longest wait. A page refused again after it is given up on.
    cap: Duration,
}

impl Backoff {
    /// The wait before retry number `retry` (0 is the first), or `None` to
    /// give up. `Retry-After`, when Meta sent one, replaces the schedule's
    /// wait, but not its length: the schedule's retries are all there are,
    /// and a `Retry-After` longer than `cap` gives up at once.
    fn wait(self, retry: u32, retry_after: Option<Duration>) -> Option<Duration> {
        let scheduled = self.scheduled(retry)?;
        let wait = retry_after.unwrap_or(scheduled);
        (wait <= self.cap).then_some(wait)
    }

    /// `first` doubled `retry` times, at most `cap`; `None` once a wait of
    /// `cap` (or a zero `first`) has been tried.
    fn scheduled(self, retry: u32) -> Option<Duration> {
        let mut wait = self.first;
        for _ in 0..retry {
            if wait >= self.cap || wait.is_zero() {
                return None;
            }
            wait = wait.saturating_mul(2);
        }
        Some(wait.min(self.cap))
    }
}

/// `Retry-After`'s value as a wait from `now`: delay-seconds or an
/// HTTP-date (RFC 9110 §10.2.3). A date already past is no wait, and
/// delay-seconds too large for a `u64` the longest; a value that is
/// neither is `None`, and the backoff schedule applies.
fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(value.parse().map_or(Duration::MAX, Duration::from_secs));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// One answer to a GET.
#[derive(Debug)]
enum Fetched {
    /// A 2xx body.
    Body(String),
    /// HTTP 429, with `Retry-After` as a wait when Meta sent a readable one.
    RateLimited(Option<Duration>),
    /// Anything else: another status, a network error, an unreadable body.
    Failed(anyhow::Error),
}

/// Everything the crawl does outside itself, so a test can script it.
trait Io {
    fn get(&mut self, url: &str) -> Fetched;
    fn sleep(&mut self, wait: Duration);
}

/// The network and the wall clock.
struct Http(ureq::Agent);

impl Http {
    fn new() -> Self {
        Self::configured(ureq::Agent::config_builder())
    }

    fn configured(config: ConfigBuilder<AgentScope>) -> Self {
        let agent = config
            .timeout_global(Some(Duration::from_secs(45)))
            .user_agent("meta-whatsapp-rs-xtask (docs mirror)")
            // A 429's headers are read below, not turned into an error.
            .http_status_as_error(false)
            .build()
            .into();
        Self(agent)
    }
}

impl Io for Http {
    fn get(&mut self, url: &str) -> Fetched {
        let mut resp = match self.0.get(url).call() {
            Ok(resp) => resp,
            Err(e) => return Fetched::Failed(anyhow!(e).context(format!("GET {url}"))),
        };
        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let wait = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_after(v, SystemTime::now()));
            return Fetched::RateLimited(wait);
        }
        if !status.is_success() {
            return Fetched::Failed(anyhow!("GET {url}: HTTP {status}"));
        }
        match resp
            .body_mut()
            .with_config()
            .limit(20 * 1024 * 1024)
            .read_to_string()
        {
            Ok(body) => Fetched::Body(body),
            Err(e) => Fetched::Failed(anyhow!(e).context(format!("reading {url}"))),
        }
    }

    fn sleep(&mut self, wait: Duration) {
        thread::sleep(wait);
    }
}

/// How one GET ended, retries included.
#[derive(Debug)]
enum Outcome {
    Body(String),
    Failed(anyhow::Error),
    /// Still HTTP 429 when the backoff ran out: stop asking Meta anything.
    RateLimited(String),
}

/// Sequential, paced GETs: `delay` between two requests, and after a 429
/// `backoff`'s wait in place of `delay` (never shorter than `delay`).
struct Crawler<I> {
    io: I,
    delay: Duration,
    backoff: Backoff,
    /// Whether a request went out yet: the first one does not wait.
    requested: bool,
}

impl<I: Io> Crawler<I> {
    fn new(io: I, delay: Duration, backoff: Backoff) -> Self {
        Self {
            io,
            delay,
            backoff,
            requested: false,
        }
    }

    fn get(&mut self, url: &str) -> Outcome {
        let mut pause = if self.requested {
            self.delay
        } else {
            Duration::ZERO
        };
        let mut retry = 0;
        loop {
            if !pause.is_zero() {
                self.io.sleep(pause);
            }
            self.requested = true;
            let after = match self.io.get(url) {
                Fetched::Body(body) => return Outcome::Body(body),
                Fetched::Failed(e) => return Outcome::Failed(e),
                Fetched::RateLimited(after) => after,
            };
            let Some(wait) = self.backoff.wait(retry, after) else {
                return Outcome::RateLimited(match after {
                    Some(after) if after > self.backoff.cap => format!(
                        "HTTP 429, Retry-After {} s: longer than the {} s the crawl waits at most",
                        after.as_secs(),
                        self.backoff.cap.as_secs()
                    ),
                    _ => format!("HTTP 429, still after {retry} retries"),
                });
            };
            // A Retry-After shorter than `delay` (0, or a date already past
            // on a clock ahead of Meta's) must not send requests back to back.
            pause = wait.max(self.delay);
            let source = match after {
                _ if pause > wait => "--delay-secs",
                Some(_) => "Retry-After",
                None => "backoff",
            };
            eprintln!(
                "  HTTP 429 on {url}: waiting {} s ({source}, retry {})",
                pause.as_secs(),
                retry + 1
            );
            retry += 1;
        }
    }
}

/// A page the mirror does not have, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Missing {
    /// The site path, `/documentation/business-messaging/whatsapp/…`.
    path: String,
    reason: String,
}

/// What a run of [`mirror`] did.
#[derive(Debug, Default)]
struct Report {
    fetched: usize,
    /// Pages already mirrored, left alone (no `--force`).
    skipped: usize,
    missing: Vec<Missing>,
    /// Why the crawl stopped before the end, if it did.
    stopped: Option<String>,
}

/// Fetch each of `pages` into `out`, skipping those already there unless
/// `force`. A persistent 429 stops the crawl; the pages it did not reach
/// are reported missing.
fn mirror<I: Io>(crawler: &mut Crawler<I>, out: &Path, pages: &[String], force: bool) -> Report {
    let todo: Vec<&String> = pages
        .iter()
        .filter(|p| force || !target(out, p).exists())
        .collect();
    let mut report = Report {
        skipped: pages.len() - todo.len(),
        ..Report::default()
    };
    eprintln!(
        "{} page(s) to fetch, {} already mirrored; {} s apart, about {} min",
        todo.len(),
        report.skipped,
        crawler.delay.as_secs(),
        crawler
            .delay
            .as_secs()
            .saturating_mul(u64::try_from(todo.len()).unwrap_or(u64::MAX))
            .div_ceil(60)
    );
    for (i, path) in todo.iter().enumerate() {
        let rel = relative(path);
        let reason = match crawler.get(&format!("{HOST}{path}.md")) {
            Outcome::Body(html) => match save(out, path, &html) {
                Ok(()) => {
                    report.fetched += 1;
                    eprintln!("[{}/{}] {rel}", i + 1, todo.len());
                    continue;
                }
                Err(e) => format!("{e:#}"),
            },
            Outcome::Failed(e) => format!("{e:#}"),
            Outcome::RateLimited(why) => {
                eprintln!("[{}/{}] {rel}: {why}; stopping", i + 1, todo.len());
                report.missing.push(Missing {
                    path: (*path).clone(),
                    reason: why.clone(),
                });
                report.missing.extend(todo[i + 1..].iter().map(|p| Missing {
                    path: (*p).clone(),
                    reason: "not fetched: the crawl stopped on HTTP 429".to_owned(),
                }));
                report.stopped = Some(why);
                break;
            }
        };
        eprintln!("[{}/{}] {rel}: {reason}", i + 1, todo.len());
        report.missing.push(Missing {
            path: (*path).clone(),
            reason,
        });
    }
    report
}

fn save(out: &Path, path: &str, html: &str) -> Result<()> {
    let md = extract_markdown(html)
        .filter(|m| !m.trim().is_empty())
        .context("no markdown body (page gated or moved)")?;
    let file = target(out, path);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&file, md).with_context(|| format!("writing {}", file.display()))
}

/// The pages the navigation of Meta's seed pages links to.
fn discover_pages<I: Io>(crawler: &mut Crawler<I>) -> Result<Vec<String>> {
    let mut pages = BTreeSet::new();
    for seed in SEEDS {
        let url = format!("{HOST}{PREFIX}{seed}");
        match crawler.get(&url) {
            Outcome::Body(html) => discover(&html, &mut pages),
            Outcome::Failed(e) => return Err(e.context("fetching a seed page")),
            Outcome::RateLimited(why) => bail!("GET {url}: {why}"),
        }
    }
    if pages.is_empty() {
        bail!("no pages discovered — Meta's page layout may have changed");
    }
    eprintln!("discovered {} pages", pages.len());
    Ok(pages.into_iter().collect())
}

/// The whole `meta-docs` command over `io`: the pages to fetch (Meta's
/// navigation, or the README's list with `--missing-only`), the crawl, then
/// the README rewritten with what is still missing, stop or not.
fn meta_docs(io: impl Io, options: &Options) -> Result<()> {
    let readme = options.out.join(README);
    let mut crawler = Crawler::new(io, options.delay, BACKOFF);
    let pages = if options.missing_only {
        let text = fs::read_to_string(&readme).with_context(|| {
            format!(
                "--missing-only reads {}: run a full crawl first",
                readme.display()
            )
        })?;
        let pages = parse_missing(&text).with_context(|| {
            format!(
                "{} has no `{MISSING_HEADER}` list: not written by `cargo xtask meta-docs`",
                readme.display()
            )
        })?;
        eprintln!(
            "{} listed as unavailable in {}",
            pages.len(),
            readme.display()
        );
        pages
    } else {
        discover_pages(&mut crawler)?
    };
    let report = mirror(&mut crawler, &options.out, &pages, options.force);
    fs::create_dir_all(&options.out)?;
    fs::write(&readme, render_readme(&report.missing))
        .with_context(|| format!("writing {}", readme.display()))?;
    eprintln!(
        "fetched {} page(s) into {}, {} already there; {} unavailable (listed in {})",
        report.fetched,
        options.out.display(),
        report.skipped,
        report.missing.len(),
        readme.display()
    );
    if let Some(why) = report.stopped {
        bail!(
            "stopped early ({why}); `just meta-docs --missing-only` later retries what {} lists",
            readme.display()
        );
    }
    Ok(())
}

/// The mirror's `README.md`: where the pages come from, and `missing`, one
/// line each (the list [`parse_missing`] reads back).
fn render_readme(missing: &[Missing]) -> String {
    let mut text = format!(
        "# Meta WhatsApp docs mirror\n\n\
         Fetched by `cargo xtask meta-docs` from {HOST}{PREFIX}.\n\
         Local reference only — Meta's copyrighted docs, never commit.\n\n\
         Paths mirror the site: `webhooks/reference/messages/text.md` is\n\
         {HOST}{PREFIX}/webhooks/reference/messages/text\n\n\
         `just meta-docs --missing-only` retries the pages below, then lists\n\
         here only the ones still missing.\n\n\
         {MISSING_HEADER} ({}):\n\n",
        missing.len()
    );
    for m in missing {
        let reason = m.reason.split_whitespace().collect::<Vec<_>>().join(" ");
        let _ = writeln!(text, "- {}: {reason}", m.path);
    }
    text
}

/// The site paths a mirror's README lists as unavailable, sorted and
/// deduplicated, or `None` when it has no such list. Items name a page as
/// its site path (what [`render_readme`] writes), its URL, or its path in
/// the mirror (`flows/changelog`, `flows/changelog.md`), backticks or not,
/// followed by `: <reason>`; other lines are skipped.
fn parse_missing(readme: &str) -> Option<Vec<String>> {
    let mut lines = readme
        .lines()
        .skip_while(|l| !l.trim_start().starts_with(MISSING_HEADER));
    lines.next()?;
    let mut pages = BTreeSet::new();
    for line in lines {
        let line = line.trim();
        if line.starts_with('#') {
            break;
        }
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            pages.extend(listed_page(item));
        }
    }
    Some(pages.into_iter().collect())
}

/// The site path a README item starts with, if it names a page.
fn listed_page(item: &str) -> Option<String> {
    let first = item
        .trim_start()
        .trim_start_matches('`')
        .split(|c: char| c.is_whitespace() || c == '`')
        .next()?
        .trim_end_matches(':');
    let path = first.strip_prefix(HOST).unwrap_or(first);
    let path = path.strip_suffix(".md").unwrap_or(path);
    let rel = match path.strip_prefix(PREFIX) {
        Some(below) if below.starts_with('/') => below,
        // The section itself, or another one (`…/whatsapp-flows/…`), which
        // [`discover`] does not collect either.
        Some(_) => return None,
        None => path,
    }
    .trim_matches('/');
    is_page(rel).then(|| format!("{PREFIX}/{rel}"))
}

/// Whether `rel`, a path below [`PREFIX`], is one a page file may be
/// written at: segments of `[A-Za-z0-9._-]`, none empty, `.` or `..`, so
/// no path from Meta's HTML or from the README escapes the mirror.
fn is_page(rel: &str) -> bool {
    !rel.is_empty()
        && rel.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
}

/// Collect every `/documentation/business-messaging/whatsapp/...` path in a
/// page, whether written plainly or JSON-escaped (`\/documentation\/...`).
fn discover(html: &str, into: &mut BTreeSet<String>) {
    let unescaped = html.replace("\\/", "/");
    let mut rest = unescaped.as_str();
    while let Some(i) = rest.find(PREFIX) {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || "/-_.".contains(c)))
            .unwrap_or(tail.len());
        let path = tail[..end].trim_end_matches('/');
        let is_asset = [".png", ".jpg", ".svg", ".md", ".gif"]
            .iter()
            .any(|ext| path.ends_with(ext));
        let rel = path.strip_prefix(PREFIX).and_then(|r| r.strip_prefix('/'));
        if !is_asset && rel.is_some_and(is_page) {
            into.insert(path.to_owned());
        }
        rest = &tail[end..];
    }
}

/// Meta wraps the Markdown in `<pre>` with HTML entities.
fn extract_markdown(html: &str) -> Option<String> {
    let start = html.find("<pre")?;
    let body_start = start + html[start..].find('>')? + 1;
    let end = body_start + html[body_start..].rfind("</pre>")?;
    Some(unescape(&html[body_start..end]))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.bytes().take(12).position(|b| b == b';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        if let Some(c) = decoded {
            out.push(c);
            rest = &tail[semi + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// A page's path below [`PREFIX`], as the mirror names it (no `.md`).
fn relative(path: &str) -> &str {
    path.strip_prefix(PREFIX)
        .unwrap_or(path)
        .trim_start_matches('/')
}

fn target(out: &Path, path: &str) -> PathBuf {
    out.join(format!("{}.md", relative(path)))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// A scripted [`Io`] for tests: answers in order, logs every call.
    struct Script {
        answers: VecDeque<Fetched>,
        log: Vec<Call>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Get(String),
        Sleep(u64),
    }

    impl Io for Script {
        fn get(&mut self, url: &str) -> Fetched {
            self.log.push(Call::Get(url.to_owned()));
            self.answers
                .pop_front()
                .unwrap_or_else(|| Fetched::Failed(anyhow!("unscripted GET {url}")))
        }

        fn sleep(&mut self, wait: Duration) {
            self.log.push(Call::Sleep(wait.as_secs()));
        }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn page(rel: &str) -> String {
        format!("{PREFIX}/{rel}")
    }

    fn url(rel: &str) -> String {
        format!("{HOST}{PREFIX}/{rel}.md")
    }

    fn md(body: &str) -> Fetched {
        Fetched::Body(format!("<html><pre>{body}</pre></html>"))
    }

    /// An empty directory of its own under the system temp dir, removed
    /// when dropped, by a failing test too.
    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// So a test keeps the script [`meta_docs`] ran on, to read its log.
    impl<T: Io> Io for &mut T {
        fn get(&mut self, url: &str) -> Fetched {
            (**self).get(url)
        }

        fn sleep(&mut self, wait: Duration) {
            (**self).sleep(wait);
        }
    }

    fn script(answers: impl IntoIterator<Item = Fetched>) -> Script {
        Script {
            answers: answers.into_iter().collect(),
            log: Vec::new(),
        }
    }

    fn crawler(answers: impl IntoIterator<Item = Fetched>) -> Crawler<Script> {
        Crawler::new(script(answers), DEFAULT_DELAY, BACKOFF)
    }

    fn options(out: &Path, missing_only: bool) -> Options {
        Options {
            out: out.to_path_buf(),
            force: false,
            missing_only,
            delay: DEFAULT_DELAY,
        }
    }

    #[test]
    fn backoff_doubles_from_a_minute_to_half_an_hour_then_gives_up() {
        let waits: Vec<_> = (0..8).map(|retry| BACKOFF.wait(retry, None)).collect();
        assert_eq!(
            waits,
            [60, 120, 240, 480, 960, 1800]
                .map(|s| Some(secs(s)))
                .into_iter()
                .chain([None, None])
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn backoff_honours_retry_after_within_the_cap_and_the_retry_count() {
        // Shorter or longer than the schedule's wait: Meta's wins.
        assert_eq!(BACKOFF.wait(0, Some(secs(5))), Some(secs(5)));
        assert_eq!(BACKOFF.wait(0, Some(secs(0))), Some(secs(0)));
        assert_eq!(BACKOFF.wait(2, Some(secs(900))), Some(secs(900)));
        assert_eq!(BACKOFF.wait(5, Some(secs(1800))), Some(secs(1800)));
        // Longer than the cap: give up rather than sleep for hours.
        assert_eq!(BACKOFF.wait(0, Some(secs(1801))), None);
        // A short Retry-After does not buy retries past the schedule's.
        assert_eq!(BACKOFF.wait(6, Some(secs(1))), None);
    }

    #[test]
    fn backoff_tries_a_reachable_cap_once() {
        let short = Backoff {
            first: secs(60),
            cap: secs(240),
        };
        let waits: Vec<_> = (0..5).map(|retry| short.wait(retry, None)).collect();
        assert_eq!(
            waits,
            [Some(secs(60)), Some(secs(120)), Some(secs(240)), None, None]
        );
    }

    #[test]
    fn backoff_ends_for_degenerate_settings() {
        let zero = Backoff {
            first: secs(0),
            cap: secs(10),
        };
        assert_eq!(zero.wait(0, None), Some(secs(0)));
        assert_eq!(zero.wait(1, None), None);
        let inverted = Backoff {
            first: secs(100),
            cap: secs(10),
        };
        assert_eq!(inverted.wait(0, None), Some(secs(10)));
        assert_eq!(inverted.wait(1, None), None);
    }

    #[test]
    fn retry_after_reads_seconds_and_http_dates() {
        let now = httpdate::parse_http_date("Sat, 26 Sep 2026 10:00:00 GMT").unwrap();
        assert_eq!(retry_after("120", now), Some(secs(120)));
        assert_eq!(retry_after(" 7 ", now), Some(secs(7)));
        assert_eq!(
            retry_after("Sat, 26 Sep 2026 10:05:00 GMT", now),
            Some(secs(300))
        );
        // The obsolete forms RFC 9110 still asks recipients to accept.
        assert_eq!(
            retry_after("Saturday, 26-Sep-26 10:01:00 GMT", now),
            Some(secs(60))
        );
        assert_eq!(retry_after("Sat Sep 26 10:00:30 2026", now), Some(secs(30)));
        assert_eq!(
            retry_after("Sat, 26 Sep 2026 09:00:00 GMT", now),
            Some(secs(0))
        );
        // Delay-seconds too large for a u64 is still delay-seconds (RFC
        // 9110: `1*DIGIT`), longer than any wait: not a missing header.
        assert_eq!(
            retry_after("99999999999999999999999", now),
            Some(Duration::MAX)
        );
        for junk in ["", "soon", "-5", "+5", "1.5", "5 s"] {
            assert_eq!(retry_after(junk, now), None, "{junk:?}");
        }
    }

    #[test]
    fn requests_are_spaced_by_the_delay_and_429s_back_off() {
        let mut c = crawler([
            md("a"),
            Fetched::RateLimited(None),
            Fetched::RateLimited(Some(secs(90))),
            md("b"),
            md("c"),
        ]);
        assert!(matches!(c.get("u1"), Outcome::Body(_)));
        assert!(matches!(c.get("u2"), Outcome::Body(_)));
        assert!(matches!(c.get("u3"), Outcome::Body(_)));
        assert_eq!(
            c.io.log,
            [
                Call::Get("u1".into()),
                Call::Sleep(15),
                Call::Get("u2".into()),
                Call::Sleep(60),
                Call::Get("u2".into()),
                // Meta's 90 s, not the schedule's 120 s for a second retry.
                Call::Sleep(90),
                Call::Get("u2".into()),
                // The schedule starts over for the next page.
                Call::Sleep(15),
                Call::Get("u3".into()),
            ]
        );
        assert!(c.io.answers.is_empty());
    }

    #[test]
    fn a_short_retry_after_never_brings_requests_closer_than_the_delay() {
        // Retry-After 0, or an HTTP-date already past (a clock a little
        // ahead of Meta's): still `delay` apart, never back to back.
        let mut c = crawler([
            md("a"),
            Fetched::RateLimited(Some(secs(0))),
            Fetched::RateLimited(Some(secs(7))),
            md("b"),
        ]);
        c.get("u1");
        assert!(matches!(c.get("u2"), Outcome::Body(_)));
        assert_eq!(
            c.io.log,
            [
                Call::Get("u1".into()),
                Call::Sleep(15),
                Call::Get("u2".into()),
                Call::Sleep(15),
                Call::Get("u2".into()),
                Call::Sleep(15),
                Call::Get("u2".into()),
            ]
        );
    }

    #[test]
    fn a_zero_delay_does_not_sleep() {
        let mut c = crawler([md("a"), md("b")]);
        c.delay = Duration::ZERO;
        c.get("u1");
        c.get("u2");
        assert_eq!(c.io.log, [Call::Get("u1".into()), Call::Get("u2".into())]);
    }

    #[test]
    fn a_page_refused_through_the_whole_schedule_is_rate_limited() {
        let mut c = crawler((0..7).map(|_| Fetched::RateLimited(None)));
        let Outcome::RateLimited(why) = c.get("u") else {
            panic!("expected RateLimited")
        };
        assert_eq!(why, "HTTP 429, still after 6 retries");
        let sleeps: Vec<_> =
            c.io.log
                .iter()
                .filter_map(|call| match call {
                    Call::Sleep(s) => Some(*s),
                    Call::Get(_) => None,
                })
                .collect();
        assert_eq!(sleeps, [60, 120, 240, 480, 960, 1800]);
        assert!(c.io.answers.is_empty());
    }

    #[test]
    fn a_retry_after_over_the_cap_gives_up_at_once() {
        let mut c = crawler([Fetched::RateLimited(Some(secs(3600)))]);
        let Outcome::RateLimited(why) = c.get("u") else {
            panic!("expected RateLimited")
        };
        assert!(why.contains("Retry-After 3600 s"), "{why}");
        assert_eq!(c.io.log, [Call::Get("u".into())]);
    }

    #[test]
    fn other_failures_are_not_retried() {
        let mut c = crawler([Fetched::Failed(anyhow!("HTTP 404 Not Found"))]);
        assert!(matches!(c.get("u"), Outcome::Failed(_)));
        assert_eq!(c.io.log, [Call::Get("u".into())]);
    }

    #[test]
    fn mirror_skips_what_exists_and_lists_what_failed() {
        let out = scratch("mirror");
        fs::create_dir_all(out.join("a")).unwrap();
        fs::write(out.join("a/have.md"), "kept").unwrap();
        let pages = ["a/have", "a/new", "gated"].map(page).to_vec();
        let mut c = crawler([md("# New &amp; fresh"), Fetched::Body("no pre".into())]);
        let report = mirror(&mut c, &out, &pages, false);
        assert_eq!(
            c.io.log,
            [
                Call::Get(url("a/new")),
                Call::Sleep(15),
                Call::Get(url("gated"))
            ]
        );
        assert_eq!((report.fetched, report.skipped), (1, 1));
        assert_eq!(fs::read_to_string(out.join("a/have.md")).unwrap(), "kept");
        assert_eq!(
            fs::read_to_string(out.join("a/new.md")).unwrap(),
            "# New & fresh"
        );
        assert_eq!(
            report.missing,
            [Missing {
                path: page("gated"),
                reason: "no markdown body (page gated or moved)".into()
            }]
        );
        assert!(report.stopped.is_none());
    }

    #[test]
    fn mirror_with_force_refetches_what_exists() {
        let out = scratch("force");
        fs::write(out.join("have.md"), "old").unwrap();
        let mut c = crawler([md("new")]);
        let report = mirror(&mut c, &out, &[page("have")], true);
        assert_eq!((report.fetched, report.skipped), (1, 0));
        assert_eq!(fs::read_to_string(out.join("have.md")).unwrap(), "new");
    }

    #[test]
    fn mirror_stops_on_a_persistent_429_and_lists_the_rest() {
        let out = scratch("stop");
        let pages = ["p1", "p2", "p3", "p4"].map(page).to_vec();
        let answers = std::iter::once(md("one"))
            .chain((0..7).map(|_| Fetched::RateLimited(None)))
            .collect::<Vec<_>>();
        let mut c = crawler(answers);
        let report = mirror(&mut c, &out, &pages, false);
        assert_eq!(report.fetched, 1);
        assert_eq!(
            report.stopped.as_deref(),
            Some("HTTP 429, still after 6 retries")
        );
        // Nothing asked of Meta after the stop: p3 and p4 never requested.
        assert!(!c.io.log.contains(&Call::Get(url("p3"))));
        assert!(c.io.answers.is_empty());
        let missing: Vec<_> = report.missing.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(missing, [page("p2"), page("p3"), page("p4")]);
        assert_eq!(
            report.missing[2].reason,
            "not fetched: the crawl stopped on HTTP 429"
        );
    }

    #[test]
    fn missing_only_retries_the_listed_pages_and_lists_only_those_still_missing() {
        let out = scratch("missing-only");
        fs::write(out.join("have.md"), "fetched by hand").unwrap();
        // The first crawler's format.
        fs::write(
            out.join(README),
            format!(
                "# Meta WhatsApp docs mirror\n\nUnavailable pages (3):\n\n\
                 - {}: GET {HOST}/x.md: http status: 429\n\
                 - {}: GET {HOST}/y.md: http status: 429\n\
                 - {}: no markdown body (page gated or moved)\n\n",
                page("now"),
                page("have"),
                page("gated"),
            ),
        )
        .unwrap();
        let mut io = script([Fetched::Body("no pre".into()), md("now here")]);
        meta_docs(&mut io, &options(&out, true)).unwrap();
        // No seed page, nothing already mirrored: the listed pages only.
        assert_eq!(
            io.log,
            [
                Call::Get(url("gated")),
                Call::Sleep(15),
                Call::Get(url("now"))
            ]
        );
        assert!(io.answers.is_empty());
        assert_eq!(fs::read_to_string(out.join("now.md")).unwrap(), "now here");
        let readme = fs::read_to_string(out.join(README)).unwrap();
        assert_eq!(parse_missing(&readme), Some(vec![page("gated")]));
    }

    #[test]
    fn a_stopped_crawl_rewrites_the_readme_then_fails() {
        let out = scratch("stopped");
        let listed = ["p1", "p2"].map(|p| Missing {
            path: page(p),
            reason: "an earlier run's reason".into(),
        });
        fs::write(out.join(README), render_readme(&listed)).unwrap();
        let mut io = script((0..7).map(|_| Fetched::RateLimited(None)));
        let err = meta_docs(&mut io, &options(&out, true)).unwrap_err();
        assert!(
            format!("{err:#}").starts_with("stopped early (HTTP 429, still after 6 retries)"),
            "{err:#}"
        );
        assert!(!io.log.contains(&Call::Get(url("p2"))));
        let readme = fs::read_to_string(out.join(README)).unwrap();
        assert_eq!(parse_missing(&readme), Some(vec![page("p1"), page("p2")]));
        assert!(
            readme.contains(&format!(
                "- {}: not fetched: the crawl stopped on HTTP 429\n",
                page("p2")
            )),
            "{readme}"
        );
    }

    #[test]
    fn a_full_crawl_follows_the_seeds_navigation_and_replaces_the_list() {
        let out = scratch("full");
        let stale = Missing {
            path: page("stale"),
            reason: "no longer linked".into(),
        };
        fs::write(out.join(README), render_readme(&[stale])).unwrap();
        let mut io = script([
            Fetched::Body(format!("<a href=\"{PREFIX}/a/\">a</a>")),
            Fetched::Body(format!("<a href=\"{PREFIX}/b\">b</a>")),
            md("page a"),
            Fetched::Failed(anyhow!("HTTP 404 Not Found")),
        ]);
        meta_docs(&mut io, &options(&out, false)).unwrap();
        assert_eq!(
            io.log,
            [
                Call::Get(format!("{HOST}{PREFIX}/overview/")),
                Call::Sleep(15),
                Call::Get(format!("{HOST}{PREFIX}/flows/")),
                Call::Sleep(15),
                Call::Get(url("a")),
                Call::Sleep(15),
                Call::Get(url("b")),
            ]
        );
        assert_eq!(fs::read_to_string(out.join("a.md")).unwrap(), "page a");
        let readme = fs::read_to_string(out.join(README)).unwrap();
        assert_eq!(parse_missing(&readme), Some(vec![page("b")]));
    }

    #[test]
    fn a_full_crawl_without_the_seeds_navigation_fails_and_keeps_the_list() {
        let seed = |s: &str| Call::Get(format!("{HOST}{PREFIX}{s}"));
        let cases: [(Vec<Fetched>, Vec<Call>, &str); 3] = [
            (
                vec![Fetched::Failed(anyhow!("HTTP 404 Not Found"))],
                vec![seed("/overview/")],
                "fetching a seed page: HTTP 404 Not Found",
            ),
            (
                // The first seed, refused through the whole schedule: the
                // second is never asked for.
                (0..7).map(|_| Fetched::RateLimited(None)).collect(),
                (0..7).map(|_| seed("/overview/")).collect(),
                "HTTP 429, still after 6 retries",
            ),
            (
                vec![Fetched::Body("<p>moved</p>".into()), md("no links")],
                vec![seed("/overview/"), seed("/flows/")],
                "no pages discovered",
            ),
        ];
        for (answers, gets, error) in cases {
            let out = scratch("seeds");
            let listed = render_readme(&[Missing {
                path: page("kept"),
                reason: "an earlier run's reason".into(),
            }]);
            fs::write(out.join(README), &listed).unwrap();
            let mut io = script(answers);
            let err = meta_docs(&mut io, &options(&out, false)).unwrap_err();
            assert!(format!("{err:#}").contains(error), "{err:#}");
            let got: Vec<_> = io
                .log
                .iter()
                .filter(|call| matches!(call, Call::Get(_)))
                .collect();
            assert_eq!(got, gets.iter().collect::<Vec<_>>(), "{error}");
            assert_eq!(io.log.len(), gets.len() * 2 - 1, "paced: {error}");
            assert!(io.answers.is_empty(), "{error}");
            // Nothing crawled, nothing learnt: the list stays as it was.
            assert_eq!(fs::read_to_string(out.join(README)).unwrap(), listed);
        }
    }

    #[test]
    fn readme_round_trips_its_missing_list() {
        let missing = vec![
            Missing {
                path: page("flows/changelog"),
                reason: "GET https://x/y.md: HTTP 404 Not Found".into(),
            },
            Missing {
                path: page("overview"),
                reason: "two\nlines:  and\tspaces".into(),
            },
        ];
        let text = render_readme(&missing);
        assert!(text.contains("Unavailable pages (2):"), "{text}");
        assert!(
            text.contains(&format!("- {}: two lines: and spaces\n", page("overview"))),
            "{text}"
        );
        assert_eq!(
            parse_missing(&text),
            Some(vec![page("flows/changelog"), page("overview")])
        );
        assert_eq!(parse_missing(&render_readme(&[])), Some(vec![]));
    }

    #[test]
    fn readme_parsing_reads_the_first_crawls_format() {
        // What the parallel crawler wrote on 2026-09-24 (reasons shortened).
        let readme = "# Meta WhatsApp docs mirror\n\n\
            Fetched by `cargo xtask meta-docs` from https://developers.facebook.com/documentation/business-messaging/whatsapp.\n\
            Local reference only — Meta's copyrighted docs, never commit.\n\n\
            Paths mirror the site: `webhooks/reference/messages/text.md` is\n\
            https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/reference/messages/text\n\n\
            Unavailable pages (3):\n\n\
            - /documentation/business-messaging/whatsapp/webhooks/reference/standby: GET https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/reference/standby.md: http status: 429\n\
            - /documentation/business-messaging/whatsapp/pricing/prepaid-billing: no markdown body (page gated or moved)\n\
            - /documentation/business-messaging/whatsapp/overview: GET https://developers.facebook.com/documentation/business-messaging/whatsapp/overview.md: http status: 404\n\n";
        assert_eq!(
            parse_missing(readme),
            Some(vec![
                page("overview"),
                page("pricing/prepaid-billing"),
                page("webhooks/reference/standby"),
            ])
        );
    }

    #[test]
    fn readme_parsing_accepts_urls_mirror_paths_and_backticks() {
        let readme = "intro\n\nUnavailable pages (7):\n\n\
            - https://developers.facebook.com/documentation/business-messaging/whatsapp/flows/changelog: gated\n\
            - `webhooks/reference/messaging-handovers`: 404\n\
            * webhooks/reference/standby.md (gated)\n\
            -   /flows/guides/x/  \n\
            - /documentation/business-messaging/whatsapp/overview: again\n\
            - /documentation/business-messaging/whatsapp/overview: twice\n\
            not a list item: /documentation/business-messaging/whatsapp/ignored\n\
            \n## Notes\n\n- after/the/list: not a missing page\n";
        assert_eq!(
            parse_missing(readme),
            Some(vec![
                page("flows/changelog"),
                page("flows/guides/x"),
                page("overview"),
                page("webhooks/reference/messaging-handovers"),
                page("webhooks/reference/standby"),
            ])
        );
    }

    #[test]
    fn readme_parsing_refuses_paths_that_escape_the_mirror() {
        let readme = "Unavailable pages (5):\n\n\
            - ../../etc/passwd: no\n\
            - /documentation/business-messaging/whatsapp/../x: no\n\
            - a//b: no\n\
            - ./a: no\n\
            - :\n";
        assert_eq!(parse_missing(readme), Some(vec![]));
    }

    #[test]
    fn readme_parsing_refuses_what_is_not_a_page_below_the_prefix() {
        let readme = "Unavailable pages (6):\n\n\
            - ..\\..\\etc\\passwd: a Windows traversal, one segment on Unix\n\
            - a?b=c: a query\n\
            - ~/x: a home directory\n\
            - /documentation/business-messaging/whatsapp: the section itself\n\
            - /documentation/business-messaging/whatsapp-flows/x: another section\n\
            - https://developers.facebook.com/documentation/business-messaging/whatsappx/y.md: again\n";
        assert_eq!(parse_missing(readme), Some(vec![]));
    }

    #[test]
    fn readme_without_the_list_is_not_parsed() {
        assert_eq!(parse_missing("# Some other README\n\n- a/b: c\n"), None);
        assert_eq!(parse_missing(""), None);
        // The header is the list's own, not any line about availability.
        assert_eq!(parse_missing("Unavailable: none\n\n- a/b: c\n"), None);
    }

    #[test]
    fn arguments() {
        let parse = |args: &[&str]| parse_args(args.iter().map(|a| (*a).to_owned()));
        assert_eq!(
            parse(&[]).unwrap(),
            Some(Options {
                out: PathBuf::from(".meta-docs"),
                force: false,
                missing_only: false,
                delay: secs(15),
            })
        );
        assert_eq!(
            parse(&[
                "--missing-only",
                "--delay-secs",
                "0",
                "--out",
                "d",
                "--force"
            ])
            .unwrap(),
            Some(Options {
                out: PathBuf::from("d"),
                force: true,
                missing_only: true,
                delay: secs(0),
            })
        );
        assert_eq!(parse(&["--help"]).unwrap(), None);
        for bad in [
            &["--delay-secs"][..],
            &["--delay-secs", "1.5"],
            &["--delay-secs", "-1"],
            &["--out"],
            &["--jobs", "6"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn discovers_plain_and_escaped_paths() {
        let html = r#"<a href="/documentation/business-messaging/whatsapp/webhooks/overview/">x</a>
            {"u":"\/documentation\/business-messaging\/whatsapp\/templates\/overview"}
            <img src="/documentation/business-messaging/whatsapp/x.png">
            <a href="/documentation/business-messaging/whatsapp/../../../escape">x</a>"#;
        let mut set = BTreeSet::new();
        discover(html, &mut set);
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec![
                "/documentation/business-messaging/whatsapp/templates/overview",
                "/documentation/business-messaging/whatsapp/webhooks/overview",
            ]
        );
    }

    /// A server on the loopback interface answering each connection with
    /// the next of `responses`; the URL it listens at.
    fn serve(responses: &'static [&'static str]) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        format!("http://{addr}/page.md")
    }

    #[test]
    fn http_reads_a_429_and_its_retry_after_instead_of_failing() {
        let url = serve(&[
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 42\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: whenever\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n<pre>x</pre>",
        ]);
        // No proxy from the environment: the server is local.
        let mut http = Http::configured(ureq::Agent::config_builder().proxy(None));
        assert!(matches!(http.get(&url), Fetched::RateLimited(Some(d)) if d == secs(42)));
        assert!(matches!(http.get(&url), Fetched::RateLimited(None)));
        assert!(matches!(http.get(&url), Fetched::RateLimited(None)));
        let Fetched::Failed(e) = http.get(&url) else {
            panic!("a 404 is a failure")
        };
        assert_eq!(e.to_string(), format!("GET {url}: HTTP 404 Not Found"));
        assert!(matches!(http.get(&url), Fetched::Body(b) if b == "<pre>x</pre>"));
    }

    #[test]
    fn extracts_and_unescapes() {
        let html = "<html><pre data-x=\"1\"># T\n&#123;&quot;a&quot;: &lt;b&gt; &amp; &#x41;&#125; &bogus &—é;</pre></html>";
        assert_eq!(
            extract_markdown(html).as_deref(),
            Some("# T\n{\"a\": <b> & A} &bogus &—é;")
        );
    }
}
