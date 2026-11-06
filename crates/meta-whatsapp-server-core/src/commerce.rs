//! Commerce (docs/design/server.md, section 4.2; roadmap M5g), as services
//! an API adapter calls, with the tenant's WABA token, over the library's
//! `client::commerce`:
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`NumberCommerce::settings`] | `GET /{pn}/whatsapp_commerce_settings` |
//! | [`NumberCommerce::update`] | `POST /{pn}/whatsapp_commerce_settings?is_cart_enabled=…&is_catalog_visible=…` (sent as the library sends it: parameters, no body; replayed on a timeout, as it sets values) |
//! | [`Catalogs::list`] | `GET /{waba_id}/product_catalogs?fields=id,name,product_count,vertical` |
//! | [`Catalogs::get`] | the same list, the catalog found among it |
//! | [`Catalogs::products`] | the same list (the catalog must be among it), then `GET /{catalog_id}/products?fields=…&limit=…[&after=…][&return_only_approved_products=…][&filter=…]` |
//! | [`Catalogs::product_sets`] | the same list, then `GET /{catalog_id}/product_sets?fields=…&limit=…[&after=…]` (roadmap M5g2) |
//! | [`Catalogs::product_set`] | the same list, then `GET /{set_id}?fields=product_catalog{id}` (the set's catalog must be that very catalog), then `GET /{set_id}?fields=…` (roadmap M5g2) |
//! | [`Catalogs::product`] | the same list, then `GET /{catalog_id}/products?fields=…&filter={"product_item_id":{"eq":<id>}}`, the product found among the answer (roadmap M5g2) |
//!
//! Pages: `catalogs/set-commerce-settings`,
//! `reference/whatsapp-business-phone-number/commerce-settings-api`, and
//! Meta's Catalog API for the reads (outside the WhatsApp docs, read for
//! roadmap L17: the library's `client::commerce` lists them).
//!
//! **A catalog is its WABA's, or it does not exist** (the design's M5.2;
//! D141). The services open only on a number or a WABA of the [`Caller`]'s
//! own tenant (step 4 of the authorization order, [`crate::authz`]); a
//! catalog id is Meta's, and one token may reach several tenants' WABAs
//! (the platform's system user token attached to WABAs of different
//! tenants), so a catalog read by its id would answer another tenant's.
//! A catalog is therefore asked for **through its WABA's own edge**
//! (`GET /{waba_id}/product_catalogs`, the check of a message that names a
//! catalog, D102): the id must be among the catalogs it lists (a WABA has
//! one at most), else `404 not_found`, without Meta's code or text, and
//! the catalog's products are never asked for. The catalog's `get` costs
//! no second call (the list holds the same fields).
//!
//! **A product set or a product is its catalog's** (roadmap M5g2; design
//! D281). A set's id and a product's id are Meta's, and one token may reach
//! several tenants' catalogs. The catalog is found among the WABA's own
//! first, as above; then a **set** is asked for its catalog alone
//! (`fields=product_catalog{id}`: the product set page's `product_catalog`
//! field, "product catalog for this product set") and read in full only
//! when that is the catalog named in the path (else `404 not_found`, the
//! full read never made); a **product** is asked for through that catalog's
//! own products edge, filtered on `product_item_id` (a field of the
//! product set page's filter rules), and must be among the answer, with
//! that very id: the edge is the catalog's, so no product of another
//! catalog can be in it. The product item page names a `product_catalog`
//! field without saying what it holds, so it is not relied on.
//!
//! **The products edge's `filter`** is Meta's documented JSON (the product
//! set page's filter rules): parsed into a [`ProductFilter`] and bounded
//! before any request, then written into the query from the parsed value,
//! never from the caller's text.
//!
//! Not offered: the products edge's `error_type`, `bulk_pagination` and
//! `error_priority`, a set's own products edge, and every catalog write
//! (the library wraps none).

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::commerce::{
    CatalogInfo, CommerceSettings, CommerceSettingsUpdate, ListProductSets, ListProducts,
    ProductFilters, ProductInfo, ProductSetInfo,
};
use meta_whatsapp_rs::core::ids::{CatalogId, ProductItemId, ProductSetId};
use serde_json::{Number, Value, json};

use crate::authz::{Authorizer, Caller, OwnedNumber, OwnedWaba, object_failed};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::{graph_id, wire};

