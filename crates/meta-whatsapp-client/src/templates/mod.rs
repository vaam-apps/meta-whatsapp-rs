//! Message templates: management (list, get, create, edit, delete, library,
//! migrate, compare, unpause) and the two builder families that must not be
//! confused:
//!
//! - **Definitions** ([`TemplateDefinition`], [`TemplateComponent`],
//!   [`Button`]): what a template *is*, sent to create or edit it, returned
//!   by `list`/`get`. Checked locally against every limit the docs state
//!   before any request ([`TemplateDefinition::validate`]).
//! - **Invocations** ([`TemplateMessage`], [`SendComponent`], [`Parameter`]):
//!   the values that fill a template's placeholders when it is *sent*, in
//!   the `template` field of a message.
//!
//! Authentication templates and the OTP service live in
//! [`crate::authentication`].
//!
//! Docs: `templates/overview`, `templates/components`,
//! `templates/template-management`, `templates/template-library`,
//! `templates/template-migration`, `templates/template-comparison`,
//! `templates/template-pausing`, `templates/time-to-live`,
//! `templates/template-media`, `templates/tap-target-url-title-override`,
//! `templates/marketing-templates/*`, `templates/utility-templates/*`,
//! `catalogs/{catalog,mpm,spm}-template-messages`,
//! `catalogs/product-card-carousel-template-messages`,
//! `flows/guides/flows-templates`, `calling/call-button-messages-deep-links`,
//! `reference/whatsapp-business-account/message-template-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! # Example
//!
//! ```no_run
//! # async fn demo(client: meta_whatsapp_client::Client) -> meta_whatsapp_core::Result<()> {
//! use meta_whatsapp_client::templates::{
//!     Button, Parameter, TemplateCategory, TemplateComponent, TemplateDefinition,
//!     TemplateMessage,
//! };
//!
//! let definition = TemplateDefinition::new("order_update", "en_US", TemplateCategory::Utility)
//!     .component(TemplateComponent::body_positional(
//!         "Hi {{1}}, order {{2}} has shipped.",
//!         ["Pablo", "860198"],
//!     ))
//!     .component(TemplateComponent::buttons([Button::url_with_example(
//!         "Track",
//!         "https://shop.example/track/{{1}}",
//!         "860198",
//!     )]));
//! let created = client.templates("<WABA_ID>").create(&definition).await?;
//!
//! // Later, once approved, the invocation for a send request:
//! let message = TemplateMessage::new("order_update", "en_US")
//!     .body([Parameter::text("Jessica"), Parameter::text("SKBUP2")])
//!     .url_button(0, "SKBUP2");
//! # let _ = (created, message); Ok(()) }
//! ```

mod definition;
mod info;
pub(crate) mod macros;
mod send;
mod types;
pub(crate) mod validate;

#[cfg(test)]
mod tests;

pub use definition::{
    BodyComponent, BodyExample, Button, CarouselCard, FlowAction, FlowButton, FooterComponent,
    HeaderComponent, HeaderExample, HeaderFormat, LimitedTimeOffer, NamedParameterExample,
    OtpButton, SupportedApp, TemplateComponent, TemplateDefinition, TemplateEdit,
};
pub use info::{
    ComparisonMetric, ComparisonMetricKind, ComparisonValue, HealthStatus, LibraryBodyInputs,
    LibraryButtonInput, LibraryButtonType, LibraryQuery, LibraryTemplate, LibraryTemplateRequest,
    LibraryUrlInput, MigrationOptions, MigrationResult, QualityScore, TemplateCreated,
    TemplateInfo, TemplateListQuery,
};
pub use send::{
    ButtonSubType, CarouselCardParameters, Currency, DateTime, DocumentMedia,
    LimitedTimeOfferParameter, MediaSource, MpmSection, Parameter, ParameterAction, ProductItem,
    ProductReference, SendComponent, TapTarget, TemplateLanguage, TemplateLocation,
    TemplateMessage,
};
pub use types::{
    DisplayFormat, ParameterFormat, QualityRating, RejectionReason, SendType, TemplateCategory,
    TemplateSource, TemplateStatus, TemplateSubCategory,
};

use futures::Stream;
use serde::{Deserialize, Serialize};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{TemplateId, WabaId};
use meta_whatsapp_core::paging::Page;

use crate::request::{paginate_or_error, reject_cursors};
use crate::{Client, GraphRequest};

