# The page side of Embedded Signup

> Verified against wa-rs 91431ae (2026-09-24), and against Meta's
> `embedded-signup/implementation` page as fetched on 2026-09-24. Meta owns
> this part: re-read that page (append `.md` to its URL for Markdown) before
> changing it.

The backend gives the page two things from the "start" call: an opaque
`state` (from `SignupSessions::start`) and `options` (from
`LaunchOptions::to_json`). The page gives the backend three things back:
`state`, the `code`, and the raw message event.

```js
// Assumes the Facebook JS SDK is loaded and FB.init({ appId, version, ... }) ran.
// Only the app id and configuration id are public; the app secret stays on the server.
async function connectWhatsApp() {
  const { state, options } = await (await fetch('/whatsapp/connect', { method: 'POST' })).json();

  let sessionEvent = null;   // the raw WA_EMBEDDED_SIGNUP message event (a JSON string)
  let code = null;

  function maybeSubmit() {
    if (!code || !sessionEvent) return;
    window.removeEventListener('message', onMessage);
    // Post at once: the code is single-use and expires after 30 seconds.
    fetch('/whatsapp/connect/callback', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ state, code, event: sessionEvent }),
    });
  }

  function onMessage(event) {
    try {
      // Meta's snippet tests origin.endsWith('facebook.com'), which also matches
      // evilfacebook.com; compare the host instead (origin may be "null": caught).
      const host = new URL(event.origin).hostname;
      if (host !== 'facebook.com' && !host.endsWith('.facebook.com')) return;
      const data = JSON.parse(event.data);
      if (data.type === 'WA_EMBEDDED_SIGNUP') { sessionEvent = event.data; maybeSubmit(); }
    } catch { /* not ours */ }
  }
  window.addEventListener('message', onMessage);

  FB.login((response) => {
    if (response.authResponse) { code = response.authResponse.code; maybeSubmit(); }
    // else: the user closed the popup; a CANCEL message event may still arrive
  }, options);
}
```

Notes:

- Forward `event.data` as the string it is; the backend parses it with
  `EmbeddedSignupEvent::from_json`. Ids must stay JSON strings — a 64-bit id
  that went through a JavaScript number may have been rounded to someone
  else's id, which is why the parser rejects numeric ids.
- A `CANCEL` or `ERROR` event without a code is worth posting too (for
  analytics, `CancelInfo::current_step`), but there is nothing to onboard.
- The callback endpoint must be authenticated as the same merchant that
  called "start"; `SignupSessions::redeem(&state, merchant_id)` enforces the
  binding.
- Everything in the event is a claim. The backend verifies it with Meta
  (`EmbeddedSignup::onboard`); the page must not decide anything from it.