/// The catalog fields the service asks of Meta, as the library types them
/// (`product-catalog`, § Fields).
pub const CATALOG_FIELDS: [&str; 4] = ["id", "name", "product_count", "vertical"];

/// The product fields the service asks of Meta, as the library types them
/// (`product-item`, § Fields).
pub const PRODUCT_FIELDS: [&str; 14] = [
    "id",
    "retailer_id",
    "name",
    "description",
    "availability",
    "condition",
    "price",
    "sale_price",
    "currency",
    "image_url",
    "url",
    "brand",
    "review_status",
    "visibility",
];

/// The product set fields the service asks of Meta, as the library types
/// them (`product-set`, § Fields).
pub const PRODUCT_SET_FIELDS: [&str; 5] = ["id", "name", "filter", "retailer_id", "product_count"];

/// Most products one page holds: the service's bound (the Catalog API's
/// reference documents none: D142).
pub const PRODUCTS_PAGE_MAX: u32 = 100;

/// Most product sets one page holds: the service's bound, as the
/// products' (D142).
pub const PRODUCT_SETS_PAGE_MAX: u32 = 100;

/// Longest `filter`, in bytes of the caller's text: the service's bound
/// (the product set page bounds a set's definition at 500 KiB and says
/// nothing of the products edge's parameter; D283).
pub const FILTER_MAX_BYTES: usize = 2048;
/// Most fields a `filter` names: the service's bound (D283).
pub const FILTER_MAX_FIELDS: usize = 10;
/// Most operators one field of a `filter` takes: the service's bound
/// (D283).
pub const FILTER_MAX_OPERATORS: usize = 4;
/// Most values an operator of a `filter` takes (`is_any`, `is_not_any`,
/// `and`, `or`), and longest a text value is, in characters: the service's
/// bounds (D283).
pub const FILTER_MAX_VALUES: usize = 50;
/// Longest text value of a `filter`, in characters (D283).
pub const FILTER_MAX_VALUE_CHARS: usize = 256;

/// The fields a `filter` rule names: the product set page's list
/// (`product-set`, § Filter Rules, Fields), as it prints them.
pub const FILTER_FIELDS: [&str; 111] = [
    "age_group",
    "agent_fb_page_id",
    "app_category",
    "app_subcategory",
    "availability",
    "awards",
    "base_price_amount",
    "body_style",
    "brand",
    "cast",
    "category",
    "city",
    "city_id",
    "city_page_id",
    "color",
    "condition",
    "content_rating",
    "country",
    "currency",
    "custom_label_0",
    "custom_label_1",
    "custom_label_2",
    "custom_label_3",
    "custom_label_4",
    "date_first_on_lot",
    "date_first_on_lot_time",
    "days_on_market",
    "dealer_communication_channel",
    "dealer_id",
    "dealer_name",
    "drivetrain",
    "description",
    "destination_airport",
    "destination_city",
    "destination_id",
    "developer",
    "director",
    "exterior_color",
    "featuring",
    "feed_id",
    "flight_id",
    "fuel_type",
    "furnish_type",
    "gender",
    "genre",
    "home_listing_id",
    "hotel_id",
    "image_tags",
    "interior_color",
    "listing_type",
    "make",
    "margin_level",
    "market_id",
    "material",
    "media_category",
    "mileage_unit",
    "mileage_value",
    "model",
    "name",
    "neighborhood",
    "neighborhood_id",
    "num_baths",
    "num_beds",
    "number_of_raters",
    "num_of_valid_guest_rating",
    "num_rooms",
    "num_units",
    "offer_type",
    "one_way_price",
    "origin_airport",
    "origin_city",
    "pattern",
    "postal_code",
    "postal_codes",
    "price",
    "price_amount",
    "price_change",
    "priority",
    "production_company",
    "product_expiration_time",
    "product_feed_id",
    "product_group_id",
    "product_item_id",
    "product_type",
    "property_type",
    "rating_system",
    "region",
    "region_id",
    "release_date",
    "release_date_time",
    "retailer_id",
    "retailer_product_group_id",
    "sale_price",
    "sale_price_amount",
    "score",
    "size",
    "star_rating_float",
    "state_of_vehicle",
    "tags",
    "title",
    "transmission",
    "trim",
    "url",
    "vehicle_ID",
    "vehicle_registration_plate",
    "vehicle_type",
    "vendor_ID",
    "videos_fetch_status",
    "vin",
    "visibility",
    "year",
];

