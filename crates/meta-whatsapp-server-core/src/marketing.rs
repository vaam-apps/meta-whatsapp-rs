//! The Marketing Messages API, max price and Click-to-WhatsApp welcome
//! message sequences (docs/design/server.md, section 4.2; roadmap M5h),
//! as services an API adapter calls, with the tenant's WABA token, over
//! the library's `client::marketing`:
//!
//! | Service | Operation | Graph calls |
//! | --- | --- | --- |
//! | [`MarketingSends`] (a number) | [`MarketingSends::send`] | `POST /{pn}/marketing_messages` |
//! | [`Marketing`] (a WABA) | [`Marketing::onboarding`] | `GET /{waba}?fields=marketing_messages_onboarding_status`, `GET /{waba}?fields=owner_business_info` |
//! | | [`Marketing::cloud_api_marketing`], [`Marketing::set_cloud_api_marketing`] | `GET /{waba}?fields=disable_marketing_messages_on_cloud_api`, `POST /{waba}` |
//! | | [`Marketing::duplicate_template`] | the source's lookup in the WABA's templates, then `POST /{waba}/message_templates` |
//! | | [`Marketing::reach_estimate`] | `GET /{waba}/reachestimate` |
//! | | [`Marketing::template_max_price`], [`Marketing::set_template_max_price`], [`Marketing::clear_template_max_price`] | the template's lookup, then `GET`, `POST /{template}` |
//! | | [`Marketing::create_sequence`], [`Marketing::sequences`], [`Marketing::sequence`], [`Marketing::update_sequence`], [`Marketing::delete_sequence`] | `POST`, `GET`, `DELETE /{waba}/welcome_message_sequences` |
//! | [`MaxPriceAgreements`] (an operator) | [`MaxPriceAgreements::sign`] | `POST /{business}/max_price_agreements` |
//!
//! Pages: `marketing-messages/send-marketing-messages`,
//! `marketing-messages/onboarding`, `marketing-messages/pricing`,
//! `marketing-messages/pricing/enroll-max-price`,
//! `marketing-messages/pricing/duplicate-templates`,
//! `ctwa/welcome-message-sequences`.
//!
//! **A template or a sequence is its WABA's, or it does not exist** (the
//! design's M5.2; D233). A template id is looked for as every operation
//! on one template id looks for it ([`crate::templates::Templates::get`]:
//! its name, then the WABA's own templates of that name) before it is
//! duplicated or its max price is read or changed; a sequence id is
//! looked for through the WABA's own edge
//! (`GET /{waba}/welcome_message_sequences?sequence_id=…`, which answers
//! the sequence or nothing). Another WABA's id, or none, is `404
//! not_found`, without Meta's code or text, and the change is never
//! sent. A catalog a template names in a send is checked as the send
//! union checks it ([`crate::messages`]).
//!
//! **The max price agreement is a legal act** (AGENTS.md, Legal acts;
//! `OPEN_QUESTIONS.md` #45; design D231). The library's only signer is
//! `MarketingBusiness::sign_max_price_agreement`; [`MaxPriceAgreements`]
//! is the only caller of it in the service, an operator's explicit act
//! under an admin key, off until the deployment enables the legal acts
//! (`WA_SERVER_LEGAL_ACTS`; [`crate::number_settings::is_legal_admin_act`]),
//! audited. No tenant operation signs it: an agreement missing is Meta's
//! refusal, returned as it is (the duplicate's `403`, a max price's
//! `100`), never answered by signing. The signer's name and email are
//! personal data: they are in no log, no audit event, no `Debug` and no
//! answer, and a failed call answers Meta's code and the service's kind,
//! never Meta's text (which quotes the email).
//!
//! **Bounds.** Meta's documented limits are the library's (a max price of
//! 1 to `i32::MAX`, 240 country multipliers in (0, 10]). Where the pages
//! state none, the service's own, named at the constants below.
//!
//! The services admit the capabilities they open on first. A failed Graph
//! call is told to the [`Observer`], a `190` marks the WABA's numbers
//! `reconnect_required`, and Meta's `details` are kept, except where a
//! call carries what the caller wrote (a send's parameters, a welcome
//! message) or the signer.

use std::collections::BTreeMap;
use std::fmt;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::marketing::{
    BidStrategy, DateInterval, DuplicateTemplate, MAX_COUNTRY_MULTIPLIER, MAX_COUNTRY_MULTIPLIERS,
    MarketingOptions, MaxPrice, MaxPriceAgreementSigner, ProductPolicy, ReachEstimateQuery,
    WelcomeMessage, WelcomeSequence, WelcomeSequenceUpdate,
};
use meta_whatsapp_rs::client::messages::{MessageContent, OutboundMessage};
use meta_whatsapp_rs::client::templates::TemplateMessage;
use meta_whatsapp_rs::core::ids::{BusinessId, TemplateId, WelcomeSequenceId, is_numeric_id};
use meta_whatsapp_rs::core::recipient::Recipient;
use meta_whatsapp_rs::core::secret::AccessToken;
use serde_json::Value;

use crate::audit::{
    MarketingFacts, Outcome, Subject, marketing_audit, max_price_agreement_audit,
};
use crate::authz::{
    AdminCaller, Authorizer, Caller, OwnedNumber, OwnedWaba, object_failed,
};
use crate::error::{ErrorCode, ServiceError};
use crate::messages::{
    Accepted, KeysOnly, MessageType, RecipientObject, SendMessage, admit_named_object, message,
};
use crate::number_settings::is_legal_admin_act;
use crate::observe::Observer;
use crate::operation::AdminOperation;
use crate::request::{graph_id, meta_object, typed_graph_id, wire};
use crate::templates::{TemplateCache, Templates, template_name};

/// The audit action of [`Marketing::duplicate_template`].
pub const TEMPLATE_DUPLICATED: &str = "marketing_template_duplicated";
/// The audit action of [`Marketing::set_template_max_price`].
pub const MAX_PRICE_SET: &str = "template_max_price_set";
/// The audit action of [`Marketing::clear_template_max_price`].
pub const MAX_PRICE_CLEARED: &str = "template_max_price_cleared";
/// The audit action of [`Marketing::set_cloud_api_marketing`].
pub const CLOUD_API_MARKETING_SET: &str = "cloud_api_marketing_set";
/// The audit action of [`Marketing::create_sequence`].
pub const SEQUENCE_CREATED: &str = "welcome_sequence_created";
/// The audit action of [`Marketing::update_sequence`].
pub const SEQUENCE_UPDATED: &str = "welcome_sequence_updated";
/// The audit action of [`Marketing::delete_sequence`].
pub const SEQUENCE_DELETED: &str = "welcome_sequence_deleted";
/// The audit action of [`MaxPriceAgreements::sign`].
pub const MAX_PRICE_AGREEMENT_SIGNED: &str = "max_price_agreement_signed";