/// Entry point, see [`Client::templates`].
#[derive(Debug, Clone)]
pub struct Templates {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Templates`] API for `waba_id`.
    pub fn templates(&self, waba_id: impl Into<WabaId>) -> Templates {
        Templates {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

/// `{"data": [...]}` without paging.
#[derive(Deserialize)]
pub(crate) struct DataList<T> {
    #[serde(default = "Vec::new")]
    pub(crate) data: Vec<T>,
}

/// Graph takes id lists in the query as a JSON-ish array; the docs print
/// bare numbers (`hsm_ids=[1387372356726668,1304694804498707]`). Ids that
/// are not all digits are quoted so the array stays valid JSON.
fn id_list(ids: &[TemplateId]) -> String {
    let items: Vec<String> = ids
        .iter()
        .map(|id| {
            let s = id.as_str();
            if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
                s.to_owned()
            } else {
                serde_json::Value::String(s.to_owned()).to_string()
            }
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn check_limit(limit: Option<u32>) -> Result<()> {
    // Reference: `limit` is "integer [min: 1]".
    if limit == Some(0) {
        return Err(ValidationError::new("limit", "must be at least 1").into());
    }
    Ok(())
}

impl Templates {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn list_request(&self, query: &TemplateListQuery) -> GraphRequest {
        let mut req = self
            .client
            .get_at(&[self.waba_id.as_str(), "message_templates"])
            .context("message templates list");
        if !query.fields.is_empty() {
            req = req.query("fields", query.fields.join(","));
        }
        req = req
            .query_opt("limit", query.limit)
            .query_opt("name", query.name.as_deref())
            .query_opt("status", query.status.as_ref())
            .query_opt("source", query.source.as_ref())
            .query_opt("correct_category", query.correct_category.as_ref());
        req
    }

    /// One page of templates (`GET /{waba}/message_templates`). The next
    /// page: the same query with `after` set to this page's
    /// [`Page::next_cursor`].
    pub async fn list(&self, query: &TemplateListQuery) -> Result<Page<TemplateInfo>> {
        check_limit(query.limit)?;
        self.list_request(query)
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every template matching `query`, page after page. The stream manages
    /// the cursors itself: a query with `after` or `before` set is refused
    /// (the stream's single item is that validation error), like a bad
    /// `limit`.
    pub fn list_stream(
        &self,
        query: &TemplateListQuery,
    ) -> impl Stream<Item = Result<TemplateInfo>> + Send + 'static {
        paginate_or_error(
            check_limit(query.limit)
                .and_then(|()| reject_cursors(query.after.as_deref(), query.before.as_deref()))
                .map(|()| self.list_request(query)),
        )
    }

    /// One template with Meta's default fields (`GET /{template_id}`).
    pub async fn get(&self, id: &TemplateId) -> Result<TemplateInfo> {
        self.client
            .get_at(&[id.as_str()])
            .context("message template")
            .send()
            .await
    }

    /// One template with the given `fields` (e.g. `["status"]`,
    /// `["quality_score"]`).
    pub async fn get_fields(&self, id: &TemplateId, fields: &[&str]) -> Result<TemplateInfo> {
        self.client
            .get_at(&[id.as_str()])
            .query("fields", fields.join(","))
            .context("message template")
            .send()
            .await
    }

    /// Create a template (`POST /{waba}/message_templates`) after checking
    /// it locally. A WABA can create at most 100 templates per hour
    /// (`templates/overview#creation`).
    pub async fn create(&self, definition: &TemplateDefinition) -> Result<TemplateCreated> {
        definition.validate()?;
        self.client
            .post_at(&[self.waba_id.as_str(), "message_templates"])
            .json(definition)
            .context("create message template response")
            .send()
            .await
    }

    /// Edit a template (`POST /{template_id}`). See [`TemplateEdit`] for
    /// Meta's rules. Not replayed on timeouts: an edit of an approved
    /// template consumes one of its limited edits.
    pub async fn edit(&self, id: &TemplateId, edit: &TemplateEdit) -> Result<()> {
        edit.validate()?;
        self.client
            .post_at(&[id.as_str()])
            .json(edit)
            .context("edit message template response")
            .send_success()
            .await
    }

    /// Delete every language version of the template called `name`.
    /// Disabled templates cannot be deleted; an approved template's name
    /// cannot be reused for 30 days (`templates/template-management`).
    pub async fn delete_by_name(&self, name: &str) -> Result<()> {
        not_empty(name, "name")?;
        self.client
            .delete_at(&[self.waba_id.as_str(), "message_templates"])
            .query("name", name)
            .context("delete message template response")
            .send_success()
            .await
    }

    /// Delete one language version: the template `id` called `name`.
    pub async fn delete_by_id(&self, name: &str, id: &TemplateId) -> Result<()> {
        not_empty(name, "name")?;
        self.client
            .delete_at(&[self.waba_id.as_str(), "message_templates"])
            .query("hsm_id", id)
            .query("name", name)
            .context("delete message template response")
            .send_success()
            .await
    }

    /// Delete up to 100 templates by id in one request; if any id is invalid
    /// nothing is deleted.
    pub async fn delete_by_ids(&self, ids: &[TemplateId]) -> Result<()> {
        // `templates/template-management#delete-templates-by-ids`: "up to 100".
        if ids.is_empty() || ids.len() > 100 {
            return Err(ValidationError::new("hsm_ids", "1 to 100 template ids").into());
        }
        self.client
            .delete_at(&[self.waba_id.as_str(), "message_templates"])
            .query("hsm_ids", id_list(ids))
            .context("delete message templates response")
            .send_success()
            .await
    }

