//! The decisive test of roadmap L22b: a scripted MCP client runs the
//! binary as a subprocess and speaks JSON-RPC 2.0 to it over stdio, one
//! message per line, against this repository's own docs. A legacy client
//! (`initialize`, then `tools/list` and `tools/call`) and a modern one
//! (revision 2026-07-28: `server/discover`, then per-request `_meta`) each
//! list the tools, search `Idempotency-Key`, read the server guide's
//! section, and get a missing page as a tool error (`isError: true`), with
//! the server still answering afterwards.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)] // test crate: a panic is the report

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(20);

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The scripted client: the server's stdin, and its stdout's lines through
/// a thread, so a server that hangs fails the test instead of blocking it.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    modern: bool,
}

impl Client {
    fn spawn(root: Option<&Path>, modern: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_meta-whatsapp-docs-mcp"));
        if let Some(root) = root {
            command.arg("--root").arg(root);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            lines,
            modern,
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn next(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(TIMEOUT)
            .expect("the server answered in time");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON on stdout ({e}): {line}"))
    }

    /// A request; its response, which must carry the same id.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let mut params = params;
        if self.modern {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": {"name": "scripted", "version": "1"}
            });
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let answer = self.next();
        assert_eq!(answer["jsonrpc"], "2.0", "{answer}");
        assert_eq!(answer["id"], id, "{answer}");
        answer
    }

    /// A `tools/call`; its result (never a JSON-RPC error here).
    fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let answer = self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        assert!(answer.get("error").is_none(), "{answer}");
        let result = answer["result"].clone();
        if self.modern {
            assert_eq!(result["resultType"], "complete", "{result}");
        }
        result
    }

    /// Close stdin: the server must exit, successfully.
    fn finish(mut self) {
        drop(self.stdin.take());
        for _ in 0..200 {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("the server did not exit when stdin closed");
    }
}

fn text(result: &Value) -> &str {
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "{result}");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().unwrap()
}

/// What both clients do once they are talking.
fn list_search_read_and_miss(client: &mut Client) {
    // The tools, in a fixed order, each with an object input schema.
    let listed = client.request(10, "tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["list", "search", "read"]);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
    }
    assert_eq!(tools[2]["inputSchema"]["required"], json!(["path"]));

    // Search: the server guide's section is a hit.
    let found = client.call(11, "search", json!({"query": "Idempotency-Key"}));
    assert_eq!(found["isError"], false, "{found}");
    let hits = found["structuredContent"]["hits"].as_array().unwrap();
    assert!(!hits.is_empty() && hits.len() <= 10, "{found}");
    let hit = hits
        .iter()
        .find(|h| h["path"] == "docs/guides/server.md" && h["section"] == "Idempotency keys")
        .unwrap_or_else(|| panic!("no hit in the server guide's section: {hits:#?}"));
    assert!(
        hit["snippet"].as_str().unwrap().contains("Idempotency-Key"),
        "{hit}"
    );
    // The text block is the structured content, serialized.
    assert_eq!(
        serde_json::from_str::<Value>(text(&found)).unwrap(),
        found["structuredContent"]
    );

    // Read that section, by the heading the hit gave.
    let section = client.call(
        12,
        "read",
        json!({"path": "docs/guides/server.md", "section": hit["section"]}),
    );
    assert_eq!(section["isError"], false, "{section}");
    let body = text(&section);
    assert!(body.starts_with("### Idempotency keys\n"), "{body}");
    assert!(body.contains("`Idempotency-Key` header"), "{body}");
    assert!(body.contains("idempotency_key_reused"), "{body}");
    // The section ends where the next one of its level or above starts.
    assert!(!body.contains("\n## Media"), "{body}");
    let guide = fs::read_to_string(repo().join("docs/guides/server.md")).unwrap();
    assert!(
        guide.contains(body.trim_end()),
        "the section is the guide's text"
    );

    // A missing page: a tool execution error, and the server goes on.
    let missing = client.call(13, "read", json!({"path": "docs/guides/no-such-guide.md"}));
    assert_eq!(missing["isError"], true, "{missing}");
    assert!(
        text(&missing).starts_with("no such page: `docs/guides/no-such-guide.md`"),
        "{missing}"
    );
    let missing_section = client.call(
        14,
        "read",
        json!({"path": "docs/guides/server.md", "section": "No such section"}),
    );
    assert_eq!(missing_section["isError"], true, "{missing_section}");
    assert!(
        text(&missing_section).contains("### Idempotency keys (line"),
        "lists the page's sections: {missing_section}"
    );

    // An unknown tool is a protocol error.
    let unknown = client.request(15, "tools/call", json!({"name": "delete", "arguments": {}}));
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");

    let again = client.request(16, "tools/list", json!({}));
    assert!(
        again["result"]["tools"].is_array(),
        "still serving: {again}"
    );
}

