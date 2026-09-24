//! Solution Partner onboarding: the credit line steps [`EmbeddedSignup`]
//! adds when a deployment is configured with [`SolutionPartner`], and
//! revocation from what onboarding stored.
//!
//! Docs: `embedded-signup/onboarding-customers-as-a-solution-partner`
//! (step order: subscribe the app, share the credit line, register the
//! number), `solution-providers/share-and-revoke-credit-lines`,
//! `solution-providers/manage-system-users`.

use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::{AllocationConfigId, CreditLineId, WabaId};
use wa_core::secret::AccessToken;

use super::EmbeddedSignup;
use super::vault::{StoredBusinessToken, TokenVault};
use crate::Client;
use crate::credit_lines::{WabaCurrency, is_shared, owned_by};
use crate::waba::WabaTask;

/// How the credit line is shared with each onboarded customer
/// (`solution-providers/share-and-revoke-credit-lines`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum CreditSharing {
    /// Meta's current method: add the partner's system user to the
    /// customer's WABA (`assign_system_user`), then one
    /// `whatsapp_credit_sharing_and_attach` call with the system user token.
    #[default]
    ShareAndAttach,
    /// Meta's "alternate method", being tested to replace the first:
    /// `whatsapp_credit_sharing` with the customer's verified business
    /// portfolio id (system user token), then `whatsapp_credit_attach`
    /// (the customer's business token). No system user step: Meta does not
    /// say whether this method needs it.
    ShareThenAttach,
}

/// Onboard as a **Solution Partner**: every customer onboarded through
/// Embedded Signup is funded by your credit line. Configured once per
/// deployment with [`EmbeddedSignup::solution_partner`]; without it,
/// onboarding is the Tech Provider flow (the customer adds a payment
/// method).
///
/// `Debug` never shows the token.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SolutionPartner {
    /// Your system user access token: `business_management`, and an Admin
    /// or Financial Editor role on your business portfolio. Used for every
    /// credit line call except the attach of [`CreditSharing::ShareThenAttach`],
    /// and to add the system user to the customer's WABA.
    pub system_token: AccessToken,
    /// The id of the system user behind [`Self::system_token`], added to
    /// the customer's WABA before [`CreditSharing::ShareAndAttach`].
    pub system_user_id: String,
    /// Your extended credit line
    /// ([`CreditLines::list`](crate::credit_lines::CreditLines::list)).
    pub credit_line_id: CreditLineId,
    /// How the line is shared.
    pub method: CreditSharing,
    /// The currency when an [`OnboardingRequest`](super::OnboardingRequest)
    /// names none.
    pub default_currency: Option<WabaCurrency>,
    /// Tasks granted to the system user on the customer's WABA
    /// (default `[MANAGE]`). Under a Multi-Partner Solution without
    /// `MESSAGING`, `MANAGE` is refused: pass granular tasks including
    /// `MANAGE_BILLING`, which credit sharing needs
    /// (`solution-providers/manage-system-users`).
    pub system_user_tasks: Vec<WabaTask>,
}

impl SolutionPartner {
    /// Share `credit_line_id` with [`CreditSharing::ShareAndAttach`],
    /// granting `system_user_id` `MANAGE` on each customer's WABA, with no
    /// default currency.
    pub fn new(
        system_token: AccessToken,
        system_user_id: impl Into<String>,
        credit_line_id: impl Into<CreditLineId>,
    ) -> Self {
        Self {
            system_token,
            system_user_id: system_user_id.into(),
            credit_line_id: credit_line_id.into(),
            method: CreditSharing::default(),
            default_currency: None,
            system_user_tasks: vec![WabaTask::Manage],
        }
    }

    /// Share with `method`.
    #[must_use]
    pub fn method(mut self, method: CreditSharing) -> Self {
        self.method = method;
        self
    }

    /// The currency of customers whose onboarding request names none.
    #[must_use]
    pub fn default_currency(mut self, currency: WabaCurrency) -> Self {
        self.default_currency = Some(currency);
        self
    }

    /// Grant these tasks to the system user instead of `MANAGE`.
    #[must_use]
    pub fn system_user_tasks(mut self, tasks: impl IntoIterator<Item = WabaTask>) -> Self {
        self.system_user_tasks = tasks.into_iter().collect();
        self
    }

