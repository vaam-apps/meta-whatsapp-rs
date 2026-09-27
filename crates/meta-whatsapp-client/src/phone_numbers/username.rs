//! Business usernames and the contact book
//! (`business-scoped-user-ids`, § Business usernames and § Contact book).
//!
//! A business username is plain, public data (it is shown on the business
//! profile and searched by users), not a secret. One number has at most one
//! username, and no two numbers on WhatsApp share one.
//!
//! # Where the docs stop
//!
//! - Meta's error codes for usernames (`147001` to `147005`) are not
//!   classified in [`meta_whatsapp_core::ErrorKind`] yet: they come back as
//!   `ErrorKind::Unknown` with the code in [`meta_whatsapp_core::Error::graph`].
//!   `100` (bad format) is `InvalidParameter`, `133010` (number not
//!   registered) is `Registration`.
//! - The number's own `POST /{Phone-Number-ID}` also lists a `username`
//!   field (`whatsapp-business-account-phone-number-api`), without an
//!   example or a response; this client uses the documented
//!   `/{Phone-Number-ID}/username` edge instead.

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::UserId;
use serde::{Deserialize, Serialize};

use super::PhoneNumber;
use crate::request::decode_json;
use crate::templates::macros::string_enum;

/// Shortest business username, in characters.
pub const MIN_USERNAME_CHARS: usize = 3;
/// Longest business username, in characters.
pub const MAX_USERNAME_CHARS: usize = 35;
/// Longest BSUID suffix (after the country code and the period).
const MAX_BSUID_SUFFIX_CHARS: usize = 128;

string_enum! {
    /// Status of a business username (`status` of the username calls). The
    /// `business_username_updates` webhook also reports `deleted`, typed by
    /// `meta-whatsapp-webhooks`; here it would be `Other("deleted")`.
    pub enum UsernameStatus {
        /// Approved: visible to WhatsApp users once the usernames feature
        /// is available to them.
        Approved => "approved",
        /// Reserved and approved for this number, not visible to users yet;
        /// it becomes visible once the feature is available to everyone.
        Reserved => "reserved",
    }
}

string_enum! {
    /// What happens when the requested username is in use on another
    /// number of the same business portfolio (`transfer_action`).
    pub enum TransferAction {
        /// Do not move it: the request fails with `147005`. Meta's default.
        None => "none",
        /// Move it: the username leaves the other number and is assigned
        /// to this one.
        ForceTransfer => "force_transfer",
    }
}

/// `GET /{Phone-Number-ID}/username`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct BusinessUsername {
    /// The current username; absent when the number has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Its status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<UsernameStatus>,
}

#[derive(Serialize)]
struct UsernameBody<'a> {
    username: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer_action: Option<&'a TransferAction>,
}

#[derive(Deserialize)]
struct UsernameStatusResponse {
    status: UsernameStatus,
}

#[derive(Deserialize)]
struct SuggestionsResponse {
    #[serde(default)]
    data: Vec<SuggestionsEntry>,
}

#[derive(Deserialize)]
struct SuggestionsEntry {
    #[serde(default)]
    username_suggestions: Vec<String>,
}

#[derive(Deserialize)]
struct ContactBookResponse {
    success: bool,
    deleted: bool,
}

/// Check a business username against the format Meta documents: 3 to 35
/// characters; only English letters, digits, `.` and `_`; at least one
/// letter; no `.` first, last or twice in a row; not starting with `www`.
///
/// The rule that it must not end with a domain (`.com`, `.org`, … "and so
/// on") has no closed list, so it is left to Meta (error `100`).
pub fn validate_username(username: &str) -> std::result::Result<(), ValidationError> {
    let invalid = |reason: &str| Err(ValidationError::new("username", reason));
    let len = username.chars().count();
    if !(MIN_USERNAME_CHARS..=MAX_USERNAME_CHARS).contains(&len) {
        return invalid("must be 3-35 characters");
    }
    if !username
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
    {
        return invalid("may only contain English letters, digits, `.` and `_`");
    }
    if !username.bytes().any(|b| b.is_ascii_alphabetic()) {
        return invalid("must contain at least one English letter");
    }
    if username.starts_with('.') || username.ends_with('.') || username.contains("..") {
        return invalid("must not start or end with `.` or contain `..`");
    }
    if username
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("www"))
    {
        return invalid("must not start with `www`");
    }
    Ok(())
}