/// The operators a `filter` rule takes: the product set page's table
/// (`product-set`, § Filter Rules, Operators).
pub const FILTER_OPERATORS: [&str; 13] = [
    "and",
    "contains",
    "or",
    "not_contains",
    "is_any",
    "is_not_any",
    "eq",
    "neq",
    "lt",
    "lte",
    "gt",
    "gte",
    "starts_with",
];

/// Whether `operator` is one of [`FILTER_OPERATORS`], or the
/// case-insensitive form of one that compares text (`i_contains`, the
/// page's example of the `i_*` operators).
fn is_filter_operator(operator: &str) -> bool {
    FILTER_OPERATORS.contains(&operator)
        || operator.strip_prefix("i_").is_some_and(|base| {
            matches!(
                base,
                "contains"
                    | "not_contains"
                    | "is_any"
                    | "is_not_any"
                    | "eq"
                    | "neq"
                    | "starts_with"
            )
        })
}

// ─── The answers ─────────────────────────────────────────────────────────

/// A number's commerce settings (`whatsapp_commerce_settings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Id of the settings object (not the number's).
    pub id: Option<String>,
    /// Whether the shopping cart is enabled.
    pub is_cart_enabled: Option<bool>,
    /// Whether the catalog storefront icon is shown.
    pub is_catalog_visible: Option<bool>,
}

impl From<CommerceSettings> for Settings {
    fn from(settings: CommerceSettings) -> Self {
        let CommerceSettings {
            id,
            is_cart_enabled,
            is_catalog_visible,
        } = settings;
        Self {
            id,
            is_cart_enabled,
            is_catalog_visible,
        }
    }
}

/// What to change in a number's commerce settings: at least one set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsChange {
    /// Enable (`true`) or disable the shopping cart.
    pub is_cart_enabled: Option<bool>,
    /// Show (`true`) or hide the catalog storefront icon.
    pub is_catalog_visible: Option<bool>,
}

impl SettingsChange {
    /// The change, checked before any request.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `update` when nothing is set.
    pub fn checked(self) -> Result<Self, ServiceError> {
        if self.is_cart_enabled.is_none() && self.is_catalog_visible.is_none() {
            return Err(ServiceError::invalid("update"));
        }
        Ok(self)
    }
}

/// A catalog connected to a WABA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: Option<String>,
    /// How many products it holds.
    pub product_count: Option<u64>,
    /// Its kind, as Meta writes it (`commerce`, …).
    pub vertical: Option<String>,
}

impl From<CatalogInfo> for Catalog {
    fn from(catalog: CatalogInfo) -> Self {
        let CatalogInfo {
            id,
            name,
            product_count,
            vertical,
            ..
        } = catalog;
        Self {
            id: id.into_inner(),
            name,
            product_count,
            vertical,
        }
    }
}

/// A product of a catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    /// Its id.
    pub id: String,
    /// The business's own id: what a product message names.
    pub retailer_id: Option<String>,
    /// Its name.
    pub name: Option<String>,
    /// Its description.
    pub description: Option<String>,
    /// `in stock`, `out of stock`, …, as Meta writes it.
    pub availability: Option<String>,
    /// `new`, `used`, …, as Meta writes it.
    pub condition: Option<String>,
    /// Its price, as Meta formats it (`10.00 USD`).
    pub price: Option<String>,
    /// Its sale price, in the same format.
    pub sale_price: Option<String>,
    /// Its currency (ISO 4217).
    pub currency: Option<String>,
    /// Its main image.
    pub image_url: Option<String>,
    /// Its page.
    pub url: Option<String>,
    /// Its brand.
    pub brand: Option<String>,
    /// Its review status, as Meta writes it.
    pub review_status: Option<String>,
    /// Its visibility, as Meta writes it.
    pub visibility: Option<String>,
}

impl From<ProductInfo> for Product {
    fn from(product: ProductInfo) -> Self {
        Self {
            availability: wire(product.availability.as_ref()),
            condition: wire(product.condition.as_ref()),
            review_status: wire(product.review_status.as_ref()),
            visibility: wire(product.visibility.as_ref()),
            id: product.id.into_inner(),
            retailer_id: product.retailer_id,
            name: product.name,
            description: product.description,
            price: product.price,
            sale_price: product.sale_price,
            currency: product.currency,
            image_url: product.image_url,
            url: product.url,
            brand: product.brand,
        }
    }
}

