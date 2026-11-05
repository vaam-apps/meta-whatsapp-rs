//! Commerce (scope `commerce`; docs/design/server.md, section 4.2; roadmap
//! M5g): a number's commerce settings and the reads of the catalog its WABA
//! connects.
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/numbers/{pn}/commerce-settings` | `GET /{pn}/whatsapp_commerce_settings` |
//! | `PATCH /v1/numbers/{pn}/commerce-settings` | `POST /{pn}/whatsapp_commerce_settings?is_cart_enabled=…&is_catalog_visible=…` |
//! | `GET /v1/wabas/{waba_id}/catalogs` | `GET /{waba_id}/product_catalogs?fields=id,name,product_count,vertical` |
//! | `GET /v1/wabas/{waba_id}/catalogs/{catalog_id}` | the same list, the catalog found among it |
//! | `GET /v1/wabas/{waba_id}/catalogs/{catalog_id}/products` | the same list (the catalog must be among it), then `GET /{catalog_id}/products` |
//!
//! Pages: `catalogs/set-commerce-settings`, Meta's Catalog API (see the
//! core's [`meta_whatsapp_server_core::commerce`]). **A catalog is its
//! WABA's, or it does not exist**: another WABA's catalog is `404
//! not_found`, and its products are never asked for. Not offered (roadmap
//! M5g, partial): a catalog's product sets, one product by its id, the
//! products edge's `filter`, and every catalog write.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server_core::commerce::{
    self as domain, Catalogs, NumberCommerce, ProductQuery, SettingsChange,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::{ApiJson, encode_cursor};
