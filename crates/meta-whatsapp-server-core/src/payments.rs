//! Payments (docs/design/server.md, section 4.2; roadmap P2), as services
//! an API adapter calls, with the tenant's WABA token, over the library's
//! `client::payments`:
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`PaymentConfigurations::list`] | `GET /{waba_id}/payment_configurations` |
//! | [`PaymentConfigurations::get`] | `GET /{waba_id}/payment_configuration/{configuration_name}` |
//! | [`PaymentConfigurations::create`] | `POST /{waba_id}/payment_configuration` |
//! | [`PaymentConfigurations::regenerate_oauth_link`] | `POST /{waba_id}/generate_payment_configuration_oauth_link` |
//! | [`PaymentConfigurations::delete`] | `DELETE /{waba_id}/payment_configuration` |
//! | [`Payments::lookup`] | `GET /{waba_id}/payment_configuration/{configuration_name}` (the configuration must be the number's WABA's), then `GET /{pn}/payments/{configuration_name}/{reference_id}` |
//! | [`Payments::refund`] | the same check of `payment_config_id`, then `POST /{pn}/payments_refund` |
//!
//! Pages: `payments/payments-in/onboarding-apis` (the configurations),
//! `payments/payments-in/pg` (the lookup, "Step 5", and refunds, "Step 8").
//! India's alone: Brazil documents neither configurations nor lookups
//! (its payments are the merchant's bank's or PSP's).
//!
//! **A configuration is its WABA's, or it does not exist.** The services
//! open only on a WABA (or a number) of the [`Caller`]'s own tenant (step
//! 4 of the authorization order, [`crate::authz`]); a configuration is
//! named under its WABA's path, so another tenant's never reaches Meta. A
//! lookup and a refund name a configuration under a **number**'s path,
//! and one token may reach several tenants' WABAs (the platform's system
//! user token attached to WABAs of different tenants): the configuration
//! is looked for in the number's own WABA first, and one that is not
//! there (Meta's empty `data`, its refusal of the name, or an answer
//! naming another configuration) is `404 not_found`, without Meta's code
//! or text, before the payment is asked for or refunded. A name is one
//! path segment: one holding `/` or `\` is refused before any request
//! ([`configuration_name`]). Meta's pages do not say whether a lookup or a refund
//! checks the configuration against the number: this check does not rely
//! on it.
//!
//! **The OAuth link is a credential.** It links whatever gateway account
//! opens it to the configuration: it is answered to the caller only,
//! never logged (the library's `PaymentOauthLink` hides it from `Debug`),
//! and an API adapter keeping answers for idempotency keys keeps them
//! without it (a repeat answers the configuration's creation without its
//! link: regenerate one).
//!
//! **Money moves once.** A refund, a configuration's creation and an OAuth
//! link are never replayed by the library after a timeout or a server
//! error (`RetryPolicy`); an idempotent request runs under
//! [`crate::idempotency::run`], and a refund, a creation, a new link and a
//! deletion are the caller's audit events ([`crate::audit::payment_audit`]:
//! `payment_refunded`, `payment_configuration_created`,
//! `payment_oauth_link_regenerated`, `payment_configuration_deleted`),
//! with what reconciles them (a refund's number, order, amount and Meta's
//! refund id), and audited with an `unknown` outcome when they may have
//! gone through unanswered.
//!
//! **No legal act.** Meta refuses these calls with `131005` ("Access
//! denied": `403 permission`) until the WABA accepted the WhatsApp
//! Payments terms of service, through a link in the error's `details`,
//! which the answer carries ([`ServiceError::with_details`]) for the
//! merchant to open. No call of the service accepts them.

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::payments::{
    Amount, CreatePaymentConfiguration, Payment, PaymentConfiguration, PaymentOauthLink,
    PaymentProvider, RefundCreated, RefundRequest, RefundSpeed,
};

use crate::audit::{Outcome, PaymentFacts, Subject, payment_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber, OwnedWaba, object_failed};
use crate::error::ServiceError;
use crate::observe::Observer;