#[test]
fn a_legacy_client_initializes_then_lists_searches_and_reads() {
    let mut client = Client::spawn(None, false);
    let init = client.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "scripted", "version": "1"}
        }),
    );
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25", "{init}");
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert_eq!(result["serverInfo"]["name"], "meta-whatsapp-docs-mcp");
    assert!(result["instructions"].as_str().unwrap().contains("search"));
    // The notification gets no answer: the next line is the next response.
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let ping = client.request(2, "ping", json!({}));
    assert_eq!(ping["result"], json!({}));
    list_search_read_and_miss(&mut client);
    client.finish();
}

#[test]
fn a_modern_client_discovers_then_lists_searches_and_reads() {
    let mut client = Client::spawn(None, true);
    let discover = client.request(1, "server/discover", json!({}));
    let result = &discover["result"];
    assert_eq!(result["resultType"], "complete", "{discover}");
    assert_eq!(result["supportedVersions"][0], "2026-07-28");
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "meta-whatsapp-docs-mcp"
    );
    assert_eq!(result["cacheScope"], "public");
    assert!(result["ttlMs"].as_u64().unwrap() > 0);
    let listed = client.request(2, "tools/list", json!({}));
    assert_eq!(listed["result"]["resultType"], "complete");
    assert_eq!(listed["result"]["cacheScope"], "public");
    assert!(listed["result"]["ttlMs"].is_u64());
    list_search_read_and_miss(&mut client);
    client.finish();
}