use super::listing;
use super::templates::ApiQuery;
use crate::auth::{Caller, OwnedNumber, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::state::Grant;

// ─── The answers ─────────────────────────────────────────────────────────

/// A number's commerce settings.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CommerceSettings)]
pub struct CommerceSettingsView {
    /// The id of the settings object (not the number's).
    #[schema(required = true)]
    pub id: Option<String>,
    /// Whether the shopping cart is enabled (Meta's default: yes).
    #[schema(required = true)]
    pub is_cart_enabled: Option<bool>,
    /// Whether the catalog storefront icon is shown (Meta's default: no).
    #[schema(required = true)]
    pub is_catalog_visible: Option<bool>,
}

impl From<domain::Settings> for CommerceSettingsView {
    fn from(settings: domain::Settings) -> Self {
        let domain::Settings {
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

/// What to change in a number's commerce settings: at least one field.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CommerceSettingsUpdate)]
pub struct CommerceSettingsBody {
    /// Enable (`true`) or disable the shopping cart.
    pub is_cart_enabled: Option<bool>,
    /// Show (`true`) or hide the catalog storefront icon. Hiding it makes
    /// existing catalog links show an "Invalid catalog link" warning.
    pub is_catalog_visible: Option<bool>,
}

/// A catalog connected to a WABA.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Catalog)]
pub struct CatalogView {
    /// Its id.
    pub id: String,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
    /// How many products it holds.
    #[schema(required = true)]
    pub product_count: Option<u64>,
    /// Its kind, as Meta writes it (`commerce`, …).
    #[schema(required = true)]
    pub vertical: Option<String>,
}

impl From<domain::Catalog> for CatalogView {
    fn from(catalog: domain::Catalog) -> Self {
        let domain::Catalog {
            id,
            name,
            product_count,
            vertical,
        } = catalog;
        Self {
            id,
            name,
            product_count,
            vertical,
        }
    }
}

/// The catalogs a WABA connects (a WhatsApp account hierarchy has one at
/// most).
#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogList {
    /// The catalogs.
    pub data: Vec<CatalogView>,
}

/// A product of a catalog.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Product)]
pub struct ProductView {
    /// Its id.
    pub id: String,
    /// The business's own id: what a product message names.
    #[schema(required = true)]
    pub retailer_id: Option<String>,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
    /// Its description.
    #[schema(required = true)]
    pub description: Option<String>,
    /// `in stock`, `out of stock`, …, as Meta writes it.
    #[schema(required = true)]
    pub availability: Option<String>,
    /// `new`, `used`, …, as Meta writes it.
    #[schema(required = true)]
    pub condition: Option<String>,
    /// Its price, as Meta formats it.
    #[schema(required = true)]
    pub price: Option<String>,
    /// Its sale price, in the same format.
    #[schema(required = true)]
    pub sale_price: Option<String>,
    /// Its currency (ISO 4217).
    #[schema(required = true)]
    pub currency: Option<String>,
    /// Its main image.
    #[schema(required = true)]
    pub image_url: Option<String>,
    /// Its page.
    #[schema(required = true)]
    pub url: Option<String>,
    /// Its brand.
    #[schema(required = true)]
    pub brand: Option<String>,
    /// Its review status, as Meta writes it.
    #[schema(required = true)]
    pub review_status: Option<String>,
    /// Its visibility, as Meta writes it.
    #[schema(required = true)]
    pub visibility: Option<String>,
}

impl From<domain::Product> for ProductView {
    fn from(product: domain::Product) -> Self {
        let domain::Product {
            id,
            retailer_id,
            name,
            description,
            availability,
            condition,
            price,
            sale_price,
            currency,
            image_url,
            url,
            brand,
            review_status,
            visibility,
        } = product;
        Self {
            id,
            retailer_id,
            name,
            description,
            availability,
            condition,
            price,
            sale_price,
            currency,
            image_url,
            url,
            brand,
            review_status,
            visibility,
        }
    }
}

/// A page of a catalog's products.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProductList {
    /// The products, in Meta's order.
    pub data: Vec<ProductView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

impl From<domain::ProductPage> for ProductList {
    fn from(page: domain::ProductPage) -> Self {
        Self {
            data: page.products.into_iter().map(ProductView::from).collect(),
            next_cursor: page.next_after.as_deref().map(encode_cursor),
        }
    }
}

/// `?limit=`, `?cursor=` and `?return_only_approved_products=`, as the
/// OpenAPI document declares them.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ProductsQuery {
    /// Page size, 1 to 100 (the service's bound; Meta's default without).
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Only the products approved for WhatsApp (Meta's default: all).
    pub return_only_approved_products: Option<bool>,
}

// ─── The services ────────────────────────────────────────────────────────

fn settings_service<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<NumberCommerce<'a>, ApiError> {
    Ok(
        NumberCommerce::new(state.authz(), caller, owned.capability(state))?
            .observed_by(state.metrics()),
    )
}

fn catalogs<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<Catalogs<'a>, ApiError> {
    Ok(Catalogs::new(state.authz(), caller, owned.capability(state))?.observed_by(state.metrics()))
}

// ─── Settings ────────────────────────────────────────────────────────────

/// `GET /v1/numbers/{pn}/commerce-settings`: the number's cart and
/// catalog settings.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/commerce-settings",
    tag = "commerce",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The settings", body = CommerceSettingsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_commerce_settings(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<CommerceSettingsView>, ApiError> {
    let settings = settings_service(&state, &caller, &owned)?
        .settings()
        .await?;
    Ok(Json(CommerceSettingsView::from(settings)))
}

/// `PATCH /v1/numbers/{pn}/commerce-settings`: enable or disable the cart,
/// show or hide the catalog.
#[utoipa::path(
    patch,
    path = "/v1/numbers/{pn}/commerce-settings",
    tag = "commerce",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = CommerceSettingsBody,
    responses(
        (status = 204, description = "Changed"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `update` (nothing to change) or `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the settings may have been changed", body = ErrorBody),
    )
)]
pub(crate) async fn update_commerce_settings(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<CommerceSettingsBody>,
) -> Result<StatusCode, ApiError> {
    let change = SettingsChange {
        is_cart_enabled: body.is_cart_enabled,
        is_catalog_visible: body.is_catalog_visible,
    }
    .checked()?;
    settings_service(&state, &caller, &owned)?
        .update(change)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Catalogs ────────────────────────────────────────────────────────────

/// `GET /v1/wabas/{waba_id}/catalogs`: the catalog the WABA connects.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/catalogs",
    tag = "commerce",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The catalogs (none, or one)", body = CatalogList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_catalogs(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<Json<CatalogList>, ApiError> {
    let listed = catalogs(&state, &caller, &owned)?.list().await?;
    Ok(Json(CatalogList {
        data: listed.into_iter().map(CatalogView::from).collect(),
    }))
}

/// `GET /v1/wabas/{waba_id}/catalogs/{catalog_id}`: one catalog of the
/// WABA.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/catalogs/{catalog_id}",
    tag = "commerce",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("catalog_id" = String, Path, description = "Catalog id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The catalog", body = CatalogView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such catalog among its own (another WABA's catalog looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `catalog_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_catalog(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, catalog_id)): Path<(String, String)>,
) -> Result<Json<CatalogView>, ApiError> {
    let catalog = catalogs(&state, &caller, &owned)?.get(&catalog_id).await?;
    Ok(Json(CatalogView::from(catalog)))
}

