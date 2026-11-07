//! Max price (beta): a template's max price, duplicating a template at
//! another max price, and reach estimates.
//!
//! Sources: `marketing-messages/pricing` (`optimization_spec`, its update
//! and reset, the reach estimation tool), `marketing-messages/pricing/duplicate-templates`,
//! `reference/whatsapp-business-account/message-template-api`
//! (`POST /{TEMPLATE_ID}` answers `{"success": true}`).
//!
//! None of these calls signs Meta's max price beta agreement, which a WABA
//! needs before it can use them: that is
//! [`MarketingBusiness::sign_max_price_agreement`](super::MarketingBusiness::sign_max_price_agreement),
//! a call of its own. Without it Meta refuses: a duplicate with HTTP 403, a
//! send with code `100` ("You need to sign the testing legal agreement").

use std::collections::BTreeMap;

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::TemplateId;
use meta_whatsapp_core::open_enum;
use serde::{Deserialize, Deserializer, Serialize};

use super::MarketingAccount;
use crate::Client;
use crate::templates::{TemplateCategory, TemplateStatus};

/// Most entries `bid_country_multiplier_overrides` may hold
/// (`marketing-messages/pricing`: "Up to **240** entries", raised from 50
/// on 2026-10-19).
pub const MAX_COUNTRY_MULTIPLIERS: usize = 240;

/// Largest per-country multiplier (`marketing-messages/pricing`: "greater
/// than `0` and at most `10`").
pub const MAX_COUNTRY_MULTIPLIER: f64 = 10.0;

open_enum! {
    /// `bid_strategy` of an `optimization_spec`.
    pub enum BidStrategy {
        /// The only strategy Meta supports today: charge at most the
        /// `bid_amount`.
        LowestCostWithBidCap => "LOWEST_COST_WITH_BID_CAP",
    }
}

/// A max price to set on a template (`optimization_spec`): the most Meta
/// charges per 1,000 deliveries, with optional per-country multipliers.
///
/// `bid_amount` is in the smallest unit of the WABA's currency (cents for
/// USD, paise for INR), **per 1,000 deliveries**: a max price of ₹0.87 per
/// delivery is `87000` ([`Self::per_delivery`] does the arithmetic). The
/// currency itself is the WABA's, never sent: [`ReachEstimate::waba_currency`]
/// reports it.
///
/// Checked locally before any request ([`Self::validate`]): `bid_amount`
/// from 1 to `i32::MAX` (Meta types it `int32`; a zero cap could deliver
/// nothing, so it is refused), at most [`MAX_COUNTRY_MULTIPLIERS`]
/// overrides, each keyed by two ASCII capital letters (the shape of an ISO
/// 3166-1 alpha-2 code; whether the code exists is Meta's check) and each
/// multiplier greater than 0 and at most [`MAX_COUNTRY_MULTIPLIER`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaxPrice {
    /// Max price per 1,000 deliveries, in the WABA currency's smallest
    /// unit.
    pub bid_amount: u32,
    /// [`BidStrategy::LowestCostWithBidCap`], the only one Meta supports.
    pub bid_strategy: BidStrategy,
    /// Destination country (ISO 3166-1 alpha-2) to the multiplier applied
    /// to `bid_amount` for sends there; unlisted countries use `1.0`.
    ///
    /// On an update, `None` keeps the template's current map, an empty
    /// map removes every override, and a non-empty one replaces the map
    /// whole (Meta does not merge).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid_country_multiplier_overrides: Option<BTreeMap<String, f64>>,
}

impl MaxPrice {
    /// A max price of `bid_amount` per 1,000 deliveries, in the WABA
    /// currency's smallest unit, with [`BidStrategy::LowestCostWithBidCap`].
    pub fn new(bid_amount: u32) -> Self {
        Self {
            bid_amount,
            bid_strategy: BidStrategy::LowestCostWithBidCap,
            bid_country_multiplier_overrides: None,
        }
    }