/// A product set of a catalog: a group of related products.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductSet {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: Option<String>,
    /// The rule that defines the set, as the JSON-encoded string Meta
    /// writes it.
    pub filter: Option<String>,
    /// The business's own id for the set.
    pub retailer_id: Option<String>,
    /// How many products it holds.
    pub product_count: Option<u64>,
}

impl From<ProductSetInfo> for ProductSet {
    fn from(set: ProductSetInfo) -> Self {
        let ProductSetInfo {
            id,
            name,
            filter,
            retailer_id,
            product_count,
            ..
        } = set;
        Self {
            id: id.into_inner(),
            name,
            filter,
            retailer_id,
            product_count,
        }
    }
}

/// A page of a catalog's product sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductSetPage {
    /// The sets, in Meta's order.
    pub sets: Vec<ProductSet>,
    /// Meta's cursor for the next page, when Meta says one follows.
    pub next_after: Option<String>,
}

/// What a page of product sets is asked with, checked ([`Self::checked`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProductSetQuery {
    /// Page size, 1 to [`PRODUCT_SETS_PAGE_MAX`] (default: Meta's).
    pub limit: Option<u32>,
    /// Meta's cursor from the previous page.
    pub after: Option<String>,
}

impl ProductSetQuery {
    /// The query, checked before any request.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `limit`.
    pub fn checked(self) -> Result<Self, ServiceError> {
        if self
            .limit
            .is_some_and(|limit| !(1..=PRODUCT_SETS_PAGE_MAX).contains(&limit))
        {
            return Err(ServiceError::invalid("limit"));
        }
        Ok(self)
    }
}

/// A page of a catalog's products.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPage {
    /// The products, in Meta's order.
    pub products: Vec<Product>,
    /// Meta's cursor for the next page, when Meta says one follows.
    pub next_after: Option<String>,
}

/// What a page of products is asked with, checked ([`Self::checked`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProductQuery {
    /// Page size, 1 to [`PRODUCTS_PAGE_MAX`] (default: Meta's).
    pub limit: Option<u32>,
    /// Meta's cursor from the previous page.
    pub after: Option<String>,
    /// Only products approved for WhatsApp.
    pub return_only_approved_products: Option<bool>,
    /// Only the products the rule matches ([`ProductFilter::parse`]).
    pub filter: Option<ProductFilter>,
}

/// A rule of the products edge's `filter`, parsed and bounded: the product
/// set page's filter rules, `{"field": {"operator": value}}`, with the
/// fields of [`FILTER_FIELDS`] and the operators of [`FILTER_OPERATORS`]
/// (and their `i_*` forms), at most [`FILTER_MAX_BYTES`] bytes long,
/// [`FILTER_MAX_FIELDS`] fields, [`FILTER_MAX_OPERATORS`] operators a field
/// and [`FILTER_MAX_VALUES`] values an operator. Only this value is
/// written into the query, serialized again: never the caller's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductFilter(Value);

impl ProductFilter {
    /// The rule `text` spells.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `filter`: too long, not JSON, not an
    /// object of fields each an object of operators, a field or an
    /// operator that is not the page's, or a value out of bounds.
    pub fn parse(text: &str) -> Result<Self, ServiceError> {
        let invalid = || ServiceError::invalid("filter");
        if text.len() > FILTER_MAX_BYTES {
            return Err(invalid());
        }
        let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(text) else {
            return Err(invalid());
        };
        if fields.is_empty() || fields.len() > FILTER_MAX_FIELDS {
            return Err(invalid());
        }
        for (field, rule) in &fields {
            let Value::Object(operators) = rule else {
                return Err(invalid());
            };
            if !FILTER_FIELDS.contains(&field.as_str())
                || operators.is_empty()
                || operators.len() > FILTER_MAX_OPERATORS
            {
                return Err(invalid());
            }
            for (operator, value) in operators {
                if !is_filter_operator(operator) || !filter_value(value, true) {
                    return Err(invalid());
                }
            }
        }
        Ok(Self(Value::Object(fields)))
    }

