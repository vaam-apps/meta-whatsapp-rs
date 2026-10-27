//! Catalog and product reads, through Meta's Catalog API (the Marketing
//! API's product catalog reference, outside the WhatsApp docs).
//!
//! - [`Waba::product_catalogs`]: the catalog connected to a WhatsApp
//!   Business Account, `GET /{WABA_ID}/product_catalogs`.
//! - [`Catalog`] ([`Client::catalog`]): `GET /{product_catalog_id}`, and
//!   its `products` and `product_sets` edges, paged with cursors.
//! - [`ProductSet`] ([`Client::product_set`]): `GET /{product_set_id}` and
//!   its `products` edge, paged with cursors.
//! - [`Product`] ([`Client::product`]): `GET /{product_item_id}`.
//!
//! Reads only: creating, updating and deleting catalogs, products and
//! product sets, and connecting a catalog to a WABA
//! (`POST /{WABA_ID}/product_catalogs`), are not wrapped.
//!
//! Every id in a path is a Graph object id, a "numeric string" in Meta's
//! field tables, and is checked as digits only before any request, so a
//! stray value (`123/products`, `..`, a retailer id) fails here instead of
//! addressing another object with the tenant's token.
//!
//! # Fields
//!
//! Each read takes the names of the fields to return, passed through as
//! the comma-separated `fields` parameter (empty: no parameter, Meta's
//! defaults: `id` and `name` for a catalog, `id`, `name`, `filter` and
//! `retailer_id` for a product set, `id`, `name` and `retailer_id` for a
//! product). The response types type the fields a WhatsApp integrator
//! needs; any other field requested lands, as Meta sent it, in their
//! `other` map.
//!
//! # Docs
//!
//! Paths relative to `https://developers.facebook.com/documentation/ads-commerce/marketing-api/reference/`
//! (append `.md` for Markdown):
//! - `product-catalog` — the catalog node, its fields and edges.
//! - `product-catalog/products` — the products edge, its `filter`,
//!   `return_only_approved_products`, `error_priority`, `error_type` and
//!   `bulk_pagination` parameters, and the `summary` (`total_count`).
//! - `product-catalog/product_sets` — the product sets edge and its
//!   `ancestor_id`, `has_children`, `parent_id` and `retailer_id` filters.
//! - `product-set` — the product set node, and the filter rules a
//!   product `filter` is written in.
//! - `product-set/products` — a set's products edge.
//!
//! Two pages Meta serves only under the Graph API reference, at
//! `https://developers.facebook.com/docs/`: `marketing-api/reference/product-item`
//! (the product item node and its fields) and
//! `graph-api/reference/whatsapp-business-account/product_catalogs` (the
//! WABA's connected catalog). Cursors follow `graph-api/results`
//! (cursor-based pagination: `after`, `before`, `limit`, and the end of
//! the list is a page without `next`).

use std::fmt;

use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::CatalogId;
use meta_whatsapp_core::paging::Page;
use serde::{Deserialize, Serialize};

use crate::request::{paginate_or_error, reject_cursors};
use crate::templates::macros::string_enum;
use crate::waba::Waba;
use crate::{Client, GraphRequest};

macro_rules! catalog_id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a raw id.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// Borrow the raw id.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Take the raw id.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

catalog_id_type!(
    /// A product item id (`{product-item-id}`): one product, or one variant
    /// of a product, in a catalog. Not its `retailer_id` (the business's
    /// own SKU, which product messages address).
    ProductItemId
);
catalog_id_type!(
    /// A product set id (`{product_set_id}`): a filtered group of a
    /// catalog's products.
    ProductSetId
);

impl Client {
    /// [`Catalog`] API for `catalog_id`.
    pub fn catalog(&self, catalog_id: impl Into<CatalogId>) -> Catalog {
        Catalog {
            client: self.clone(),
            catalog_id: catalog_id.into(),
        }
    }

    /// [`ProductSet`] API for `product_set_id`.
    pub fn product_set(&self, product_set_id: impl Into<ProductSetId>) -> ProductSet {
        ProductSet {
            client: self.clone(),
            product_set_id: product_set_id.into(),
        }
    }

    /// [`Product`] API for `product_item_id`.
    pub fn product(&self, product_item_id: impl Into<ProductItemId>) -> Product {
        Product {
            client: self.clone(),
            product_item_id: product_item_id.into(),
        }
    }
}

impl Waba {
    /// The catalog connected to this WABA:
    /// `GET /{WABA_ID}/product_catalogs`, with `fields` (empty: Meta's
    /// defaults, `id` and `name`).
    ///
    /// Meta answers a `data` list of catalog nodes; a WhatsApp account
    /// hierarchy has at most one catalog (`catalogs/upload-inventory`), so
    /// the page holds zero or one. The reference documents no pagination:
    /// the page is returned as is. The WABA id must be digits only.
    pub async fn product_catalogs(&self, fields: &[&str]) -> Result<Page<CatalogInfo>> {
        let waba_id = graph_id("waba_id", self.id().as_str())?;
        self.client()
            .get_at(&[waba_id, "product_catalogs"])
            .query_opt("fields", fields_param(fields))
            .context("WABA product catalogs response")
            .send()
            .await
    }
}

/// Entry point, see [`Client::catalog`].
#[derive(Debug, Clone)]
pub struct Catalog {
    client: Client,
    catalog_id: CatalogId,
}