/// A BSUID as the contact book takes it: a two-letter uppercase country
/// code, a period, then up to 128 letters or digits (`US.13491208655302741918`).
/// A parent BSUID (`US.ENT.…`) has a second period and is refused: the
/// contact book does not support them.
fn validate_bsuid(bsuid: &UserId) -> std::result::Result<(), ValidationError> {
    let ok = bsuid.as_str().split_once('.').is_some_and(|(cc, rest)| {
        cc.len() == 2
            && cc.bytes().all(|b| b.is_ascii_uppercase())
            && !rest.is_empty()
            && rest.len() <= MAX_BSUID_SUFFIX_CHARS
            && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    });
    if ok {
        Ok(())
    } else {
        Err(ValidationError::new(
            "bsuid",
            "must be a BSUID such as `US.13491208655302741918` (parent BSUIDs are not supported)",
        ))
    }
}

impl PhoneNumber {
    /// `POST /{Phone-Number-ID}/username`: adopt a business username, or
    /// change the current one. Returns the new username's status.
    ///
    /// `transfer_action` matters only when the username is in use on
    /// another number of the same portfolio: `None` (the field left out)
    /// and [`TransferAction::None`] fail with `147005` then,
    /// [`TransferAction::ForceTransfer`] moves it here.
    ///
    /// The format is checked first ([`validate_username`]). Not replayed
    /// after a timeout (a `POST`); read [`Self::username`] to see whether it
    /// was applied.
    pub async fn set_username(
        &self,
        username: &str,
        transfer_action: Option<&TransferAction>,
    ) -> Result<UsernameStatus> {
        validate_username(username)?;
        let resp: UsernameStatusResponse = self
            .client
            .post_at(&[self.phone_number_id.as_str(), "username"])
            .json(&UsernameBody {
                username,
                transfer_action,
            })
            .context("username response")
            .send()
            .await?;
        Ok(resp.status)
    }

    /// `GET /{Phone-Number-ID}/username`: the current username and its
    /// status.
    pub async fn username(&self) -> Result<BusinessUsername> {
        self.client
            .get_at(&[self.phone_number_id.as_str(), "username"])
            .context("username")
            .send()
            .await
    }

    /// `GET /{Phone-Number-ID}/username_suggestions`: the usernames
    /// WhatsApp reserved for this business portfolio, which have a higher
    /// chance of approval. Claim one with [`Self::set_username`].
    ///
    /// Meta nests them (`data[].username_suggestions[]`); they come back
    /// flattened, in order. The page documents no pagination.
    pub async fn username_suggestions(&self) -> Result<Vec<String>> {
        let resp: SuggestionsResponse = self
            .client
            .get_at(&[self.phone_number_id.as_str(), "username_suggestions"])
            .context("username suggestions")
            .send()
            .await?;
        Ok(resp
            .data
            .into_iter()
            .flat_map(|entry| entry.username_suggestions)
            .collect())
    }

    /// `DELETE /{Phone-Number-ID}/username`: remove this number's business
    /// username. Meta answers `{"success": false}` when it did not delete
    /// one; that is an error here, like every `success` response.
    ///
    /// A `DELETE` is replayed on transient errors (the retry policy); a
    /// replay after a lost answer may find nothing left to delete and fail
    /// that way. Read [`Self::username`] to see where things stand.
    pub async fn delete_username(&self) -> Result<()> {
        self.client
            .delete_at(&[self.phone_number_id.as_str(), "username"])
            .context("delete username response")
            .send_success()
            .await
    }