    /// A max price of `minor_units` per delivery (87 paise, 5 cents),
    /// converted to Meta's per-1,000 `bid_amount` (`87000`, `5000`), as
    /// `marketing-messages/pricing` § "Calculating max price amounts" does.
    /// Refused when the result exceeds `i32::MAX`.
    pub fn per_delivery(minor_units: u32) -> Result<Self, ValidationError> {
        let bid_amount = minor_units
            .checked_mul(1_000)
            .filter(|amount| i32::try_from(*amount).is_ok())
            .ok_or_else(|| {
                ValidationError::new(
                    "optimization_spec.bid_amount",
                    "a per-delivery price this large exceeds int32 per 1,000 deliveries",
                )
            })?;
        Ok(Self::new(bid_amount))
    }

    /// Scale the max price by `multiplier` for sends to `country` (ISO
    /// 3166-1 alpha-2, e.g. `IN`).
    #[must_use]
    pub fn country_multiplier(mut self, country: impl Into<String>, multiplier: f64) -> Self {
        self.bid_country_multiplier_overrides
            .get_or_insert_with(BTreeMap::new)
            .insert(country.into(), multiplier);
        self
    }

    /// Send an empty map: on an update, removes every override.
    #[must_use]
    pub fn clear_country_multipliers(mut self) -> Self {
        self.bid_country_multiplier_overrides = Some(BTreeMap::new());
        self
    }

    /// Check the documented limits (see the type's docs).
    pub fn validate(&self) -> Result<(), ValidationError> {
        const FIELD: &str = "optimization_spec.bid_country_multiplier_overrides";
        if self.bid_amount == 0 || i32::try_from(self.bid_amount).is_err() {
            return Err(ValidationError::new(
                "optimization_spec.bid_amount",
                "must be from 1 to 2147483647 (int32)",
            ));
        }
        let Some(overrides) = &self.bid_country_multiplier_overrides else {
            return Ok(());
        };
        if overrides.len() > MAX_COUNTRY_MULTIPLIERS {
            return Err(ValidationError::new(FIELD, "at most 240 entries"));
        }
        for (country, multiplier) in overrides {
            if !is_alpha2(country) {
                return Err(ValidationError::new(
                    FIELD,
                    "keys must be ISO 3166-1 alpha-2 country codes (two capital letters)",
                ));
            }
            if !(multiplier.is_finite()
                && *multiplier > 0.0
                && *multiplier <= MAX_COUNTRY_MULTIPLIER)
            {
                return Err(ValidationError::new(
                    FIELD,
                    "each multiplier must be greater than 0 and at most 10",
                ));
            }
        }
        Ok(())
    }
}

fn is_alpha2(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase())
}

/// A template's `optimization_spec` as Meta returns it
/// ([`MarketingTemplate::max_price`]).
///
/// Meta reads `bid_country_multiplier_overrides` back as a list of
/// `{"key", "value"}` objects although it is written as a map; both shapes
/// are read into the map here, and an empty list is an empty map.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[non_exhaustive]
pub struct OptimizationSpec {
    /// Max price per 1,000 deliveries; `None` on a rate-card template.
    #[serde(default)]
    pub bid_amount: Option<u64>,
    /// The bid strategy; `None` on a rate-card template.
    #[serde(default)]
    pub bid_strategy: Option<BidStrategy>,
    /// The per-country multipliers.
    #[serde(default, deserialize_with = "overrides_as_map")]
    pub bid_country_multiplier_overrides: BTreeMap<String, f64>,
}

fn overrides_as_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, f64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Entry {
        key: String,
        value: f64,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Overrides {
        List(Vec<Entry>),
        Map(BTreeMap<String, f64>),
    }
    Ok(match Option::<Overrides>::deserialize(deserializer)? {
        None => BTreeMap::new(),
        Some(Overrides::List(entries)) => entries.into_iter().map(|e| (e.key, e.value)).collect(),
        Some(Overrides::Map(map)) => map,
    })
}

/// A request to duplicate an approved `MARKETING` template at another max
/// price ([`MarketingAccount::duplicate_template`]).
///
/// Only `name` and the max price can differ from the source: Meta copies
/// the components, `category` and `language`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DuplicateTemplate {
    /// The approved template to clone, in the same WABA.
    pub source_template_id: TemplateId,
    /// The duplicate's name, unique in the WABA (the template name rules:
    /// lowercase letters, digits and underscores, at most 512 characters).
    pub name: String,
    /// The duplicate's max price (`optimization_spec`). The duplication
    /// page shows only `bid_amount` and `bid_strategy`; country multipliers,
    /// documented for creating and updating a template, are sent when set,
    /// and Meta's answer is the authority on them.
    #[serde(rename = "optimization_spec")]
    pub max_price: MaxPrice,
}