impl Catalog {
    /// The id this API is scoped to.
    pub fn id(&self) -> &CatalogId {
        &self.catalog_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn checked_id(&self) -> Result<&str> {
        graph_id("catalog_id", self.catalog_id.as_str())
    }

    /// `GET /{product_catalog_id}` with `fields` (empty: Meta's defaults,
    /// `id` and `name`), e.g. `product_count`, `vertical`.
    pub async fn get(&self, fields: &[&str]) -> Result<CatalogInfo> {
        self.client
            .get_at(&[self.checked_id()?])
            .query_opt("fields", fields_param(fields))
            .context("product catalog response")
            .send()
            .await
    }

    /// One page of the catalog's products:
    /// `GET /{product_catalog_id}/products`.
    ///
    /// The caller pages on with [`Page::next_cursor`] as the next query's
    /// `after`; with [`ProductFilters::summary`] the page's `summary` holds
    /// `total_count`.
    pub async fn products(&self, query: &ListProducts) -> Result<Page<ProductInfo>> {
        self.products_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every product the query matches, following `paging.cursors.after`
    /// until a page comes without `next`.
    ///
    /// The stream manages cursors itself, so `query.after`/`query.before`
    /// must be `None`; otherwise (or on a bad id) the stream yields that
    /// single error.
    pub fn products_stream(
        &self,
        query: &ListProducts,
    ) -> impl Stream<Item = Result<ProductInfo>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.products_request(query)),
        )
    }

    fn products_request(&self, query: &ListProducts) -> Result<GraphRequest> {
        let request = self
            .client
            .get_at(&[self.checked_id()?, "products"])
            .context("catalog products response");
        Ok(query.common.apply(request).query_opt(
            "return_only_approved_products",
            query.return_only_approved_products,
        ))
    }

    /// One page of the catalog's product sets:
    /// `GET /{product_catalog_id}/product_sets`.
    pub async fn product_sets(&self, query: &ListProductSets) -> Result<Page<ProductSetInfo>> {
        self.product_sets_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every product set the query matches, following
    /// `paging.cursors.after` until a page comes without `next`.
    ///
    /// The stream manages cursors itself, so `query.after`/`query.before`
    /// must be `None`; otherwise (or on a bad id) the stream yields that
    /// single error.
    pub fn product_sets_stream(
        &self,
        query: &ListProductSets,
    ) -> impl Stream<Item = Result<ProductSetInfo>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.product_sets_request(query)),
        )
    }

    fn product_sets_request(&self, query: &ListProductSets) -> Result<GraphRequest> {
        let catalog_id = self.checked_id()?;
        let ancestor_id = query
            .ancestor_id
            .as_ref()
            .map(|id| graph_id("ancestor_id", id.as_str()))
            .transpose()?;
        let parent_id = query
            .parent_id
            .as_ref()
            .map(|id| graph_id("parent_id", id.as_str()))
            .transpose()?;
        Ok(self
            .client
            .get_at(&[catalog_id, "product_sets"])
            .query_opt("fields", fields_param_owned(&query.fields))
            .query_opt("ancestor_id", ancestor_id)
            .query_opt("has_children", query.has_children)
            .query_opt("parent_id", parent_id)
            .query_opt("retailer_id", query.retailer_id.as_deref())
            .query_opt("summary", query.summary.then_some("true"))
            .query_opt("limit", query.limit)
            .context("catalog product sets response"))
    }
}

/// Entry point, see [`Client::product_set`].
#[derive(Debug, Clone)]
pub struct ProductSet {
    client: Client,
    product_set_id: ProductSetId,
}

impl ProductSet {
    /// The id this API is scoped to.
    pub fn id(&self) -> &ProductSetId {
        &self.product_set_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn checked_id(&self) -> Result<&str> {
        graph_id("product_set_id", self.product_set_id.as_str())
    }

    /// `GET /{product_set_id}` with `fields` (empty: Meta's defaults, `id`,
    /// `name`, `filter` and `retailer_id`), e.g. `product_count`.
    pub async fn get(&self, fields: &[&str]) -> Result<ProductSetInfo> {
        self.client
            .get_at(&[self.checked_id()?])
            .query_opt("fields", fields_param(fields))
            .context("product set response")
            .send()
            .await
    }

    /// One page of the set's products: `GET /{product_set_id}/products`.
    pub async fn products(&self, query: &ListSetProducts) -> Result<Page<ProductInfo>> {
        self.products_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every product of the set the query matches, following
    /// `paging.cursors.after` until a page comes without `next`.
    ///
    /// The stream manages cursors itself, so `query.after`/`query.before`
    /// must be `None`; otherwise (or on a bad id) the stream yields that
    /// single error.
    pub fn products_stream(
        &self,
        query: &ListSetProducts,
    ) -> impl Stream<Item = Result<ProductInfo>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.products_request(query)),
        )
    }

    fn products_request(&self, query: &ListSetProducts) -> Result<GraphRequest> {
        let request = self
            .client
            .get_at(&[self.checked_id()?, "products"])
            .context("product set products response");
        Ok(query.common.apply(request))
    }
}

/// Entry point, see [`Client::product`].
#[derive(Debug, Clone)]
pub struct Product {
    client: Client,
    product_item_id: ProductItemId,
}

impl Product {
    /// The id this API is scoped to.
    pub fn id(&self) -> &ProductItemId {
        &self.product_item_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `GET /{product_item_id}` with `fields` (empty: Meta's defaults,
    /// `id`, `name` and `retailer_id`), e.g. `price`, `availability`,
    /// `review_status`, `errors`.
    ///
    /// The node's other parameters (`catalog_id`, `image_height`,
    /// `image_width`, `override_country`, `override_language`) are not
    /// sent: the reference names them without saying what they do.
    pub async fn get(&self, fields: &[&str]) -> Result<ProductInfo> {
        let id = graph_id("product_item_id", self.product_item_id.as_str())?;
        self.client
            .get_at(&[id])
            .query_opt("fields", fields_param(fields))
            .context("product item response")
            .send()
            .await
    }
}

/// A Graph object id is a "numeric string": digits only.
fn graph_id<'a>(field: &'static str, id: &'a str) -> Result<&'a str> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ValidationError::new(field, "must be a Graph object id: digits only").into());
    }
    Ok(id)
}