    fn library_request(query: &LibraryQuery, client: &Client) -> GraphRequest {
        // The request syntax on `templates/template-library` is
        // `GET /message_template_library` (not under the WABA); its one
        // curl example uses `/{waba}/message_templates?search=` instead,
        // which is the regular list endpoint. The syntax is followed.
        client
            .get("message_template_library")
            .query_opt("search", query.search.as_deref())
            .query_opt("topic", query.topic.as_deref())
            .query_opt("usecase", query.usecase.as_deref())
            .query_opt("industry", query.industry.as_deref())
            .query_opt("language", query.language.as_deref())
            .query_opt("name", query.name.as_deref())
            .context("message template library")
    }

    /// Browse the Template Library (`GET /message_template_library`). Not
    /// WABA-scoped; offered here next to [`Self::create_from_library`].
    ///
    /// Unlike the other lists, [`LibraryQuery`] has no cursor: the library
    /// page documents neither `after`/`before` nor a `paging` object. Should
    /// Meta page it anyway, [`Self::library_stream`] follows the cursors.
    pub async fn library(&self, query: &LibraryQuery) -> Result<Page<LibraryTemplate>> {
        Self::library_request(query, &self.client).send().await
    }

    /// Every Template Library entry matching `query`.
    pub fn library_stream(
        &self,
        query: &LibraryQuery,
    ) -> impl Stream<Item = Result<LibraryTemplate>> + Send + 'static {
        Self::library_request(query, &self.client).paginate()
    }

    /// Create a template from a Template Library entry.
    ///
    /// `library_template_button_inputs` is sent as a JSON array of objects,
    /// as the reference schema says; the library page's example shows it as
    /// a string of single-quoted pseudo-JSON.
    pub async fn create_from_library(
        &self,
        request: &LibraryTemplateRequest,
    ) -> Result<TemplateCreated> {
        validate::name(&request.name, "name")?;
        if request.language.trim().is_empty() {
            return Err(ValidationError::new("language", "must not be empty").into());
        }
        if request.library_template_name.trim().is_empty() {
            return Err(ValidationError::new("library_template_name", "must not be empty").into());
        }
        if let Some(minutes) = request
            .library_template_body_inputs
            .as_ref()
            .and_then(|b| b.code_expiration_minutes)
        {
            validate::code_expiration_minutes(
                minutes,
                "library_template_body_inputs.code_expiration_minutes",
            )?;
        }
        self.client
            .post_at(&[self.waba_id.as_str(), "message_templates"])
            .json(request)
            .context("create message template response")
            .send()
            .await
    }

    /// Recreate `source`'s templates in this WABA
    /// (`POST /{this_waba}/migrate_message_templates`). Only `APPROVED`
    /// templates with a `GREEN` or `UNKNOWN` quality score migrate, and only
    /// between WABAs of the same business.
    ///
    /// The parameters are sent as a JSON body, as the page's request syntax
    /// shows; its example passes them in the query string instead.
    pub async fn migrate_from(
        &self,
        source: &WabaId,
        options: &MigrationOptions,
    ) -> Result<MigrationResult> {
        #[derive(Serialize)]
        struct Body<'a> {
            source_waba_id: &'a WabaId,
            #[serde(flatten)]
            options: &'a MigrationOptions,
        }
        // `templates/template-migration`: `count` "maximum count of 500",
        // `template_ids` "max array length of 500".
        if options.count.is_some_and(|c| c == 0 || c > 500) {
            return Err(ValidationError::new("count", "1 to 500").into());
        }
        if options.template_ids.len() > 500 {
            return Err(ValidationError::new("template_ids", "at most 500 ids").into());
        }
        self.client
            .post_at(&[self.waba_id.as_str(), "migrate_message_templates"])
            .json(&Body {
                source_waba_id: source,
                options,
            })
            .context("migrate message templates response")
            .send()
            .await
    }

    /// Compare template `id` with `other` over `[start, end]`
    /// (`GET /{template_id}/compare`). Both must belong to the same WABA and
    /// have been sent 1000+ times in the window.
    ///
    /// `start`/`end` are passed through unchanged: the page calls them UNIX
    /// timestamps and describes 7/30/60/90-day windows in seconds, but its
    /// example uses millisecond values, so no unit or window is enforced.
    pub async fn compare(
        &self,
        id: &TemplateId,
        other: &TemplateId,
        start: i64,
        end: i64,
    ) -> Result<Vec<ComparisonMetric>> {
        let list: DataList<ComparisonMetric> = self
            .client
            .get_at(&[id.as_str(), "compare"])
            .query("template_ids", id_list(std::slice::from_ref(other)))
            .query("start", start)
            .query("end", end)
            .context("template comparison")
            .send()
            .await?;
        Ok(list.data)
    }

    /// Unpause a template paused by quality or pacing
    /// (`POST /{template_id}/unpause`, `templates/template-pausing`). The
    /// docs do not show the response, so it is returned as raw JSON.
    pub async fn unpause(&self, id: &TemplateId) -> Result<serde_json::Value> {
        self.client
            .post_at(&[id.as_str(), "unpause"])
            .context("unpause message template response")
            .send()
            .await
    }
}

/// Reject an empty or blank required string.
pub(crate) fn not_empty(value: &str, field: &str) -> std::result::Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    Ok(())
}
