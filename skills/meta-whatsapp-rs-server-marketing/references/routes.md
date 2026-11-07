# The marketing routes

> **Verified against meta-whatsapp-rs 6ec0b6a3b857837c58481d371d4537004966484f (2026-11-06).** On another revision, trust the service's `/v1/openapi.json` over this page.

| Route | Does |
| --- | --- |
| `POST /v1/numbers/{pn}/marketing-messages` | a marketing template through the Marketing Messages API |
| `GET /v1/wabas/{waba_id}/marketing/onboarding` | the WABA's eligibility and its owner business's terms state |
| `GET`, `PUT /v1/wabas/{waba_id}/marketing/cloud-api-marketing` | whether marketing templates are blocked on Cloud API |
| `POST /v1/wabas/{waba_id}/marketing/template-duplicates` | a template of the WABA duplicated at another max price |
| `GET /v1/wabas/{waba_id}/marketing/reach-estimates` | estimated deliveries and costs at several max prices |
| `GET`, `PUT`, `DELETE /v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price` | a template's max price read, set, or cleared |
| `POST`, `GET /v1/wabas/{waba_id}/welcome-sequences`, `GET`, `PATCH`, `DELETE …/{sequence_id}` | welcome message sequences |
| `POST /v1/admin/businesses/{business_id}/max-price-agreement` | the operator's signing of the max price agreement (admin key) |
| `POST /v1/admin/businesses/{business_id}/max-price-agreement/state` | the agreement's state, read with the operator's `token` in the body; the answer is `{status}`, never the signer (admin key; not a legal act) |
| `POST /v1/admin/partners/businesses/{business_id}/marketing-onboarding` | the partner Intent API: an invitation to a business's admins to accept the terms; a legal act (admin key) |
| `GET /v1/admin/partners/client-wabas` | the client WABAs by onboarding status (admin key) |
