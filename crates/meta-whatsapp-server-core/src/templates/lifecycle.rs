//! A template's life after its creation (roadmap M5b): an edit, the
//! Template Library (browsed, and a template created from it), a migration
//! from another of the tenant's WABAs, a comparison of two templates, an
//! unpause, archiving and unarchiving. Each goes through the library's
//! call (`client::templates::Templates`), after the checks the service
//! adds: every template id a request names is the WABA's before anything
//! acts on it, a single id by its name ([`Templates::owned_template`]), a
//! list in one walk of the WABA's templates ([`Templates::all_in_waba`];
//! design D116), and a migration's source is a WABA of the caller's own
//! tenant (design D117).
//!
//! Pages: `templates/template-management` (§ Edit templates),
//! `templates/template-library`, `templates/template-migration`,
//! `templates/template-comparison`, `templates/template-pausing`,
//! `templates/template-archival`,
//! `reference/whatsapp-business-account/message-template-api`.

use meta_whatsapp_rs::client::templates::{
    ArchiveResult, ComparisonMetric, LibraryQuery, LibraryTemplate, LibraryTemplateRequest,
    MigrationOptions, MigrationResult, TemplateEdit, TemplateStatus, UnarchiveResult,
};
use meta_whatsapp_rs::core::ids::{TemplateId, WabaId};
use serde_json::Value;

use super::{TemplateCreated, Templates, template_name, zero_tap_terms};
use crate::error::ServiceError;
use crate::request::{graph_id, meta_object, typed_graph_id};

/// Most template ids one archive or unarchive names
/// (`templates/template-archival`: "up to 100 template IDs per request").
pub const MAX_BULK_IDS: usize = 100;

/// Most template ids one migration names, and the most templates it
/// migrates (`templates/template-migration`: `template_ids` "max array
/// length of 500", `count` "a maximum count of 500").
pub const MAX_MIGRATION_IDS: usize = 500;

/// Longest Template Library search accepted. Meta's page states none; the
/// bound keeps a query string's value short (a library template's content
/// is a body of at most 1,024 characters, `templates/overview`).
const MAX_SEARCH_CHARS: usize = 1024;

// ─── The requests' checks ────────────────────────────────────────────────

/// A refusal of the zero-tap terms anywhere in `body` (the creation's
/// rule, [`super::ZERO_TAP_TERMS`]): an edit's buttons and a library
/// template's button inputs may carry `zero_tap_terms_accepted` too.
fn no_zero_tap_terms(body: &Value) -> Result<(), ServiceError> {
    match zero_tap_terms(body, "") {
        Some(path) => {
            tracing::warn!(
                field = path.as_str(),
                "refused a template accepting Meta's zero-tap terms: a legal act the service does not take for a tenant"
            );
            Err(ServiceError::invalid(path))
        }
        None => Ok(()),
    }
}

/// The edit a request carries, in Meta's JSON (`POST /{TEMPLATE_ID}`'s
/// body: `category`, `components`, `parameter_format`,
/// `allow_category_change`, `cta_url_link_tracking_opted_out`,
/// `message_send_ttl_seconds`, `sub_category`, `display_format`,
/// `is_primary_device_delivery_only`), read with no key dropped and
/// checked against every limit the docs state. A template's `name` and
/// `language` cannot be edited (`templates/template-management`: "You can
/// only edit a template's category, components, or time-to-live"), so they
/// are refused on their path.
///
/// # Errors
///
/// `422 invalid_request`: on `body` (not an edit), on the path of a key
/// the library would not send, of a `zero_tap_terms_accepted: true`, or of
/// the field breaking a documented limit (`edit` when it changes nothing).
pub fn edit(body: &Value) -> Result<TemplateEdit, ServiceError> {
    let edit: TemplateEdit = meta_object("", body)?;
    no_zero_tap_terms(body)?;
    edit.validate()
        .map_err(|invalid| ServiceError::invalid(invalid.field))?;
    Ok(edit)
}

/// A template from the Template Library a request carries, in Meta's JSON
/// (`name`, `language`, `category`, `library_template_name`,
/// `library_template_button_inputs` as an array of objects,
/// `library_template_body_inputs`), read with no key dropped and checked
/// as the library checks it, before any request.
///
/// # Errors
///
/// `422 invalid_request` on `body`, on a dropped key's path, on a
/// `zero_tap_terms_accepted: true` (`library_template_button_inputs[0].zero_tap_terms_accepted`),
/// on `name`, `language` or `library_template_name`.
pub fn library_request(body: &Value) -> Result<LibraryTemplateRequest, ServiceError> {
    let request: LibraryTemplateRequest = meta_object("", body)?;
    no_zero_tap_terms(body)?;
    template_name("name", &request.name)?;
    if request.language.trim().is_empty() {
        return Err(ServiceError::invalid("language"));
    }
    if request.library_template_name.trim().is_empty() {
        return Err(ServiceError::invalid("library_template_name"));
    }
    Ok(request)
}

