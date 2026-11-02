//! MCP over JSON-RPC 2.0, one message per line, by hand.
//!
//! The server is **dual-era**, as the 2026-07-28 revision's versioning
//! page defines it:
//!
//! - **Modern, revision 2026-07-28** (the current one): no handshake.
//!   Every request carries `_meta` with
//!   `io.modelcontextprotocol/protocolVersion` and
//!   `io.modelcontextprotocol/clientCapabilities`, and is served on its
//!   own; results carry `resultType: "complete"` and the server's
//!   identity in `_meta` (`io.modelcontextprotocol/serverInfo`);
//!   `server/discover`, `tools/list` and `tools/call` are served, and the
//!   first two carry the caching hints (`ttlMs`, `cacheScope`). A version
//!   it does not speak is `UnsupportedProtocolVersionError` (`-32022`)
//!   listing the ones it does.
//! - **Legacy, revisions 2025-11-25 and 2025-06-18**: an `initialize`
//!   request selects them for the rest of the process (stdio), after
//!   which requests without `_meta` are served under the negotiated
//!   revision (`ping` too).
//!
//! A request with neither (no `_meta` version, no `initialize` before
//! it) is malformed: `-32602`, as the modern revision says.
//!
//! Errors, as the tools page of both revisions sorts them: an unknown
//! tool or malformed `tools/call` params are JSON-RPC errors (`-32602`);
//! a missing page, a path outside the corpus, a missing section or bad
//! arguments are a result with `isError: true` (a tool execution error,
//! which the model can correct).

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::tools::{self, CallError};

/// The revision the server implements in full.
pub const MODERN: &str = "2026-07-28";

/// The handshake revisions it answers `initialize` with, newest first.
pub const LEGACY: &[&str] = &["2025-11-25", "2025-06-18"];

/// `_meta` keys of the modern revision.
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// JSON-RPC and MCP error codes.
pub const PARSE_ERROR: i64 = -32700;
/// The message is not a valid request.
pub const INVALID_REQUEST: i64 = -32600;
/// No such method.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Malformed params, a missing `_meta` field, an unknown tool.
pub const INVALID_PARAMS: i64 = -32602;
/// The modern revision's `UnsupportedProtocolVersionError`.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// How long the tool list and the discovery answer stay fresh: they change
/// only with the binary.
const TTL_MS: u64 = 60 * 60 * 1000;

/// What the server tells a model about itself.
const INSTRUCTIONS: &str = "The docs and consumer skills of meta-whatsapp-rs, a Rust toolkit for \
    Meta's WhatsApp Business Platform, and of its HTTP service, meta-whatsapp-server. `search` \
    finds the sections that mention something (an API name, a header, an error code); `read` \
    returns a page or one section by its heading; `list` gives every page, a skill with its \
    description. Prefer reading one section over a whole page.";

/// The server: the corpus root, and the legacy revision an `initialize`
/// chose, if one did.
#[derive(Debug)]
pub struct Server {
    root: std::path::PathBuf,
    legacy: Option<&'static str>,
}

/// The era a request is served in.
#[derive(Clone, Copy)]
enum Era {
    Modern,
    Legacy,
}

/// A JSON-RPC error.
struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