    /// `DELETE /{Phone-Number-ID}/contact_book?messaging_product=whatsapp&bsuid=…`:
    /// **erase** one user's entry from the business portfolio's contact
    /// book, at Meta.
    ///
    /// What it erases: the phone number Meta keeps with that user's BSUID
    /// for the whole portfolio, which is what puts the phone number in
    /// webhooks of users who hide it behind a username. After the deletion,
    /// webhooks of every number of the portfolio carry the BSUID without
    /// the phone number (unless the number was messaged or called in the
    /// last 30 days, or a new interaction records the entry again).
    ///
    /// **It cannot be undone**: there is no call to restore an entry. This
    /// crate never calls it for you; nothing else in it deletes a contact
    /// book entry.
    ///
    /// Returns `true` if an entry existed and was deleted, `false` if there
    /// was none for `bsuid`. `bsuid` must be a BSUID of the same portfolio
    /// as this number, in the standard form (`US.13491208655302741918`);
    /// parent BSUIDs are refused locally, before any request. A
    /// `{"success": false}` answer is an error.
    ///
    /// A `DELETE` is replayed on transient errors (the retry policy): the
    /// entry is gone either way, but after a replay `false` may mean the
    /// lost first attempt deleted it.
    pub async fn delete_contact_book_entry(&self, bsuid: &UserId) -> Result<bool> {
        validate_bsuid(bsuid)?;
        let context = "contact book deletion response";
        let resp = self
            .client
            .delete_at(&[self.phone_number_id.as_str(), "contact_book"])
            .query("messaging_product", "whatsapp")
            .query("bsuid", bsuid)
            .context(context)
            .send_raw()
            .await?;
        let body: ContactBookResponse = decode_json(context, &resp.body)?;
        if body.success {
            Ok(body.deleted)
        } else {
            Err(meta_whatsapp_core::Error::Http {
                status: resp.status.as_u16(),
                body_snippet: meta_whatsapp_core::error::snippet(&resp.body),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use meta_whatsapp_core::ErrorKind;
    use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{Client, RetryPolicy};

    const ID: &str = "106540352242922";
    const BSUID: &str = "US.13491208655302741918";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn graph_error(code: i64) -> serde_json::Value {
        json!({"error": {"message": format!("(#{code}) x"), "type": "OAuthException", "code": code, "fbtrace_id": "A"}})
    }

    #[tokio::test]
    async fn set_username_sends_username_and_transfer_action() {
        // business-scoped-user-ids, "Adopt or change a business username".
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"status": "approved"}));
        t.push_json(200, json!({"status": "reserved"}));
        let pn = client(&t).phone_number(ID);
        let first = pn
            .set_username("lucky_shrub", Some(&TransferAction::ForceTransfer))
            .await
            .unwrap();
        let second = pn.set_username("lucky.shrub", None).await.unwrap();
        assert_eq!(first, UsernameStatus::Approved);
        assert_eq!(second, UsernameStatus::Reserved);
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(reqs[0].path(), "/v25.0/106540352242922/username");
        assert_eq!(reqs[0].url.query(), None);
        assert_eq!(reqs[0].bearer(), Some("TOKEN"));
        assert_eq!(
            reqs[0].json(),
            Some(json!({"username": "lucky_shrub", "transfer_action": "force_transfer"}))
        );
        assert_eq!(reqs[1].json(), Some(json!({"username": "lucky.shrub"})));
        assert_eq!(t.remaining(), 0);

        // The explicit default is sent as written.
        t.push_json(200, json!({"status": "approved"}));
        pn.set_username("lucky_shrub", Some(&TransferAction::None))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({"username": "lucky_shrub", "transfer_action": "none"}))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn set_username_maps_errors_and_is_not_replayed() {
        let t = ScriptedTransport::new();
        let pn = client(&t).phone_number(ID);
        t.push_json(400, graph_error(100));
        let err = pn.set_username("lucky_shrub", None).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
        t.push_json(400, graph_error(133010));
        let err = pn.set_username("lucky_shrub", None).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Registration);
        // 147005 (transfer required) is not classified yet: the code is kept.
        t.push_json(400, graph_error(147005));
        let err = pn.set_username("lucky_shrub", None).await.unwrap_err();
        assert_eq!(err.graph().map(|g| g.code), Some(147005));
        assert_eq!(t.remaining(), 0);

        let t = ScriptedTransport::new();
        t.push_error(|| meta_whatsapp_core::error::TransportError::Timeout);
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 3,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        assert!(
            c.phone_number(ID)
                .set_username("lucky_shrub", None)
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 1, "a timed-out POST is not replayed");
    }

