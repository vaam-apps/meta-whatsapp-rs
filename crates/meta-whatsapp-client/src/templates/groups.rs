//! Template groups (`templates/template-groups`) and bulk archiving
//! (`templates/template-archival`).
//!
//! A template group gathers templates so their analytics can be read as a
//! set ([`crate::analytics::Analytics::template_group`]). A template is in
//! at most one group; group names are unique; removing a template from a
//! group keeps the metrics it already contributed.
//!
//! Archiving takes templates out of use: an archived template cannot be
//! sent and is deleted 28 days later unless it is unarchived first, which
//! restores its previous status (`APPROVED` when there is none). Each
//! archived or unarchived template also arrives as a
//! `message_template_status_update` webhook.
//!
//! Not wrapped, because no page documents them: the list of a WABA's
//! groups (`GET /{WABA_ID}/template_groups`, named only in Meta's
//! changelog of 2025-02-14; its Graph API reference page shows no
//! operation), and the pages after the first of a group's templates (the
//! group's `whatsapp_business_templates` carries cursors, but no page says
//! how to request the next one).

use std::collections::BTreeMap;

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{TemplateGroupId, TemplateId};
use meta_whatsapp_core::paging::Page;
use serde::{Deserialize, Serialize};

use super::{TemplateInfo, Templates};
use crate::waba::WabaInfo;

/// `templates/template-groups`: a group name is "Maximum 256 characters".
const MAX_GROUP_NAME: usize = 256;
/// `templates/template-groups`: a group description is "Maximum 512
/// characters".
const MAX_GROUP_DESCRIPTION: usize = 512;
/// `templates/template-archival`: "up to 100 template IDs per request", for
/// both archive and unarchive.
const MAX_ARCHIVE_IDS: usize = 100;

type Check = std::result::Result<(), ValidationError>;

/// Body of `POST /{WABA_ID}/template_groups`: a new template group.
///
/// The template ids are written as JSON numbers, as the page's example
/// does (`"whatsapp_business_templates": [278077987957091, …]`, typed
/// "Array of integers"), so each must be digits that fit in a `u64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateGroupCreate {
    /// Group name: required, unique, at most 256 characters.
    pub name: String,
    /// Description, at most 512 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ids of the templates to put in the group (required by the page).
    #[serde(with = "numeric_ids")]
    pub whatsapp_business_templates: Vec<TemplateId>,
}

impl TemplateGroupCreate {
    /// A group called `name` holding `templates`.
    pub fn new<I>(name: impl Into<String>, templates: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<TemplateId>,
    {
        Self {
            name: name.into(),
            description: None,
            whatsapp_business_templates: templates.into_iter().map(Into::into).collect(),
        }
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Check the limits `templates/template-groups` states: a non-blank
    /// name of at most 256 characters, a description of at most 512, and
    /// template ids that can be written as the integers the page asks for.
    ///
    /// An empty template list is left to Meta: the page calls the field
    /// required without saying whether it may be empty.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        group_name(&self.name)?;
        if let Some(description) = &self.description {
            group_description(description)?;
        }
        integer_ids(
            &self.whatsapp_business_templates,
            "whatsapp_business_templates",
        )
    }
}

/// Body of `POST /{TEMPLATE_GROUP_ID}`: what to change in a template group.
/// Every field is optional; unset ones are not sent.
///
/// Like [`TemplateGroupCreate`], the template ids are written as JSON
/// numbers (the page's example: `"add_templates": [844006344365270, …]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateGroupUpdate {
    /// New name: unique, at most 256 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New description, at most 512 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Templates to add to the group.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "numeric_ids")]
    pub add_templates: Vec<TemplateId>,
    /// Templates to remove from the group (their metrics so far stay in
    /// the group's).
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "numeric_ids")]
    pub remove_templates: Vec<TemplateId>,
}

impl TemplateGroupUpdate {
    /// An update that changes nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rename the group.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Replace the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add these templates to the group.
    #[must_use]
    pub fn add_templates<I>(mut self, ids: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<TemplateId>,
    {
        self.add_templates.extend(ids.into_iter().map(Into::into));
        self
    }

    /// Remove these templates from the group.
    #[must_use]
    pub fn remove_templates<I>(mut self, ids: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<TemplateId>,
    {
        self.remove_templates
            .extend(ids.into_iter().map(Into::into));
        self
    }

    /// Check the same limits as [`TemplateGroupCreate::validate`] on the
    /// fields that are set, and that something is set.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        if self == &Self::default() {
            return Err(ValidationError::new("update", "nothing to change"));
        }
        if let Some(name) = &self.name {
            group_name(name)?;
        }
        if let Some(description) = &self.description {
            group_description(description)?;
        }
        integer_ids(&self.add_templates, "add_templates")?;
        integer_ids(&self.remove_templates, "remove_templates")
    }
}