/// The most countries a reach estimate may name: the service's own, the
/// number of countries a max price may carry multipliers for
/// ([`MAX_COUNTRY_MULTIPLIERS`]); Meta states none.
pub const MAX_REACH_COUNTRIES: usize = MAX_COUNTRY_MULTIPLIERS;
/// The longest sequence name, in characters: the service's own (the
/// guide states none).
pub const SEQUENCE_NAME_MAX_CHARS: usize = 128;
/// The longest welcome text, prefilled message or quick reply, in
/// characters: the service's own (the guide states none).
pub const WELCOME_TEXT_MAX_CHARS: usize = 1024;
/// The most quick replies (ice breakers) a sequence carries: the
/// service's own (the guide shows three).
pub const WELCOME_ICE_BREAKERS_MAX: usize = 20;
/// The most sequences one listing asks for: the service's own.
pub const SEQUENCES_LIMIT_MAX: u32 = 100;
/// The longest signer's name, in characters: the service's own (Meta
/// states none).
pub const SIGNER_NAME_MAX_CHARS: usize = 256;
/// The longest signer's email, in characters: the service's own, the
/// longest address there is (RFC 5321).
pub const SIGNER_EMAIL_MAX_CHARS: usize = 320;

// ─── The request's checks ────────────────────────────────────────────────

/// A template id from a path or a body: Meta's numeric id.
///
/// # Errors
///
/// `422 invalid_request` on `field`.
pub fn template_id(field: &'static str, id: &str) -> Result<TemplateId, ServiceError> {
    typed_graph_id(field, id)
}

/// A welcome message sequence id from a path: digits, a leading zero
/// allowed (the guide's list shows `0139138`), unlike a node id that is
/// compared with a binding.
///
/// # Errors
///
/// `422 invalid_request` on `field`.
pub fn sequence_id(field: &'static str, id: &str) -> Result<WelcomeSequenceId, ServiceError> {
    if is_numeric_id(id) {
        Ok(WelcomeSequenceId::new(id))
    } else {
        Err(ServiceError::invalid(field))
    }
}

/// A max price as a request carries it (`optimization_spec`).
#[derive(Debug, Clone, PartialEq)]
pub struct MaxPriceRequest {
    /// Per 1,000 deliveries, in the WABA currency's smallest unit.
    pub bid_amount: u64,
    /// `LOWEST_COST_WITH_BID_CAP`, the only one Meta supports; absent is
    /// that.
    pub bid_strategy: Option<String>,
    /// Country (two capital letters) to multiplier; absent keeps the
    /// template's map on an update.
    pub bid_country_multiplier_overrides: Option<BTreeMap<String, f64>>,
}

/// The library's [`MaxPrice`] for `request`, every documented limit
/// checked, on the request's names for the fields (`prefix` is the
/// object's path: `optimization_spec.` in a duplication's body, empty in
/// a max price's own).
///
/// # Errors
///
/// `422 invalid_request` on the field at fault, never quoting a value.
pub fn max_price(prefix: &str, request: &MaxPriceRequest) -> Result<MaxPrice, ServiceError> {
    let field = |name: &str| ServiceError::invalid(format!("{prefix}{name}"));
    let bid_amount = u32::try_from(request.bid_amount)
        .ok()
        .filter(|amount| (1..=i32::MAX.unsigned_abs()).contains(amount))
        .ok_or_else(|| field("bid_amount"))?;
    if let Some(strategy) = &request.bid_strategy
        && strategy != "LOWEST_COST_WITH_BID_CAP"
    {
        return Err(field("bid_strategy"));
    }
    let mut price = MaxPrice::new(bid_amount);
    price.bid_strategy = BidStrategy::LowestCostWithBidCap;
    if let Some(overrides) = &request.bid_country_multiplier_overrides {
        const NAME: &str = "bid_country_multiplier_overrides";
        if overrides.len() > MAX_COUNTRY_MULTIPLIERS {
            return Err(field(NAME));
        }
        for (country, multiplier) in overrides {
            let shaped = country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase());
            let ranged =
                multiplier.is_finite() && *multiplier > 0.0 && *multiplier <= MAX_COUNTRY_MULTIPLIER;
            if !(shaped && ranged) {
                return Err(field(NAME));
            }
        }
        price.bid_country_multiplier_overrides = Some(overrides.clone());
    }
    price
        .validate()
        .map_err(|error| ServiceError::invalid(format!("{prefix}{}", leaf(&error.field))))?;
    Ok(price)
}

/// The last segment of the library's field path (`optimization_spec.bid_amount`).
fn leaf(field: &str) -> &str {
    field.rsplit('.').next().unwrap_or(field)
}

/// A duplication as a request carries it.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicationRequest {
    /// The approved template to clone, a template of the WABA.
    pub source_template_id: String,
    /// The duplicate's name.
    pub name: String,
    /// The duplicate's max price.
    pub optimization_spec: MaxPriceRequest,
}

/// The library's duplication for `request`, checked; the source's
/// ownership is [`Marketing::duplicate_template`]'s.
///
/// # Errors
///
/// `422 invalid_request` on `source_template_id`, `name` or
/// `optimization_spec.*`.
pub fn duplication(request: &DuplicationRequest) -> Result<DuplicateTemplate, ServiceError> {
    let source = template_id("source_template_id", &request.source_template_id)?;
    let price = max_price("optimization_spec.", &request.optimization_spec)?;
    let duplicate = DuplicateTemplate::new(source, request.name.clone(), price);
    template_name("name", &duplicate.name)?;
    Ok(duplicate)
}

/// A reach estimate's countries and history, checked.
///
/// # Errors
///
/// `422 invalid_request` on `countries` (none, more than
/// [`MAX_REACH_COUNTRIES`], or one that is not two capital letters) or
/// `date_interval` (not `L1D`, `L7D`, `L14D` or `L28D`).
pub fn reach_query(
    countries: &[String],
    date_interval: &str,
) -> Result<ReachEstimateQuery, ServiceError> {
    if countries.is_empty()
        || countries.len() > MAX_REACH_COUNTRIES
        || countries
            .iter()
            .any(|c| !(c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase())))
    {
        return Err(ServiceError::invalid("countries"));
    }
    let interval = match date_interval {
        "L1D" => DateInterval::LastDay,
        "L7D" => DateInterval::Last7Days,
        "L14D" => DateInterval::Last14Days,
        "L28D" => DateInterval::Last28Days,
        _ => return Err(ServiceError::invalid("date_interval")),
    };
    Ok(ReachEstimateQuery::new(countries.iter().cloned(), interval))
}