fn fields_param(fields: &[&str]) -> Option<String> {
    (!fields.is_empty()).then(|| fields.join(","))
}

fn fields_param_owned(fields: &[String]) -> Option<String> {
    (!fields.is_empty()).then(|| fields.join(","))
}

/// The parameters a catalog's and a set's `products` edges share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProductFilters {
    /// Fields to return (empty: Meta's defaults, `id`, `name` and
    /// `retailer_id`).
    pub fields: Vec<String>,
    /// `filter`: a rule in the product set filter syntax (`product-set`,
    /// § Filter Rules), sent JSON-encoded, e.g.
    /// `{"name": {"i_contains": "shoe"}}`.
    pub filter: Option<serde_json::Value>,
    /// `error_priority`: only products with errors of this priority.
    pub error_priority: Option<ProductErrorPriority>,
    /// `error_type`: only products affected by this issue type, one of the
    /// values the reference lists (e.g. `IMAGE_RESOLUTION_LOW`), sent as
    /// given.
    pub error_type: Option<String>,
    /// `bulk_pagination`: "used for iterating over the edge in large
    /// chunks".
    pub bulk_pagination: Option<bool>,
    /// Send `summary=true`: the page's `summary` then holds `total_count`,
    /// the number of products the query matches.
    pub summary: bool,
    /// Page size. The reference documents no bounds; Meta may return fewer.
    pub limit: Option<u32>,
}

impl ProductFilters {
    fn apply(&self, request: GraphRequest) -> GraphRequest {
        let mut request = request.query_opt("fields", fields_param_owned(&self.fields));
        if let Some(filter) = &self.filter {
            request = request.query_json("filter", filter);
        }
        request
            .query_opt("error_priority", self.error_priority.as_ref())
            .query_opt("error_type", self.error_type.as_deref())
            .query_opt("bulk_pagination", self.bulk_pagination)
            .query_opt("summary", self.summary.then_some("true"))
            .query_opt("limit", self.limit)
    }
}

/// Query of [`Catalog::products`] and [`Catalog::products_stream`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListProducts {
    /// Fields, filter, summary and page size.
    pub common: ProductFilters,
    /// `return_only_approved_products`: only products approved for
    /// WhatsApp ("only working for WhatsApp capability for now"; Meta's
    /// default is `false`).
    pub return_only_approved_products: Option<bool>,
    /// Cursor from a previous page's `paging.cursors.after`.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

/// Query of [`ProductSet::products`] and [`ProductSet::products_stream`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListSetProducts {
    /// Fields, filter, summary and page size.
    pub common: ProductFilters,
    /// Cursor from a previous page's `paging.cursors.after`.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

/// Query of [`Catalog::product_sets`] and [`Catalog::product_sets_stream`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListProductSets {
    /// Fields to return (empty: Meta's defaults, `id`, `name`, `filter`
    /// and `retailer_id`).
    pub fields: Vec<String>,
    /// `ancestor_id`: only sets descending from this set.
    pub ancestor_id: Option<ProductSetId>,
    /// `has_children`: only sets with (`true`) or without children.
    pub has_children: Option<bool>,
    /// `parent_id`: only direct children of this set (`0` for the root
    /// sets).
    pub parent_id: Option<ProductSetId>,
    /// `retailer_id`: only the set with this retailer id.
    pub retailer_id: Option<String>,
    /// Send `summary=true`: the page's `summary` then holds `total_count`.
    pub summary: bool,
    /// Page size. The reference documents no bounds.
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

/// A product catalog node (`product-catalog`, § Fields). Only `id` is
/// always present; the rest come back when requested (`name` by default).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogInfo {
    /// Catalog id.
    pub id: CatalogId,
    /// Name given by the catalog's creator.
    #[serde(default)]
    pub name: Option<String>,
    /// Total number of products in the catalog.
    #[serde(default)]
    pub product_count: Option<u64>,
    /// Type of catalog (commerce, hotels, …); the reference lists no
    /// values, so it is kept as sent.
    #[serde(default)]
    pub vertical: Option<String>,
    /// Image used for products without one.
    #[serde(default)]
    pub default_image_url: Option<String>,
    /// Images used for auto-generated dynamic items.
    #[serde(default)]
    pub fallback_image_url: Option<Vec<String>>,
    /// Number of feeds the catalog uses.
    #[serde(default)]
    pub feed_count: Option<u64>,
    /// Whether this is a catalog segment.
    #[serde(default)]
    pub is_catalog_segment: Option<bool>,
    /// `is_local_catalog`.
    #[serde(default)]
    pub is_local_catalog: Option<bool>,
    /// Every other field requested (e.g. `business`), as Meta sent it.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// A product set node (`product-set`, § Fields).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProductSetInfo {
    /// Product set id.
    pub id: ProductSetId,
    /// Name given by the set's owner.
    #[serde(default)]
    pub name: Option<String>,
    /// The filter rule defining the set, as the JSON-encoded string Meta
    /// returns.
    #[serde(default)]
    pub filter: Option<String>,
    /// The retailer's id for the set.
    #[serde(default)]
    pub retailer_id: Option<String>,
    /// Number of products in the set.
    #[serde(default)]
    pub product_count: Option<u64>,
    /// URL scraped to create the set.
    #[serde(default)]
    pub auto_creation_url: Option<String>,
    /// The catalog the set belongs to (`product_catalog`).
    #[serde(default)]
    pub product_catalog: Option<CatalogInfo>,
    /// Every other field requested (e.g. `live_metadata`), as Meta sent it.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// A product item node (`product-item`, § Fields): one product, or one