    /// The rule that matches the product `id` and no other (the
    /// `product_item_id` field, an integer on the page, `eq`).
    fn product_item(id: &ProductItemId) -> Option<Self> {
        let number = id.as_str().parse::<u64>().ok()?;
        Some(Self(
            json!({"product_item_id": {"eq": Value::Number(Number::from(number))}}),
        ))
    }
}

/// Whether `value` is a value of a rule: a text of at most
/// [`FILTER_MAX_VALUE_CHARS`] characters, a number, a boolean, or (when
/// `list` is allowed) a list of at most [`FILTER_MAX_VALUES`] of those.
fn filter_value(value: &Value, list: bool) -> bool {
    match value {
        Value::String(text) => text.chars().count() <= FILTER_MAX_VALUE_CHARS,
        Value::Number(_) | Value::Bool(_) => true,
        Value::Array(items) => {
            list && items.len() <= FILTER_MAX_VALUES
                && items.iter().all(|item| filter_value(item, false))
        }
        Value::Null | Value::Object(_) => false,
    }
}

impl ProductQuery {
    /// The query, checked before any request.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `limit`.
    pub fn checked(self) -> Result<Self, ServiceError> {
        if self
            .limit
            .is_some_and(|limit| !(1..=PRODUCTS_PAGE_MAX).contains(&limit))
        {
            return Err(ServiceError::invalid("limit"));
        }
        Ok(self)
    }
}

// ─── A number's commerce settings ────────────────────────────────────────