fn short_text(field: &str, text: &str, max: usize) -> Result<(), ServiceError> {
    let len = text.chars().count();
    if text.trim().is_empty() || len > max || text.chars().any(is_unsafe_char) {
        return Err(ServiceError::invalid(field));
    }
    Ok(())
}

/// A control character other than a line break, which a welcome message's
/// text may hold.
fn is_unsafe_char(c: char) -> bool {
    c.is_control() && c != '\n'
}

/// A sequence's name, checked.
///
/// # Errors
///
/// `422 invalid_request` on `name`.
pub fn sequence_name(name: &str) -> Result<(), ServiceError> {
    short_text("name", name, SEQUENCE_NAME_MAX_CHARS)?;
    if name.contains('\n') {
        return Err(ServiceError::invalid("name"));
    }
    Ok(())
}

/// A welcome message (`welcome_message_sequence`) read from Meta's JSON
/// and checked: a key the library would drop is refused on its path, the
/// text, the prefilled message and the quick replies bounded
/// ([`WELCOME_TEXT_MAX_CHARS`], [`WELCOME_ICE_BREAKERS_MAX`]), and at
/// least one part present.
///
/// # Errors
///
/// `422 invalid_request` on `welcome_message_sequence` or the part at
/// fault, never quoting it.
pub fn welcome_message(value: &Value) -> Result<WelcomeMessage, ServiceError> {
    let message: WelcomeMessage = meta_object("welcome_message_sequence", value)?;
    let part = |name: &str| format!("welcome_message_sequence.{name}");
    if message.text.is_none() && message.autofill_message.is_none() && message.ice_breakers.is_empty()
    {
        return Err(ServiceError::invalid("welcome_message_sequence"));
    }
    if let Some(text) = &message.text {
        short_text(&part("text"), text, WELCOME_TEXT_MAX_CHARS)?;
    }
    if let Some(autofill) = &message.autofill_message {
        short_text(
            &part("autofill_message.content"),
            &autofill.content,
            WELCOME_TEXT_MAX_CHARS,
        )?;
    }
    if message.ice_breakers.len() > WELCOME_ICE_BREAKERS_MAX {
        return Err(ServiceError::invalid(part("ice_breakers")));
    }
    for (i, breaker) in message.ice_breakers.iter().enumerate() {
        short_text(
            &part(&format!("ice_breakers[{i}].title")),
            &breaker.title,
            WELCOME_TEXT_MAX_CHARS,
        )?;
    }
    Ok(message)
}

/// What a sequence's change carries: at least one of the two.
///
/// # Errors
///
/// `422 invalid_request` on `update` (neither), `name` or the message's
/// part.
pub fn sequence_update(
    name: Option<&str>,
    message: Option<&Value>,
) -> Result<WelcomeSequenceUpdate, ServiceError> {
    if name.is_none() && message.is_none() {
        return Err(ServiceError::invalid("update"));
    }
    let mut update = WelcomeSequenceUpdate::new();
    if let Some(name) = name {
        sequence_name(name)?;
        update = update.name(name);
    }
    if let Some(message) = message {
        update = update.message(welcome_message(message)?);
    }
    Ok(update)
}

// ─── The marketing send ──────────────────────────────────────────────────

/// A marketing send as the request carried it. Its `Debug` shows the
/// template's keys only ([`KeysOnly`]): its parameters are a customer's
/// data.
#[derive(Clone, PartialEq)]
pub struct MarketingSend {
    /// Who receives it.
    pub to: RecipientObject,
    /// Meta's template object (`name`, `language`, `components`).
    pub template: Value,
    /// `product_policy`: `CLOUD_API_FALLBACK` or `STRICT`.
    pub product_policy: Option<String>,
    /// `message_activity_sharing`.
    pub message_activity_sharing: Option<bool>,
    /// `bid_spec.per_message_bid_multiplier` (max price beta).
    pub per_message_bid_multiplier: Option<f64>,
}

impl fmt::Debug for MarketingSend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            to,
            template,
            product_policy,
            message_activity_sharing,
            per_message_bid_multiplier,
        } = self;
        f.debug_struct("MarketingSend")
            .field("to", to)
            .field("template", &KeysOnly(&Some(template.clone())))
            .field("product_policy", product_policy)
            .field("message_activity_sharing", message_activity_sharing)
            .field("per_message_bid_multiplier", per_message_bid_multiplier)
            .finish()
    }
}

/// A marketing send, built and checked: what the library sends, and the
/// message the send union's checks read.
#[derive(Debug, Clone)]
pub struct MarketingMessage {
    outbound: OutboundMessage,
    recipient: Recipient,
    template: TemplateMessage,
    options: MarketingOptions,
}

/// The library's marketing send for `request`, checked as the send union
/// checks a template message ([`crate::messages::message`]: the recipient,
/// the template's object, the library's limits), and the options.
///
/// # Errors
///
/// `422 invalid_request` on the field at fault (`to.*`, `template.*`,
/// `product_policy`, `bid_spec.per_message_bid_multiplier`).
pub fn marketing_message(request: MarketingSend) -> Result<MarketingMessage, ServiceError> {
    let MarketingSend {
        to,
        template,
        product_policy,
        message_activity_sharing,
        per_message_bid_multiplier,
    } = request;
    let mut options = MarketingOptions::new();
    if let Some(policy) = product_policy {
        options = options.product_policy(match policy.as_str() {
            "CLOUD_API_FALLBACK" => ProductPolicy::CloudApiFallback,
            "STRICT" => ProductPolicy::Strict,
            _ => return Err(ServiceError::invalid("product_policy")),
        });
    }
    if let Some(share) = message_activity_sharing {
        options = options.message_activity_sharing(share);
    }
    if let Some(multiplier) = per_message_bid_multiplier {
        if !(multiplier.is_finite() && multiplier > 0.0) {
            return Err(ServiceError::invalid("bid_spec.per_message_bid_multiplier"));
        }
        options = options.bid_multiplier(multiplier);
    }
    let mut send = SendMessage::new(to, MessageType::Template);
    send.template = Some(template);
    let outbound = message(send)?;
    let MessageContent::Template(template) = &outbound.content else {
        return Err(ServiceError::internal());
    };
    Ok(MarketingMessage {
        recipient: outbound.recipient.clone(),
        template: template.clone(),
        outbound,
        options,
    })
}