/// Longest configuration name: 60 characters, the longest an order's
/// `configuration_name` takes (`payments/payments-in/pg`), as the
/// library checks it.
pub const CONFIGURATION_NAME_MAX: usize = 60;

/// Longest `reference_id` of an India order: 35 characters
/// (`payments/payments-in/pg`, the parameters object).
pub const REFERENCE_ID_MAX: usize = 35;

/// The audit actions (docs/design/server.md, D58).
const REFUNDED: &str = "payment_refunded";
const CONFIGURATION_CREATED: &str = "payment_configuration_created";
const CONFIGURATION_DELETED: &str = "payment_configuration_deleted";
const OAUTH_LINK_REGENERATED: &str = "payment_oauth_link_regenerated";

/// The providers a configuration may be created for
/// (`payments/payments-in/onboarding-apis`: "Must be one of").
pub const PROVIDERS: [&str; 4] = ["upi_vpa", "razorpay", "payu", "zaakpay"];

// ─── The request's checks ────────────────────────────────────────────────

/// A configuration name, from a path or a body: 1 to
/// [`CONFIGURATION_NAME_MAX`] characters, no control character, no `/` or
/// `\` (a name is one path segment of Meta's, which Meta could read as
/// several once it decodes the library's `%2F`), and not `.` or `..` (a
/// path segment that names its parent), else `422 invalid_request` on
/// `field` (docs/design/server.md, D51).
///
/// # Errors
///
/// As above.
pub fn configuration_name(field: &'static str, name: &str) -> Result<(), ServiceError> {
    let valid = !name.trim().is_empty()
        && name.chars().count() <= CONFIGURATION_NAME_MAX
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
        && name != "."
        && name != "..";
    if valid {
        Ok(())
    } else {
        Err(ServiceError::invalid(field))
    }
}

/// An order's `reference_id`, as India's pages define it: 1 to
/// [`REFERENCE_ID_MAX`] characters among ASCII letters, digits, `_`, `-`
/// and `.`, and not `.` or `..`, else `422 invalid_request` on `field`.
///
/// # Errors
///
/// As above.
pub fn reference_id(field: &'static str, id: &str) -> Result<(), ServiceError> {
    let valid = !id.is_empty()
        && id.len() <= REFERENCE_ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && id != "."
        && id != "..";
    if valid {
        Ok(())
    } else {
        Err(ServiceError::invalid(field))
    }
}

/// A payment configuration to create, as the request carried it
/// (`payments/payments-in/onboarding-apis`, "Create a payment
/// configuration"), free of any API toolkit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewPaymentConfiguration {
    /// Its name, what orders name it by: 1 to 60 characters.
    pub configuration_name: String,
    /// One of [`PROVIDERS`].
    pub provider_name: String,
    /// Merchant category code (`0000`).
    pub merchant_category_code: Option<String>,
    /// Purpose code (`00`).
    pub purpose_code: Option<String>,
    /// The merchant's UPI handle (a `upi_vpa` configuration).
    pub merchant_vpa: Option<String>,
    /// Where the merchant lands once the gateway is linked.
    pub redirect_url: Option<String>,
}

/// The library's configuration for `request`, checked: the name
/// ([`configuration_name`]), the provider (one of [`PROVIDERS`], any
/// case), then the library's own checks (the optional fields not empty).
///
/// # Errors
///
/// `422 invalid_request` on the field at fault, before any request.
pub fn new_configuration(
    request: NewPaymentConfiguration,
) -> Result<CreatePaymentConfiguration, ServiceError> {
    let NewPaymentConfiguration {
        configuration_name: name,
        provider_name,
        merchant_category_code,
        purpose_code,
        merchant_vpa,
        redirect_url,
    } = request;
    configuration_name("configuration_name", &name)?;
    let provider = PROVIDERS
        .iter()
        .find(|p| p.eq_ignore_ascii_case(&provider_name))
        .ok_or_else(|| ServiceError::invalid("provider_name"))?;
    let provider = provider
        .parse::<PaymentProvider>()
        .unwrap_or_else(|never| match never {});
    let mut configuration = CreatePaymentConfiguration::new(name, provider);
    configuration.merchant_category_code = merchant_category_code;
    configuration.purpose_code = purpose_code;
    configuration.merchant_vpa = merchant_vpa;
    configuration.redirect_url = redirect_url;
    configuration
        .validate()
        .map_err(|invalid| ServiceError::invalid(invalid.field))?;
    Ok(configuration)
}

