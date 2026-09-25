# The page side of Embedded Signup

> **Verified against meta-whatsapp-rs cdf6f6e0db7dfa4896a3ca8d79600e7a429660125dfc66259e67d04784d0b2 (2026-09-25).** On another revision, trust the code over this page.

> **Verified against wa-rs 8bc676747a09a3c9225a53954030ed7d4eb44adf (2026-09-24).** Also checked against Meta's
> `embedded-signup/implementation` page as fetched on 2026-09-24. Meta owns
> this part: re-read that page (append `.md` to its URL for Markdown) before
> changing it.

The backend gives the page two things from the "start" call: an opaque
`state` (from `SignupSessions::start`) and `options` (from
`LaunchOptions::to_json`). The page gives the backend three things back:
`state`, the `code`, and the raw message event — plus, for a Cloud API
number, the two-step verification PIN the merchant types into your page.

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
    // pin: the number's two-step verification PIN, typed by the merchant in your page
    const pin = document.getElementById('pin').value || null;
    fetch('/whatsapp/connect/callback', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' }, // your session cookie authenticates the merchant
      body: JSON.stringify({ state, code, event: sessionEvent, pin }),
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
  called "start" (your session, never a tenant id the page sends);
  `SignupSessions::redeem(&state, merchant_id)` enforces the binding.
- The PIN field belongs to your page, e.g. an `<input id="pin">` with
  `type="password" inputmode="numeric" maxlength="6" autocomplete="off"`;
  send it with the attempt and nowhere else. The backend parses it before `redeem`, never
  logs or stores it. ~~`body: JSON.stringify({ state, code, event })`~~
  (until 2026-09-24): the PIN had no path from the merchant to the backend.
- Everything in the event is a claim. The backend verifies it with Meta
  (`EmbeddedSignup::onboard`); the page must not decide anything from it.