    fn validate(&self) -> Result<(), ValidationError> {
        if self.system_token.expose_secret().trim().is_empty() {
            return Err(ValidationError::new("system_token", "required"));
        }
        if self.credit_line_id.as_str().trim().is_empty() {
            return Err(ValidationError::new("credit_line_id", "required"));
        }
        if self.method == CreditSharing::ShareAndAttach {
            if self.system_user_id.trim().is_empty() {
                return Err(ValidationError::new(
                    "system_user_id",
                    "required: the system user is added to the customer's WABA before sharing",
                ));
            }
            if self.system_user_tasks.is_empty() {
                return Err(ValidationError::new(
                    "system_user_tasks",
                    "at least one task is required",
                ));
            }
        }
        if let Some(currency) = &self.default_currency {
            currency.validate()?;
        }
        Ok(())
    }
}

/// What the credit steps of one onboarding need, checked before any request.
pub(super) struct CreditPlan<'a> {
    pub(super) partner: &'a SolutionPartner,
    pub(super) currency: WabaCurrency,
}

impl EmbeddedSignup {
    /// Onboard as a Solution Partner with `partner` (one choice per
    /// deployment): [`Self::onboard`] and [`Self::resume`] then add the
    /// credit line steps between `subscribe_app` and `register_phone`,
    /// following Meta's Solution Partner onboarding order. See the
    /// [module docs](super#solution-partner-mode).
    #[must_use]
    pub fn solution_partner(mut self, partner: SolutionPartner) -> Self {
        self.partner = Some(partner);
        self
    }

    /// The Solution Partner configuration, if this deployment is one.
    pub fn partner(&self) -> Option<&SolutionPartner> {
        self.partner.as_ref()
    }

    /// The credit plan for `currency` (the request's), or `None` for a Tech
    /// Provider. Every refusal here happens before any request.
    pub(super) fn credit_plan(
        &self,
        currency: Option<&WabaCurrency>,
    ) -> Result<Option<CreditPlan<'_>>, ValidationError> {
        let Some(partner) = &self.partner else {
            return match currency {
                Some(_) => Err(ValidationError::new(
                    "currency",
                    "only used when onboarding as a Solution Partner (EmbeddedSignup::solution_partner)",
                )),
                None => Ok(None),
            };
        };
        partner.validate()?;
        let currency = currency
            .or(partner.default_currency.as_ref())
            .cloned()
            .ok_or_else(|| {
                ValidationError::new(
                    "currency",
                    "required to share the credit line: set OnboardingRequest::currency or SolutionPartner::default_currency",
                )
            })?;
        currency.validate()?;
        Ok(Some(CreditPlan { partner, currency }))
    }

    /// The partner's client (system user token).
    fn system_client(&self, partner: &SolutionPartner) -> Client {
        self.client.with_token(partner.system_token.clone())
    }

    /// Revoke the credit line from the customer business that owns
    /// `waba_id`, from what onboarding stored in `vault`, and return the
    /// allocation ids revoked.
    ///
    /// Use it when the customer unshares the WABA or removes you as a
    /// partner (`account_update` `PARTNER_REMOVED`): messaging on the WABA is
    /// then blocked, `owner_business_info` can no longer be read, and Meta
    /// recommends revoking at once. The business id comes from the vault
    /// (verified with Meta at onboarding), never from a new lookup. When it
    /// is missing, or the lookup by it finds no record naming that business,
    /// the allocation id stored at onboarding is revoked instead (an error
    /// then, if Meta already deleted it, rather than a line left shared).
    ///
    /// Revocation applies to **every** WABA of that customer business
    /// shared with you. The vault entry is left as it is; delete it
    /// ([`TokenVault::delete`]) if the merchant is gone.
    pub async fn revoke_credit_line(
        &self,
        waba_id: &WabaId,
        vault: &TokenVault,
    ) -> Result<Vec<AllocationConfigId>> {
        let partner = self.partner.as_ref().ok_or_else(|| {
            ValidationError::new(
                "solution_partner",
                "this EmbeddedSignup is not configured as a Solution Partner",
            )
        })?;
        partner.validate()?;
        let stored = vault
            .get(waba_id)
            .await?
            .ok_or_else(|| ValidationError::new("waba_id", "no token is stored for this WABA"))?;
        let lines = self.system_client(partner).credit_lines();
        if let Some(business) = &stored.business_id {
            let revoked = lines
                .revoke_for_business(&partner.credit_line_id, business)
                .await?;
            if !revoked.is_empty() {
                return Ok(revoked);
            }
        }
        // Nothing attributable found (or no business stored): never leave
        // the line shared silently while onboarding recorded an allocation.
        match (stored.allocation_config_id, &stored.business_id) {
            (Some(allocation), _) => {
                lines.revoke(&allocation).await?;
                Ok(vec![allocation])
            }
            (None, Some(_)) => Ok(Vec::new()),
            (None, None) => Err(ValidationError::new(
                "business_id",
                "the stored record names neither the customer's business nor an allocation",
            )
            .into()),
        }
    }
}