/// An amount's `value` or `offset`: a string, as the refund's page writes
/// it (`"100"`), or a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountPart {
    /// A number.
    Number(u64),
    /// A string of digits, as Meta's page writes it.
    Text(String),
}

impl AmountPart {
    fn value(&self) -> Option<u64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Text(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                s.parse().ok()
            }
            Self::Text(_) => None,
        }
    }
}

/// A refund's `amount`, as the request carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundAmount {
    /// `INR`, the only one the page supports.
    pub currency: String,
    /// The amount times `offset`: a positive integer.
    pub value: AmountPart,
    /// `100` for INR.
    pub offset: AmountPart,
}

/// A refund to ask for, as the request carried it (`payments/payments-in/pg`,
/// "Step 8: Refunds"), free of any API toolkit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundOrder {
    /// The order's `reference_id`.
    pub reference_id: String,
    /// `instant` or `normal`; the gateway decides.
    pub speed: Option<String>,
    /// The payment configuration the order was paid through, by name.
    pub payment_config_id: String,
    /// How much to refund.
    pub amount: RefundAmount,
}

/// The library's refund for `request`, checked: the `reference_id`
/// ([`reference_id`]) and the configuration's name
/// ([`configuration_name`]), the currency (`INR`), the amount's value and
/// offset (digits; the offset 100), the speed, then the library's own
/// checks (a positive amount).
///
/// # Errors
///
/// `422 invalid_request` on the field at fault, before any request.
pub fn refund_request(request: RefundOrder) -> Result<RefundRequest, ServiceError> {
    let RefundOrder {
        reference_id: reference,
        speed,
        payment_config_id,
        amount,
    } = request;
    reference_id("reference_id", &reference)?;
    configuration_name("payment_config_id", &payment_config_id)?;
    if amount.currency != "INR" {
        return Err(ServiceError::invalid("amount.currency"));
    }
    let value = amount
        .value
        .value()
        .ok_or_else(|| ServiceError::invalid("amount.value"))?;
    let offset = amount
        .offset
        .value()
        .and_then(|o| u32::try_from(o).ok())
        .ok_or_else(|| ServiceError::invalid("amount.offset"))?;
    let mut refund = RefundRequest::inr(reference, payment_config_id, Amount::new(value, offset));
    if let Some(speed) = speed {
        let speed = match speed.as_str() {
            "instant" => RefundSpeed::Instant,
            "normal" => RefundSpeed::Normal,
            _ => return Err(ServiceError::invalid("speed")),
        };
        refund = refund.speed(speed);
    }
    refund
        .validate()
        .map_err(|invalid| ServiceError::invalid(invalid.field))?;
    Ok(refund)
}

// ─── The operations ──────────────────────────────────────────────────────

