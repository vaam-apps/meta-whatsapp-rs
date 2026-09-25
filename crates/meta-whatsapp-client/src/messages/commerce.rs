//! Commerce `interactive` messages (`catalogs/share-products`): single
//! product, multi-product, catalog and product carousel.
//!
//! These can only be sent inside an existing chat thread, not as
//! notifications. A catalog link is just a `wa.me/c/<number>` URL in a text
//! message (`catalogs/catalog-link-messages`) and needs no type of its own.

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::CatalogId;

use super::interactive::{FOOTER_MAX, Header, TextObject, section_title};
use super::validate::{self, Check};

/// Single-product message (`catalogs/single-product-messages`). Headers are
/// not allowed on this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleProduct {
    /// Optional body text.
    pub body: Option<String>,
    /// Optional footer text.
    pub footer: Option<String>,
    /// Catalog holding the product.
    pub catalog_id: CatalogId,
    /// The product's retailer id (SKU).
    pub product_retailer_id: String,
}

impl SingleProduct {
    /// Show `product_retailer_id` from `catalog_id`.
    pub fn new(catalog_id: impl Into<CatalogId>, product_retailer_id: impl Into<String>) -> Self {
        Self {
            body: None,
            footer: None,
            catalog_id: catalog_id.into(),
            product_retailer_id: product_retailer_id.into(),
        }
    }

    /// Set the body.
    #[must_use]
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    pub(crate) fn validate(&self) -> Check {
        // "Action object — Must include both catalog_id and
        // product_retailer_id." No lengths are documented for this type.
        validate::opt_text("interactive.body.text", self.body.as_deref(), usize::MAX)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            usize::MAX,
        )?;
        validate::non_empty("interactive.action.catalog_id", self.catalog_id.as_str())?;
        validate::non_empty(
            "interactive.action.product_retailer_id",
            &self.product_retailer_id,
        )
    }
}

impl Serialize for SingleProduct {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Action<'a> {
            catalog_id: &'a CatalogId,
            product_retailer_id: &'a str,
        }
        let mut map = serializer.serialize_map(None)?;
        if let Some(b) = &self.body {
            map.serialize_entry("body", &TextObject(b))?;
        }
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &Action {
                catalog_id: &self.catalog_id,
                product_retailer_id: &self.product_retailer_id,
            },
        )?;
        map.end()
    }
}

/// Multi-product message (`catalogs/multi-product-messages`): up to 30
/// products in titled sections. The header is required and must be text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductList {
    /// Header text (required).
    pub header: String,
    /// Body text (required).
    pub body: String,
    /// Footer text.
    pub footer: Option<String>,
    /// Catalog holding the products.
    pub catalog_id: CatalogId,
    /// Sections.
    pub sections: Vec<ProductSection>,
}

/// A multi-product section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductSection {
    /// Section title; required once there is more than one section.
    pub title: Option<String>,
    /// Product retailer ids (SKUs), in display order.
    pub product_retailer_ids: Vec<String>,
}

impl ProductSection {
    /// Titled section.
    pub fn new(
        title: impl Into<String>,
        product_retailer_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            title: Some(title.into()),
            product_retailer_ids: product_retailer_ids.into_iter().map(Into::into).collect(),
        }
    }
}

/// "display up to 30 products" (`catalogs/multi-product-messages`,
/// `catalogs/share-products`).
const MPM_MAX_PRODUCTS: usize = 30;

impl ProductList {
    /// Header, body, catalog and sections; no footer.
    pub fn new(
        header: impl Into<String>,
        body: impl Into<String>,
        catalog_id: impl Into<CatalogId>,
        sections: impl IntoIterator<Item = ProductSection>,
    ) -> Self {
        Self {
            header: header.into(),
            body: body.into(),
            footer: None,
            catalog_id: catalog_id.into(),
            sections: sections.into_iter().collect(),
        }
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    pub(crate) fn validate(&self) -> Check {
        // Required components per catalogs/multi-product-messages: text
        // header, body, catalog_id and sections. No text lengths are
        // documented for this type, so only presence is checked.
        validate::non_empty("interactive.header.text", &self.header)?;
        validate::non_empty("interactive.body.text", &self.body)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            usize::MAX,
        )?;
        validate::non_empty("interactive.action.catalog_id", self.catalog_id.as_str())?;
        validate::count(
            "interactive.action.sections",
            self.sections.len(),
            1,
            usize::MAX,
        )?;
        let total: usize = self
            .sections
            .iter()
            .map(|s| s.product_retailer_ids.len())
            .sum();
        validate::count(
            "interactive.action.sections[].product_items",
            total,
            1,
            MPM_MAX_PRODUCTS,
        )?;
        let multi = self.sections.len() > 1;
        for (i, s) in self.sections.iter().enumerate() {
            let path = format!("interactive.action.sections[{i}]");
            section_title(&format!("{path}.title"), s.title.as_deref(), multi)?;
            for (j, id) in s.product_retailer_ids.iter().enumerate() {
                validate::non_empty(
                    &format!("{path}.product_items[{j}].product_retailer_id"),
                    id,
                )?;
            }
        }
        Ok(())
    }
}