    #[tokio::test]
    async fn set_username_checks_the_format_first() {
        let t = ScriptedTransport::new();
        let pn = client(&t).phone_number(ID);
        for bad in [
            "ab",
            &"a".repeat(36),
            "lucky-shrub",
            "lücky",
            "12345",
            "._",
            ".lucky",
            "lucky.",
            "lucky..shrub",
            "www_lucky",
            "WWWlucky",
        ] {
            let err = pn.set_username(bad, None).await.unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == "username"),
                "{bad:?}: {err:?}"
            );
        }
        assert!(
            t.requests().is_empty(),
            "invalid usernames never reach the wire"
        );
        for good in ["abc", "my_id", "My.Id", "a1_", &"a".repeat(35), "shop.www"] {
            assert!(validate_username(good).is_ok(), "{good:?}");
        }
    }

    #[tokio::test]
    async fn username_reads_the_current_one() {
        // business-scoped-user-ids, "Get current username".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"username": "lucky_shrub", "status": "approved"}),
        );
        t.push_json(200, json!({"status": "reserved"}));
        t.push_json(200, json!({"username": "x_y", "status": "active"}));
        let pn = client(&t).phone_number(ID);
        let current = pn.username().await.unwrap();
        assert_eq!(current.username.as_deref(), Some("lucky_shrub"));
        assert_eq!(current.status, Some(UsernameStatus::Approved));
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922/username");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        // No username: the field is omitted.
        let none = pn.username().await.unwrap();
        assert_eq!(none.username, None);
        // A status Meta adds later is kept.
        let later = pn.username().await.unwrap();
        assert_eq!(later.status, Some(UsernameStatus::Other("active".into())));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn username_suggestions_are_flattened() {
        // business-scoped-user-ids, "Get reserved usernames".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"username_suggestions": ["lucky_shrub", "luckyshrub_store"]}]}),
        );
        let got = client(&t)
            .phone_number(ID)
            .username_suggestions()
            .await
            .unwrap();
        assert_eq!(got, vec!["lucky_shrub", "luckyshrub_store"]);
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922/username_suggestions");
        assert_eq!(req.url.query(), None);
        assert_eq!(t.remaining(), 0);

        // Every entry of `data` counts, in order (an empty one adds nothing).
        t.push_json(
            200,
            json!({"data": [
                {"username_suggestions": ["lucky_shrub"]},
                {"username_suggestions": []},
                {},
                {"username_suggestions": ["luckyshrub_store", "lucky.shrub"]}
            ]}),
        );
        assert_eq!(
            client(&t)
                .phone_number(ID)
                .username_suggestions()
                .await
                .unwrap(),
            vec!["lucky_shrub", "luckyshrub_store", "lucky.shrub"]
        );
        assert_eq!(t.remaining(), 0);

        t.push_json(200, json!({"data": []}));
        assert!(
            client(&t)
                .phone_number(ID)
                .username_suggestions()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn delete_username_is_a_delete_on_the_edge() {
        // business-scoped-user-ids, "Delete a username".
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t).phone_number(ID).delete_username().await.unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::DELETE);
        assert_eq!(req.path(), "/v25.0/106540352242922/username");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.body, RecordedBody::Empty);
        assert_eq!(t.remaining(), 0);

        t.push_json(200, json!({"success": false}));
        assert!(client(&t).phone_number(ID).delete_username().await.is_err());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn contact_book_deletion_is_a_delete_with_the_bsuid() {
        // business-scoped-user-ids, "Delete a contact book entry".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "success": true, "deleted": true}),
        );
        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "success": true, "deleted": false}),
        );
        let pn = client(&t).phone_number(ID);
        assert!(
            pn.delete_contact_book_entry(&UserId::new(BSUID))
                .await
                .unwrap()
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::DELETE);
        assert_eq!(req.path(), "/v25.0/106540352242922/contact_book");
        assert_eq!(req.query("messaging_product").as_deref(), Some("whatsapp"));
        assert_eq!(req.query("bsuid").as_deref(), Some(BSUID));
        assert_eq!(req.url.query_pairs().count(), 2);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(req.body, RecordedBody::Empty);
        // No entry for that BSUID.
        assert!(
            !pn.delete_contact_book_entry(&UserId::new(BSUID))
                .await
                .unwrap()
        );
        assert_eq!(t.remaining(), 0);

        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "success": false, "deleted": false}),
        );
        assert!(
            pn.delete_contact_book_entry(&UserId::new(BSUID))
                .await
                .is_err()
        );
        assert_eq!(t.remaining(), 0);
    }

    /// What the rustdoc says: a `DELETE`, so a timed-out deletion is sent
    /// again, the same request, and the replay's answer is returned.
    #[tokio::test]
    async fn contact_book_deletion_is_replayed_after_a_timeout() {
        let t = ScriptedTransport::new();
        t.push_error(|| meta_whatsapp_core::error::TransportError::Timeout);
        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "success": true, "deleted": false}),
        );
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 1,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        let deleted = c
            .phone_number(ID)
            .delete_contact_book_entry(&UserId::new(BSUID))
            .await
            .unwrap();
        assert!(!deleted, "the replay's answer");
        let reqs = t.requests();
        assert_eq!(reqs.len(), 2);
        for r in &reqs {
            assert_eq!(r.method, Method::DELETE);
            assert_eq!(r.path(), "/v25.0/106540352242922/contact_book");
            assert_eq!(r.query("bsuid").as_deref(), Some(BSUID));
        }
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn contact_book_deletion_refuses_other_ids_before_sending() {
        let t = ScriptedTransport::new();
        let pn = client(&t).phone_number(ID);
        let long = format!("US.{}", "1".repeat(129));
        for bad in [
            "US.ENT.11815799212886844830",
            "13491208655302741918",
            "us.13491208655302741918",
            "USA.1",
            "US.",
            "US.12&bsuid=x",
            "",
            long.as_str(),
        ] {
            let err = pn
                .delete_contact_book_entry(&UserId::new(bad))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == "bsuid"),
                "{bad:?}: {err:?}"
            );
        }
        assert!(t.requests().is_empty(), "nothing is deleted on a bad id");
        assert!(validate_bsuid(&UserId::new(format!("BR.{}", "a".repeat(128)))).is_ok());
    }

    /// Every `.rs` file under `dir`.
    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// The library lines of `file` (number, text), and the paths of the
    /// out-of-line test modules it declares. Left out: comment lines, and
    /// everything from an inline `#[cfg(test)] mod … {` on (where this
    /// workspace keeps its test modules: at the end). Everything else
    /// after a `#[cfg(test)]` is kept.
    fn library_lines(
        file: &std::path::Path,
        text: &str,
    ) -> (Vec<(usize, String)>, Vec<std::path::PathBuf>) {
        let lines: Vec<&str> = text.lines().collect();
        // Where this file's `mod x;` children live.
        let children = match file.file_name().and_then(|n| n.to_str()) {
            Some("mod.rs" | "lib.rs" | "main.rs") => file.parent().unwrap().to_path_buf(),
            _ => file.with_extension(""),
        };
        let (mut code, mut test_modules) = (Vec::new(), Vec::new());
        for (i, raw) in lines.iter().enumerate() {
            let line = raw.trim();
            if line == "#[cfg(test)]" {
                // The item it gates, past any further attributes.
                let item = lines[i + 1..]
                    .iter()
                    .map(|l| l.trim())
                    .find(|l| !l.starts_with("#["))
                    .unwrap_or_default();
                let module = item
                    .strip_prefix("pub(crate) mod ")
                    .or_else(|| item.strip_prefix("mod "));
                match module.map(|m| m.strip_suffix(';')) {
                    Some(Some(name)) => {
                        test_modules.push(children.join(format!("{name}.rs")));
                        test_modules.push(children.join(name));
                    }
                    Some(None) => break,
                    None => {}
                }
            }
            if !line.starts_with("//") {
                code.push((i + 1, (*raw).to_owned()));
            }
        }
        (code, test_modules)
    }

    /// What [`library_lines`] keeps, on a sample: code past an out-of-line
    /// `mod tests;` and past a gated `impl`, not comments, and nothing from
    /// an inline test module on; the declared test files, beside a
    /// `mod.rs` and beside any other file.
    #[test]
    fn the_contact_book_scan_keeps_library_lines_only() {
        use std::path::Path;
        let sample = "fn a() {}\n\
            #[cfg(test)]\n\
            mod tests;\n\
            fn b() {}\n\
            // a comment\n\
            #[cfg(test)]\n\
            impl A {}\n\
            fn c() {}\n\
            #[cfg(test)]\n\
            #[allow(unused)]\n\
            mod inline {\n\
            fn t() {}\n\
            }\n";
        let (code, tests) = library_lines(Path::new("src/m/mod.rs"), sample);
        let kept: Vec<usize> = code.iter().map(|(n, _)| *n).collect();
        assert_eq!(kept, [1, 2, 3, 4, 6, 7, 8]);
        assert_eq!(
            tests,
            [
                Path::new("src/m").join("tests.rs"),
                Path::new("src/m").join("tests")
            ]
        );
        let (_, tests) = library_lines(Path::new("src/m/otp.rs"), "#[cfg(test)]\nmod tests;\n");
        assert_eq!(tests[0], Path::new("src/m/otp").join("tests.rs"));
    }

    /// Nothing in the library's own code calls the deletion or builds its
    /// path: outside tests and comments ([`library_lines`]), the word
    /// `contact_book` appears exactly twice in the `src/` of every crate
    /// of the workspace, in this method's signature and in its path
    /// segment. A call from anywhere else (as a method, or as
    /// `PhoneNumber::delete_contact_book_entry`), or a second path to the
    /// edge, fails it.
    #[test]
    fn only_its_own_call_names_the_contact_book() {
        use std::path::Path;
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        for krate in std::fs::read_dir(&crates).unwrap() {
            let src = krate.unwrap().path().join("src");
            if src.is_dir() {
                rust_files(&src, &mut files);
            }
        }
        assert!(files.len() > 50, "walked the workspace's sources");
        let mut scanned = Vec::new();
        let mut test_modules = Vec::new();
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            let (code, tests) = library_lines(file, &text);
            test_modules.extend(tests);
            scanned.push((file.clone(), code));
        }
        scanned.retain(|(file, _)| !test_modules.iter().any(|t| file.starts_with(t)));
        let mut hits = Vec::new();
        for (file, code) in &scanned {
            for (number, line) in code {
                if line.contains("contact_book") {
                    hits.push((file.clone(), *number, line.trim().to_owned()));
                }
            }
        }
        let own = |needle: &str| {
            hits.iter().any(|(file, _, line)| {
                file.ends_with(Path::new("phone_numbers").join("username.rs"))
                    && line.contains(needle)
            })
        };
        assert_eq!(hits.len(), 2, "{hits:#?}");
        assert!(own("pub async fn delete_contact_book_entry("), "{hits:#?}");
        assert!(own("\"contact_book\"])"), "{hits:#?}");
    }

    /// The contact book is only ever touched by its own explicit call:
    /// every other call of this module leaves it alone.
    #[tokio::test]
    async fn no_other_call_reaches_the_contact_book() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"status": "approved"}));
        t.push_json(
            200,
            json!({"username": "lucky_shrub", "status": "approved"}),
        );
        t.push_json(200, json!({"data": []}));
        t.push_json(200, json!({"success": true}));
        let pn = client(&t).phone_number(ID);
        pn.set_username("lucky_shrub", None).await.unwrap();
        pn.username().await.unwrap();
        pn.username_suggestions().await.unwrap();
        pn.delete_username().await.unwrap();
        assert!(
            t.requests()
                .iter()
                .all(|r| !r.path().contains("contact_book")),
            "{:?}",
            t.requests()
                .iter()
                .map(|r| r.path().to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(t.remaining(), 0);
    }
}
