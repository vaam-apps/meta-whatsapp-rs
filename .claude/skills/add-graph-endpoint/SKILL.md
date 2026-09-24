---
name: add-graph-endpoint
description: "Recipe for adding or changing a Graph API endpoint wrapper in wa-client — typed request/response, local validation, secrets, pagination with cursors, ScriptedTransport tests. Use when wrapping a new WhatsApp Cloud API or Business Management API endpoint or fixing an existing one."
metadata:
  internal: true
---

# Adding a Graph endpoint

1. **Read the page** (`meta-docs` skill). Note method, path, required
   permission, every request field with its limits, the response example,
   and endpoint-specific error codes.
2. **Pick the module** by the object the path hangs off: `/{PHONE_NUMBER_ID}/…`
   → a phone-number-scoped module (`messages`, `media`, `phone_numbers`, …),
   `/{WABA_ID}/…` → `waba`, `templates`, `flows`, `analytics`, `signups`.
3. **Types**: request struct `#[derive(Serialize)]` with
   `#[serde(skip_serializing_if = "Option::is_none")]` on optionals;
   response `#[derive(Deserialize)]`, no `deny_unknown_fields`. Ids use
   `wa_core::ids` newtypes. Secrets the caller supplies (PINs, codes) are
   newtypes that validate on construction and redact `Debug`
   (`phone_numbers::TwoStepPin`, `embedded_signup::SignupCode`); take them
   by reference and call `expose_secret()` only to fill the request body.
   - **Enums Meta may extend** need a catch-all variant so a new value
     never fails parsing. Which name and shape every enum should share
     (today there are five, see `OPEN_QUESTIONS.md` #27) and the
     `#[non_exhaustive]` policy (#28) are undecided: follow the module you
     are in, and **do not add a unit `#[serde(other)] Unknown`**. It drops
     Meta's value, and if the enum also derives `Serialize` it writes its
     own name back instead. The value-keeping macros are `open_enum!`
     (wa-webhooks), `string_enum!` (templates) and `wire_enum!` (flows).
4. **Method** on the module's API struct. Paths containing an id are built
   **only** with the segment API `self.client.get_at/post_at/delete_at(&[..])`
   — never `client.post(&format!("{}/…", id))`: `GraphEndpoint::url` splits
   on `/`, so an id like `123/subscribed_apps` would address another object
   with the tenant's token. `client.get/post/delete("literal/path")` is for
   literal paths only (e.g. `"oauth/access_token"`).

   The real `phone_numbers::PhoneNumber::register`:

   ```rust
   pub async fn register(
       &self,
       pin: &TwoStepPin, // validated when built: exactly 6 digits
       data_localization_region: Option<&DataLocalizationRegion>,
   ) -> Result<()> {
       if let Some(region) = data_localization_region {
           region.validate()?; // ValidationError naming the field
       }
       self.client
           .post_at(&[self.phone_number_id.as_str(), "register"])
           .json(&RegisterBody {
               messaging_product: "whatsapp",
               pin: pin.expose_secret(),
               data_localization_region,
           })
           .context("register response")
           .send_success()
           .await
   }
   ```

   - `{"success": true}` responses → `send_success()`.
   - Mark a POST `.idempotent(true)` only if replaying cannot duplicate an
     effect (`register` is not: Meta counts 10 registrations per 72 h).
5. **Lists** return one page, `Page<T>`, and take the cursor in a query
   struct, next to the endpoint's filters and `limit`:

   ```rust
   #[derive(Debug, Clone, Default, PartialEq, Eq)]
   pub struct ListQrCodes {
       pub limit: Option<u32>,
       /// Cursor from a previous page's `paging.cursors.after`.
       pub after: Option<String>,
       /// Cursor from a previous page's `paging.cursors.before`.
       pub before: Option<String>,
       // … the endpoint's own filters
   }

   pub async fn list(&self, query: &ListQrCodes) -> Result<Page<QrCode>> {
       self.list_request(query)?
           .query_opt("after", query.after.as_deref())
           .query_opt("before", query.before.as_deref())
           .send()
           .await
   }
   ```

   The caller pages on with `page.next_cursor()` (`Some` only when Meta
   sent a `next` link) as the next query's `after`. Add `…_stream(&query)`
   returning `request.paginate::<T>()`: it manages the cursors itself
   (re-issuing the request with `after=`, never following `paging.next`),
   so it refuses a query that already has `after`/`before` with a
   `ValidationError`, yielded as the stream's single item. Reference:
   `qr_codes` (`ListQrCodes`, `list`, `list_stream`) and `block_users`.
   Some older lists do not follow this yet (at 8ee6fab: `signups.list` and
   `waba.phone_numbers` take no cursor, `flows.list` a bare
   `after: Option<&str>`); do not copy them.
6. **Tests** (in the module, `#[cfg(test)]`), with the docs' examples:

   ```rust
   let t = ScriptedTransport::new();
   t.push_json(200, json!({"success": true}));
   let client = Client::builder().transport(t.clone()).access_token("T").build().unwrap();
   let pin = TwoStepPin::new("212834").unwrap();
   client.phone_number("106540352242922").register(&pin, None).await.unwrap();
   let req = t.last_request().unwrap();
   assert_eq!(req.method, Method::POST);
   assert_eq!(req.path(), "/v25.0/106540352242922/register");
   assert_eq!(req.json(), Some(json!({"messaging_product": "whatsapp", "pin": "212834"})));
   assert_eq!(t.remaining(), 0);
   ```

   Cover: the happy path with the docs' example response, one Graph error
   mapped to the right `ErrorKind`, every local validation, that an id
   containing `/` stays one segment (`/v25.0/123%2Fx/register`), and for a
   list: the cursor reaches the query (`req.query("after")`), and the
   stream follows two pages and refuses a caller's cursor.
7. **Docs**: rustdoc on every public item; module doc lists the Meta
   paths. Update `docs/coverage.md`.
8. `just ci`.