impl Serialize for ProductList {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Item<'a> {
            product_retailer_id: &'a str,
        }
        #[derive(Serialize)]
        struct Section<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            title: Option<&'a str>,
            product_items: Vec<Item<'a>>,
        }
        #[derive(Serialize)]
        struct Action<'a> {
            catalog_id: &'a CatalogId,
            sections: Vec<Section<'a>>,
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("header", &Header::text(self.header.clone()))?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &Action {
                catalog_id: &self.catalog_id,
                sections: self
                    .sections
                    .iter()
                    .map(|s| Section {
                        title: s.title.as_deref(),
                        product_items: s
                            .product_retailer_ids
                            .iter()
                            .map(|id| Item {
                                product_retailer_id: id,
                            })
                            .collect(),
                    })
                    .collect(),
            },
        )?;
        map.end()
    }
}

/// Catalog message (`catalogs/catalog-messages`): body, optional footer and
/// a "View catalog" button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMessage {
    /// Body text.
    pub body: String,
    /// Footer text.
    pub footer: Option<String>,
    /// SKU whose image becomes the header thumbnail (default: the first
    /// catalog item).
    pub thumbnail_product_retailer_id: Option<String>,
}

impl CatalogMessage {
    /// Catalog message with `body`.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            footer: None,
            thumbnail_product_retailer_id: None,
        }
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    /// Use this SKU's image as the thumbnail.
    #[must_use]
    pub fn thumbnail(mut self, product_retailer_id: impl Into<String>) -> Self {
        self.thumbnail_product_retailer_id = Some(product_retailer_id.into());
        self
    }

    pub(crate) fn validate(&self) -> Check {
        // catalogs/catalog-messages: body max 1024, footer max 60.
        validate::text("interactive.body.text", &self.body, 1024)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            FOOTER_MAX,
        )?;
        validate::opt_text(
            "interactive.action.parameters.thumbnail_product_retailer_id",
            self.thumbnail_product_retailer_id.as_deref(),
            usize::MAX,
        )
    }
}

impl Serialize for CatalogMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Parameters<'a> {
            thumbnail_product_retailer_id: &'a str,
        }
        #[derive(Serialize)]
        struct Action<'a> {
            name: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            parameters: Option<Parameters<'a>>,
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        map.serialize_entry(
            "action",
            &Action {
                name: "catalog_message",
                parameters: self
                    .thumbnail_product_retailer_id
                    .as_deref()
                    .map(|id| Parameters {
                        thumbnail_product_retailer_id: id,
                    }),
            },
        )?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.end()
    }
}

/// Product card carousel (`catalogs/interactive-product-carousel-messages`):
/// 2–10 product cards from one catalog. Body only; no header, footer or
/// buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductCarousel {
    /// Body text.
    pub body: String,
    /// Cards, left to right; `card_index` is their position.
    pub cards: Vec<ProductCard>,
}

/// One product card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductCard {
    /// Catalog; identical on every card.
    pub catalog_id: CatalogId,
    /// Product retailer id (SKU).
    pub product_retailer_id: String,
}

impl ProductCard {
    /// Card for `product_retailer_id` in `catalog_id`.
    pub fn new(catalog_id: impl Into<CatalogId>, product_retailer_id: impl Into<String>) -> Self {
        Self {
            catalog_id: catalog_id.into(),
            product_retailer_id: product_retailer_id.into(),
        }
    }
}

impl ProductCarousel {
    /// Body plus cards.
    pub fn new(body: impl Into<String>, cards: impl IntoIterator<Item = ProductCard>) -> Self {
        Self {
            body: body.into(),
            cards: cards.into_iter().collect(),
        }
    }

    pub(crate) fn validate(&self) -> Check {
        // catalogs/interactive-product-carousel-messages: body required,
        // max 1024; two to ten cards; every card references the same
        // catalog.
        validate::text("interactive.body.text", &self.body, 1024)?;
        validate::count("interactive.action.cards", self.cards.len(), 2, 10)?;
        let catalog = self.cards.first().map(|c| &c.catalog_id);
        for (i, card) in self.cards.iter().enumerate() {
            let path = format!("interactive.action.cards[{i}].action");
            validate::non_empty(&format!("{path}.catalog_id"), card.catalog_id.as_str())?;
            validate::non_empty(
                &format!("{path}.product_retailer_id"),
                &card.product_retailer_id,
            )?;
            if catalog != Some(&card.catalog_id) {
                return Err(ValidationError::new(
                    format!("{path}.catalog_id"),
                    "every card must reference the same catalog",
                ));
            }
        }
        Ok(())
    }
}

impl Serialize for ProductCarousel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct CardAction<'a> {
            product_retailer_id: &'a str,
            catalog_id: &'a CatalogId,
        }
        #[derive(Serialize)]
        struct Wire<'a> {
            card_index: usize,
            #[serde(rename = "type")]
            kind: &'static str,
            action: CardAction<'a>,
        }
        #[derive(Serialize)]
        struct Action<'a> {
            cards: Vec<Wire<'a>>,
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        map.serialize_entry(
            "action",
            &Action {
                cards: self
                    .cards
                    .iter()
                    .enumerate()
                    .map(|(card_index, c)| Wire {
                        card_index,
                        kind: "product",
                        action: CardAction {
                            product_retailer_id: &c.product_retailer_id,
                            catalog_id: &c.catalog_id,
                        },
                    })
                    .collect(),
            },
        )?;
        map.end()
    }
}