/// variant of a product, in a catalog.
///
/// Prices are strings as Meta formats them (`price`, `sale_price`), never
/// parsed into numbers here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProductInfo {
    /// Product item id.
    pub id: ProductItemId,
    /// The business's own id for the item: what product messages and
    /// templates address as `product_retailer_id`.
    #[serde(default)]
    pub retailer_id: Option<String>,
    /// Name (title) of the item.
    #[serde(default)]
    pub name: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Short description.
    #[serde(default)]
    pub short_description: Option<String>,
    /// Availability.
    #[serde(default)]
    pub availability: Option<ProductAvailability>,
    /// Condition.
    #[serde(default)]
    pub condition: Option<ProductCondition>,
    /// Price, as Meta formats it.
    #[serde(default)]
    pub price: Option<String>,
    /// Sale price, as Meta formats it.
    #[serde(default)]
    pub sale_price: Option<String>,
    /// Currency (ISO 4217).
    #[serde(default)]
    pub currency: Option<String>,
    /// Inventory count.
    #[serde(default)]
    pub inventory: Option<i64>,
    /// Main image URL.
    #[serde(default)]
    pub image_url: Option<String>,
    /// Additional image URLs.
    #[serde(default)]
    pub additional_image_urls: Option<Vec<String>>,
    /// Product page URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Brand.
    #[serde(default)]
    pub brand: Option<String>,
    /// Google product category.
    #[serde(default)]
    pub category: Option<String>,
    /// Retailer-defined category.
    #[serde(default)]
    pub product_type: Option<String>,
    /// Color.
    #[serde(default)]
    pub color: Option<String>,
    /// Size.
    #[serde(default)]
    pub size: Option<String>,
    /// Item group this item is a variant of.
    #[serde(default)]
    pub retailer_product_group_id: Option<String>,
    /// Review status.
    #[serde(default)]
    pub review_status: Option<ProductReviewStatus>,
    /// Visibility.
    #[serde(default)]
    pub visibility: Option<ProductVisibility>,
    /// Issues found with the item (`errors`).
    #[serde(default)]
    pub errors: Option<Vec<ProductError>>,
    /// Every other field requested, as Meta sent it.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// An issue with a product item (`errors`, as the products edge's example
/// shows it).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProductError {
    /// Issue type, e.g. `IMAGE_RESOLUTION_LOW` (one of the many values of
    /// the products edge's `error_type`, kept as sent).
    #[serde(default)]
    pub error_type: Option<String>,
    /// Priority.
    #[serde(default)]
    pub error_priority: Option<ProductErrorPriority>,
    /// Short title.
    #[serde(default)]
    pub title: Option<String>,
    /// What is wrong.
    #[serde(default)]
    pub description: Option<String>,
    /// What to do about it.
    #[serde(default)]
    pub call_to_action: Option<String>,
    /// Any other field, as Meta sent it.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

string_enum! {
    /// `availability` of a product item.
    pub enum ProductAvailability {
        /// `in stock`.
        InStock => "in stock",
        /// `out of stock`.
        OutOfStock => "out of stock",
        /// `preorder`.
        Preorder => "preorder",
        /// `available for order`.
        AvailableForOrder => "available for order",
        /// `discontinued`.
        Discontinued => "discontinued",
        /// `pending`.
        Pending => "pending",
        /// `mark_as_sold`.
        MarkAsSold => "mark_as_sold",
        /// `mark_as_expired`.
        MarkAsExpired => "mark_as_expired",
    }
}

string_enum! {
    /// `condition` of a product item.
    pub enum ProductCondition {
        /// `new`.
        New => "new",
        /// `refurbished`.
        Refurbished => "refurbished",
        /// `used`.
        Used => "used",
        /// `used_like_new`.
        UsedLikeNew => "used_like_new",
        /// `used_good`.
        UsedGood => "used_good",
        /// `used_fair`.
        UsedFair => "used_fair",
        /// `cpo`.
        Cpo => "cpo",
        /// `open_box_new`.
        OpenBoxNew => "open_box_new",
    }
}

string_enum! {
    /// `review_status` of a product item.
    pub enum ProductReviewStatus {
        /// The empty value the reference lists first; it does not say what
        /// it means.
        Empty => "",
        /// `pending`.
        Pending => "pending",
        /// `rejected`.
        Rejected => "rejected",
        /// `approved`.
        Approved => "approved",
        /// `outdated`.
        Outdated => "outdated",
    }
}

string_enum! {
    /// `visibility` of a product item.
    pub enum ProductVisibility {
        /// `staging`.
        Staging => "staging",
        /// `published`.
        Published => "published",
    }
}