/// `assign_system_user`: add the partner's system user to the customer's
/// WABA with the configured tasks (system user token). Repeatable: it sets
/// the same grant again.
pub(super) async fn assign_system_user(
    es: &EmbeddedSignup,
    plan: &CreditPlan<'_>,
    waba_id: &WabaId,
) -> Result<()> {
    es.system_client(plan.partner)
        .waba(waba_id.clone())
        .assign_user(
            &plan.partner.system_user_id,
            &plan.partner.system_user_tasks,
        )
        .await
}

/// `share_credit_line`: make the partner's credit line fund `stored`'s
/// WABA, **checking first** whether it already does, and record the
/// allocation id in the vault.
///
/// A POST that timed out may have succeeded, and Meta refuses to attach a
/// line to a WABA that already has one, so nothing is posted before the
/// check: the records of the line shared with the customer business
/// (`owning_credit_allocation_configs`, by the verified owner business id)
/// and any stored allocation are compared with the WABA's
/// `primary_funding_id`. When the check cannot run (no business id and no
/// stored allocation), a first attempt shares with the one-call method, but
/// a resumed one refuses rather than POST blindly.
pub(super) async fn share_credit_line(
    es: &EmbeddedSignup,
    plan: &CreditPlan<'_>,
    business: &Client,
    vault: &TokenVault,
    stored: &mut StoredBusinessToken,
    resuming: bool,
) -> Result<AllocationConfigId> {
    let partner = plan.partner;
    let line = &partner.credit_line_id;
    let waba = stored.waba_id.clone();
    if partner.method == CreditSharing::ShareThenAttach && stored.business_id.is_none() {
        return Err(ValidationError::new(
            "business_id",
            "Meta did not report the WABA's owner business, which whatsapp_credit_sharing needs",
        )
        .into());
    }
    if resuming && stored.business_id.is_none() && stored.allocation_config_id.is_none() {
        return Err(ValidationError::new(
            "business_id",
            "cannot check whether the credit line already funds this WABA (no owner business or allocation stored); refusing to share it again blindly",
        )
        .into());
    }
    let system = es.system_client(partner).credit_lines();

    // 1. What is already shared with this customer business.
    let shared_with_business = match &stored.business_id {
        Some(owner) => owned_by(system.allocations_for(line, owner).await?, owner),
        None => Vec::new(),
    };
    let mut candidates = shared_with_business.clone();
    if let Some(known) = &stored.allocation_config_id
        && !candidates.contains(known)
    {
        candidates.insert(0, known.clone());
    }

    // 2. Does one of them already fund the WABA?
    let mut funding = None;
    let mut already = None;
    for candidate in &candidates {
        let allocation = system.receiving_credential(candidate).await?;
        if funding.is_none() {
            funding = Some(business.credit_lines().primary_funding(&waba).await?);
        }
        if let Some(funding) = &funding
            && is_shared(&allocation, funding)
        {
            already = Some(candidate.clone());
            break;
        }
    }

    // 3. If not, share it.
    let allocation = match already {
        Some(id) => id,
        None => match partner.method {
            CreditSharing::ShareAndAttach => {
                system
                    .share_and_attach(line, &waba, &plan.currency)
                    .await?
                    .allocation_config_id
            }
            CreditSharing::ShareThenAttach => {
                if shared_with_business.is_empty()
                    && let Some(owner) = &stored.business_id
                {
                    system.share(line, owner).await?;
                }
                business
                    .credit_lines()
                    .attach(line, &waba, &plan.currency)
                    .await?
                    .allocation_config_id
            }
        },
    };

    // 4. Remember it with the token, for resume and revocation.
    if stored.allocation_config_id.as_ref() != Some(&allocation) {
        stored.allocation_config_id = Some(allocation.clone());
        vault.store(stored).await?;
    }
    Ok(allocation)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::clock::ManualClock;
    use wa_core::error::TransportError;
    use wa_core::ids::{BusinessId, PhoneNumberId};
    use wa_core::secret::SecretBytes;
    use wa_core::testing::{RecordedBody, RecordedRequest, ScriptedTransport};
    use wa_core::{Error, ErrorKind};

    use super::super::event::EmbeddedSignupEvent;
    use super::super::onboard::OnboardingRequest;
    use super::super::onboard::steps::{
        ASSIGN_SYSTEM_USER, DEBUG_TOKEN, EXCHANGE_CODE, LOAD_TOKEN, REGISTER_PHONE,
        SHARE_CREDIT_LINE, STORE_TOKEN, SUBSCRIBE_APP, VERIFY_ASSETS,
    };
    use super::super::token::SignupCode;
    use super::super::vault::{VaultKey, VaultKeys};
    use super::*;
    use crate::phone_numbers::TwoStepPin;
    use crate::{AppCredentials, RetryPolicy};

    const APP_ID: &str = "236484624622562";
    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    const CODE: &str = "AQBhlXsctMxJYbwbrpybxlo9tLPGy";
    const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89Xnn";
    const SYSTEM_TOKEN: &str = "EAAAN6tcBzAUBOZC82CW7iR2LiaZBwUHS4Y7FDtQ";
    const SYSTEM_USER: &str = "1972555232742222";
    const LINE: &str = "1972385232742146";
    const WABA: &str = "102290129340398";
    const PHONE: &str = "106540352242922";
    /// The WABA's owner as Meta reports it (`owner_business_info`).
    const BUSINESS: &str = "2729063490586005";
    /// What the browser's event claims; must never reach a credit call.
    const CLAIMED: &str = "CLAIMED_BY_THE_BROWSER";
    const ALLOCATION: &str = "58501441721238";
    const CREDENTIAL: &str = "7340125593810";
    const PIN: &str = "581063";

    struct Harness {
        t: ScriptedTransport,
        es: EmbeddedSignup,
        vault: TokenVault,
    }

    fn partner(method: CreditSharing) -> SolutionPartner {
        SolutionPartner::new(AccessToken::new(SYSTEM_TOKEN), SYSTEM_USER, LINE).method(method)
    }

    fn harness_with(partner: Option<SolutionPartner>) -> Harness {
        let t = ScriptedTransport::new();
        let client = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let vault = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC))));
        let es = client.embedded_signup(AppCredentials::new(APP_ID, APP_SECRET));
        let es = match partner {
            Some(p) => es.solution_partner(p),
            None => es,
        };
        Harness { t, es, vault }
    }

    fn harness(method: CreditSharing) -> Harness {
        harness_with(Some(partner(method)))
    }

    fn request() -> OnboardingRequest {
        let event = EmbeddedSignupEvent::from_value(json!({
            "data": {"phone_number_id": PHONE, "waba_id": WABA, "business_id": CLAIMED},
            "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH"
        }))
        .unwrap();
        OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .register_with_pin(TwoStepPin::new(PIN).unwrap())
    }

    fn success() -> serde_json::Value {
        json!({"success": true})
    }

    /// Up to and including `subscribe_app`, as in the Tech Provider flow;
    /// `owner` is the `owner_business_info` Meta answers.
    fn script_until_subscribe(t: &ScriptedTransport, owner: serde_json::Value) {
        t.push_json(200, json!({"access_token": TOKEN, "token_type": "bearer"}));
        t.push_json(
            200,
            json!({"data": {
              "app_id": APP_ID, "type": "SYSTEM_USER", "is_valid": true,
              "granular_scopes": [
                {"scope": "whatsapp_business_management", "target_ids": [WABA]},
                {"scope": "whatsapp_business_messaging", "target_ids": [WABA]}
              ]
            }}),
        );
        t.push_json(200, owner);
        t.push_json(200, json!({"data": [{"id": PHONE}]}));
        t.push_json(200, success());
    }

    fn owner() -> serde_json::Value {
        json!({"owner_business_info": {"name": "Wind & Wool", "id": BUSINESS}, "id": WABA})
    }

    fn nothing_shared() -> serde_json::Value {
        json!({"data": []})
    }

    /// `share-and-revoke-credit-lines`, "Get the customer's credit sharing
    /// record": the page's single-object shape.
    fn shared_record(allocation: &str) -> serde_json::Value {
        json!({"id": allocation, "receiving_business": {"name": "Wind & Wool", "id": BUSINESS}})
    }

    fn receiving_credential(allocation: &str, credential: &str) -> serde_json::Value {
        json!({"receiving_credential": {"id": credential}, "id": allocation})
    }

    fn funding(credential: &str) -> serde_json::Value {
        json!({"primary_funding_id": credential, "id": WABA})
    }

    fn shared_and_attached() -> serde_json::Value {
        json!({"allocation_config_id": ALLOCATION, "waba_id": WABA})
    }

    fn step(err: &Error) -> &'static str {
        match err {
            Error::Step { step, .. } => step,
            other => panic!("not a step error: {other}"),
        }
    }

    fn assert_request(
        r: &RecordedRequest,
        method: &Method,
        path: &str,
        query: &[(&str, &str)],
        bearer: &str,
    ) {
        assert_eq!(&r.method, method, "{path}");
        assert_eq!(r.path(), path);
        let got: Vec<(String, String)> = r.url.query_pairs().into_owned().collect();
        let want: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        assert_eq!(got, want, "query of {path}");
        assert_eq!(r.bearer(), Some(bearer), "token of {path}");
    }

    fn posts_to(reqs: &[RecordedRequest], edge: &str) -> usize {
        reqs.iter()
            .filter(|r| r.method == Method::POST && r.path().ends_with(edge))
            .count()
    }

    #[tokio::test]
    async fn share_and_attach_runs_every_step_in_meta_order_with_the_right_tokens() {
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // owning_credit_allocation_configs
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register

        let done =
            h.es.onboard(&request().currency(WabaCurrency::Eur), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                ASSIGN_SYSTEM_USER,
                SHARE_CREDIT_LINE,
                REGISTER_PHONE
            ]
        );
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );

        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 9);
        assert_eq!(reqs[0].path(), "/v25.0/oauth/access_token");
        assert_eq!(reqs[0].header("authorization"), None);
        assert_eq!(reqs[1].path(), "/v25.0/debug_token");
        assert_request(
            &reqs[2],
            &Method::GET,
            &format!("/v25.0/{WABA}"),
            &[("fields", "owner_business_info")],
            TOKEN,
        );
        assert_request(
            &reqs[4],
            &Method::POST,
            &format!("/v25.0/{WABA}/subscribed_apps"),
            &[],
            TOKEN,
        );
        assert_request(
            &reqs[5],
            &Method::POST,
            &format!("/v25.0/{WABA}/assigned_users"),
            &[("user", SYSTEM_USER), ("tasks", r#"["MANAGE"]"#)],
            SYSTEM_TOKEN,
        );
        assert_request(
            &reqs[6],
            &Method::GET,
            &format!("/v25.0/{LINE}/owning_credit_allocation_configs"),
            &[
                ("receiving_business_id", BUSINESS),
                ("fields", "id,receiving_business"),
            ],
            SYSTEM_TOKEN,
        );
        assert_request(
            &reqs[7],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach"),
            &[("waba_currency", "EUR"), ("waba_id", WABA)],
            SYSTEM_TOKEN,
        );
        assert_eq!(reqs[7].body, RecordedBody::Empty);
        assert_request(
            &reqs[8],
            &Method::POST,
            &format!("/v25.0/{PHONE}/register"),
            &[],
            TOKEN,
        );
        assert_eq!(
            reqs[8].json(),
            Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
        );
        assert_eq!(h.t.remaining(), 0);

        let stored = h.vault.get(&WabaId::new(WABA)).await.unwrap().unwrap();
        assert_eq!(
            stored.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        assert_eq!(stored.business_id, Some(BusinessId::new(BUSINESS)));
        assert_eq!(stored.token.expose_secret(), TOKEN);
        assert!(
            h.vault
                .get_by_phone_number(&PhoneNumberId::new(PHONE))
                .await
                .unwrap()
                .is_some(),
            "the re-store kept the phone index"
        );
    }

    #[tokio::test]
    async fn share_then_attach_shares_with_the_verified_owner_and_attaches_as_the_merchant() {
        let h = harness_with(Some(
            partner(CreditSharing::ShareThenAttach).default_currency(WabaCurrency::Idr),
        ));
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(
            200,
            json!({"success": true, "waba_id": WABA, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(200, success()); // register

        let done = h.es.onboard(&request(), &h.vault).await.unwrap();
        assert_eq!(
            done.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                SHARE_CREDIT_LINE,
                REGISTER_PHONE
            ],
            "no system user step for the two-call method"
        );
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 9);
        assert_request(
            &reqs[4],
            &Method::POST,
            &format!("/v25.0/{WABA}/subscribed_apps"),
            &[],
            TOKEN,
        );
        assert_request(
            &reqs[5],
            &Method::GET,
            &format!("/v25.0/{LINE}/owning_credit_allocation_configs"),
            &[
                ("receiving_business_id", BUSINESS),
                ("fields", "id,receiving_business"),
            ],
            SYSTEM_TOKEN,
        );
        assert_request(
            &reqs[6],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_sharing"),
            &[("receiving_business_id", BUSINESS)],
            SYSTEM_TOKEN,
        );
        assert_request(
            &reqs[7],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_attach"),
            &[("waba_currency", "IDR"), ("waba_id", WABA)],
            TOKEN,
        );
        assert_eq!(reqs[6].body, RecordedBody::Empty);
        assert_eq!(reqs[7].body, RecordedBody::Empty);
        assert_eq!(reqs[8].path(), format!("/v25.0/{PHONE}/register"));
        for r in &reqs {
            assert!(
                !r.url.as_str().contains(CLAIMED),
                "the browser's business id reached {}",
                r.url
            );
        }
        assert_eq!(h.t.remaining(), 0);
        assert_eq!(
            h.vault
                .get(&WabaId::new(WABA))
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
    }

    #[tokio::test]
    async fn missing_or_misplaced_settings_fail_before_any_request() {
        // Solution Partner mode, no currency anywhere: the code is not spent.
        let h = harness(CreditSharing::ShareAndAttach);
        h.t.push_json(200, json!({"access_token": TOKEN}));
        let err = h.es.onboard(&request(), &h.vault).await.unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "currency"),
            "{err}"
        );
        let err =
            h.es.resume(&WabaId::new(WABA), &request(), &h.vault)
                .await
                .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "currency"),
            "{err}"
        );
        assert!(
            h.t.requests().is_empty(),
            "nothing sent, the code not exchanged"
        );
        assert_eq!(h.t.remaining(), 1);

        // A currency Meta does not list, unless chosen as `Other` on purpose.
        let err =
            h.es.onboard(
                &request().currency(WabaCurrency::Other("eur".into())),
                &h.vault,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "waba_currency"),
            "{err}"
        );

        // A Tech Provider request naming a currency is a caller mistake.
        let tp = harness_with(None);
        let err = tp
            .es
            .onboard(&request().currency(WabaCurrency::Usd), &tp.vault)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "currency"),
            "{err}"
        );

        // Incomplete partner settings.
        for (bad, field) in [
            (
                SolutionPartner::new(AccessToken::new(SYSTEM_TOKEN), " ", LINE),
                "system_user_id",
            ),
            (
                SolutionPartner::new(AccessToken::new(""), SYSTEM_USER, LINE),
                "system_token",
            ),
            (
                SolutionPartner::new(AccessToken::new(SYSTEM_TOKEN), SYSTEM_USER, ""),
                "credit_line_id",
            ),
            (
                partner(CreditSharing::ShareAndAttach).system_user_tasks([]),
                "system_user_tasks",
            ),
        ] {
            let h = harness_with(Some(bad));
            let err =
                h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
                    .await
                    .unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == field),
                "{err}"
            );
            assert!(h.t.requests().is_empty());
        }
        assert!(tp.t.requests().is_empty());
    }

    #[tokio::test]
    async fn a_refused_share_keeps_the_token_and_resume_finishes() {
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            400,
            json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException", "code": 100, "fbtrace_id": "A"}}),
        );
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
        assert!(!err.may_have_been_sent());
        assert_eq!(h.t.requests().len(), 8, "register never ran");
        let stored = h.vault.get(&WabaId::new(WABA)).await.unwrap().unwrap();
        assert_eq!(stored.token.expose_secret(), TOKEN, "the token is kept");
        assert_eq!(stored.allocation_config_id, None);
        assert_eq!(h.t.remaining(), 0);

        // Fixed on Meta's side: resume checks, finds nothing, shares.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        let done =
            h.es.resume(&WabaId::new(WABA), &request, &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.steps_completed,
            [
                LOAD_TOKEN,
                VERIFY_ASSETS,
                SUBSCRIBE_APP,
                ASSIGN_SYSTEM_USER,
                SHARE_CREDIT_LINE,
                REGISTER_PHONE
            ]
        );
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let reqs = h.t.requests();
        assert_request(
            &reqs[10],
            &Method::GET,
            &format!("/v25.0/{LINE}/owning_credit_allocation_configs"),
            &[
                ("receiving_business_id", BUSINESS),
                ("fields", "id,receiving_business"),
            ],
            SYSTEM_TOKEN,
        );
        assert_request(
            &reqs[11],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach"),
            &[("waba_currency", "USD"), ("waba_id", WABA)],
            SYSTEM_TOKEN,
        );
        assert_eq!(h.t.remaining(), 0);
        assert_eq!(
            h.vault
                .get(&WabaId::new(WABA))
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
    }

    #[tokio::test]
    async fn resume_after_an_ambiguous_share_checks_first_and_posts_nothing_when_shared() {
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_error(|| TransportError::Timeout); // did Meta act? unknown
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(err.may_have_been_sent());
        assert_eq!(h.t.remaining(), 0);
        let before = h.t.requests().len();

        // It had gone through: the record exists and funds the WABA.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        let done =
            h.es.resume(&WabaId::new(WABA), &request, &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let reqs = h.t.requests();
        let resumed = &reqs[before..];
        assert_eq!(resumed.len(), 6);
        assert_eq!(
            posts_to(resumed, "/whatsapp_credit_sharing_and_attach"),
            0,
            "shared already: never posted again"
        );
        assert_request(
            &resumed[3],
            &Method::GET,
            &format!("/v25.0/{ALLOCATION}"),
            &[("fields", "receiving_credential")],
            SYSTEM_TOKEN,
        );
        assert_request(
            &resumed[4],
            &Method::GET,
            &format!("/v25.0/{WABA}"),
            &[("fields", "primary_funding_id")],
            TOKEN,
        );
        assert_eq!(resumed[5].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(h.t.remaining(), 0);
        assert_eq!(
            h.vault
                .get(&WabaId::new(WABA))
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION)),
            "found, then recorded"
        );
    }

    #[tokio::test]
    async fn a_record_that_does_not_fund_the_waba_is_shared_again() {
        // The business has a record (another of its WABAs), but this WABA's
        // funding is something else: the one-call share runs.
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(
            200,
            json!({"data": [
                {"id": "OTHER_BUSINESS_RECORD", "receiving_business": {"id": "SOMEONE_ELSE"}},
                {"id": "EARLIER", "receiving_business": {"id": BUSINESS}}
            ]}),
        );
        h.t.push_json(200, receiving_credential("EARLIER", "CRED_OF_ANOTHER_WABA"));
        h.t.push_json(200, json!({"id": WABA})); // no primary_funding_id yet
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        let done =
            h.es.onboard(&request().currency(WabaCurrency::Gbp), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let reqs = h.t.requests();
        assert_eq!(
            reqs.iter()
                .filter(|r| r.path() == "/v25.0/OTHER_BUSINESS_RECORD")
                .count(),
            0,
            "another business's record is never considered"
        );
        assert_eq!(reqs[7].path(), "/v25.0/EARLIER");
        assert_eq!(
            reqs[9].path(),
            format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach")
        );
        assert_eq!(reqs[9].query("waba_currency").as_deref(), Some("GBP"));
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn share_then_attach_resume_attaches_without_sharing_twice() {
        let h = harness(CreditSharing::ShareThenAttach);
        let request = request().currency(WabaCurrency::Inr);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(
            400,
            json!({"error": {"message": "(#100) Invalid currency", "type": "OAuthException", "code": 100}}),
        );
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        let before = h.t.requests().len();

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(
            200,
            json!({"receiving_credential": {"id": CREDENTIAL}, "id": ALLOCATION}),
        );
        h.t.push_json(200, json!({"id": WABA}));
        h.t.push_json(
            200,
            json!({"success": true, "waba_id": WABA, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(200, success()); // register
        h.es.resume(&WabaId::new(WABA), &request, &h.vault)
            .await
            .unwrap();
        let reqs = h.t.requests();
        let resumed = &reqs[before..];
        assert_eq!(
            posts_to(resumed, "/whatsapp_credit_sharing"),
            0,
            "shared once"
        );
        assert_request(
            &resumed[4],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_attach"),
            &[("waba_currency", "INR"), ("waba_id", WABA)],
            TOKEN,
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_resume_that_cannot_check_refuses_to_share_blindly() {
        // Meta reported no owner business, so nothing can be looked up.
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, json!({"id": WABA}));
        h.t.push_json(200, success()); // assigned_users
        h.t.push_error(|| TransportError::Timeout);
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        let reqs = h.t.requests();
        assert_eq!(
            reqs[6].path(),
            format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach"),
            "a first attempt without an owner shares directly"
        );
        let before = reqs.len();

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err =
            h.es.resume(&WabaId::new(WABA), &request, &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(matches!(
            &err,
            Error::Step { source, .. } if matches!(&**source, Error::Validation(v) if v.field == "business_id")
        ));
        assert_eq!(h.t.requests().len(), before + 2, "no share posted");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn revocation_uses_the_stored_owner_not_the_unreadable_waba() {
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success());
        h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
            .await
            .unwrap();
        let before = h.t.requests().len();

        // PARTNER_REMOVED: `GET /{WABA}?fields=owner_business_info` would now
        // fail; revocation never asks.
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, success());
        let revoked =
            h.es.revoke_credit_line(&WabaId::new(WABA), &h.vault)
                .await
                .unwrap();
        assert_eq!(revoked, [AllocationConfigId::new(ALLOCATION)]);
        let reqs = h.t.requests();
        let revocation = &reqs[before..];
        assert_eq!(revocation.len(), 2);
        assert_request(
            &revocation[0],
            &Method::GET,
            &format!("/v25.0/{LINE}/owning_credit_allocation_configs"),
            &[
                ("receiving_business_id", BUSINESS),
                ("fields", "id,receiving_business"),
            ],
            SYSTEM_TOKEN,
        );
        assert_request(
            &revocation[1],
            &Method::DELETE,
            &format!("/v25.0/{ALLOCATION}"),
            &[],
            SYSTEM_TOKEN,
        );
        assert!(
            revocation
                .iter()
                .all(|r| r.path() != format!("/v25.0/{WABA}"))
        );
        assert_eq!(h.t.remaining(), 0);

        // Without a partner configuration, or a stored record: refused.
        let tp = harness_with(None);
        assert!(matches!(
            tp.es.revoke_credit_line(&WabaId::new(WABA), &h.vault).await,
            Err(Error::Validation(_))
        ));
        assert!(matches!(
            h.es.revoke_credit_line(&WabaId::new("UNKNOWN"), &h.vault)
                .await,
            Err(Error::Validation(_))
        ));
        assert_eq!(h.t.requests().len(), before + 2);
    }

    #[tokio::test]
    async fn revocation_falls_back_to_the_stored_allocation_when_the_lookup_finds_none() {
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault
            .store(
                &StoredBusinessToken::new(WABA, AccessToken::new(TOKEN))
                    .business_id(BUSINESS)
                    .allocation_config_id(ALLOCATION),
            )
            .await
            .unwrap();
        // The lookup answers records Meta did not attribute to the business.
        h.t.push_json(200, json!({"data": [{"id": "UNATTRIBUTED"}]}));
        h.t.push_json(200, success());
        let revoked =
            h.es.revoke_credit_line(&WabaId::new(WABA), &h.vault)
                .await
                .unwrap();
        assert_eq!(revoked, [AllocationConfigId::new(ALLOCATION)]);
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 2);
        assert_request(
            &reqs[1],
            &Method::DELETE,
            &format!("/v25.0/{ALLOCATION}"),
            &[],
            SYSTEM_TOKEN,
        );
        assert_eq!(h.t.remaining(), 0);

        // Nothing found and nothing stored: nothing to revoke.
        h.vault
            .store(&StoredBusinessToken::new("W2", AccessToken::new(TOKEN)).business_id(BUSINESS))
            .await
            .unwrap();
        h.t.push_json(200, json!({"data": []}));
        let revoked =
            h.es.revoke_credit_line(&WabaId::new("W2"), &h.vault)
                .await
                .unwrap();
        assert!(revoked.is_empty());
        assert_eq!(h.t.requests().len(), 3);
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn revocation_without_a_stored_owner_uses_the_stored_allocation() {
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault
            .store(
                &StoredBusinessToken::new(WABA, AccessToken::new(TOKEN))
                    .allocation_config_id(ALLOCATION),
            )
            .await
            .unwrap();
        h.t.push_json(200, success());
        let revoked =
            h.es.revoke_credit_line(&WabaId::new(WABA), &h.vault)
                .await
                .unwrap();
        assert_eq!(revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_request(
            &h.t.last_request().unwrap(),
            &Method::DELETE,
            &format!("/v25.0/{ALLOCATION}"),
            &[],
            SYSTEM_TOKEN,
        );
        h.vault
            .store(&StoredBusinessToken::new("BARE", AccessToken::new(TOKEN)))
            .await
            .unwrap();
        assert!(matches!(
            h.es.revoke_credit_line(&WabaId::new("BARE"), &h.vault).await,
            Err(Error::Validation(v)) if v.field == "business_id"
        ));
        assert_eq!(h.t.requests().len(), 1);
        assert_eq!(h.t.remaining(), 0);
    }

    #[test]
    fn debug_never_shows_the_system_token() {
        let h = harness(CreditSharing::ShareAndAttach);
        let text = format!("{:?} {:?}", h.es, h.es.partner());
        assert!(text.contains(SYSTEM_USER), "vacuous: {text}");
        assert!(!text.contains(SYSTEM_TOKEN), "{text}");
    }
}