impl DuplicateTemplate {
    /// Clone `source_template_id` as `name` with `max_price`.
    pub fn new(
        source_template_id: impl Into<TemplateId>,
        name: impl Into<String>,
        max_price: MaxPrice,
    ) -> Self {
        Self {
            source_template_id: source_template_id.into(),
            name: name.into(),
            max_price,
        }
    }

    fn validate(&self) -> Result<(), ValidationError> {
        crate::templates::validate::name(&self.name, "name")?;
        self.max_price.validate()
    }
}

/// What [`MarketingAccount::duplicate_template`] answers: the duplicate's
/// id, its status (`APPROVED` at once when the source is approved) and
/// category (the source's), and `source_template_id`, which the guide
/// says links the duplicate back to its source (its example omits it,
/// hence the `Option`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DuplicatedTemplate {
    /// The duplicate's template id.
    pub id: TemplateId,
    /// Its status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TemplateStatus>,
    /// Its category, cloned from the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<TemplateCategory>,
    /// The template it was cloned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_template_id: Option<TemplateId>,
}

open_enum! {
    /// `date_interval` of a reach estimate: how much history the estimate
    /// is computed from.
    pub enum DateInterval {
        /// The last day.
        LastDay => "L1D",
        /// The last 7 days.
        Last7Days => "L7D",
        /// The last 14 days.
        Last14Days => "L14D",
        /// The last 28 days.
        Last28Days => "L28D",
    }
}

/// Query of [`MarketingAccount::reach_estimate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachEstimateQuery {
    /// The destination countries (`targeting_spec.geo_locations.countries`,
    /// e.g. `IN`). At least one.
    pub countries: Vec<String>,
    /// The history the estimate is computed from.
    pub date_interval: DateInterval,
}

impl ReachEstimateQuery {
    /// Estimates for sends to `countries`, from `date_interval` of history.
    pub fn new(
        countries: impl IntoIterator<Item = impl Into<String>>,
        date_interval: DateInterval,
    ) -> Self {
        Self {
            countries: countries.into_iter().map(Into::into).collect(),
            date_interval,
        }
    }
}

/// Estimated deliveries and costs at several max prices
/// (`GET /{WABA_ID}/reachestimate`). Meta's estimates come from history and
/// are for planning only: they guarantee nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReachEstimate {
    /// The WABA's currency (`USD`): the unit of every amount below is its
    /// smallest unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_currency: Option<String>,
    /// One entry per max price.
    #[serde(default)]
    pub estimates: Vec<ReachEstimatePoint>,
}

/// One max price of a [`ReachEstimate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReachEstimatePoint {
    /// Max price per 1,000 deliveries, in the currency's smallest unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_amount: Option<u64>,
    /// Targeted users (fixed at 1,000 during the beta).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub users: Option<u64>,
    /// Lower bound of the estimated deliveries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliveries_lower_bound: Option<u64>,
    /// Upper bound of the estimated deliveries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliveries_upper_bound: Option<u64>,
    /// Lower bound of the estimated average cost per 1,000 deliveries, in
    /// the currency's smallest unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_lower_bound: Option<f64>,
    /// Upper bound of the estimated average cost per 1,000 deliveries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_upper_bound: Option<f64>,
}

impl MarketingAccount {
    /// Duplicate an approved `MARKETING` template at another max price
    /// (`POST /{WABA_ID}/message_templates` with `source_template_id`).
    ///
    /// The duplicate comes back `APPROVED` at once, without review, and
    /// counts toward the WABA's template limit; deleting the source deletes
    /// its duplicates. Meta caps duplicates per source by the business's
    /// trust level (at most 25). Meta answers HTTP 400 for a source that is
    /// not approved, a name in use or a source at its cap, and HTTP 403
    /// when the WABA is not enrolled in max price; that 403 is returned as
    /// is: enrolling is a separate, explicit act.
    ///
    /// Not retried on timeouts: a replay would make a second duplicate or
    /// fail on the name.
    pub async fn duplicate_template(
        &self,
        request: &DuplicateTemplate,
    ) -> Result<DuplicatedTemplate> {
        request.validate()?;
        self.client()
            .post_at(&[self.id().as_str(), "message_templates"])
            .json(request)
            .context("duplicate message template response")
            .send()
            .await
    }