#[test]
fn protocol_errors_are_json_rpc_errors_and_the_server_survives_them() {
    let mut client = Client::spawn(None, false);
    // No era: neither `_meta` nor `initialize`.
    let bare = client.request(1, "tools/list", json!({}));
    assert_eq!(bare["error"]["code"], -32602, "{bare}");
    // A version the server does not speak.
    client.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "1900-01-01",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    let old = client.next();
    assert_eq!(old["id"], 2);
    assert_eq!(old["error"]["code"], -32022, "{old}");
    assert_eq!(old["error"]["data"]["requested"], "1900-01-01");
    assert!(
        old["error"]["data"]["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
    // Not JSON, then a batch.
    client
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"jsonrpc\": \n[]\n")
        .unwrap();
    client.stdin.as_mut().unwrap().flush().unwrap();
    let parse = client.next();
    assert_eq!(parse["error"]["code"], -32700, "{parse}");
    assert_eq!(parse["id"], Value::Null);
    let batch = client.next();
    assert_eq!(batch["error"]["code"], -32600, "{batch}");
    let unknown = client.request(3, "resources/list", json!({}));
    assert_eq!(unknown["error"]["code"], -32602, "no era yet: {unknown}");
    client.modern = true;
    let unknown = client.request(4, "resources/list", json!({}));
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
    let fine = client.request(5, "tools/list", json!({}));
    assert!(fine["result"]["tools"].is_array());
    client.finish();
}

/// No `read` leaves the corpus: not by `..`, not by an absolute path, not
/// to a repository file outside it, not to Meta's mirror.
#[test]
fn a_read_never_escapes_the_corpus() {
    let mut client = Client::spawn(None, true);
    let absolute = repo().join("Cargo.toml").display().to_string();
    for (id, path) in [
        "../Cargo.toml",
        "docs/../Cargo.toml",
        "docs/guides/../../Cargo.toml",
        "./README.md",
        "/etc/passwd",
        absolute.as_str(),
        "Cargo.toml",
        "crates/meta-whatsapp-core/src/lib.rs",
        ".meta-docs/README.md",
        "docs\\guides\\server.md",
        "docs/guides/",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        let result = client.call(100 + id as u64, "read", json!({"path": path}));
        assert_eq!(result["isError"], true, "{path:?}: {result}");
        let body = text(&result);
        assert!(
            body.starts_with("not a corpus path") || body.starts_with("no such page"),
            "{path:?}: {body}"
        );
        assert!(!body.contains("[workspace]"), "{path:?}");
    }
    client.finish();
}

/// A symbolic link in a corpus tree, to a file or a directory outside it,
/// is neither listed nor read, nor searched.
#[cfg(unix)]
#[test]
fn a_symbolic_link_out_of_the_corpus_is_not_served() {
    use std::os::unix::fs::symlink;
    let base =
        std::env::temp_dir().join(format!("meta-whatsapp-docs-mcp-it-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("root");
    let outside = base.join("outside");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(root.join("docs/a.md"), "# A\n\npublic text\n").unwrap();
    fs::write(outside.join("secret.md"), "# Secret\n\nsecret-token-text\n").unwrap();
    symlink(outside.join("secret.md"), root.join("docs/link.md")).unwrap();
    symlink(&outside, root.join("docs/linked")).unwrap();

    let mut client = Client::spawn(Some(&root), true);
    let listed = client.call(1, "list", json!({}));
    let paths: Vec<&str> = listed["structuredContent"]["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["docs/a.md"]);
    for (id, path) in [(2, "docs/link.md"), (3, "docs/linked/secret.md")] {
        let result = client.call(id, "read", json!({"path": path}));
        assert_eq!(result["isError"], true, "{result}");
        assert!(!text(&result).contains("secret-token-text"));
    }
    let searched = client.call(4, "search", json!({"query": "secret-token-text"}));
    assert_eq!(searched["structuredContent"]["total"], 0, "{searched}");
    client.finish();
    fs::remove_dir_all(&base).unwrap();
}

/// The corpus is what `docs/public/llms.txt` indexes, minus what is not
/// prose (the OpenAPI document): every Markdown or text page it links is
/// served. And every guide and skill is.
#[test]
fn the_corpus_covers_what_llms_txt_indexes() {
    let raw = "https://raw.githubusercontent.com/vaam-apps/meta-whatsapp-rs/main/";
    let llms = fs::read_to_string(repo().join("docs/public/llms.txt")).unwrap();
    let mut client = Client::spawn(None, true);
    let listed = client.call(1, "list", json!({}));
    let pages: Vec<String> = listed["structuredContent"]["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_owned())
        .collect();
    let mut linked = 0;
    for url in llms.split(['(', ')']).filter_map(|s| s.strip_prefix(raw)) {
        if Path::new(url)
            .extension()
            .is_some_and(|e| e == "md" || e == "txt")
        {
            linked += 1;
            assert!(
                pages.contains(&url.to_owned()),
                "llms.txt links {url}, not served"
            );
        }
    }
    assert!(linked > 30, "{linked} links read");
    assert!(pages.contains(&"docs/public/llms.txt".to_owned()));
    for page in &pages {
        assert!(
            page.starts_with("docs/") || page.starts_with("skills/") || !page.contains('/'),
            "{page}"
        );
        assert!(!page.starts_with(".meta-docs"), "{page}");
    }
    // A skill's listing carries its description.
    let skill = listed["structuredContent"]["pages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["path"] == "skills/meta-whatsapp-rs-server/SKILL.md")
        .unwrap();
    assert!(
        skill["description"]
            .as_str()
            .unwrap()
            .contains("meta-whatsapp-server"),
        "{skill}"
    );
    client.finish();
}

/// The tool stays outside the library: no crate under `crates/` names it
/// in its manifest.
#[test]
fn no_crate_depends_on_the_tool() {
    let mut checked = 0;
    for entry in fs::read_dir(repo().join("crates")).unwrap() {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        if let Ok(text) = fs::read_to_string(&manifest) {
            checked += 1;
            assert!(
                !text.contains("meta-whatsapp-docs-mcp") && !text.contains("tools/docs-mcp"),
                "{} names the docs MCP server",
                manifest.display()
            );
        }
    }
    assert!(checked >= 7, "{checked} manifests read");
    let workspace = fs::read_to_string(repo().join("Cargo.toml")).unwrap();
    let defaults = workspace
        .split("default-members = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap();
    assert!(!defaults.contains("tools/docs-mcp"), "not a default member");
}