impl Server {
    /// A server for the corpus under `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            legacy: None,
        }
    }

    /// Handle one line from the client; the line to answer with, if any
    /// (a notification and a response get none).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                return Some(error_response(
                    &Value::Null,
                    &RpcError::new(PARSE_ERROR, "Parse error: not one JSON value"),
                ));
            }
        };
        let Some(object) = message.as_object() else {
            let why = if message.is_array() {
                "Invalid request: batches are not part of MCP"
            } else {
                "Invalid request: not a JSON-RPC object"
            };
            return Some(error_response(
                &Value::Null,
                &RpcError::new(INVALID_REQUEST, why),
            ));
        };
        let Some(method) = object.get("method") else {
            // A response (the server sends no requests): nothing to answer.
            return None;
        };
        let Some(id) = object.get("id") else {
            // A notification (`notifications/initialized`,
            // `notifications/cancelled`): no answer, ever.
            return None;
        };
        if !(id.is_string() || id.is_i64() || id.is_u64()) {
            return Some(error_response(
                &Value::Null,
                &RpcError::new(
                    INVALID_REQUEST,
                    "Invalid request: the id must be a string or an integer",
                ),
            ));
        }
        let outcome = match (object.get("jsonrpc"), method.as_str()) {
            (Some(Value::String(v)), Some(method)) if v == "2.0" => {
                self.request(method, object.get("params"))
            }
            _ => Err(RpcError::new(
                INVALID_REQUEST,
                "Invalid request: `jsonrpc` must be \"2.0\" and `method` a string",
            )),
        };
        Some(match outcome {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            Err(e) => error_response(id, &e),
        })
    }

    fn request(&mut self, method: &str, params: Option<&Value>) -> Result<Value, RpcError> {
        let empty = Map::new();
        let params = match params {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(p)) => p,
            Some(_) => return Err(RpcError::new(INVALID_PARAMS, "`params` must be an object")),
        };
        if method == "initialize" && !has_modern_version(params) {
            return Ok(self.initialize(params));
        }
        let era = self.era(params)?;
        let mut result = match (era, method) {
            (Era::Modern, "server/discover") => json!({
                "supportedVersions": supported(),
                "capabilities": capabilities(),
                "instructions": INSTRUCTIONS,
                "ttlMs": TTL_MS,
                "cacheScope": "public",
            }),
            (Era::Legacy, "ping") => json!({}),
            (_, "tools/list") => {
                if params.get("cursor").is_some_and(|c| !c.is_null()) {
                    return Err(RpcError::new(
                        INVALID_PARAMS,
                        "invalid cursor: the tool list has one page",
                    ));
                }
                let mut list = json!({ "tools": tools::definitions() });
                if let Era::Modern = era {
                    list["ttlMs"] = json!(TTL_MS);
                    list["cacheScope"] = json!("public");
                }
                list
            }
            (_, "tools/call") => self.call(params)?,
            (_, other) => {
                return Err(RpcError::new(
                    METHOD_NOT_FOUND,
                    format!("Method not found: {other}"),
                ));
            }
        };
        if let Era::Modern = era {
            result["resultType"] = json!("complete");
            result["_meta"] = json!({ META_SERVER_INFO: server_info() });
        }
        Ok(result)
    }

    /// Legacy: answer the handshake with the client's revision when the
    /// server speaks it, else its newest legacy one.
    fn initialize(&mut self, params: &Map<String, Value>) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = LEGACY
            .iter()
            .copied()
            .find(|v| Some(*v) == asked)
            .unwrap_or(LEGACY[0]);
        self.legacy = Some(version);
        json!({
            "protocolVersion": version,
            "capabilities": capabilities(),
            "serverInfo": server_info(),
            "instructions": INSTRUCTIONS,
        })
    }

    fn era(&self, params: &Map<String, Value>) -> Result<Era, RpcError> {
        let meta = params.get("_meta").and_then(Value::as_object);
        match meta.and_then(|m| m.get(META_VERSION)) {
            Some(Value::String(version)) if version == MODERN => {
                if meta
                    .and_then(|m| m.get(META_CAPABILITIES))
                    .is_some_and(Value::is_object)
                {
                    Ok(Era::Modern)
                } else {
                    Err(RpcError::new(
                        INVALID_PARAMS,
                        format!(
                            "`_meta` lacks `{META_CAPABILITIES}` (an object), required on every request"
                        ),
                    ))
                }
            }
            Some(requested) => Err(RpcError {
                code: UNSUPPORTED_PROTOCOL_VERSION,
                message: "Unsupported protocol version".to_owned(),
                data: Some(json!({ "supported": supported(), "requested": requested })),
            }),
            None if self.legacy.is_some() => Ok(Era::Legacy),
            None => Err(RpcError::new(
                INVALID_PARAMS,
                format!(
                    "`_meta` lacks `{META_VERSION}`: this server speaks {MODERN} (per-request \
                     `_meta`) and, after `initialize`, {}",
                    LEGACY.join(" and ")
                ),
            )),
        }
    }

    fn call(&self, params: &Map<String, Value>) -> Result<Value, RpcError> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "`tools/call` needs `name`, a string",
            ));
        };
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(a)) => a,
            Some(_) => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "`arguments` must be an object",
                ));
            }
        };
        match tools::call(&self.root, name, arguments) {
            Ok(output) => {
                let mut result = json!({
                    "content": [{"type": "text", "text": output.text}],
                    "isError": false,
                });
                if let Some(structured) = output.structured {
                    result["structuredContent"] = structured;
                }
                Ok(result)
            }
            Err(CallError::Tool(why)) => Ok(json!({
                "content": [{"type": "text", "text": why}],
                "isError": true,
            })),
            Err(CallError::UnknownTool(name)) => Err(RpcError::new(
                INVALID_PARAMS,
                format!("Unknown tool: {name}"),
            )),
        }
    }
}

fn has_modern_version(params: &Map<String, Value>) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .is_some_and(|m| m.contains_key(META_VERSION))
}

fn supported() -> Vec<&'static str> {
    std::iter::once(MODERN)
        .chain(LEGACY.iter().copied())
        .collect()
}

fn capabilities() -> Value {
    json!({ "tools": {} })
}