    /// Estimated deliveries and costs at several max prices
    /// (`GET /{WABA_ID}/reachestimate?targeting_spec=…&date_interval=…`).
    /// `targeting_spec` is sent as the serialized JSON Meta requires,
    /// `{"geo_locations":{"countries":[…]}}`.
    pub async fn reach_estimate(&self, query: &ReachEstimateQuery) -> Result<ReachEstimate> {
        if query.countries.is_empty() {
            return Err(ValidationError::new(
                "targeting_spec.geo_locations.countries",
                "at least one country is required",
            )
            .into());
        }
        if query.countries.iter().any(|c| c.trim().is_empty()) {
            return Err(ValidationError::new(
                "targeting_spec.geo_locations.countries",
                "must not hold an empty country",
            )
            .into());
        }
        let targeting_spec = serde_json::json!({
            "geo_locations": { "countries": query.countries },
        });
        self.client()
            .get_at(&[self.id().as_str(), "reachestimate"])
            .query_json("targeting_spec", &targeting_spec)
            .query("date_interval", &query.date_interval)
            .context("reach estimate response")
            .send()
            .await
    }
}

/// The max price of one template. See [`Client::marketing_template`].
#[derive(Debug, Clone)]
pub struct MarketingTemplate {
    client: Client,
    template_id: TemplateId,
}

impl Client {
    /// [`MarketingTemplate`] API for `template_id`: read, set or clear its
    /// max price (`optimization_spec`).
    pub fn marketing_template(&self, template_id: impl Into<TemplateId>) -> MarketingTemplate {
        MarketingTemplate {
            client: self.clone(),
            template_id: template_id.into(),
        }
    }
}

#[derive(Serialize)]
struct SetMaxPrice<'a> {
    optimization_spec: &'a MaxPrice,
}

impl MarketingTemplate {
    /// The id this API is scoped to.
    pub fn id(&self) -> &TemplateId {
        &self.template_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The template's max price (`GET /{TEMPLATE_ID}?fields=optimization_spec`).
    /// `None` when Meta returns no `optimization_spec`; a rate-card
    /// template may also come back with its fields empty.
    pub async fn max_price(&self) -> Result<Option<OptimizationSpec>> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            optimization_spec: Option<OptimizationSpec>,
        }
        let r: Response = self
            .client
            .get_at(&[self.template_id.as_str()])
            .query("fields", "optimization_spec")
            .context("template max price response")
            .send()
            .await?;
        Ok(r.optimization_spec)
    }

    /// Set or change the template's max price (`POST /{TEMPLATE_ID}` with
    /// `optimization_spec`). Switching a rate-card template needs the WABA
    /// enrolled in max price and a template not created for campaign sends.
    /// New bids take up to an hour to apply.
    ///
    /// Not replayed on timeouts: like [`Templates::edit`](crate::templates::Templates::edit),
    /// each update counts toward the template's edit limits (100 per hour
    /// for an approved one).
    pub async fn set_max_price(&self, max_price: &MaxPrice) -> Result<()> {
        max_price.validate()?;
        self.client
            .post_at(&[self.template_id.as_str()])
            .json(&SetMaxPrice {
                optimization_spec: max_price,
            })
            .context("set template max price response")
            .send_success()
            .await
    }

    /// Return the template to rate-card pricing (`POST /{TEMPLATE_ID}` with
    /// `bid_amount` and `bid_strategy` both `null`, and nothing else, as
    /// Meta requires). Clears the country multipliers too. Not replayed on
    /// timeouts, as [`Self::set_max_price`].
    pub async fn clear_max_price(&self) -> Result<()> {
        self.client
            .post_at(&[self.template_id.as_str()])
            .json(&serde_json::json!({
                "optimization_spec": { "bid_strategy": null, "bid_amount": null },
            }))
            .context("clear template max price response")
            .send_success()
            .await
    }
}
