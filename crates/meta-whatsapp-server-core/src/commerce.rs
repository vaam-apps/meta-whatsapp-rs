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
//! | [`Catalogs::products`] | the same list (the catalog must be among it), then `GET /{catalog_id}/products?fields=…&limit=…[&after=…][&return_only_approved_products=…]` |
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
//! Not offered: a catalog's product sets and one product by its id (no
//! page documents that a product set or a product names the catalog it
//! belongs to in a way this check could rely on: roadmap M5g, partial),
//! the products edge's `filter`, `error_type` and `bulk_pagination`, and
//! every catalog write (the library wraps none).

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::commerce::{
    CatalogInfo, CommerceSettings, CommerceSettingsUpdate, ListProducts, ProductFilters,
    ProductInfo,
};
use meta_whatsapp_rs::core::ids::CatalogId;

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

/// Most products one page holds: the service's bound (the Catalog API's
/// reference documents none: D142).
pub const PRODUCTS_PAGE_MAX: u32 = 100;

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
}