/// The marketing send on a number the caller's tenant owns: an
/// [`OwnedNumber`] its [`Authorizer`] made (ownership and the vault
/// already checked: steps 4 and 5 of [`crate::authz`]), with that
/// number's token. Cheap: a few references.
pub struct MarketingSends<'a> {
    authz: &'a Authorizer,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl fmt::Debug for MarketingSends<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MarketingSends")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> MarketingSends<'a> {
    /// The send on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]).
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, and nothing
    /// is sent.
    pub fn new(authz: &'a Authorizer, number: &'a OwnedNumber) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        Ok(Self {
            authz,
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

    /// Send `message` (built by [`marketing_message`]) through the
    /// Marketing Messages API: the accepted message. A catalog its template
    /// names is checked first to be the number's WABA's. Never sent twice:
    /// a timeout is `may_have_been_sent`, and the library does not replay
    /// it. Meta's texts are in no log and no answer (they may quote a
    /// parameter): its code and the service's kind are.
    ///
    /// # Errors
    ///
    /// `404 not_found`: a catalog that is not the WABA's, nothing sent.
    /// Meta's refusal or failure as the error model maps it
    /// (`403 marketing_not_allowed` for `131055`, `134100`, …).
    pub async fn send(&self, message: &MarketingMessage) -> Result<Accepted, ServiceError> {
        admit_named_object(self.authz, self.number, self.observer, &message.outbound).await?;
        let sent = self
            .number
            .client()
            .marketing(self.number.phone_number_id().clone())
            .send(&message.recipient, &message.template, &message.options)
            .await;
        match sent {
            Ok(response) => Accepted::from_response(response),
            Err(error) => {
                let api = self
                    .number
                    .failed_withholding_texts(self.authz, &error)
                    .await;
                if error.graph().is_some() {
                    self.observer.graph_error(api.code());
                }
                Err(api)
            }
        }
    }
}

// ─── The answers ─────────────────────────────────────────────────────────

/// The Terms of Service state of the business that owns a WABA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerBusiness {
    /// The business portfolio's id.
    pub id: Option<String>,
    /// Its name.
    pub name: Option<String>,
    /// `TERM_OF_SERVICE_SIGNED`, `REQUEST_SENT` or `NOT_STARTED`.
    pub terms_status: Option<String>,
    /// When the status was set, as Meta writes it.
    pub terms_time: Option<String>,
}

/// A WABA's Marketing Messages API onboarding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Onboarding {
    /// `ELIGIBLE`, `ONBOARDED`, … as Meta writes it.
    pub onboarding_status: Option<String>,
    /// The owning business and its Terms of Service state.
    pub owner_business: Option<OwnerBusiness>,
}

/// A template duplicated at another max price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicated {
    /// The duplicate's id.
    pub id: String,
    /// Its status (`APPROVED` at once for an approved source).
    pub status: Option<String>,
    /// Its category.
    pub category: Option<String>,
    /// The template it was cloned from.
    pub source_template_id: Option<String>,
}

/// A template's max price.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateMaxPrice {
    /// Per 1,000 deliveries; `None` on a rate-card template.
    pub bid_amount: Option<u64>,
    /// `LOWEST_COST_WITH_BID_CAP`; `None` on a rate-card template.
    pub bid_strategy: Option<String>,
    /// Country to multiplier.
    pub bid_country_multiplier_overrides: BTreeMap<String, f64>,
}

/// A reach estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct Reach {
    /// The WABA's currency.
    pub waba_currency: Option<String>,
    /// One point per max price.
    pub estimates: Vec<ReachPoint>,
}

/// One max price of a [`Reach`].
#[derive(Debug, Clone, PartialEq)]
pub struct ReachPoint {
    /// Per 1,000 deliveries, in the currency's smallest unit.
    pub bid_amount: Option<u64>,
    /// Targeted users.
    pub users: Option<u64>,
    /// Estimated deliveries, lower bound.
    pub deliveries_lower_bound: Option<u64>,
    /// Estimated deliveries, upper bound.
    pub deliveries_upper_bound: Option<u64>,
    /// Estimated cost per 1,000 deliveries, lower bound.
    pub cost_lower_bound: Option<f64>,
    /// Estimated cost per 1,000 deliveries, upper bound.
    pub cost_upper_bound: Option<f64>,
}

/// A welcome message sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequence {
    /// Its id.
    pub sequence_id: String,
    /// Its name.
    pub name: Option<String>,
    /// Its welcome message; `None` when Meta's is in neither shape the
    /// library reads.
    pub welcome_message: Option<WelcomeMessage>,
    /// Whether an ad uses it.
    pub is_used_in_ad: Option<bool>,
}

impl From<WelcomeSequence> for Sequence {
    fn from(sequence: WelcomeSequence) -> Self {
        let welcome_message = sequence.message();
        Self {
            sequence_id: sequence.sequence_id.into_inner(),
            name: sequence.name,
            welcome_message,
            is_used_in_ad: sequence.is_used_in_ad,
        }
    }
}

// ─── The WABA's operations ───────────────────────────────────────────────

/// The marketing operations on a WABA of the caller's tenant: an
/// [`OwnedWaba`] its [`Authorizer`] made from that tenant's binding, for a
/// [`Caller`] it made, both admitted ([`Self::new`]), with the WABA's
/// token. Cheap: a few references.
pub struct Marketing<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    waba: &'a OwnedWaba,
    templates: Templates<'a>,
    observer: &'a dyn Observer,
}

