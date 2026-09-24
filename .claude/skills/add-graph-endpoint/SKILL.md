---
name: add-graph-endpoint
description: "Recipe for adding or changing a Graph API endpoint wrapper in wa-client — typed request/response, local validation, pagination, ScriptedTransport tests. Use when wrapping a new WhatsApp Cloud API or Business Management API endpoint or fixing an existing one."
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
   response `#[derive(Deserialize)]`, no `deny_unknown_fields`; extensible
   enums get `#[serde(other)] Unknown`. Ids use `wa_core::ids` newtypes.
4. **Method** on the module's API struct. Paths containing an id are built
   **only** with the segment API `self.client.get_at/post_at/delete_at(&[..])`
   — never `client.post(&format!("{}/…", id))`: `GraphEndpoint::url` splits
   on `/`, so an id like `123/subscribed_apps` would address another object
   with the tenant's token. `client.get/post/delete("literal/path")` is for
   literal paths only (e.g. `"oauth/access_token"`).

   ```rust
   pub async fn register(&self, pin: &str) -> Result<()> {
       validate_pin(pin)?; // ValidationError::new("pin", "must be 6 digits")
       self.client
           .post_at(&[self.phone_number_id.as_str(), "register"])
           .json(&RegisterRequest { messaging_product: "whatsapp", pin })
           .context("register response")
           .send_success()
           .await
   }
   ```

   - `{"success": true}` responses → `send_success()`.
   - Lists → return `Page<T>`; add `…_stream()` returning
     `request.paginate::<T>()`.
   - Mark a POST `.idempotent(true)` only if replaying cannot duplicate an
     effect.
5. **Tests** (in the module, `#[cfg(test)]`):

   ```rust
   let t = ScriptedTransport::new();
   t.push_json(200, json!({"success": true}));
   let client = Client::builder().transport(t.clone()).access_token("T").build().unwrap();
   client.phone_number("123").register("123456").await.unwrap();
   let req = t.last_request().unwrap();
   assert_eq!(req.method, Method::POST);
   assert_eq!(req.path(), "/v25.0/123/register");
   assert_eq!(req.json(), Some(json!({"messaging_product": "whatsapp", "pin": "123456"})));
   assert_eq!(t.remaining(), 0);
   ```

   Cover: the happy path with the docs' example response, one Graph error
   mapped to the right `ErrorKind`, every local validation, and that an id
   containing `/` stays one segment (`/v25.0/123%2Fx/register`).
6. **Docs**: rustdoc on every public item; module doc lists the Meta
   paths. Update `docs/coverage.md`.
7. `just ci`.