/// The Template Library's filters, as the request carried them
/// (`templates/template-library`, "Query string parameters").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryFilter {
    /// A substring of a library template's content, name, header, body or
    /// footer.
    pub search: Option<String>,
    /// Its topic (`ORDER_MANAGEMENT`, `PAYMENTS`, …).
    pub topic: Option<String>,
    /// Its use case (`SHIPMENT_CONFIRMATION`, …).
    pub usecase: Option<String>,
    /// Its industry (`E_COMMERCE`, `FINANCIAL_SERVICES`).
    pub industry: Option<String>,
    /// Its language (`en_US`).
    pub language: Option<String>,
    /// Its exact name (`delivery_update_1`).
    pub name: Option<String>,
}

/// One of Meta's enum values as a filter: letters and `_`, at most 64,
/// upper-cased as Meta writes them, else `422` on `field`.
fn enum_filter(field: &'static str, value: Option<String>) -> Result<Option<String>, ServiceError> {
    value
        .map(|value| {
            let valid = !value.is_empty()
                && value.len() <= 64
                && value.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_');
            if valid {
                Ok(value.to_ascii_uppercase())
            } else {
                Err(ServiceError::invalid(field))
            }
        })
        .transpose()
}

impl LibraryFilter {
    /// The library's query, each filter checked: `search` 1 to 1,024
    /// characters; `topic`, `usecase` and `industry` letters and `_`
    /// (upper-cased); `language` a locale code (letters, digits and `_`, at
    /// most 16); `name` a template name.
    fn query(self) -> Result<LibraryQuery, ServiceError> {
        let Self {
            search,
            topic,
            usecase,
            industry,
            language,
            name,
        } = self;
        if let Some(search) = &search
            && (search.trim().is_empty() || search.chars().count() > MAX_SEARCH_CHARS)
        {
            return Err(ServiceError::invalid("search"));
        }
        if let Some(language) = &language {
            let valid = !language.is_empty()
                && language.len() <= 16
                && language
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if !valid {
                return Err(ServiceError::invalid("language"));
            }
        }
        if let Some(name) = &name {
            template_name("name", name)?;
        }
        Ok(LibraryQuery {
            search,
            topic: enum_filter("topic", topic)?,
            usecase: enum_filter("usecase", usecase)?,
            industry: enum_filter("industry", industry)?,
            language,
            name,
        })
    }
}

/// A migration, as the request carried it (`templates/template-migration`:
/// `source_waba_id`, `page_number`, `count`, `template_ids`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationRequest {
    /// The WABA whose templates are recreated here: one of the caller's
    /// tenant's.
    pub source_waba_id: String,
    /// The zero-based batch of 500 templates.
    pub page_number: Option<u32>,
    /// The batch's size, 1 to 500.
    pub count: Option<u32>,
    /// Only these templates of the source, at most 500.
    pub template_ids: Vec<String>,
}

/// A migration checked: its source WABA's id and the library's options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The source WABA.
    pub source: WabaId,
    /// What to migrate.
    pub options: MigrationOptions,
}

/// The migration `request`, checked before any request.
///
/// # Errors
///
/// `422 invalid_request` on `source_waba_id` (not a Meta id), `count` (not
/// 1 to 500), `template_ids` (more than 500) or `template_ids[i]` (not a
/// Meta id).
pub fn migration(request: MigrationRequest) -> Result<Migration, ServiceError> {
    let MigrationRequest {
        source_waba_id,
        page_number,
        count,
        template_ids,
    } = request;
    let source: WabaId = typed_graph_id("source_waba_id", &source_waba_id)?;
    if count.is_some_and(|c| c == 0 || c as usize > MAX_MIGRATION_IDS) {
        return Err(ServiceError::invalid("count"));
    }
    if template_ids.len() > MAX_MIGRATION_IDS {
        return Err(ServiceError::invalid("template_ids"));
    }
    let template_ids = ids_at("template_ids", &template_ids)?;
    Ok(Migration {
        source,
        options: MigrationOptions {
            page_number,
            count,
            template_ids,
        },
    })
}