/// `GET /v1/wabas/{waba_id}/catalogs/{catalog_id}/products`: a page of the
/// catalog's products.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/catalogs/{catalog_id}/products",
    tag = "commerce",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("catalog_id" = String, Path, description = "Catalog id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ProductsQuery,
    ),
    responses(
        (status = 200, description = "A page of products", body = ProductList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such catalog among its own (its products are not asked for)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `catalog_id`, `limit`, `cursor` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_catalog_products(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, catalog_id)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<ProductsQuery>,
) -> Result<Json<ProductList>, ApiError> {
    let after = listing::after(query.cursor.as_deref())?;
    let page = catalogs(&state, &caller, &owned)?
        .products(
            &catalog_id,
            ProductQuery {
                limit: query.limit,
                after,
                return_only_approved_products: query.return_only_approved_products,
            },
        )
        .await?;
    Ok(Json(ProductList::from(page)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Every field of the core's catalog, product and settings reaches the
    /// answer, a missing one as `null`. Decisive: each field of the
    /// conversions.
    #[test]
    fn every_field_of_the_cores_answers_is_answered() {
        assert_eq!(
            serde_json::to_value(CommerceSettingsView::from(domain::Settings {
                id: Some("727705352028726".to_owned()),
                is_cart_enabled: Some(true),
                is_catalog_visible: None,
            }))
            .unwrap(),
            json!({"id": "727705352028726", "is_cart_enabled": true, "is_catalog_visible": null})
        );
        assert_eq!(
            serde_json::to_value(CatalogView::from(domain::Catalog {
                id: "1".to_owned(),
                name: Some("Shop".to_owned()),
                product_count: Some(3),
                vertical: Some("commerce".to_owned()),
            }))
            .unwrap(),
            json!({"id": "1", "name": "Shop", "product_count": 3, "vertical": "commerce"})
        );
        let product = domain::Product {
            id: "9".to_owned(),
            retailer_id: Some("sku-9".to_owned()),
            name: Some("Mug".to_owned()),
            description: Some("A mug".to_owned()),
            availability: Some("in stock".to_owned()),
            condition: Some("new".to_owned()),
            price: Some("10.00 USD".to_owned()),
            sale_price: Some("8.00 USD".to_owned()),
            currency: Some("USD".to_owned()),
            image_url: Some("https://example.com/m.png".to_owned()),
            url: Some("https://example.com/m".to_owned()),
            brand: Some("Acme".to_owned()),
            review_status: Some("approved".to_owned()),
            visibility: Some("published".to_owned()),
        };
        let list = ProductList::from(domain::ProductPage {
            products: vec![product],
            next_after: Some("MjQ=".to_owned()),
        });
        assert_eq!(
            serde_json::to_value(list).unwrap(),
            json!({"data": [{
                "id": "9", "retailer_id": "sku-9", "name": "Mug", "description": "A mug",
                "availability": "in stock", "condition": "new", "price": "10.00 USD",
                "sale_price": "8.00 USD", "currency": "USD",
                "image_url": "https://example.com/m.png", "url": "https://example.com/m",
                "brand": "Acme", "review_status": "approved", "visibility": "published"}],
                "next_cursor": "4d6a343d"})
        );
    }
}