/// The payment configuration operations on a WABA of the caller's tenant:
/// an [`OwnedWaba`] its [`Authorizer`] made from that tenant's binding,
/// for a [`Caller`] it made, both admitted ([`Self::new`]), with the
/// WABA's token. A failed Graph call is told to the [`Observer`], and a
/// `190` marks the WABA's numbers `reconnect_required`
/// ([`OwnedWaba::failed`]). Cheap: a few references.
pub struct PaymentConfigurations<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    waba: &'a OwnedWaba,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for PaymentConfigurations<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaymentConfigurations")
            .field("caller", self.caller)
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> PaymentConfigurations<'a> {
    /// The operations on `waba` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_waba`]) and `waba` was
    /// made from a binding of `caller`'s tenant. Failed Graph calls are
    /// reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`.
    /// `waba` another tenant's: `404 not_found`, the answer for a WABA
    /// that does not exist, logged at `warn`. Either way, before anything
    /// is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!("refused the payment configuration services on another tenant's WABA");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            waba,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The error of a failed Graph call, counted, without Meta's
    /// `details` yet.
    async fn counted(&self, error: &Error) -> ServiceError {
        let api = self.waba.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    /// [`Self::counted`], with Meta's `details` (where the terms of
    /// service's link is, when they are not accepted yet).
    async fn failed(&self, error: &Error) -> ServiceError {
        self.counted(error).await.with_details(error)
    }

    /// The caller's audit event `action` on the configuration `name`
    /// (D58).
    fn audit(&self, action: &'static str, name: &str, outcome: Outcome) {
        payment_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.waba.waba_id().as_str()),
                ..Subject::default()
            },
            &PaymentFacts {
                configuration_name: Some(name),
                ..PaymentFacts::default()
            },
            outcome,
        );
    }

    /// The error of a change that failed, audited as `action` with an
    /// unknown outcome when it may have gone through.
    async fn change_failed(&self, action: &'static str, name: &str, error: &Error) -> ServiceError {
        let failed = self.failed(error).await;
        if failed.may_have_been_sent() {
            self.audit(action, name, Outcome::Unknown);
        }
        failed
    }

    /// Every payment configuration of the WABA (Meta documents no pages).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn list(&self) -> Result<Vec<PaymentConfiguration>, ServiceError> {
        let listed = self
            .waba
            .client()
            .payment_configurations(self.waba.waba_id().clone())
            .list()
            .await;
        match listed {
            Ok(configurations) => Ok(configurations),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The configuration `name` of the WABA.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `configuration_name`, before any request.
    /// `404 not_found`, without Meta's code or text: none of that name in
    /// the WABA (Meta's empty `data`, or its refusal of the name). Meta's
    /// failure otherwise, with its `details`.
    pub async fn get(&self, name: &str) -> Result<PaymentConfiguration, ServiceError> {
        configuration_name("configuration_name", name)?;
        let found = self
            .waba
            .client()
            .payment_configurations(self.waba.waba_id().clone())
            .get(name)
            .await;
        match found {
            Ok(Some(configuration)) => Ok(configuration),
            Ok(None) => Err(ServiceError::not_found()),
            Err(error) => Err(object_failed(self.counted(&error).await, &error)),
        }
    }

    /// Create `configuration` (checked by [`new_configuration`]) in the
    /// WABA: a gateway's answer carries the OAuth link the merchant opens
    /// to link their account; a `upi_vpa` one's has none. Never replayed
    /// by the library after a timeout. The caller's audit event
    /// `payment_configuration_created`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`; `success: false` is `502 upstream`).
    pub async fn create(
        &self,
        configuration: &CreatePaymentConfiguration,
    ) -> Result<PaymentOauthLink, ServiceError> {
        let created = self
            .waba
            .client()
            .payment_configurations(self.waba.waba_id().clone())
            .create(configuration)
            .await;
        match created {
            Ok(link) => {
                self.audit(
                    CONFIGURATION_CREATED,
                    &configuration.configuration_name,
                    Outcome::Answered,
                );
                Ok(link)
            }
            Err(error) => Err(self
                .change_failed(
                    CONFIGURATION_CREATED,
                    &configuration.configuration_name,
                    &error,
                )
                .await),
        }
    }

    /// A new OAuth link for the gateway configuration `name`, landing the
    /// merchant on `redirect_url` once linked. Never replayed by the
    /// library after a timeout. The caller's audit event
    /// `payment_oauth_link_regenerated`.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `configuration_name` or `redirect_url`
    /// (empty), before any request. Meta's refusal or failure, with its
    /// `details`.
    pub async fn regenerate_oauth_link(
        &self,
        name: &str,
        redirect_url: Option<&str>,
    ) -> Result<PaymentOauthLink, ServiceError> {
        configuration_name("configuration_name", name)?;
        if redirect_url.is_some_and(|url| url.trim().is_empty()) {
            return Err(ServiceError::invalid("redirect_url"));
        }
        let link = self
            .waba
            .client()
            .payment_configurations(self.waba.waba_id().clone())
            .regenerate_oauth_link(name, redirect_url)
            .await;
        match link {
            Ok(link) => {
                self.audit(OAUTH_LINK_REGENERATED, name, Outcome::Answered);
                Ok(link)
            }
            Err(error) => Err(self
                .change_failed(OAUTH_LINK_REGENERATED, name, &error)
                .await),
        }
    }

    /// Remove the configuration `name` from the WABA (send no more orders
    /// naming it first: their buyers could not pay). The caller's audit
    /// event `payment_configuration_deleted`.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `configuration_name`, before any request.
    /// Meta's refusal or failure, with its `details`.
    pub async fn delete(&self, name: &str) -> Result<(), ServiceError> {
        configuration_name("configuration_name", name)?;
        let deleted = self
            .waba
            .client()
            .payment_configurations(self.waba.waba_id().clone())
            .delete(name)
            .await;
        match deleted {
            Ok(()) => {
                self.audit(CONFIGURATION_DELETED, name, Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self
                .change_failed(CONFIGURATION_DELETED, name, &error)
                .await),
        }
    }
}

/// The payment operations on a number of the caller's tenant (the lookup,
/// refunds): an [`OwnedNumber`] its [`Authorizer`] made, for a [`Caller`]
/// it made, both admitted ([`Self::new`]), with the number's WABA token.
/// Cheap: a few references.
pub struct Payments<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Payments<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Payments")
            .field("caller", self.caller)
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Payments<'a> {
    /// The operations on `number` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_number`]) and `number`
    /// was made from a binding of `caller`'s tenant. Failed Graph calls are
    /// reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `number` another [`Authorizer`] made: `403 forbidden`.
    /// `number` another tenant's: `404 not_found`, logged at `warn`.
    /// Either way, before anything is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        if number.tenant() != caller.tenant() {
            tracing::warn!("refused the payment services on another tenant's number");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            number,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    async fn counted(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    async fn failed(&self, error: &Error) -> ServiceError {
        self.counted(error).await.with_details(error)
    }

    /// The configuration `name` is one of the number's WABA's, asked of
    /// Meta through the WABA's own path (see the [module](self)).
    ///
    /// # Errors
    ///
    /// `404 not_found`, without Meta's code or text: none of that name in
    /// the WABA. Meta's failure otherwise, with its `details`, and never
    /// `may_have_been_sent`: what the caller asked for (a lookup, a
    /// refund) was not sent, so an `Idempotency-Key` is released, not kept
    /// as a refund that may have been made.
    async fn configuration_of_the_waba(&self, name: &str) -> Result<(), ServiceError> {
        let found = self
            .number
            .client()
            .payment_configurations(self.number.waba_id().clone())
            .get(name)
            .await;
        match found {
            // Meta's answer is the configuration of that very name, or the
            // name is not the WABA's: a match by case, prefix or anything
            // else Meta might do is not trusted.
            Ok(Some(configuration)) if configuration.configuration_name == name => Ok(()),
            Ok(_) => {
                tracing::info!("a payment configuration named is not the number's WABA's");
                Err(ServiceError::not_found())
            }
            Err(error) => {
                Err(object_failed(self.counted(&error).await, &error)
                    .with_may_have_been_sent(false))
            }
        }
    }

    /// The caller's audit event `payment_refunded` (D58): what reconciles
    /// the refund, Meta's answer when there is one.
    fn audit_refund(
        &self,
        refund: &RefundRequest,
        created: Option<&RefundCreated>,
        outcome: Outcome,
    ) {
        payment_audit(
            REFUNDED,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                ..Subject::default()
            },
            &PaymentFacts {
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                configuration_name: Some(&refund.payment_config_id),
                reference_id: Some(&refund.reference_id),
                amount_value: Some(refund.amount.value),
                amount_offset: Some(refund.amount.offset),
                currency: Some(refund.currency.as_str()),
                refund_id: created.map(|c| c.id.as_str()),
                refund_status: created.map(|c| c.status.as_str()),
            },
            outcome,
        );
    }

    /// The payment of the order `reference_id`, sent with the
    /// configuration `configuration_name` (both as the `order_details`
    /// message named them): its status, attempts and refunds, as Meta
    /// asks the merchant to read them rather than trust the webhook alone.
    /// The configuration is looked for in the number's WABA first.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `configuration_name` or `reference_id`,
    /// before any request. `404 not_found`: the configuration is not the
    /// WABA's, and the payment is not asked for. Meta's refusal or
    /// failure, with its `details` (Meta's lookup error is not a Graph
    /// error: `502 upstream`).
    pub async fn lookup(&self, name: &str, reference: &str) -> Result<Vec<Payment>, ServiceError> {
        configuration_name("configuration_name", name)?;
        reference_id("reference_id", reference)?;
        self.configuration_of_the_waba(name).await?;
        let found = self
            .number
            .client()
            .payments(self.number.phone_number_id().clone())
            .lookup(name, reference)
            .await;
        match found {
            Ok(payments) => Ok(payments),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Refund `refund` (checked by [`refund_request`]), once its
    /// configuration is found in the number's WABA. **Never replayed** by
    /// the library after a timeout or a server error: a replay could
    /// refund twice. The amount is not compared with what was paid (look
    /// the payment up first). The caller's audit event `payment_refunded`.
    ///
    /// # Errors
    ///
    /// `404 not_found`: the configuration is not the WABA's, and nothing is
    /// refunded. Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`).
    pub async fn refund(&self, refund: &RefundRequest) -> Result<RefundCreated, ServiceError> {
        self.configuration_of_the_waba(&refund.payment_config_id)
            .await?;
        let refunded = self
            .number
            .client()
            .payments(self.number.phone_number_id().clone())
            .refund(refund)
            .await;
        match refunded {
            Ok(created) => {
                self.audit_refund(refund, Some(&created), Outcome::Answered);
                Ok(created)
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                if failed.may_have_been_sent() {
                    self.audit_refund(refund, None, Outcome::Unknown);
                }
                Err(failed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_reference_ids_are_checked() {
        for good in [
            "test-payment-configuration",
            "shop razorpay",
            &"é".repeat(60),
        ] {
            assert!(
                configuration_name("configuration_name", good).is_ok(),
                "{good:?}"
            );
        }
        for bad in [
            "",
            " ",
            ".",
            "..",
            "a\nb",
            "a/b",
            "../x",
            "a\\..\\b",
            &"a".repeat(61),
        ] {
            let refused = configuration_name("configuration_name", bad).unwrap_err();
            assert_eq!(refused.field(), Some("configuration_name"), "{bad:?}");
        }
        for good in [
            "BM345A-12",
            "reference_id_value",
            "a.b_c-1",
            &"9".repeat(35),
        ] {
            assert!(reference_id("reference_id", good).is_ok(), "{good:?}");
        }
        for bad in ["", ".", "..", "a/b", "a b", "ref?a=1", &"9".repeat(36)] {
            let refused = reference_id("reference_id", bad).unwrap_err();
            assert_eq!(refused.field(), Some("reference_id"), "{bad:?}");
        }
    }

    /// `payments-in/onboarding-apis`, "Create a payment configuration":
    /// the gateway example becomes the library's, every field kept; a
    /// provider the page does not list, an empty field or a bad name is
    /// refused on its field. Decisive: the provider list and each check.
    #[test]
    fn a_configuration_is_checked_before_any_request() {
        let example = NewPaymentConfiguration {
            configuration_name: "test-payment-configuration".to_owned(),
            provider_name: "razorpay".to_owned(),
            merchant_category_code: Some("0000".to_owned()),
            purpose_code: Some("00".to_owned()),
            merchant_vpa: None,
            redirect_url: Some("https://test-redirect-url.com".to_owned()),
        };
        let built = new_configuration(example.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&built).unwrap(),
            serde_json::json!({
                "configuration_name": "test-payment-configuration",
                "purpose_code": "00",
                "merchant_category_code": "0000",
                "provider_name": "razorpay",
                "redirect_url": "https://test-redirect-url.com"
            })
        );
        for (field, change) in [
            (
                "provider_name",
                NewPaymentConfiguration {
                    provider_name: "stripe".to_owned(),
                    ..example.clone()
                },
            ),
            (
                "configuration_name",
                NewPaymentConfiguration {
                    configuration_name: "..".to_owned(),
                    ..example.clone()
                },
            ),
            (
                "merchant_vpa",
                NewPaymentConfiguration {
                    merchant_vpa: Some(String::new()),
                    ..example.clone()
                },
            ),
            (
                "redirect_url",
                NewPaymentConfiguration {
                    redirect_url: Some(String::new()),
                    ..example.clone()
                },
            ),
        ] {
            assert_eq!(new_configuration(change).unwrap_err().field(), Some(field));
        }
        let upper = NewPaymentConfiguration {
            provider_name: "RazorPay".to_owned(),
            ..example
        };
        assert_eq!(
            serde_json::to_value(new_configuration(upper).unwrap()).unwrap()["provider_name"],
            "razorpay"
        );
    }

    /// `payments-in/pg`, "Step 8": the example refund, its amount as the
    /// page's strings or as numbers, is the library's; each documented rule
    /// is refused on its field. Decisive: each check.
    #[test]
    fn a_refund_is_checked_before_any_request() {
        let example = RefundOrder {
            reference_id: "reference-id-value".to_owned(),
            speed: Some("normal".to_owned()),
            payment_config_id: "payment-config-id".to_owned(),
            amount: RefundAmount {
                currency: "INR".to_owned(),
                value: AmountPart::Text("100".to_owned()),
                offset: AmountPart::Text("100".to_owned()),
            },
        };
        let expected = serde_json::json!({
            "reference_id": "reference-id-value",
            "speed": "normal",
            "payment_config_id": "payment-config-id",
            "amount": {"currency": "INR", "value": "100", "offset": "100"}
        });
        let built = refund_request(example.clone()).unwrap();
        assert_eq!(serde_json::to_value(&built).unwrap(), expected);
        let numbers = RefundOrder {
            amount: RefundAmount {
                value: AmountPart::Number(100),
                offset: AmountPart::Number(100),
                ..example.amount.clone()
            },
            ..example.clone()
        };
        assert_eq!(
            serde_json::to_value(refund_request(numbers).unwrap()).unwrap(),
            expected
        );
        let amount = |currency: &str, value: AmountPart, offset: AmountPart| RefundOrder {
            amount: RefundAmount {
                currency: currency.to_owned(),
                value,
                offset,
            },
            ..example.clone()
        };
        let text = |s: &str| AmountPart::Text(s.to_owned());
        for (field, change) in [
            (
                "reference_id",
                RefundOrder {
                    reference_id: "a/b".to_owned(),
                    ..example.clone()
                },
            ),
            (
                "payment_config_id",
                RefundOrder {
                    payment_config_id: String::new(),
                    ..example.clone()
                },
            ),
            (
                "speed",
                RefundOrder {
                    speed: Some("fast".to_owned()),
                    ..example.clone()
                },
            ),
            ("amount.currency", amount("BRL", text("100"), text("100"))),
            ("amount.value", amount("INR", text("1.5"), text("100"))),
            ("amount.value", amount("INR", text("-1"), text("100"))),
            ("amount.value", amount("INR", text("0"), text("100"))),
            ("amount.offset", amount("INR", text("100"), text("10"))),
            (
                "amount.offset",
                amount("INR", text("100"), AmountPart::Number(u64::MAX)),
            ),
        ] {
            assert_eq!(
                refund_request(change.clone()).unwrap_err().field(),
                Some(field),
                "{change:?}"
            );
        }
    }
}
