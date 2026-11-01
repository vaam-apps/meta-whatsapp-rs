//! `meta-whatsapp-docs-mcp`: an MCP server, over the stdio transport,
//! serving this repository's docs and consumer skills (roadmap L22b).
//!
//! Three tools, all read-only: `list` (every page, a skill with its
//! description), `search` (the sections containing every term of a query)
//! and `read` (a page, or one section by its heading). It speaks the MCP
//! revision 2026-07-28 (per-request `_meta`, `server/discover`) and, after
//! an `initialize`, 2025-11-25 and 2025-06-18 ([`protocol`]). What it
//! serves, and how a read is kept inside it, is [`corpus`].
//!
//! ```text
//! meta-whatsapp-docs-mcp [--root <checkout>]
//! ```
//!
//! `--root` defaults to the checkout the binary was built from. The
//! server reads one JSON-RPC message per line on stdin, answers on
//! stdout, writes nothing else there, and exits when stdin closes. A tool,
//! not part of the library: no crate depends on it.

mod corpus;
mod protocol;
mod tools;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// The longest message read; a longer line is answered with an error and
/// skipped.
const MAX_LINE_BYTES: u64 = 1024 * 1024;

const USAGE: &str = "usage: meta-whatsapp-docs-mcp [--root <checkout>]\n\n\
    An MCP server (stdio) serving meta-whatsapp-rs's docs and consumer skills:\n\
    the tools `list`, `search` and `read`. `--root` defaults to the checkout\n\
    it was built from.\n";

fn main() -> ExitCode {
    let mut root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => match args.next() {
                Some(dir) => root = PathBuf::from(dir),
                None => return usage_error("--root needs a directory"),
            },
            "-h" | "--help" => {
                eprint!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                eprintln!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => return usage_error(&format!("unknown argument `{other}`")),
        }
    }
    let root = match root.canonicalize() {
        Ok(r) if r.is_dir() => r,
        _ => return usage_error(&format!("`{}` is not a directory", root.display())),
    };
    match serve(&root, io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        // The client went away mid-answer: not this server's failure.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("meta-whatsapp-docs-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage_error(why: &str) -> ExitCode {
    eprint!("meta-whatsapp-docs-mcp: {why}\n\n{USAGE}");
    ExitCode::from(2)
}

/// Serve messages from `input` until it ends.
fn serve(root: &std::path::Path, input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut server = protocol::Server::new(root);
    let mut reader = BufReader::new(input);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        let answer = if line.len() as u64 > MAX_LINE_BYTES && line.last() != Some(&b'\n') {
            // Skip the rest of the oversized line.
            loop {
                let mut rest = Vec::new();
                let n = (&mut reader)
                    .take(MAX_LINE_BYTES)
                    .read_until(b'\n', &mut rest)?;
                if n == 0 || rest.last() == Some(&b'\n') {
                    break;
                }
            }
            Some(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": protocol::INVALID_REQUEST,
                        "message": format!("Invalid request: a message over {MAX_LINE_BYTES} bytes"),
                    },
                })
                .to_string(),
            )
        } else {
            match std::str::from_utf8(&line) {
                Ok(text) if text.trim().is_empty() => None,
                Ok(text) => server.handle_line(text.trim_end_matches(['\n', '\r'])),
                Err(_) => server.handle_line("\u{0}not utf-8"),
            }
        };
        if let Some(answer) = answer {
            output.write_all(answer.as_bytes())?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::tests::Scratch;

    fn run(input: &[u8]) -> Vec<serde_json::Value> {
        let s = Scratch::new("serve");
        let mut out = Vec::new();
        serve(&s.0, input, &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn an_oversized_line_is_refused_and_the_next_one_served() {
        let mut input = vec![b'x'; usize::try_from(MAX_LINE_BYTES).unwrap() * 2 + 10];
        input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n");
        let answers = run(&input);
        assert_eq!(answers.len(), 2, "{answers:?}");
        assert_eq!(answers[0]["error"]["code"], protocol::INVALID_REQUEST);
        assert_eq!(answers[1]["result"]["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn blank_lines_and_invalid_utf8_do_not_stop_the_server() {
        let mut input = b"\n\r\n".to_vec();
        input.extend_from_slice(&[0xff, 0xfe, b'\n']);
        input.extend_from_slice(
            b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{}}\r\n",
        );
        let answers = run(&input);
        assert_eq!(answers.len(), 2, "{answers:?}");
        assert_eq!(answers[0]["error"]["code"], protocol::PARSE_ERROR);
        assert_eq!(answers[1]["id"], 2);
    }
}