fn server_info() -> Value {
    json!({
        "name": env!("CARGO_PKG_NAME"),
        "title": "meta-whatsapp-rs docs and skills",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn error_response(id: &Value, e: &RpcError) -> String {
    let mut error = json!({"code": e.code, "message": e.message});
    if let Some(data) = &e.data {
        error["data"] = data.clone();
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::tests::Scratch;

    fn modern(method: &str, id: u64, mut params: Value) -> String {
        params["_meta"] = json!({
            META_VERSION: MODERN,
            META_CAPABILITIES: {},
            "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"}
        });
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
    }

    fn answer(server: &mut Server, line: &str) -> Value {
        serde_json::from_str(&server.handle_line(line).unwrap()).unwrap()
    }

    #[test]
    fn a_request_without_an_era_is_malformed_until_initialize() {
        let s = Scratch::new("era");
        let mut server = Server::new(&s.0);
        let bare = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        assert_eq!(answer(&mut server, bare)["error"]["code"], INVALID_PARAMS);
        let init = answer(
            &mut server,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(
            server.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        let listed = answer(&mut server, bare);
        assert!(listed["result"]["tools"].is_array(), "{listed}");
        assert!(
            listed["result"].get("resultType").is_none(),
            "legacy results have none"
        );
        assert_eq!(
            answer(&mut server, r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#)["result"],
            json!({})
        );
    }

    #[test]
    fn initialize_answers_the_newest_legacy_revision_for_one_it_does_not_speak() {
        let s = Scratch::new("init");
        let mut server = Server::new(&s.0);
        let init = answer(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
        );
        assert_eq!(init["result"]["protocolVersion"], LEGACY[0]);
        assert_eq!(init["result"]["capabilities"], json!({"tools": {}}));
        assert_eq!(
            init["result"]["serverInfo"]["name"],
            "meta-whatsapp-docs-mcp"
        );
    }

    #[test]
    fn modern_requests_carry_their_version_and_capabilities() {
        let s = Scratch::new("modern");
        let mut server = Server::new(&s.0);
        let discover = answer(&mut server, &modern("server/discover", 1, json!({})));
        assert_eq!(
            discover["result"],
            json!({
                "resultType": "complete",
                "supportedVersions": ["2026-07-28", "2025-11-25", "2025-06-18"],
                "capabilities": {"tools": {}},
                "instructions": INSTRUCTIONS,
                "ttlMs": TTL_MS,
                "cacheScope": "public",
                "_meta": {META_SERVER_INFO: server_info()},
            })
        );
        let mut old = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {META_VERSION: "1900-01-01", META_CAPABILITIES: {}}}});
        let refused = answer(&mut server, &old.to_string());
        assert_eq!(
            refused["error"],
            json!({
                "code": -32022,
                "message": "Unsupported protocol version",
                "data": {"supported": ["2026-07-28", "2025-11-25", "2025-06-18"], "requested": "1900-01-01"},
            })
        );
        old["params"]["_meta"] = json!({META_VERSION: MODERN});
        assert_eq!(
            answer(&mut server, &old.to_string())["error"]["code"],
            INVALID_PARAMS
        );
        // `ping` is gone from the modern revision.
        assert_eq!(
            answer(&mut server, &modern("ping", 3, json!({})))["error"]["code"],
            METHOD_NOT_FOUND
        );
        // Modern stays stateless: no `initialize` needed, none remembered.
        assert!(server.legacy.is_none());
    }

    #[test]
    fn malformed_messages_get_json_rpc_errors() {
        let s = Scratch::new("malformed");
        let mut server = Server::new(&s.0);
        for (line, code, id) in [
            ("{not json", PARSE_ERROR, Value::Null),
            ("[1, 2]", INVALID_REQUEST, Value::Null),
            ("3", INVALID_REQUEST, Value::Null),
            (
                r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#,
                INVALID_REQUEST,
                Value::Null,
            ),
            (
                r#"{"jsonrpc":"1.0","id":7,"method":"tools/list"}"#,
                INVALID_REQUEST,
                json!(7),
            ),
            (
                r#"{"jsonrpc":"2.0","id":"x","method":"tools/list","params":[]}"#,
                INVALID_PARAMS,
                json!("x"),
            ),
        ] {
            let v = answer(&mut server, line);
            assert_eq!(v["error"]["code"], code, "{line}: {v}");
            assert_eq!(v["id"], id, "{line}");
        }
        // A response from the client gets no answer.
        assert_eq!(
            server.handle_line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            None
        );
        let unknown = answer(
            &mut server,
            &modern("tools/call", 9, json!({"name": "write"})),
        );
        assert_eq!(unknown["error"]["code"], INVALID_PARAMS);
        assert_eq!(unknown["error"]["message"], "Unknown tool: write");
        let no_name = answer(&mut server, &modern("tools/call", 10, json!({})));
        assert_eq!(no_name["error"]["code"], INVALID_PARAMS);
        let bad_args = answer(
            &mut server,
            &modern("tools/call", 11, json!({"name": "list", "arguments": "x"})),
        );
        assert_eq!(bad_args["error"]["code"], INVALID_PARAMS);
        let cursor = answer(
            &mut server,
            &modern("tools/list", 12, json!({"cursor": "next"})),
        );
        assert_eq!(cursor["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn a_missing_page_is_a_tool_error_not_a_protocol_error() {
        let s = Scratch::new("missing");
        s.write("docs/a.md", "# a\n");
        let mut server = Server::new(&s.0);
        let v = answer(
            &mut server,
            &modern(
                "tools/call",
                1,
                json!({"name": "read", "arguments": {"path": "docs/nope.md"}}),
            ),
        );
        assert_eq!(v["result"]["isError"], true, "{v}");
        assert_eq!(v["result"]["resultType"], "complete");
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("no such page: `docs/nope.md`"), "{text}");
        assert!(v.get("error").is_none());
    }
}