string_enum! {
    /// Priority of a product item's issue (`error_priority`), and the
    /// products edges' filter of that name.
    pub enum ProductErrorPriority {
        /// `HIGH`: Meta suggests looking at these first.
        High => "HIGH",
        /// `MEDIUM`.
        Medium => "MEDIUM",
        /// `LOW`.
        Low => "LOW",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::testing::{RecordedRequest, ScriptedTransport};
    use meta_whatsapp_core::{Error, ErrorKind};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::RetryPolicy;

    const CATALOG: &str = "1418424521799234";
    const WABA: &str = "102290129340398";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    /// The decoded query, in order: the exact request, readable.
    fn pairs(req: &RecordedRequest) -> Vec<(String, String)> {
        req.url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn validation_field(err: &Error) -> &str {
        match err {
            Error::Validation(v) => &v.field,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    fn product(id: &str, sku: &str) -> serde_json::Value {
        json!({"id": id, "retailer_id": sku, "name": format!("Product {sku}"), "price": "$9.99"})
    }

    /// A page of the catalog's products, with Meta's `paging` object; the
    /// `next` link carries a token, as Meta's do, to show it is never used.
    fn products_page(items: &[(&str, &str)], cursor: &str, next: bool) -> serde_json::Value {
        let data: Vec<_> = items.iter().map(|(id, sku)| product(id, sku)).collect();
        let mut paging = json!({"cursors": {"before": format!("B-{cursor}"), "after": cursor}});
        if next {
            paging["next"] = json!(format!(
                "https://graph.facebook.com/v25.0/{CATALOG}/products?access_token=LEAKED&limit=2&after={cursor}"
            ));
        }
        json!({"data": data, "paging": paging})
    }

    // ── The decisive test ─────────────────────────────────────────────────

    #[tokio::test]
    async fn products_stream_follows_three_pages_to_the_end_with_no_repeat() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            products_page(&[("1001", "SKU-1"), ("1002", "SKU-2")], "QVFIUa1", true),
        );
        t.push_json(
            200,
            products_page(&[("1003", "SKU-3"), ("1004", "SKU-4")], "QVFIUa2", true),
        );
        // The last page still carries cursors, but no `next`: the end.
        t.push_json(200, products_page(&[("1005", "SKU-5")], "QVFIUa3", false));

        let query = ListProducts {
            common: ProductFilters {
                fields: vec![
                    "id".into(),
                    "retailer_id".into(),
                    "name".into(),
                    "price".into(),
                ],
                limit: Some(2),
                ..ProductFilters::default()
            },
            return_only_approved_products: Some(true),
            ..ListProducts::default()
        };
        let products: Vec<ProductInfo> = client(&t)
            .catalog(CATALOG)
            .products_stream(&query)
            .map(Result::unwrap)
            .collect()
            .await;

        let ids: Vec<&str> = products.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["1001", "1002", "1003", "1004", "1005"]);
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "a product repeated: {ids:?}");
        assert_eq!(products[4].retailer_id.as_deref(), Some("SKU-5"));

        let reqs = t.requests();
        assert_eq!(reqs.len(), 3, "stopped at the page without `next`");
        let base = [
            ("fields", "id,retailer_id,name,price"),
            ("limit", "2"),
            ("return_only_approved_products", "true"),
        ];
        let with_after = |cursor: &'static str| {
            let mut p = base.to_vec();
            p.push(("after", cursor));
            owned(&p)
        };
        assert_eq!(pairs(&reqs[0]), owned(&base));
        assert_eq!(pairs(&reqs[1]), with_after("QVFIUa1"));
        assert_eq!(pairs(&reqs[2]), with_after("QVFIUa2"));
        for req in &reqs {
            assert_eq!(req.method, Method::GET);
            assert_eq!(req.url.host_str(), Some("graph.facebook.com"));
            assert_eq!(req.path(), format!("/v25.0/{CATALOG}/products"));
            assert_eq!(req.bearer(), Some("TOKEN"));
            assert_eq!(
                req.query("access_token"),
                None,
                "next links are not followed"
            );
        }
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn products_stream_crosses_an_empty_page_that_has_next() {
        // `graph-api/results`: a page may be empty but carry a `next` link;
        // stop only when `next` is gone.
        let t = ScriptedTransport::new();
        t.push_json(200, products_page(&[("1001", "SKU-1")], "c1", true));
        t.push_json(200, products_page(&[], "c2", true));
        t.push_json(200, products_page(&[("1002", "SKU-2")], "c3", false));
        let products: Vec<ProductInfo> = client(&t)
            .catalog(CATALOG)
            .products_stream(&ListProducts::default())
            .map(Result::unwrap)
            .collect()
            .await;
        let ids: Vec<&str> = products.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["1001", "1002"]);
        let reqs = t.requests();
        assert_eq!(reqs[0].url.query(), None);
        assert_eq!(reqs[1].url.query(), Some("after=c1"));
        assert_eq!(reqs[2].url.query(), Some("after=c2"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn products_stream_stops_on_a_page_error() {
        let t = ScriptedTransport::new();
        t.push_json(200, products_page(&[("1001", "SKU-1")], "c1", true));
        // graph-api/results: the error at the cursor limit.
        t.push_json(
            400,
            json!({"error": {"message": "(#100) The After Cursor specified exceeds the max limit supported by this endpoint", "type": "OAuthException", "code": 100}}),
        );
        let items: Vec<Result<ProductInfo>> = client(&t)
            .catalog(CATALOG)
            .products_stream(&ListProducts::default())
            .collect()
            .await;
        assert_eq!(items.len(), 2);
        assert!(items[0].is_ok());
        let err = items[1].as_ref().unwrap_err();
        assert_eq!(err.graph().map(|g| g.code), Some(100), "{err:?}");
        assert_eq!(t.remaining(), 0);
    }

    // ── Products ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn products_matches_docs_example() {
        let t = ScriptedTransport::new();
        // product-catalog/products: the example response, its elisions
        // (`....`) and trailing commas removed.
        t.push_json(
            200,
            json!({
                "data": [
                    {"category": "mens_shoes", "name": "Awesome Mens Shoes in Black", "id": "1234"},
                    {
                        "category": "mens_shoes",
                        "name": "Hipster Mens Shoes in Red",
                        "id": "5678",
                        "errors": [{
                            "error_type": "IMAGE_RESOLUTION_LOW",
                            "error_priority": "HIGH",
                            "title": "Missing or invalid images",
                            "description": "The main image of this item cannot be displayed.",
                            "call_to_action": "After you've made changes, save the image under a different link (URL) and provide the new link."
                        }]
                    }
                ],
                "summary": {"total_count": 316}
            }),
        );
        // The example's request: fields, a filter on the name, summary.
        let page = client(&t)
            .catalog(CATALOG)
            .products(&ListProducts {
                common: ProductFilters {
                    fields: vec!["category".into(), "name".into(), "errors".into()],
                    filter: Some(json!({"name": {"i_contains": "shoe"}})),
                    summary: true,
                    ..ProductFilters::default()
                },
                ..ListProducts::default()
            })
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), format!("/v25.0/{CATALOG}/products"));
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            pairs(&req),
            owned(&[
                ("fields", "category,name,errors"),
                ("filter", r#"{"name":{"i_contains":"shoe"}}"#),
                ("summary", "true"),
            ])
        );
        assert_eq!(page.data.len(), 2);
        assert_eq!(page.data[0].id, ProductItemId::new("1234"));
        assert_eq!(
            page.data[0].name.as_deref(),
            Some("Awesome Mens Shoes in Black")
        );
        assert_eq!(page.data[0].category.as_deref(), Some("mens_shoes"));
        assert_eq!(page.data[0].errors, None);
        let errors = page.data[1].errors.as_ref().unwrap();
        assert_eq!(
            errors[0].error_type.as_deref(),
            Some("IMAGE_RESOLUTION_LOW")
        );
        assert_eq!(errors[0].error_priority, Some(ProductErrorPriority::High));
        assert_eq!(
            errors[0].title.as_deref(),
            Some("Missing or invalid images")
        );
        assert_eq!(page.summary, Some(json!({"total_count": 316})));
        assert_eq!(page.next_cursor(), None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn products_sends_every_filter_and_both_cursors() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": []}));
        client(&t)
            .catalog(CATALOG)
            .products(&ListProducts {
                common: ProductFilters {
                    error_priority: Some(ProductErrorPriority::High),
                    error_type: Some("PRODUCT_OUT_OF_STOCK".into()),
                    bulk_pagination: Some(true),
                    limit: Some(50),
                    ..ProductFilters::default()
                },
                return_only_approved_products: Some(false),
                after: Some("A".into()),
                before: Some("B".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            pairs(&t.last_request().unwrap()),
            owned(&[
                ("error_priority", "HIGH"),
                ("error_type", "PRODUCT_OUT_OF_STOCK"),
                ("bulk_pagination", "true"),
                ("limit", "50"),
                ("return_only_approved_products", "false"),
                ("after", "A"),
                ("before", "B"),
            ])
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn the_next_cursor_of_a_page_is_the_next_querys_after() {
        let t = ScriptedTransport::new();
        t.push_json(200, products_page(&[("1001", "SKU-1")], "QVFIUa1", true));
        t.push_json(200, products_page(&[("1002", "SKU-2")], "QVFIUa2", false));
        let catalog = client(&t).catalog(CATALOG);
        let mut query = ListProducts::default();
        let first = catalog.products(&query).await.unwrap();
        query.after = first.next_cursor().map(str::to_owned);
        let second = catalog.products(&query).await.unwrap();
        assert_eq!(second.data[0].id.as_str(), "1002");
        assert_eq!(second.next_cursor(), None);
        assert_eq!(t.requests()[1].url.query(), Some("after=QVFIUa1"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn products_stream_refuses_caller_cursors() {
        let t = ScriptedTransport::new();
        let catalog = client(&t).catalog(CATALOG);
        for (field, query) in [
            (
                "after",
                ListProducts {
                    after: Some("c".into()),
                    ..ListProducts::default()
                },
            ),
            (
                "before",
                ListProducts {
                    before: Some("c".into()),
                    ..ListProducts::default()
                },
            ),
        ] {
            let items: Vec<Result<ProductInfo>> = catalog.products_stream(&query).collect().await;
            assert_eq!(items.len(), 1);
            assert_eq!(validation_field(items[0].as_ref().unwrap_err()), field);
        }
        assert!(t.requests().is_empty());
    }

    // ── Product items ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn product_get_parses_the_documented_fields() {
        let t = ScriptedTransport::new();
        // Field names and enum values from product-item § Fields (the
        // `videos` value is the page's own example); other values ours: the
        // page shows no response.
        t.push_json(
            200,
            json!({
                "id": "5678",
                "retailer_id": "SKU-1",
                "name": "Hipster Mens Shoes in Red",
                "description": "Red shoes",
                "availability": "in stock",
                "condition": "used_like_new",
                "price": "$9.99",
                "sale_price": "$7.99",
                "currency": "USD",
                "inventory": 12,
                "image_url": "https://example.com/shoe.png",
                "additional_image_urls": ["https://example.com/shoe-2.png"],
                "url": "https://example.com/shoe",
                "brand": "Example",
                "review_status": "approved",
                "visibility": "published",
                "retailer_product_group_id": "SHOES",
                "videos": [
                    {"url": "https://facebook.com/video1", "tags": ["abcd"]},
                    {"url": "https://facebook.com/video2", "tags": ["efgh"]}
                ]
            }),
        );
        let p = client(&t)
            .product("5678")
            .get(&["retailer_id", "name", "availability", "price", "videos"])
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/5678");
        assert_eq!(
            pairs(&req),
            owned(&[("fields", "retailer_id,name,availability,price,videos")])
        );
        assert_eq!(p.id, ProductItemId::new("5678"));
        assert_eq!(p.retailer_id.as_deref(), Some("SKU-1"));
        assert_eq!(p.availability, Some(ProductAvailability::InStock));
        assert_eq!(p.condition, Some(ProductCondition::UsedLikeNew));
        assert_eq!(p.price.as_deref(), Some("$9.99"));
        assert_eq!(p.inventory, Some(12));
        assert_eq!(p.review_status, Some(ProductReviewStatus::Approved));
        assert_eq!(p.visibility, Some(ProductVisibility::Published));
        // An untyped field requested by name is kept as sent.
        assert_eq!(
            p.other.get("videos"),
            Some(&json!([
                {"url": "https://facebook.com/video1", "tags": ["abcd"]},
                {"url": "https://facebook.com/video2", "tags": ["efgh"]}
            ]))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn product_with_default_fields_sends_no_query_and_keeps_unknown_values() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"id": "1", "name": "n", "retailer_id": "r", "availability": "back_soon", "review_status": ""}),
        );
        let p = client(&t).product("1").get(&[]).await.unwrap();
        assert_eq!(t.last_request().unwrap().url.query(), None);
        assert_eq!(
            p.availability,
            Some(ProductAvailability::Other("back_soon".into()))
        );
        assert_eq!(p.review_status, Some(ProductReviewStatus::Empty));
        assert_eq!(t.remaining(), 0);
    }

    // ── Catalogs ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn waba_product_catalogs_matches_the_reference() {
        let t = ScriptedTransport::new();
        // graph-api/reference/whatsapp-business-account/product_catalogs:
        // the example response is an empty `data` list.
        t.push_json(200, json!({"data": []}));
        let page = client(&t).waba(WABA).product_catalogs(&[]).await.unwrap();
        assert!(page.data.is_empty());
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), format!("/v25.0/{WABA}/product_catalogs"));
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn waba_product_catalogs_parses_a_connected_catalog() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": CATALOG, "name": "Shop", "product_count": 316, "vertical": "commerce"}]}),
        );
        let page = client(&t)
            .waba(WABA)
            .product_catalogs(&["id", "name", "product_count", "vertical"])
            .await
            .unwrap();
        assert_eq!(
            pairs(&t.last_request().unwrap()),
            owned(&[("fields", "id,name,product_count,vertical")])
        );
        let catalog = &page.data[0];
        assert_eq!(catalog.id, CatalogId::new(CATALOG));
        assert_eq!(catalog.name.as_deref(), Some("Shop"));
        assert_eq!(catalog.product_count, Some(316));
        assert_eq!(catalog.vertical.as_deref(), Some("commerce"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn catalog_get_reads_the_node() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "id": CATALOG,
                "name": "Shop",
                "product_count": 316,
                "is_catalog_segment": false,
                "fallback_image_url": ["https://example.com/fallback.png"],
                "business": {"id": "506914307656634", "name": "Lucky Shrub"}
            }),
        );
        let info = client(&t)
            .catalog(CATALOG)
            .get(&[
                "name",
                "product_count",
                "is_catalog_segment",
                "fallback_image_url",
                "business",
            ])
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.path(), format!("/v25.0/{CATALOG}"));
        assert_eq!(
            pairs(&req),
            owned(&[(
                "fields",
                "name,product_count,is_catalog_segment,fallback_image_url,business"
            )])
        );
        assert_eq!(info.product_count, Some(316));
        assert_eq!(info.is_catalog_segment, Some(false));
        assert_eq!(
            info.fallback_image_url,
            Some(vec!["https://example.com/fallback.png".to_owned()])
        );
        assert_eq!(
            info.other.get("business"),
            Some(&json!({"id": "506914307656634", "name": "Lucky Shrub"}))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn graph_error_is_classified() {
        let t = ScriptedTransport::new();
        t.push_json(
            403,
            json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException", "code": 200}}),
        );
        let err = client(&t).catalog(CATALOG).get(&[]).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }

    // ── Product sets ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn product_sets_stream_sends_the_filters_and_follows_cursors() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [{"id": "2001", "name": "Shirts", "filter": "{\"product_type\":{\"i_contains\":\"shirt\"}}", "retailer_id": "shirts"}],
                "paging": {"cursors": {"before": "b1", "after": "a1"}, "next": "https://graph.facebook.com/v25.0/x"}
            }),
        );
        t.push_json(
            200,
            json!({
                "data": [{"id": "2002", "name": "Belts", "product_count": 4}],
                "paging": {"cursors": {"before": "b2", "after": "a2"}}
            }),
        );
        let sets: Vec<ProductSetInfo> = client(&t)
            .catalog(CATALOG)
            .product_sets_stream(&ListProductSets {
                fields: vec!["name".into(), "filter".into(), "product_count".into()],
                parent_id: Some(ProductSetId::new("0")),
                has_children: Some(false),
                retailer_id: Some("shirts".into()),
                summary: true,
                limit: Some(1),
                ..ListProductSets::default()
            })
            .map(Result::unwrap)
            .collect()
            .await;
        let ids: Vec<&str> = sets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["2001", "2002"]);
        assert_eq!(
            sets[0].filter.as_deref(),
            Some(r#"{"product_type":{"i_contains":"shirt"}}"#)
        );
        assert_eq!(sets[1].product_count, Some(4));
        let reqs = t.requests();
        let base = [
            ("fields", "name,filter,product_count"),
            ("has_children", "false"),
            ("parent_id", "0"),
            ("retailer_id", "shirts"),
            ("summary", "true"),
            ("limit", "1"),
        ];
        assert_eq!(reqs[0].path(), format!("/v25.0/{CATALOG}/product_sets"));
        assert_eq!(pairs(&reqs[0]), owned(&base));
        let mut second = base.to_vec();
        second.push(("after", "a1"));
        assert_eq!(pairs(&reqs[1]), owned(&second));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn product_sets_page_sends_ancestor_and_cursors() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": [], "summary": {"total_count": 0}}));
        let page = client(&t)
            .catalog(CATALOG)
            .product_sets(&ListProductSets {
                ancestor_id: Some(ProductSetId::new("2001")),
                after: Some("A".into()),
                before: Some("B".into()),
                ..ListProductSets::default()
            })
            .await
            .unwrap();
        assert!(page.data.is_empty());
        assert_eq!(
            pairs(&t.last_request().unwrap()),
            owned(&[("ancestor_id", "2001"), ("after", "A"), ("before", "B")])
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn product_set_get_and_its_products() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "id": "2001",
                "name": "Shirts",
                "filter": "{\"product_type\":{\"i_contains\":\"shirt\"}}",
                "retailer_id": "shirts",
                "product_catalog": {"id": CATALOG, "name": "Shop"},
                "live_metadata": {"description": "d"}
            }),
        );
        t.push_json(200, products_page(&[("1001", "SKU-1")], "a1", true));
        t.push_json(200, products_page(&[("1002", "SKU-2")], "a2", false));
        let set = client(&t).product_set("2001");
        let info = set
            .get(&[
                "name",
                "filter",
                "retailer_id",
                "product_catalog",
                "live_metadata",
            ])
            .await
            .unwrap();
        assert_eq!(info.id, ProductSetId::new("2001"));
        assert_eq!(
            info.product_catalog.as_ref().map(|c| c.id.as_str()),
            Some(CATALOG)
        );
        assert_eq!(
            info.other.get("live_metadata"),
            Some(&json!({"description": "d"}))
        );
        let products: Vec<ProductInfo> = set
            .products_stream(&ListSetProducts {
                common: ProductFilters {
                    filter: Some(json!({"availability": {"eq": "in stock"}})),
                    ..ProductFilters::default()
                },
                ..ListSetProducts::default()
            })
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(products.len(), 2);
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/2001");
        assert_eq!(reqs[1].path(), "/v25.0/2001/products");
        let filter = ("filter", r#"{"availability":{"eq":"in stock"}}"#);
        assert_eq!(pairs(&reqs[1]), owned(&[filter]));
        assert_eq!(pairs(&reqs[2]), owned(&[filter, ("after", "a1")]));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn product_set_products_page_and_stream_refusal() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": []}));
        let set = client(&t).product_set("2001");
        set.products(&ListSetProducts {
            after: Some("A".into()),
            ..ListSetProducts::default()
        })
        .await
        .unwrap();
        assert_eq!(t.last_request().unwrap().url.query(), Some("after=A"));
        let items: Vec<Result<ProductInfo>> = set
            .products_stream(&ListSetProducts {
                before: Some("B".into()),
                ..ListSetProducts::default()
            })
            .collect()
            .await;
        assert_eq!(items.len(), 1);
        assert_eq!(validation_field(items[0].as_ref().unwrap_err()), "before");
        assert_eq!(t.requests().len(), 1);
        assert_eq!(t.remaining(), 0);
    }

    // ── Local id checks ───────────────────────────────────────────────────

    #[tokio::test]
    async fn ids_that_are_not_digits_are_refused_before_any_request() {
        let t = ScriptedTransport::new();
        let c = client(&t);
        for bad in ["", "123/products", "..", "SKU-1", "12 3", "%31"] {
            let err = c.catalog(bad).get(&[]).await.unwrap_err();
            assert_eq!(validation_field(&err), "catalog_id", "{bad:?}");
            let err = c
                .catalog(bad)
                .products(&ListProducts::default())
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "catalog_id", "{bad:?}");
            let items: Vec<Result<ProductInfo>> = c
                .catalog(bad)
                .products_stream(&ListProducts::default())
                .collect()
                .await;
            assert_eq!(items.len(), 1);
            assert_eq!(
                validation_field(items[0].as_ref().unwrap_err()),
                "catalog_id"
            );
            let items: Vec<Result<ProductSetInfo>> = c
                .catalog(bad)
                .product_sets_stream(&ListProductSets::default())
                .collect()
                .await;
            assert_eq!(
                validation_field(items[0].as_ref().unwrap_err()),
                "catalog_id"
            );
            let err = c.product_set(bad).get(&[]).await.unwrap_err();
            assert_eq!(validation_field(&err), "product_set_id", "{bad:?}");
            let items: Vec<Result<ProductInfo>> = c
                .product_set(bad)
                .products_stream(&ListSetProducts::default())
                .collect()
                .await;
            assert_eq!(
                validation_field(items[0].as_ref().unwrap_err()),
                "product_set_id"
            );
            let err = c.product(bad).get(&[]).await.unwrap_err();
            assert_eq!(validation_field(&err), "product_item_id", "{bad:?}");
            let err = c.waba(bad).product_catalogs(&[]).await.unwrap_err();
            assert_eq!(validation_field(&err), "waba_id", "{bad:?}");
        }
        let catalog = c.catalog(CATALOG);
        for (field, query) in [
            (
                "ancestor_id",
                ListProductSets {
                    ancestor_id: Some(ProductSetId::new("1/x")),
                    ..ListProductSets::default()
                },
            ),
            (
                "parent_id",
                ListProductSets {
                    parent_id: Some(ProductSetId::new("root")),
                    ..ListProductSets::default()
                },
            ),
        ] {
            let err = catalog.product_sets(&query).await.unwrap_err();
            assert_eq!(validation_field(&err), field);
        }
        assert!(t.requests().is_empty());
    }

    #[test]
    fn ids_are_transparent_in_json_and_readable_in_debug() {
        let id: ProductItemId = serde_json::from_value(json!("5678")).unwrap();
        assert_eq!(id, ProductItemId::from("5678"));
        assert_eq!(format!("{id:?}"), "ProductItemId(5678)");
        assert_eq!(
            serde_json::to_value(ProductSetId::new("1")).unwrap(),
            json!("1")
        );
    }
}
