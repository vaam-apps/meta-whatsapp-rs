# `ErrorKind` reference

> Verified against wa-rs 91431ae (2026-09-24): `crates/wa-core/src/error/graph.rs`
> (`ErrorKind::from_code`, `is_retryable`, `is_rejected_before_processing`).
> `ErrorKind` is `#[non_exhaustive]`: always keep a `_ =>` arm.

`err.kind()` classifies `Error::Api` by Graph error `code`. Other variants map
to the closest kind: `Http` 5xx and `Transport` → `ServiceUnavailable`, `Http`
429 → `RateLimited`, `Validation` → `InvalidParameter`, `Step` → its source's
kind, everything else → `Unknown`.

"Auto-retry" = `ErrorKind::is_retryable()`. "Replay a send" =
`is_rejected_before_processing()`: the only kinds for which the client replays
a non-idempotent request.

| Kind | Codes | Auto-retry | Replay a send | What to do |
| --- | --- | --- | --- | --- |
| `Authentication` | 0, 190 | no | no | Token expired/revoked: re-onboard the merchant (Embedded Signup) or rotate your system user token |
| `Permission` | 3, 10, 200–299, 131005 | no | no | Permission not granted or removed; check the token's scopes/assets |
| `RateLimited` | 4, 80007, 130429 | yes | yes | Back off; the client already retried |
| `SpamRateLimited` | 131048 | **no** | no | Quality problem; retrying makes it worse |
| `PairRateLimited` | 131056 | yes | yes | Too many messages to the same user; slow down for that user |
| `ClassificationLimitReached` | 131064 | no | no | Template classification violations; fix categories |
| `AccountRestricted` | 368, 131031 | no | no | Policy restriction on the account |
| `CountryRestricted` | 130497 | no | no | Cannot message users in that country |
| `InvalidParameter` | 33, 100, 131008, 131009, 131021, 135000, 2494166–2494168, 2494176, 2494177, 2494179 | no | no | Fix the request; `GraphApiError::code` tells In-App Signup errors apart |
| `UnsupportedMessageType` | 131051 | no | no | — |
| `CustomerServiceWindowClosed` | 131047 | no | no | >24 h since the user's last message: send a template |
| `EcosystemEngagementLimit` | 131049 | **no** | no | Per-user marketing limit; do not retry for at least 24 h |
| `MarketingOptedOut` | 131050 | no | no | User stopped marketing: record the opt-out, never retry |
| `MarketingNotAllowed` | 131055, 131063, 134100 | no | no | Not a marketing template on the MM API, or marketing disabled on this API |
| `Undeliverable` | 131026 | no | no | Not on WhatsApp, old client, ToS pending… |
| `BlockedByBusiness` | 130403 | no | no | You blocked this user; unblock first |
| `ExperimentHoldout` | 130472 | no | no | Held back by a Meta experiment |
| `RecipientNotSupported` | 131062 | no | no | This message cannot go to a BSUID; address by phone number |
| `MediaDownloadFailed` | 131052 | no | no | — |
| `MediaUploadFailed` | 131053 | no | no | — |
| `TemplateParameterMismatch` | 132000, 132012, 132018 | no | no | Parameters do not match the approved template |
| `TemplateNotFound` | 132001 | no | no | Missing in that language, or not approved |
| `TemplateTextTooLong` | 132005 | no | no | — |
| `TemplatePolicyViolation` | 132007 | no | no | — |
| `TemplatePaused` | 132015 | no | no | Paused for low quality |
| `TemplateDisabled` | 132016 | no | no | Permanently disabled |
| `TemplateSyncing` | 134101 | yes | no | MM API ad sync (~10 min after creating/reviving); retry later from your queue |
| `TemplateUnavailable` | 134102 | no | no | Not eligible on the MM API; check onboarding status |
| `TemplateLimitReached` | 2388019 | no | no | WABA template limit |
| `TemplateRejected` | 2388039, 2388040, 2388047, 2388072, 2388073, 2388293, 2388299 | no | no | Creation rejected for content/format |
| `FlowUnavailable` | 132068, 132069 | no | no | Flow blocked or throttled |
| `Registration` | 131037, 131045, 133000, 133006, 133010, 133015, 133016, 136024 | no | no | Number not registered/verified, display name, or 133016 = 72 h registration lock |
| `TwoStepVerification` | 133005, 133008, 133009 | no | no | Wrong PIN, or too many guesses |
| `SyncNotAllowed` | 2593107–2593109 | no | no | Coexistence sync already done, too late, or declined |
| `DuplicateOnboarding` | 1752041 | no | no | A partner already requested onboarding |
| `NotFound` | 2494164 | no | no | e.g. unknown In-App Signup |
| `FeatureNotAvailable` | 2494165 | no | no | e.g. In-App Signup not enabled on the WABA |
| `Payment` | 131042, 134011 | no | no | Payment method / payments ToS |
| `ServiceUnavailable` | 1, 2, 131000, 131016, 131057, 133004, 2494100 | yes | **no** | Meta-side; a send that got this may have gone out |
| `Unknown` | anything else | no¹ | no | Read `err.graph()` |

¹ `GraphApiError::is_retryable()` is also `true` when Meta sets
`is_transient: true`, whatever the kind.

The same `GraphApiError` type arrives in webhooks: `Status::errors` of a
`failed` status, `InboundMessage::errors`, `WebhookEvent::ErrorReported`.
Call `.kind()` on those too — an opt-out or engagement limit can surface
there rather than on the send call.
