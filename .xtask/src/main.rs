//! `cargo xtask <command>`.
//!
//! - `meta-docs [--out DIR] [--force]`: mirror Meta's WhatsApp Business
//!   Platform docs as Markdown (Meta serves `<page>.md`) so agents and humans
//!   can grep the real spec. The output is gitignored: the docs are Meta's
//!   copyrighted material, fetched for local reference only.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

const HOST: &str = "https://developers.facebook.com";
const PREFIX: &str = "/documentation/business-messaging/whatsapp";
/// Pages whose HTML embeds the documentation navigation tree.
const SEEDS: &[&str] = &["/overview/", "/flows/"];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("meta-docs") {
        eprintln!("usage: cargo xtask meta-docs [--out DIR] [--force]");
        std::process::exit(2);
    }
    let mut out = PathBuf::from(".meta-docs");
    let mut force = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(args.next().context("--out needs a value")?),
            "--force" => force = true,
            other => bail!("unknown argument `{other}`"),
        }
    }
    meta_docs(&out, force)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(45)))
        .user_agent("wa-rs-xtask (docs mirror)")
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut resp = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    resp.body_mut()
        .with_config()
        .limit(20 * 1024 * 1024)
        .read_to_string()
        .with_context(|| format!("reading {url}"))
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
        if !is_asset && path.len() > PREFIX.len() {
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

fn target(out: &Path, path: &str) -> PathBuf {
    let rel = path
        .strip_prefix(PREFIX)
        .unwrap_or(path)
        .trim_start_matches('/');
    out.join(format!("{rel}.md"))
}

fn meta_docs(out: &Path, force: bool) -> Result<()> {
    let agent = agent();
    let mut pages = BTreeSet::new();
    for seed in SEEDS {
        let html = get(&agent, &format!("{HOST}{PREFIX}{seed}"))?;
        discover(&html, &mut pages);
    }
    if pages.is_empty() {
        bail!("no pages discovered — Meta's page layout may have changed");
    }
    eprintln!("discovered {} pages", pages.len());

    let queue = Arc::new(Mutex::new(pages.into_iter().collect::<Vec<_>>()));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let fetched = Arc::new(Mutex::new(0usize));
    let workers: Vec<_> = (0..6)
        .map(|_| {
            let (queue, failures, fetched) = (queue.clone(), failures.clone(), fetched.clone());
            let agent = agent.clone();
            let out = out.to_path_buf();
            thread::spawn(move || {
                while let Some(path) = queue.lock().ok().and_then(|mut q| q.pop()) {
                    let file = target(&out, &path);
                    if !force && file.exists() {
                        continue;
                    }
                    let result = get(&agent, &format!("{HOST}{path}.md")).and_then(|html| {
                        let md = extract_markdown(&html)
                            .filter(|m| !m.trim().is_empty())
                            .context("no markdown body (page gated or moved)")?;
                        if let Some(dir) = file.parent() {
                            fs::create_dir_all(dir)?;
                        }
                        fs::write(&file, md)?;
                        Ok(())
                    });
                    match result {
                        Ok(()) => {
                            if let Ok(mut n) = fetched.lock() {
                                *n += 1;
                            }
                        }
                        Err(e) => {
                            if let Ok(mut f) = failures.lock() {
                                f.push(format!("{path}: {e:#}"));
                            }
                        }
                    }
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    let failures = failures.lock().map(|f| f.clone()).unwrap_or_default();
    let fetched = fetched.lock().map(|n| *n).unwrap_or_default();
    fs::create_dir_all(out)?;
    fs::write(
        out.join("README.md"),
        format!(
            "# Meta WhatsApp docs mirror\n\nFetched by `cargo xtask meta-docs` from {HOST}{PREFIX}.\n\
             Local reference only — Meta's copyrighted docs, never commit.\n\n\
             Paths mirror the site: `webhooks/reference/messages/text.md` is\n\
             {HOST}{PREFIX}/webhooks/reference/messages/text\n\n\
             Unavailable pages ({}):\n\n{}\n",
            failures.len(),
            failures.iter().fold(String::new(), |mut acc, f| {
                acc.push_str("- ");
                acc.push_str(f);
                acc.push('\n');
                acc
            })
        ),
    )?;
    eprintln!(
        "fetched {fetched} page(s) into {}; {} unavailable (see README.md there)",
        out.display(),
        failures.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_plain_and_escaped_paths() {
        let html = r#"<a href="/documentation/business-messaging/whatsapp/webhooks/overview/">x</a>
            {"u":"\/documentation\/business-messaging\/whatsapp\/templates\/overview"}
            <img src="/documentation/business-messaging/whatsapp/x.png">"#;
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

    #[test]
    fn extracts_and_unescapes() {
        let html = "<html><pre data-x=\"1\"># T\n&#123;&quot;a&quot;: &lt;b&gt; &amp; &#x41;&#125; &bogus &—é;</pre></html>";
        assert_eq!(
            extract_markdown(html).as_deref(),
            Some("# T\n{\"a\": <b> & A} &bogus &—é;")
        );
    }
}