/// Response of a group creation: `{"id": "<TEMPLATE_GROUP_ID>"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateGroupCreated {
    /// The new group's id.
    pub id: TemplateGroupId,
}

/// A template group, from `GET /{TEMPLATE_GROUP_ID}`.
///
/// Only `id` is sure to be present; the rest is optional so that a field
/// Meta leaves out never fails parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateGroup {
    /// Group id.
    pub id: TemplateGroupId,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When the group was created, as Meta writes it (ISO 8601 with a
    /// `+0000` offset, e.g. `2025-01-06T23:05:12+0000`), kept verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_time: Option<String>,
    /// When the group was last updated, in the same format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
    /// The WABA that owns the group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_business_account: Option<WabaInfo>,
    /// The first page of the group's templates, with their components.
    /// Its cursors are kept, but no page documents how to request the
    /// next one, so this client offers no call that does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_business_templates: Option<Page<TemplateInfo>>,
}

/// Response of `POST /{WABA_ID}/message_templates/archive`.
///
/// Meta answers with the templates it archived and, for each one it did
/// not, why (`"Only approved or rejected templates can be archived"`): a
/// partial failure is not an error, so check `failed_templates`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveResult {
    /// Ids of the templates archived.
    #[serde(default)]
    pub archived_templates: Vec<TemplateId>,
    /// Template id → why it was not archived.
    #[serde(default)]
    pub failed_templates: BTreeMap<TemplateId, String>,
}

/// Response of `POST /{WABA_ID}/message_templates/unarchive`; partial
/// failures as in [`ArchiveResult`] (`"Template is not archived"`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnarchiveResult {
    /// Ids of the templates unarchived.
    #[serde(default)]
    pub unarchived_templates: Vec<TemplateId>,
    /// Template id → why it was not unarchived.
    #[serde(default)]
    pub failed_templates: BTreeMap<TemplateId, String>,
}

/// The body of an archive or unarchive call: ids as JSON strings, as the
/// page's request syntax writes them (`"hsm_ids": ["<TEMPLATE_ID_1>", …]`).
#[derive(Serialize)]
struct HsmIds<'a> {
    hsm_ids: &'a [TemplateId],
}

impl Templates {
    /// Create a template group (`POST /{WABA_ID}/template_groups`) after
    /// [`TemplateGroupCreate::validate`]. Not replayed on timeouts: a
    /// replay could meet the first attempt's group, whose name must be
    /// unique.
    pub async fn create_group(&self, group: &TemplateGroupCreate) -> Result<TemplateGroupCreated> {
        group.validate()?;
        self.client
            .post_at(&[self.waba_id.as_str(), "template_groups"])
            .json(group)
            .context("create template group response")
            .send()
            .await
    }

    /// A template group with its WABA and the first page of its templates
    /// (`GET /{TEMPLATE_GROUP_ID}`). `id` must be digits only: it is
    /// checked before any request.
    pub async fn get_group(&self, id: &TemplateGroupId) -> Result<TemplateGroup> {
        group_id(id)?;
        self.client
            .get_at(&[id.as_str()])
            .context("template group")
            .send()
            .await
    }

    /// Rename a group, change its description, or add and remove templates
    /// (`POST /{TEMPLATE_GROUP_ID}`), after checking `id` is digits only and
    /// [`TemplateGroupUpdate::validate`]. Not replayed on timeouts.
    pub async fn update_group(
        &self,
        id: &TemplateGroupId,
        update: &TemplateGroupUpdate,
    ) -> Result<()> {
        group_id(id)?;
        update.validate()?;
        self.client
            .post_at(&[id.as_str()])
            .json(update)
            .context("update template group response")
            .send_success()
            .await
    }

    /// Delete a template group (`DELETE /{TEMPLATE_GROUP_ID}`); its
    /// templates are kept. `id` must be digits only: it is checked before
    /// any request.
    pub async fn delete_group(&self, id: &TemplateGroupId) -> Result<()> {
        group_id(id)?;
        self.client
            .delete_at(&[id.as_str()])
            .context("delete template group response")
            .send_success()
            .await
    }