/// `ids`, each a Meta id, else `422` on `field[i]`.
pub(super) fn ids_at(field: &str, ids: &[String]) -> Result<Vec<TemplateId>, ServiceError> {
    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            let at = || ServiceError::invalid(format!("{field}[{i}]"));
            graph_id("id", id).map_err(|_| at())?;
            TemplateId::new(id.as_str()).map_err(|_| at())
        })
        .collect()
}

/// An archive's or unarchive's template ids (Meta's `hsm_ids`): 1 to 100
/// Meta ids, checked before any request.
///
/// # Errors
///
/// `422 invalid_request` on `hsm_ids` (none, or more than 100) or
/// `hsm_ids[i]` (not a Meta id).
pub fn bulk_ids(ids: &[String]) -> Result<Vec<TemplateId>, ServiceError> {
    if ids.is_empty() || ids.len() > MAX_BULK_IDS {
        return Err(ServiceError::invalid("hsm_ids"));
    }
    ids_at("hsm_ids", ids)
}

// ─── The operations ──────────────────────────────────────────────────────

impl Templates<'_> {
    /// Edit the template `id` with `edit` (read by [`edit`]), if it is the
    /// WABA's (looked for by its name first, as a get is): `POST
    /// /{template_id}`, the library's `Templates::edit`, not replayed on a
    /// timeout (an approved template's edit counts against its limit). The
    /// WABA's cached lists go, whatever Meta answers; an edit is the
    /// caller's audit event (`template_edited`).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `id`, before any request. `404 not_found`
    /// for a template not the WABA's, and nothing edited. Meta's refusal
    /// or failure, with its `details` (a template whose status forbids
    /// editing is Meta's).
    pub async fn edit(&self, id: &str, edit: &TemplateEdit) -> Result<(), ServiceError> {
        let id: TemplateId = typed_graph_id("id", id)?;
        self.owned_template(&id, &["name"]).await?;
        let edited = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .edit(&id, edit)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match edited {
            Ok(()) => {
                self.audit("template_edited");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Unpause the template `id`, if it is the WABA's: `POST
    /// /{template_id}/unpause` (`templates/template-pausing`), the
    /// library's `Templates::unpause`. Meta's page prints no answer, so
    /// none is kept. The WABA's cached lists go; an unpause is the caller's
    /// audit event (`template_unpaused`).
    ///
    /// # Errors
    ///
    /// As [`Self::edit`].
    pub async fn unpause(&self, id: &str) -> Result<(), ServiceError> {
        let id: TemplateId = typed_graph_id("id", id)?;
        self.owned_template(&id, &["name"]).await?;
        let unpaused = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .unpause(&id)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match unpaused {
            Ok(_) => {
                self.audit("template_unpaused");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Compare the template `id` with `other` over `[start, end]` (UNIX
    /// timestamps, passed as given: the page's example uses milliseconds),
    /// both the WABA's (each looked for by its name first): `GET
    /// /{template_id}/compare?template_ids=[other]&start=…&end=…`
    /// (`templates/template-comparison`), the library's
    /// `Templates::compare`. Never cached.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `id` or `template_ids` (not a Meta id, or
    /// `id` itself), before any request. `404 not_found` when either is not
    /// the WABA's. Meta's refusal or failure, with its `details` (too few
    /// sends in the window is Meta's).
    pub async fn compare(
        &self,
        id: &str,
        other: &str,
        start: i64,
        end: i64,
    ) -> Result<Vec<ComparisonMetric>, ServiceError> {
        let id: TemplateId = typed_graph_id("id", id)?;
        let other: TemplateId = typed_graph_id("template_ids", other)?;
        if other == id {
            return Err(ServiceError::invalid("template_ids"));
        }
        self.owned_template(&id, &["name"]).await?;
        self.owned_template(&other, &["name"]).await?;
        let compared = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .compare(&id, &other, start, end)
            .await;
        match compared {
            Ok(metrics) => Ok(metrics),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Archive `ids` (read by [`bulk_ids`]), every one the WABA's (one walk
    /// of its templates finds them all, or nothing is archived): `POST
    /// /{waba_id}/message_templates/archive`, the library's
    /// `Templates::archive`, not replayed on a timeout. Meta's answer, its
    /// partial failures included. The WABA's cached lists go; an archive
    /// is the caller's audit event (`templates_archived`).
    ///
    /// # Errors
    ///
    /// `404 not_found` when one id is not the WABA's, and nothing archived.
    /// Meta's refusal or failure, with its `details`.
    pub async fn archive(&self, ids: &[TemplateId]) -> Result<ArchiveResult, ServiceError> {
        self.all_in_waba(self.waba, ids, None).await?;
        let archived = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .archive(ids)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match archived {
            Ok(result) => {
                self.audit("templates_archived");
                Ok(result)
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Unarchive `ids`, as [`Self::archive`] archives them, found among the
    /// WABA's archived templates (the walk asks for `status=ARCHIVED`):
    /// `POST /{waba_id}/message_templates/unarchive`. The caller's audit
    /// event is `templates_unarchived`.
    ///
    /// # Errors
    ///
    /// As [`Self::archive`].
    pub async fn unarchive(&self, ids: &[TemplateId]) -> Result<UnarchiveResult, ServiceError> {
        self.all_in_waba(self.waba, ids, Some(TemplateStatus::Archived))
            .await?;
        let unarchived = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .unarchive(ids)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match unarchived {
            Ok(result) => {
                self.audit("templates_unarchived");
                Ok(result)
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The Template Library's entries matching `filter` (`GET
    /// /message_template_library`, the library's `Templates::library`), with
    /// the WABA's token: Meta's first page (the page documents no cursor).
    /// Never cached.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on a filter, before any request. Meta's
    /// refusal or failure, with its `details`.
    pub async fn library(
        &self,
        filter: LibraryFilter,
    ) -> Result<Vec<LibraryTemplate>, ServiceError> {
        let query = filter.query()?;
        let page = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .library(&query)
            .await;
        match page {
            Ok(page) => Ok(page.data),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Create a template in the WABA from the Template Library's entry
    /// `request` names (read by [`library_request`]): `POST
    /// /{waba_id}/message_templates` with `library_template_name`, the
    /// library's `Templates::create_from_library`. The WABA's cached lists
    /// go either way.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (`422
    /// template_rejected`, `409 template_limit_reached`; a timeout
    /// `may_have_been_sent`).
    pub async fn create_from_library(
        &self,
        request: &LibraryTemplateRequest,
    ) -> Result<TemplateCreated, ServiceError> {
        let created = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .create_from_library(request)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match created {
            Ok(created) => Ok(TemplateCreated::from(created)),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Recreate `migration`'s templates in this WABA (`POST
    /// /{waba_id}/migrate_message_templates`, the library's
    /// `Templates::migrate_from`) with this WABA's token, from a source
    /// WABA of the caller's own tenant (opened as the caller's, so another
    /// tenant's, or none, is `404` before its token is read; design D117),
    /// its `template_ids` found among the source's templates first (one
    /// walk, with the source's token). Meta's answer, its failures
    /// included. The WABA's cached lists go; a migration is the caller's
    /// audit event (`templates_migrated`).
    ///
    /// # Errors
    ///
    /// `404 not_found`: a source not the caller's tenant's, or an id not
    /// the source's; nothing migrated. `409` for a source whose token is
    /// unusable (`reconnect_required`). Meta's refusal or failure, with its
    /// `details` (a source of another business is Meta's refusal).
    pub async fn migrate(&self, migration: &Migration) -> Result<MigrationResult, ServiceError> {
        let source = self
            .authz
            .owned_waba(self.caller, migration.source.clone())
            .await?;
        self.all_in_waba(&source, &migration.options.template_ids, None)
            .await?;
        let migrated = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .migrate_from(&migration.source, &migration.options)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match migrated {
            Ok(result) => {
                self.audit("templates_migrated");
                Ok(result)
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    /// An edit is Meta's object, with no key dropped and no name, checked
    /// by the library, zero-tap terms refused wherever they are.
    /// Decisive: each check of [`edit`].
    #[test]
    fn an_edit_is_read_then_checked() {
        let good = json!({"category": "MARKETING"});
        assert!(edit(&good).unwrap().category.is_some());
        for (body, at) in [
            (json!({"name": "renamed"}), "name"),
            (json!({"language": "de"}), "language"),
            (json!({}), "edit"),
            (json!("x"), "body"),
            (
                json!({"components": [{"type": "BUTTONS", "buttons": [{"type": "OTP",
                    "otp_type": "ZERO_TAP", "zero_tap_terms_accepted": true}]}]}),
                "components[0].buttons[0].zero_tap_terms_accepted",
            ),
            (
                json!({"message_send_ttl_seconds": 1, "category": "UTILITY"}),
                "message_send_ttl_seconds",
            ),
        ] {
            assert_eq!(field(&edit(&body).unwrap_err()), at, "{body}");
        }
    }

    /// A library creation: Meta's object, its names checked, zero-tap
    /// terms refused. Decisive: each check of [`library_request`].
    #[test]
    fn a_library_creation_is_read_then_checked() {
        let good = json!({"name": "my_delivery_update", "language": "en_US",
            "category": "UTILITY", "library_template_name": "delivery_update_1"});
        assert_eq!(
            library_request(&good).unwrap().library_template_name,
            "delivery_update_1"
        );
        let with = |key: &str, value: Value| {
            let mut body = good.clone();
            body[key] = value;
            body
        };
        for (body, at) in [
            (with("name", json!("My Update")), "name"),
            (with("language", json!(" ")), "language"),
            (
                with("library_template_name", json!("")),
                "library_template_name",
            ),
            (
                with("library_templat_name", json!("x")),
                "library_templat_name",
            ),
            (
                with(
                    "library_template_button_inputs",
                    json!([{"type": "OTP", "zero_tap_terms_accepted": true}]),
                ),
                "library_template_button_inputs[0].zero_tap_terms_accepted",
            ),
        ] {
            assert_eq!(field(&library_request(&body).unwrap_err()), at, "{body}");
        }
    }

    /// The library's filters: Meta's enums upper-cased, the rest checked.
    /// Decisive: each filter's check.
    #[test]
    fn the_librarys_filters_are_checked() {
        let query = LibraryFilter {
            search: Some("payments".to_owned()),
            topic: Some("payments".to_owned()),
            usecase: Some("low_balance_warning".to_owned()),
            industry: Some("e_commerce".to_owned()),
            language: Some("en_US".to_owned()),
            name: Some("delivery_update_1".to_owned()),
        }
        .query()
        .unwrap();
        assert_eq!(
            (
                query.topic.as_deref(),
                query.usecase.as_deref(),
                query.industry.as_deref()
            ),
            (
                Some("PAYMENTS"),
                Some("LOW_BALANCE_WARNING"),
                Some("E_COMMERCE")
            )
        );
        let refused = |filter: LibraryFilter| field(&filter.query().unwrap_err());
        let one = |set: &dyn Fn(&mut LibraryFilter)| {
            let mut filter = LibraryFilter::default();
            set(&mut filter);
            filter
        };
        assert_eq!(refused(one(&|f| f.search = Some(" ".to_owned()))), "search");
        assert_eq!(
            refused(one(&|f| f.search = Some("x".repeat(1025)))),
            "search"
        );
        assert!(one(&|f| f.search = Some("x".repeat(1024))).query().is_ok());
        assert_eq!(refused(one(&|f| f.topic = Some("A&B".to_owned()))), "topic");
        assert_eq!(
            refused(one(&|f| f.usecase = Some("A1".to_owned()))),
            "usecase"
        );
        assert_eq!(
            refused(one(&|f| f.industry = Some(String::new()))),
            "industry"
        );
        assert_eq!(
            refused(one(&|f| f.language = Some("en-US".to_owned()))),
            "language"
        );
        assert_eq!(refused(one(&|f| f.name = Some("A".to_owned()))), "name");
    }

    /// A migration's ids and limits, and an archive's. Decisive: each
    /// limit, and each id's check.
    #[test]
    fn a_migrations_and_an_archives_ids_are_checked() {
        let request = |source: &str, count: Option<u32>, ids: Vec<String>| MigrationRequest {
            source_waba_id: source.to_owned(),
            page_number: Some(0),
            count,
            template_ids: ids,
        };
        let checked =
            migration(request("102290129340398", Some(500), vec!["1".to_owned()])).unwrap();
        assert_eq!(checked.source.as_str(), "102290129340398");
        assert_eq!(checked.options.template_ids.len(), 1);
        for (bad, at) in [
            (request("WABA", None, vec![]), "source_waba_id"),
            (request("0102", None, vec![]), "source_waba_id"),
            (request("1", Some(0), vec![]), "count"),
            (request("1", Some(501), vec![]), "count"),
            (
                request("1", None, vec!["1".to_owned(); 501]),
                "template_ids",
            ),
            (
                request("1", None, vec!["1".to_owned(), "x".to_owned()]),
                "template_ids[1]",
            ),
        ] {
            assert_eq!(field(&migration(bad).unwrap_err()), at);
        }
        assert!(migration(request("1", None, vec!["1".to_owned(); 500])).is_ok());
        assert_eq!(bulk_ids(&vec!["1".to_owned(); 100]).unwrap().len(), 100);
        assert_eq!(field(&bulk_ids(&[]).unwrap_err()), "hsm_ids");
        assert_eq!(
            field(&bulk_ids(&vec!["1".to_owned(); 101]).unwrap_err()),
            "hsm_ids"
        );
        assert_eq!(
            field(&bulk_ids(&["1".to_owned(), "1/2".to_owned()]).unwrap_err()),
            "hsm_ids[1]"
        );
    }
}