impl fmt::Debug for Marketing<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Marketing")
            .field("caller", self.caller)
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> Marketing<'a> {
    /// The operations on `waba` for `caller`, once `authz` admits both and
    /// `waba` was made from a binding of `caller`'s tenant. `cache` is the
    /// template services' (the lookup of a template reads Meta, never the
    /// cache).
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`.
    /// `waba` another tenant's: `404 not_found`. Either way, before
    /// anything is read or asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
        cache: &'a TemplateCache,
    ) -> Result<Self, ServiceError> {
        // The template services admit the caller and the WABA, and refuse
        // another tenant's WABA as `404`.
        let templates = Templates::new(authz, caller, waba, cache)?;
        Ok(Self {
            authz,
            caller,
            waba,
            templates,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.templates = self.templates.observed_by(observer);
        self.observer = observer;
        self
    }

    /// The error of a failed Graph call, with Meta's `details`.
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.waba.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    /// The error of a failed call that carried what the caller wrote:
    /// Meta's texts in no log and no answer.
    async fn failed_withholding(&self, error: &Error) -> ServiceError {
        let api = self.waba.failed_withholding_texts(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    /// The error of a failed call on an object named by id: Meta refusing
    /// the object is `404`.
    async fn object_failed(&self, error: &Error) -> ServiceError {
        let api = self.waba.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        object_failed(api, error)
    }

    fn audit(&self, action: &'static str, facts: &MarketingFacts<'_>, outcome: Outcome) {
        marketing_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.waba.waba_id().as_str()),
                ..Subject::default()
            },
            facts,
            outcome,
        );
    }

    /// A change: audited when Meta answered, and when it may have gone
    /// through unanswered.
    fn changed(
        &self,
        action: &'static str,
        facts: &MarketingFacts<'_>,
        failure: Option<&ServiceError>,
    ) {
        match failure {
            None => self.audit(action, facts, Outcome::Answered),
            Some(failed) if failed.may_have_been_sent() => {
                self.audit(action, facts, Outcome::Unanswered);
            }
            Some(_) => {}
        }
    }

    fn account(&self) -> meta_whatsapp_rs::client::marketing::MarketingAccount {
        self.waba
            .client()
            .marketing_account(self.waba.waba_id().clone())
    }

    // ─── Onboarding and the Cloud API switch ─────────────────────────────

    /// The WABA's Marketing Messages API onboarding status and its owner
    /// business's Terms of Service state: two reads. Accepting the terms
    /// is not an operation of the service: it is done in WhatsApp Manager
    /// by someone with full control of the portfolio (Meta's
    /// `marketing-messages/onboarding`).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn onboarding(&self) -> Result<Onboarding, ServiceError> {
        let status = match self.account().onboarding_status().await {
            Ok(status) => status,
            Err(error) => return Err(self.failed(&error).await),
        };
        let owner = match self.account().owner_business_info().await {
            Ok(owner) => owner,
            Err(error) => return Err(self.failed(&error).await),
        };
        Ok(Onboarding {
            onboarding_status: wire(status.as_ref()),
            owner_business: owner.map(|owner| {
                let terms = owner.marketing_messages_onboarding_status;
                OwnerBusiness {
                    id: owner.id.map(meta_whatsapp_rs::core::ids::BusinessId::into_inner),
                    name: owner.name,
                    terms_status: terms.as_ref().and_then(|t| wire(Some(&t.status))),
                    terms_time: terms.and_then(|t| t.time),
                }
            }),
        })
    }

    /// Whether marketing templates are blocked on Cloud API `/messages`
    /// (`None`: Meta answers no value; its default is `false`).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn cloud_api_marketing(&self) -> Result<Option<bool>, ServiceError> {
        match self.account().cloud_api_marketing_disabled().await {
            Ok(disabled) => Ok(disabled),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Block (`true`) or allow (`false`) marketing templates on Cloud API
    /// `/messages`. Audited as [`CLOUD_API_MARKETING_SET`]; repeating it
    /// is harmless.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn set_cloud_api_marketing(&self, disabled: bool) -> Result<(), ServiceError> {
        let facts = MarketingFacts {
            disabled: Some(disabled),
            ..MarketingFacts::default()
        };
        match self.account().set_cloud_api_marketing_disabled(disabled).await {
            Ok(()) => {
                self.changed(CLOUD_API_MARKETING_SET, &facts, None);
                Ok(())
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                self.changed(CLOUD_API_MARKETING_SET, &facts, Some(&failed));
                Err(failed)
            }
        }
    }

    // ─── Max price ───────────────────────────────────────────────────────

    /// Duplicate the WABA's approved `MARKETING` template at another max
    /// price, once the source is found among the WABA's own templates
    /// (nothing is sent otherwise). Audited as [`TEMPLATE_DUPLICATED`].
    /// Never signs the max price agreement: a WABA not enrolled is Meta's
    /// `403`, returned as it is. Not replayed after a timeout (a second
    /// duplicate, or a name in use).
    ///
    /// # Errors
    ///
    /// `404 not_found` (the source is not the WABA's); Meta's refusal or
    /// failure, with its `details`.
    pub async fn duplicate_template(
        &self,
        request: &DuplicateTemplate,
    ) -> Result<Duplicated, ServiceError> {
        self.templates.require_owned(&request.source_template_id).await?;
        let source = request.source_template_id.as_str();
        let facts = MarketingFacts {
            template_id: Some(source),
            bid_amount: Some(request.max_price.bid_amount),
            ..MarketingFacts::default()
        };
        match self.account().duplicate_template(request).await {
            Ok(duplicated) => {
                self.templates_changed();
                self.changed(
                    TEMPLATE_DUPLICATED,
                    &MarketingFacts {
                        new_template_id: Some(duplicated.id.as_str()),
                        ..facts
                    },
                    None,
                );
                Ok(Duplicated {
                    id: duplicated.id.as_str().to_owned(),
                    status: wire(duplicated.status.as_ref()),
                    category: wire(duplicated.category.as_ref()),
                    source_template_id: duplicated
                        .source_template_id
                        .map(TemplateId::into_inner),
                })
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                self.changed(TEMPLATE_DUPLICATED, &facts, Some(&failed));
                Err(failed)
            }
        }
    }

    /// The WABA's cached template lists are stale after a new template.
    fn templates_changed(&self) {
        self.templates.invalidate_cache();
    }

    /// Estimated deliveries and costs at several max prices
    /// ([`reach_query`]).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn reach_estimate(&self, query: &ReachEstimateQuery) -> Result<Reach, ServiceError> {
        match self.account().reach_estimate(query).await {
            Ok(reach) => Ok(Reach {
                waba_currency: reach.waba_currency,
                estimates: reach
                    .estimates
                    .into_iter()
                    .map(|point| ReachPoint {
                        bid_amount: point.bid_amount,
                        users: point.users,
                        deliveries_lower_bound: point.deliveries_lower_bound,
                        deliveries_upper_bound: point.deliveries_upper_bound,
                        cost_lower_bound: point.cost_lower_bound,
                        cost_upper_bound: point.cost_upper_bound,
                    })
                    .collect(),
            }),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The template's max price, once it is found among the WABA's own
    /// templates; `None` when Meta returns none.
    ///
    /// # Errors
    ///
    /// `404 not_found` (not the WABA's); Meta's refusal or failure, with
    /// its `details`.
    pub async fn template_max_price(
        &self,
        id: &TemplateId,
    ) -> Result<Option<TemplateMaxPrice>, ServiceError> {
        self.templates.require_owned(id).await?;
        match self.waba.client().marketing_template(id.clone()).max_price().await {
            Ok(spec) => Ok(spec.map(|spec| TemplateMaxPrice {
                bid_amount: spec.bid_amount,
                bid_strategy: wire(spec.bid_strategy.as_ref()),
                bid_country_multiplier_overrides: spec.bid_country_multiplier_overrides,
            })),
            Err(error) => Err(self.object_failed(&error).await),
        }
    }

    /// Set the template's max price, once it is found among the WABA's own
    /// templates. Audited as [`MAX_PRICE_SET`]. Never signs the agreement.
    /// Not replayed after a timeout (each update counts toward the
    /// template's edit limits).
    ///
    /// # Errors
    ///
    /// `404 not_found` (not the WABA's); Meta's refusal or failure, with
    /// its `details`.
    pub async fn set_template_max_price(
        &self,
        id: &TemplateId,
        price: &MaxPrice,
    ) -> Result<(), ServiceError> {
        self.templates.require_owned(id).await?;
        let facts = MarketingFacts {
            template_id: Some(id.as_str()),
            bid_amount: Some(price.bid_amount),
            ..MarketingFacts::default()
        };
        match self
            .waba
            .client()
            .marketing_template(id.clone())
            .set_max_price(price)
            .await
        {
            Ok(()) => {
                self.templates_changed();
                self.changed(MAX_PRICE_SET, &facts, None);
                Ok(())
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                self.changed(MAX_PRICE_SET, &facts, Some(&failed));
                Err(failed)
            }
        }
    }

    /// Return the template to rate-card pricing, once it is found among
    /// the WABA's own templates. Audited as [`MAX_PRICE_CLEARED`].
    ///
    /// # Errors
    ///
    /// As [`Self::set_template_max_price`].
    pub async fn clear_template_max_price(&self, id: &TemplateId) -> Result<(), ServiceError> {
        self.templates.require_owned(id).await?;
        let facts = MarketingFacts {
            template_id: Some(id.as_str()),
            ..MarketingFacts::default()
        };
        match self
            .waba
            .client()
            .marketing_template(id.clone())
            .clear_max_price()
            .await
        {
            Ok(()) => {
                self.templates_changed();
                self.changed(MAX_PRICE_CLEARED, &facts, None);
                Ok(())
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                self.changed(MAX_PRICE_CLEARED, &facts, Some(&failed));
                Err(failed)
            }
        }
    }

    // ─── Welcome message sequences ───────────────────────────────────────

    fn sequences_api(&self) -> meta_whatsapp_rs::client::marketing::WelcomeSequences {
        self.waba
            .client()
            .welcome_sequences(self.waba.waba_id().clone())
    }

    /// Create a sequence ([`sequence_name`], [`welcome_message`]):
    /// its id. Audited as [`SEQUENCE_CREATED`]; not replayed after a
    /// timeout. Meta's texts are in no log and no answer (they may quote
    /// the welcome message).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `name`; Meta's refusal or failure.
    pub async fn create_sequence(
        &self,
        name: &str,
        message: &WelcomeMessage,
    ) -> Result<String, ServiceError> {
        sequence_name(name)?;
        match self.sequences_api().create(name, message).await {
            Ok(created) => {
                let id = created.sequence_id.into_inner();
                self.changed(
                    SEQUENCE_CREATED,
                    &MarketingFacts {
                        sequence_id: Some(&id),
                        ..MarketingFacts::default()
                    },
                    None,
                );
                Ok(id)
            }
            Err(error) => {
                let failed = self.failed_withholding(&error).await;
                self.changed(SEQUENCE_CREATED, &MarketingFacts::default(), Some(&failed));
                Err(failed)
            }
        }
    }

    /// A page of the WABA's sequences (`limit` of them at most, 1 to
    /// [`SEQUENCES_LIMIT_MAX`]; Meta documents no cursor).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `limit`; Meta's refusal or failure, with
    /// its `details`.
    pub async fn sequences(&self, limit: Option<u32>) -> Result<Vec<Sequence>, ServiceError> {
        if limit.is_some_and(|limit| !(1..=SEQUENCES_LIMIT_MAX).contains(&limit)) {
            return Err(ServiceError::invalid("limit"));
        }
        let mut query = meta_whatsapp_rs::client::marketing::ListWelcomeSequences::new();
        if let Some(limit) = limit {
            query = query.limit(limit);
        }
        match self.sequences_api().list(&query).await {
            Ok(list) => Ok(list.into_iter().map(Sequence::from).collect()),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The sequence `id`, looked for through the WABA's own edge
    /// (`sequence_id` filter); `None` when it answers none, or answers
    /// another sequence.
    async fn owned_sequence(&self, id: &WelcomeSequenceId) -> Result<Option<Sequence>, ServiceError> {
        match self.sequences_api().get(id).await {
            Ok(found) => Ok(found
                .filter(|sequence| sequence.sequence_id == *id)
                .map(Sequence::from)),
            Err(error) => Err(self.object_failed(&error).await),
        }
    }

    /// The sequence `id`, if it is the WABA's.
    ///
    /// # Errors
    ///
    /// `404 not_found`, without Meta's code or text: not the WABA's, or
    /// Meta refusing the id. Meta's failure otherwise, with its `details`.
    pub async fn sequence(&self, id: &WelcomeSequenceId) -> Result<Sequence, ServiceError> {
        self.owned_sequence(id)
            .await?
            .ok_or_else(ServiceError::not_found)
    }

    /// Change the sequence `id` (found among the WABA's own first).
    /// Audited as [`SEQUENCE_UPDATED`]. Meta refuses a sequence an active
    /// ad uses (`4027012`).
    ///
    /// # Errors
    ///
    /// `404 not_found`; Meta's refusal or failure.
    pub async fn update_sequence(
        &self,
        id: &WelcomeSequenceId,
        update: &WelcomeSequenceUpdate,
    ) -> Result<(), ServiceError> {
        self.sequence(id).await?;
        let facts = MarketingFacts {
            sequence_id: Some(id.as_str()),
            ..MarketingFacts::default()
        };
        match self.sequences_api().update(id, update).await {
            Ok(()) => {
                self.changed(SEQUENCE_UPDATED, &facts, None);
                Ok(())
            }
            Err(error) => {
                let failed = self.failed_withholding(&error).await;
                self.changed(SEQUENCE_UPDATED, &facts, Some(&failed));
                Err(failed)
            }
        }
    }

    /// Delete the sequence `id` (found among the WABA's own first).
    /// Audited as [`SEQUENCE_DELETED`]. Meta refuses a sequence an active
    /// ad uses (`4027012`).
    ///
    /// # Errors
    ///
    /// `404 not_found`; Meta's refusal or failure, with its `details`.
    pub async fn delete_sequence(&self, id: &WelcomeSequenceId) -> Result<(), ServiceError> {
        self.sequence(id).await?;
        let facts = MarketingFacts {
            sequence_id: Some(id.as_str()),
            ..MarketingFacts::default()
        };
        match self.sequences_api().delete(id).await {
            Ok(()) => {
                self.changed(SEQUENCE_DELETED, &facts, None);
                Ok(())
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                self.changed(SEQUENCE_DELETED, &facts, Some(&failed));
                Err(failed)
            }
        }
    }
}

// ─── The operator's agreement ────────────────────────────────────────────

/// The signer of the max price beta agreement as the request carried it.
/// No `Debug` shows the name or the email.
#[derive(Clone, PartialEq, Eq)]
pub struct Signer {
    /// The person signing, printed on the agreement.
    pub name: String,
    /// Where Meta emails the acceptance link.
    pub email: String,
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signer").finish_non_exhaustive()
    }
}

/// What Meta answered to the agreement, without the signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreementSigned {
    /// `none`, `pending` or `accepted`, as Meta writes it.
    pub status: Option<String>,
}

/// The operator's act of signing Meta's max price beta agreement for a
/// business portfolio (roadmap M5h; design D231): **a legal act**, off
/// until [`Self::with_legal_acts`] (the setting `WA_SERVER_LEGAL_ACTS`),
/// audited under the admin key. Nothing else in the service signs it.
pub struct MaxPriceAgreements<'a> {
    authz: &'a Authorizer,
    admin: &'a AdminCaller,
    observer: &'a dyn Observer,
    legal_acts: bool,
}

impl fmt::Debug for MaxPriceAgreements<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MaxPriceAgreements")
            .field("admin", self.admin)
            .field("legal_acts", &self.legal_acts)
            .finish_non_exhaustive()
    }
}

impl<'a> MaxPriceAgreements<'a> {
    /// The act for `admin`, once `authz` admits it
    /// ([`Authorizer::admit_admin`]). Off until [`Self::with_legal_acts`].
    ///
    /// # Errors
    ///
    /// `admin` another [`Authorizer`] made: `403 forbidden`.
    pub fn new(authz: &'a Authorizer, admin: &'a AdminCaller) -> Result<Self, ServiceError> {
        authz.admit_admin(admin)?;
        Ok(Self {
            authz,
            admin,
            observer: &(),
            legal_acts: false,
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// Whether the deployment enables the legal acts (the setting
    /// `WA_SERVER_LEGAL_ACTS`).
    #[must_use]
    pub fn with_legal_acts(mut self, enabled: bool) -> Self {
        self.legal_acts = enabled;
        self
    }

    /// Sign Meta's max price beta agreement for the business portfolio
    /// `business_id`, with `token` (a system user or business token with
    /// `business_management`, an admin of the portfolio's: used for this
    /// call only, never stored), naming `signer`. Meta emails the signer
    /// an acceptance link and answers `pending`; the business accepts it
    /// there. Exactly one request, never replayed (not after a timeout, a
    /// `429` within 5 minutes of the last email, or any failure).
    /// Audited as [`MAX_PRICE_AGREEMENT_SIGNED`] under the admin key,
    /// when Meta answered or may have.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` with the legal acts off, before the
    /// input is read and before any request; `422 invalid_request` on
    /// `business_id`, `signer_name`, `signer_email` or `token`; Meta's
    /// refusal or failure as its code and the service's kind (a `429` is
    /// `too_many_requests` with Meta's `retry_after`), never Meta's text.
    pub async fn sign(
        &self,
        business_id: &str,
        signer: &Signer,
        token: AccessToken,
    ) -> Result<AgreementSigned, ServiceError> {
        if !self.legal_acts && is_legal_admin_act(AdminOperation::SignMaxPriceAgreement) {
            return Err(ServiceError::new(ErrorCode::FEATURE_NOT_AVAILABLE));
        }
        graph_id("business_id", business_id)?;
        let business = BusinessId::new(business_id)
            .map_err(|_| ServiceError::invalid("business_id"))?;
        short_text("signer_name", &signer.name, SIGNER_NAME_MAX_CHARS)?;
        if signer.name.contains('\n') {
            return Err(ServiceError::invalid("signer_name"));
        }
        short_text("signer_email", &signer.email, SIGNER_EMAIL_MAX_CHARS)?;
        if signer.email.contains('\n') || !signer.email.contains('@') {
            return Err(ServiceError::invalid("signer_email"));
        }
        if token.expose_secret().trim().is_empty() {
            return Err(ServiceError::invalid("token"));
        }
        let result = self
            .authz
            .client()
            .with_token(token)
            .marketing_business(business.clone())
            .sign_max_price_agreement(&MaxPriceAgreementSigner::new(
                signer.name.clone(),
                signer.email.clone(),
            ))
            .await;
        match result {
            Ok(agreement) => {
                max_price_agreement_audit(
                    MAX_PRICE_AGREEMENT_SIGNED,
                    self.admin.key_id(),
                    business.as_str(),
                    Outcome::Answered,
                );
                Ok(AgreementSigned {
                    status: wire(Some(&agreement.status)),
                })
            }
            Err(error) => {
                // The token is the request's: Meta refusing it is the
                // operator's input to fix. Meta's texts quote the signer's
                // email: in no log, no answer.
                let api = ServiceError::from_library_withholding_texts(&error);
                if error.graph().is_some() {
                    self.observer.graph_error(api.code());
                }
                if api.may_have_been_sent() {
                    max_price_agreement_audit(
                        MAX_PRICE_AGREEMENT_SIGNED,
                        self.admin.key_id(),
                        business.as_str(),
                        Outcome::Unanswered,
                    );
                }
                Err(api)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::client::marketing::{AutofillMessage, IceBreaker};
    use serde_json::json;

    use super::*;

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    fn price(amount: u64) -> MaxPriceRequest {
        MaxPriceRequest {
            bid_amount: amount,
            bid_strategy: None,
            bid_country_multiplier_overrides: None,
        }
    }

    /// Meta's limits of a max price, each at its edge: `bid_amount` 1 to
    /// `i32::MAX`, 240 multipliers in (0, 10], two-capital-letter keys, the
    /// one strategy. Decisive: each clause.
    #[test]
    fn a_max_price_is_checked_at_its_edges() {
        assert!(max_price("", &price(1)).is_ok());
        assert!(max_price("", &price(2_147_483_647)).is_ok());
        assert_eq!(field(&max_price("", &price(0)).unwrap_err()), "bid_amount");
        assert_eq!(
            field(&max_price("optimization_spec.", &price(2_147_483_648)).unwrap_err()),
            "optimization_spec.bid_amount"
        );
        let with = |overrides: BTreeMap<String, f64>| MaxPriceRequest {
            bid_country_multiplier_overrides: Some(overrides),
            ..price(5)
        };
        let n = |count: usize| -> BTreeMap<String, f64> {
            (0..count)
                .map(|i| {
                    let code = format!(
                        "{}{}",
                        char::from(b'A' + u8::try_from(i / 26).unwrap()),
                        char::from(b'A' + u8::try_from(i % 26).unwrap())
                    );
                    (code, 1.0)
                })
                .collect()
        };
        assert!(max_price("", &with(n(240))).is_ok());
        assert!(max_price("", &with(n(241))).is_err());
        for bad in [0.0, -1.0, 10.000_1, f64::NAN, f64::INFINITY] {
            let overrides = BTreeMap::from([("IN".to_owned(), bad)]);
            assert_eq!(
                field(&max_price("", &with(overrides)).unwrap_err()),
                "bid_country_multiplier_overrides",
                "{bad}"
            );
        }
        assert!(max_price("", &with(BTreeMap::from([("IN".to_owned(), 10.0)]))).is_ok());
        for key in ["in", "IND", "I", "1N"] {
            let overrides = BTreeMap::from([(key.to_owned(), 1.0)]);
            assert!(max_price("", &with(overrides)).is_err(), "{key}");
        }
        let other = MaxPriceRequest {
            bid_strategy: Some("MANUAL".to_owned()),
            ..price(5)
        };
        assert_eq!(field(&max_price("", &other).unwrap_err()), "bid_strategy");
    }

    /// A reach estimate names at least one country, at most 240, in two
    /// capital letters, and one of Meta's four intervals.
    #[test]
    fn a_reach_estimate_is_checked() {
        let one = vec!["IN".to_owned()];
        assert!(reach_query(&one, "L7D").is_ok());
        assert_eq!(field(&reach_query(&[], "L7D").unwrap_err()), "countries");
        assert_eq!(field(&reach_query(&one, "L30D").unwrap_err()), "date_interval");
        assert!(reach_query(&["in".to_owned()], "L1D").is_err());
        let many = vec!["IN".to_owned(); MAX_REACH_COUNTRIES + 1];
        assert!(reach_query(&many, "L1D").is_err());
    }

    /// A welcome message needs a part, refuses a key the library would
    /// drop and bounds its texts.
    #[test]
    fn a_welcome_message_is_checked() {
        let ok = json!({"text": "Hi", "autofill_message": {"content": "Hello"},
                        "ice_breakers": [{"title": "One"}]});
        let message = welcome_message(&ok).unwrap();
        assert_eq!(
            message,
            WelcomeMessage {
                text: Some("Hi".to_owned()),
                autofill_message: Some(AutofillMessage {
                    content: "Hello".to_owned()
                }),
                ice_breakers: vec![IceBreaker {
                    title: "One".to_owned()
                }],
            }
        );
        assert_eq!(
            field(&welcome_message(&json!({})).unwrap_err()),
            "welcome_message_sequence"
        );
        assert_eq!(
            field(&welcome_message(&json!({"text": "x", "tex": "y"})).unwrap_err()),
            "welcome_message_sequence.tex"
        );
        let long = "x".repeat(WELCOME_TEXT_MAX_CHARS + 1);
        assert_eq!(
            field(&welcome_message(&json!({"text": long})).unwrap_err()),
            "welcome_message_sequence.text"
        );
        let breakers: Vec<_> = (0..=WELCOME_ICE_BREAKERS_MAX)
            .map(|_| json!({"title": "t"}))
            .collect();
        assert_eq!(
            field(&welcome_message(&json!({"ice_breakers": breakers})).unwrap_err()),
            "welcome_message_sequence.ice_breakers"
        );
        assert!(sequence_update(None, None).is_err());
        assert!(sequence_update(Some("n"), None).is_ok());
    }

    /// A sequence id is digits, a leading zero allowed; a template id is
    /// Meta's node id.
    #[test]
    fn ids_are_checked_before_they_become_path_segments() {
        assert!(sequence_id("sequence_id", "0139138").is_ok());
        for bad in ["", "12a", "../1", "1/2"] {
            assert!(sequence_id("sequence_id", bad).is_err(), "{bad}");
        }
        assert!(template_id("t", "1945418102598215").is_ok());
        assert!(template_id("t", "0123").is_err());
    }

    /// The signer shows nothing in `Debug`; the marketing send shows its
    /// template's keys only.
    #[test]
    fn personal_data_stays_out_of_debug() {
        let signer = Signer {
            name: "Jane Doe".to_owned(),
            email: "jane.doe@example.com".to_owned(),
        };
        let shown = format!("{signer:?}");
        assert!(!shown.contains("Jane") && !shown.contains("example.com"), "{shown}");
        let send = MarketingSend {
            to: RecipientObject {
                phone: Some("+16505551234".to_owned()),
                ..RecipientObject::default()
            },
            template: json!({"name": "promo", "components": [{"parameters": [{"text": "SECRET-CODE"}]}]}),
            product_policy: None,
            message_activity_sharing: None,
            per_message_bid_multiplier: None,
        };
        assert!(!format!("{send:?}").contains("SECRET-CODE"));
    }

    /// The options of a send: Meta's two policies, a positive multiplier.
    #[test]
    fn a_sends_options_are_checked() {
        let send = |policy: Option<&str>, multiplier: Option<f64>| MarketingSend {
            to: RecipientObject {
                phone: Some("+16505551234".to_owned()),
                ..RecipientObject::default()
            },
            template: json!({"name": "seasonal_sale_promo", "language": {"code": "en_US"}}),
            product_policy: policy.map(str::to_owned),
            message_activity_sharing: Some(true),
            per_message_bid_multiplier: multiplier,
        };
        assert!(marketing_message(send(Some("STRICT"), Some(1.5))).is_ok());
        assert!(marketing_message(send(Some("CLOUD_API_FALLBACK"), None)).is_ok());
        assert_eq!(
            field(&marketing_message(send(Some("NEVER"), None)).unwrap_err()),
            "product_policy"
        );
        for bad in [0.0, -1.0, f64::NAN] {
            assert_eq!(
                field(&marketing_message(send(None, Some(bad))).unwrap_err()),
                "bid_spec.per_message_bid_multiplier"
            );
        }
    }
}