    /// Archive up to 100 templates (`POST /{WABA_ID}/message_templates/archive`).
    /// Each id must be digits only. Not replayed on timeouts.
    ///
    /// Sent to the client's Graph endpoint, like every other template call;
    /// Meta's page prints the request against `https://api.facebook.com/`
    /// with no API version.
    pub async fn archive(&self, ids: &[TemplateId]) -> Result<ArchiveResult> {
        archive_ids(ids)?;
        self.client
            .post_at(&[self.waba_id.as_str(), "message_templates", "archive"])
            .json(&HsmIds { hsm_ids: ids })
            .context("archive message templates response")
            .send()
            .await
    }

    /// Unarchive up to 100 templates archived in the last 28 days
    /// (`POST /{WABA_ID}/message_templates/unarchive`); each goes back to
    /// its status before archival and its 12-month inactivity clock
    /// restarts. Each id must be digits only. Not replayed on timeouts;
    /// sent to the Graph endpoint as [`Self::archive`] is.
    pub async fn unarchive(&self, ids: &[TemplateId]) -> Result<UnarchiveResult> {
        archive_ids(ids)?;
        self.client
            .post_at(&[self.waba_id.as_str(), "message_templates", "unarchive"])
            .json(&HsmIds { hsm_ids: ids })
            .context("unarchive message templates response")
            .send()
            .await
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// A template group id is a Graph object id: digits only. Checked so that
/// a stray value (`123/template_groups`, `..`, a name) fails here instead of
/// addressing another object with the tenant's token.
fn group_id(id: &TemplateGroupId) -> Check {
    if !digits(id.as_str()) {
        return Err(ValidationError::new(
            "template_group_id",
            "must be a template group id: digits only",
        ));
    }
    Ok(())
}

fn group_name(name: &str) -> Check {
    if name.trim().is_empty() {
        return Err(ValidationError::new("name", "must not be empty"));
    }
    if name.chars().count() > MAX_GROUP_NAME {
        return Err(ValidationError::new(
            "name",
            format!("must be at most {MAX_GROUP_NAME} characters"),
        ));
    }
    Ok(())
}

fn group_description(description: &str) -> Check {
    if description.chars().count() > MAX_GROUP_DESCRIPTION {
        return Err(ValidationError::new(
            "description",
            format!("must be at most {MAX_GROUP_DESCRIPTION} characters"),
        ));
    }
    Ok(())
}

/// Ids written as JSON integers: digits that fit in a `u64`.
fn integer_ids(ids: &[TemplateId], field: &str) -> Check {
    for (i, id) in ids.iter().enumerate() {
        if !digits(id.as_str()) || id.as_str().parse::<u64>().is_err() {
            return Err(ValidationError::new(
                format!("{field}[{i}]"),
                "must be a template id: digits only, at most 2^64 - 1",
            ));
        }
    }
    Ok(())
}

fn archive_ids(ids: &[TemplateId]) -> Check {
    if ids.is_empty() || ids.len() > MAX_ARCHIVE_IDS {
        return Err(ValidationError::new(
            "hsm_ids",
            format!("1 to {MAX_ARCHIVE_IDS} template ids"),
        ));
    }
    for (i, id) in ids.iter().enumerate() {
        if !digits(id.as_str()) {
            return Err(ValidationError::new(
                format!("hsm_ids[{i}]"),
                "must be a template id: digits only",
            ));
        }
    }
    Ok(())
}

/// Template ids as JSON integers on the wire. Reading accepts integers or
/// strings, so a body written either way parses.
mod numeric_ids {
    use meta_whatsapp_core::ids::TemplateId;
    use serde::Deserialize;
    use serde::de::Deserializer;
    use serde::ser::{Error as _, SerializeSeq, Serializer};

    pub(super) fn serialize<S: Serializer>(
        ids: &[TemplateId],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(ids.len()))?;
        for id in ids {
            // `validate` refuses these first; `u64::from_str` alone would
            // accept a leading `+`.
            if !super::digits(id.as_str()) {
                return Err(S::Error::custom("a template id must be digits only"));
            }
            let n: u64 = id
                .as_str()
                .parse()
                .map_err(|_| S::Error::custom("a template id must fit in a u64"))?;
            seq.serialize_element(&n)?;
        }
        seq.end()
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<TemplateId>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(u64),
            Text(String),
        }
        Ok(Vec::<Raw>::deserialize(deserializer)?
            .into_iter()
            .map(|raw| match raw {
                Raw::Number(n) => TemplateId::new(n.to_string()),
                Raw::Text(s) => TemplateId::new(s),
            })
            .collect())
    }
}