/// The commerce settings of a number of the caller's tenant: an
/// [`OwnedNumber`] its [`Authorizer`] made from that tenant's binding, for
/// a [`Caller`] it made, both admitted ([`Self::new`]), with the number's
/// WABA token. Cheap: a few references.
pub struct NumberCommerce<'a> {
    authz: &'a Authorizer,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for NumberCommerce<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NumberCommerce")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> NumberCommerce<'a> {
    /// The services on `number` for `caller`, once `authz` admits both
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
            tracing::warn!("refused the commerce services on another tenant's number");
            return Err(ServiceError::not_found());
        }
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

    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    /// The number's commerce settings.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (an empty `data` is a
    /// decode error: `502 upstream`).
    pub async fn settings(&self) -> Result<Settings, ServiceError> {
        let read = self
            .number
            .client()
            .commerce(self.number.phone_number_id().clone())
            .settings()
            .await;
        match read {
            Ok(settings) => Ok(Settings::from(settings)),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Change the settings with `change` ([`SettingsChange::checked`]). The
    /// library marks the call idempotent (it sets values), so a timeout is
    /// replayed.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn update(&self, change: SettingsChange) -> Result<(), ServiceError> {
        let change = change.checked()?;
        let updated = self
            .number
            .client()
            .commerce(self.number.phone_number_id().clone())
            .update_settings(&CommerceSettingsUpdate {
                is_cart_enabled: change.is_cart_enabled,
                is_catalog_visible: change.is_catalog_visible,
            })
            .await;
        match updated {
            Ok(()) => Ok(()),
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

// ─── A WABA's catalogs ───────────────────────────────────────────────────

/// The catalog reads on a WABA of the caller's tenant: an [`OwnedWaba`]
/// its [`Authorizer`] made, for a [`Caller`] it made ([`Self::new`]), with
/// the WABA's token. Cheap: a few references.
pub struct Catalogs<'a> {
    authz: &'a Authorizer,
    waba: &'a OwnedWaba,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Catalogs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalogs")
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> Catalogs<'a> {
    /// The reads on `waba` for `caller`, as [`NumberCommerce::new`].
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`.
    /// `waba` another tenant's: `404 not_found`, logged at `warn`. Either
    /// way, before anything is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!("refused the catalog services on another tenant's WABA");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
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

    async fn counted(&self, error: &Error) -> ServiceError {
        let api = self.waba.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    /// The catalogs the WABA connects (a hierarchy has one at most), with
    /// [`CATALOG_FIELDS`].
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn list(&self) -> Result<Vec<Catalog>, ServiceError> {
        let listed = self
            .waba
            .client()
            .waba(self.waba.waba_id().clone())
            .product_catalogs(&CATALOG_FIELDS)
            .await;
        match listed {
            Ok(page) => Ok(page.data.into_iter().map(Catalog::from).collect()),
            Err(error) => Err(self.counted(&error).await.with_details(&error)),
        }
    }

    /// The catalog `id`, if it is among the WABA's ([`Self::list`]).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `catalog_id` (not a Meta id), before any
    /// request. `404 not_found` for a catalog not the WABA's. Meta's
    /// failure, with its `details`.
    pub async fn get(&self, id: &str) -> Result<Catalog, ServiceError> {
        graph_id("catalog_id", id)?;
        let id = CatalogId::new(id);
        self.list()
            .await?
            .into_iter()
            .find(|catalog| catalog.id == id.as_str())
            .ok_or_else(|| {
                tracing::warn!("refused a catalog that is not the WABA's");
                ServiceError::not_found()
            })
    }

    /// A page of the products of the catalog `id`, if it is the WABA's.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `catalog_id` or `limit`, before any
    /// request. `404 not_found` for a catalog not the WABA's (its products
    /// are not asked for). Meta's failure, with its `details`; a refusal of
    /// the catalog after it was found is Meta's own.
    pub async fn products(
        &self,
        id: &str,
        query: ProductQuery,
    ) -> Result<ProductPage, ServiceError> {
        graph_id("catalog_id", id)?;
        let query = query.checked()?;
        let catalog = self.get(id).await?;
        let listed = self
            .waba
            .client()
            .catalog(CatalogId::new(catalog.id))
            .products(&ListProducts {
                common: ProductFilters {
                    fields: PRODUCT_FIELDS.iter().map(|&f| f.to_owned()).collect(),
                    filter: query.filter.map(|filter| filter.0),
                    limit: query.limit,
                    ..ProductFilters::default()
                },
                return_only_approved_products: query.return_only_approved_products,
                after: query.after,
                before: None,
            })
            .await;
        match listed {
            Ok(page) => {
                let next_after = page.next_cursor().map(str::to_owned);
                Ok(ProductPage {
                    products: page.data.into_iter().map(Product::from).collect(),
                    next_after,
                })
            }
            Err(error) => Err(object_failed(self.counted(&error).await, &error)),
        }
    }

    /// A page of the product sets of the catalog `id`, if it is the WABA's
    /// (roadmap M5g2): the catalog's own edge, so no set of another catalog
    /// is in it.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `catalog_id` or `limit`, before any
    /// request. `404 not_found` for a catalog not the WABA's (its sets are
    /// not asked for). Meta's failure, with its `details`.
    pub async fn product_sets(
        &self,
        id: &str,
        query: ProductSetQuery,
    ) -> Result<ProductSetPage, ServiceError> {
        graph_id("catalog_id", id)?;
        let query = query.checked()?;
        let catalog = self.get(id).await?;
        let listed = self
            .waba
            .client()
            .catalog(CatalogId::new(catalog.id))
            .product_sets(&ListProductSets {
                fields: PRODUCT_SET_FIELDS.iter().map(|&f| f.to_owned()).collect(),
                limit: query.limit,
                after: query.after,
                ..ListProductSets::default()
            })
            .await;
        match listed {
            Ok(page) => {
                let next_after = page.next_cursor().map(str::to_owned);
                Ok(ProductSetPage {
                    sets: page.data.into_iter().map(ProductSet::from).collect(),
                    next_after,
                })
            }
            Err(error) => Err(object_failed(self.counted(&error).await, &error)),
        }
    }

    /// The product set `set_id`, if its catalog is the catalog `id` and
    /// that is the WABA's (roadmap M5g2): the catalog is found among the
    /// WABA's first, then the set is asked for its catalog alone
    /// (`fields=product_catalog{id}`) and read in full only when that
    /// catalog is `id`.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `catalog_id` or `set_id`, before any
    /// request. `404 not_found` for a catalog not the WABA's, a set that
    /// names another catalog or none, a set Meta refuses, or an answer that
    /// is not the set asked for, with only the lookups sent. Meta's failure
    /// otherwise, with its `details`.
    pub async fn product_set(&self, id: &str, set_id: &str) -> Result<ProductSet, ServiceError> {
        graph_id("catalog_id", id)?;
        graph_id("set_id", set_id)?;
        let catalog = self.get(id).await?;
        let set_id = ProductSetId::new(set_id).map_err(|_| ServiceError::invalid("set_id"))?;
        let sets = self.waba.client().product_set(set_id.clone());
        // The set's catalog alone, before the set is read.
        let owner = match sets.get(&["product_catalog{id}"]).await {
            Ok(owner) => owner,
            Err(error) => return Err(object_failed(self.counted(&error).await, &error)),
        };
        let owned = owner.id == set_id
            && owner
                .product_catalog
                .as_ref()
                .is_some_and(|c| c.id.as_str() == catalog.id);
        if !owned {
            tracing::warn!("refused a product set that is not the WABA's catalog's");
            return Err(ServiceError::not_found());
        }
        match sets.get(&PRODUCT_SET_FIELDS).await {
            Ok(set) if set.id == set_id => Ok(ProductSet::from(set)),
            Ok(_) => Err(ServiceError::not_found()),
            Err(error) => Err(object_failed(self.counted(&error).await, &error)),
        }
    }

    /// The product `product_id`, if it is in the catalog `id` and that is
    /// the WABA's (roadmap M5g2): asked for through the catalog's own
    /// products edge, filtered on `product_item_id`, and found among the
    /// answer with that very id.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `catalog_id` or `product_id`, before any
    /// request. `404 not_found` for a catalog not the WABA's (its products
    /// are not asked for) or a product the catalog's edge does not hold.
    /// Meta's failure otherwise, with its `details`.
    pub async fn product(&self, id: &str, product_id: &str) -> Result<Product, ServiceError> {
        graph_id("catalog_id", id)?;
        graph_id("product_id", product_id)?;
        let product_id =
            ProductItemId::new(product_id).map_err(|_| ServiceError::invalid("product_id"))?;
        let filter = ProductFilter::product_item(&product_id)
            .ok_or_else(|| ServiceError::invalid("product_id"))?;
        let catalog = self.get(id).await?;
        let listed = self
            .waba
            .client()
            .catalog(CatalogId::new(catalog.id))
            .products(&ListProducts {
                common: ProductFilters {
                    fields: PRODUCT_FIELDS.iter().map(|&f| f.to_owned()).collect(),
                    filter: Some(filter.0),
                    ..ProductFilters::default()
                },
                ..ListProducts::default()
            })
            .await;
        match listed {
            // The filter is Meta's to apply; the id is checked here too.
            Ok(page) => page
                .data
                .into_iter()
                .find(|product| product.id == product_id)
                .map(Product::from)
                .ok_or_else(ServiceError::not_found),
            Err(error) => Err(object_failed(self.counted(&error).await, &error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change names a setting; a page's size is bounded by the service's
    /// maximum. Decisive: each clause.
    #[test]
    fn a_change_and_a_page_are_checked_before_any_request() {
        let field = |e: ServiceError| e.field().unwrap_or("?").to_owned();
        assert_eq!(
            field(SettingsChange::default().checked().unwrap_err()),
            "update"
        );
        assert!(
            SettingsChange {
                is_cart_enabled: Some(false),
                is_catalog_visible: None
            }
            .checked()
            .is_ok()
        );
        for limit in [0, PRODUCTS_PAGE_MAX + 1] {
            let query = ProductQuery {
                limit: Some(limit),
                ..ProductQuery::default()
            };
            assert_eq!(field(query.checked().unwrap_err()), "limit", "{limit}");
        }
        for limit in [1, PRODUCTS_PAGE_MAX] {
            let query = ProductQuery {
                limit: Some(limit),
                ..ProductQuery::default()
            };
            assert!(query.checked().is_ok(), "{limit}");
        }
    }

    /// The products edge's `filter` is the product set page's rule syntax
    /// and no more: its own examples parse, and each bound refuses at its
    /// edge. Decisive: each clause of `ProductFilter::parse`.
    #[test]
    fn a_filter_is_the_documented_rule_syntax_and_bounded() {
        for ok in [
            r#"{"product_type": {"i_contains": "shirt"}}"#,
            r#"{"category": {"eq": "Luggage & Bags"}}"#,
            r#"{"color": {"is_any": ["black", "blue", "brown"]}}"#,
            r#"{"priority": {"lt": 3}, "brand": {"neq": "Instagram"}}"#,
            r#"{"availability": {"eq": "in stock"}, "price_amount": {"gte": 100, "lte": 900}}"#,
        ] {
            assert!(ProductFilter::parse(ok).is_ok(), "{ok}");
        }
        let many_fields = format!(
            "{{{}}}",
            FILTER_FIELDS[..=FILTER_MAX_FIELDS]
                .iter()
                .map(|f| format!(r#""{f}": {{"eq": 1}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let at_the_fields_edge = format!(
            "{{{}}}",
            FILTER_FIELDS[..FILTER_MAX_FIELDS]
                .iter()
                .map(|f| format!(r#""{f}": {{"eq": 1}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(ProductFilter::parse(&at_the_fields_edge).is_ok());
        let many_operators = r#"{"price": {"eq": 1, "neq": 2, "lt": 3, "gt": 4, "lte": 5}}"#;
        let many_values = format!(
            r#"{{"color": {{"is_any": [{}]}}}}"#,
            vec![r#""a""#; FILTER_MAX_VALUES + 1].join(",")
        );
        let at_the_values_edge = format!(
            r#"{{"color": {{"is_any": [{}]}}}}"#,
            vec![r#""a""#; FILTER_MAX_VALUES].join(",")
        );
        assert!(ProductFilter::parse(&at_the_values_edge).is_ok());
        let long_value = format!(
            r#"{{"name": {{"eq": "{}"}}}}"#,
            "a".repeat(FILTER_MAX_VALUE_CHARS + 1)
        );
        let at_the_value_edge = format!(
            r#"{{"name": {{"eq": "{}"}}}}"#,
            "a".repeat(FILTER_MAX_VALUE_CHARS)
        );
        assert!(ProductFilter::parse(&at_the_value_edge).is_ok());
        let long_text = format!(
            r#"{{"name": {{"eq": "{}"}}}}"#,
            "a".repeat(FILTER_MAX_BYTES)
        );
        for bad in [
            "",
            "x",
            "[]",
            "null",
            "{}",
            r#"{"name": "shirt"}"#,
            r#"{"name": {}}"#,
            r#"{"no_such_field": {"eq": "a"}}"#,
            r#"{"name": {"matches": "a"}}"#,
            r#"{"name": {"i_lt": 1}}"#,
            r#"{"name": {"eq": null}}"#,
            r#"{"name": {"eq": {"x": 1}}}"#,
            r#"{"name": {"eq": [["a"]]}}"#,
            r#"{"name": {"eq": "a"}} trailing"#,
            many_fields.as_str(),
            many_operators,
            many_values.as_str(),
            long_value.as_str(),
            long_text.as_str(),
        ] {
            let error = ProductFilter::parse(bad).unwrap_err();
            assert_eq!(error.field(), Some("filter"), "{bad}");
        }
    }

    /// A parsed filter holds the parsed value, not the caller's text: a
    /// key given twice is written once, whitespace is gone. Decisive: the
    /// `Value` kept by `ProductFilter::parse`.
    #[test]
    fn a_filter_is_written_from_the_parsed_value() {
        let filter =
            ProductFilter::parse(r#"{ "name" : {"eq":"a"}, "name": {"eq": "b"} }"#).unwrap();
        assert_eq!(filter.0.to_string(), r#"{"name":{"eq":"b"}}"#);
    }

    /// The rule that finds one product names its id as the page types the
    /// field (`product_item_id`, an integer), and a page of sets is
    /// bounded as a page of products. Decisive: `ProductFilter::product_item`
    /// and `ProductSetQuery::checked`.
    #[test]
    fn a_products_rule_is_its_item_id_equal_and_a_page_of_sets_is_bounded() {
        let id = ProductItemId::new("5000000000000001").unwrap();
        assert_eq!(
            ProductFilter::product_item(&id).unwrap().0.to_string(),
            r#"{"product_item_id":{"eq":5000000000000001}}"#
        );
        for limit in [0, PRODUCT_SETS_PAGE_MAX + 1] {
            let sets = ProductSetQuery {
                limit: Some(limit),
                after: None,
            };
            assert_eq!(sets.checked().unwrap_err().field(), Some("limit"));
        }
        for limit in [1, PRODUCT_SETS_PAGE_MAX] {
            let sets = ProductSetQuery {
                limit: Some(limit),
                after: None,
            };
            assert!(sets.checked().is_ok(), "{limit}");
        }
    }
}
