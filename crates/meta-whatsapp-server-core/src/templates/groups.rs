//! Template groups (roadmap M5b, `templates/template-groups`): a WABA's
//! sets of templates, read together in analytics. Each operation goes
//! through the library's call (`client::templates::Templates`), after the
//! design's M5.2 check: **a group id is the WABA's, or it does not exist**.
//! Meta's group object names its WABA (`whatsapp_business_account`), so a
//! group is read first (`GET /{group_id}`) and acted on only when that WABA
//! is the path's; a group of another WABA, an id Meta refuses, or another
//! kind of node (which names no such WABA) is `404 not_found`, without
//! Meta's code or text, and nothing of it is answered, changed or deleted
//! (`DELETE /{id}` deletes whatever node an id names). The templates a
//! creation or an update adds are found among the WABA's first, in one walk
//! of its list (design D116).
//!
//! Not offered, because no page documents them: the list of a WABA's
//! groups, and the pages after the first of a group's templates (the
//! library's own gaps, `client::templates::TemplateGroup`).

use meta_whatsapp_rs::client::templates::{
    TemplateGroup, TemplateGroupCreate, TemplateGroupUpdate,
};
use meta_whatsapp_rs::core::ids::{TemplateGroupId, TemplateId};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{Template, Templates};
use crate::authz::{Authorizer, OwnedWaba, object_failed};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::{dropped_key, graph_id};

/// A template group, as Meta describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateGroupDetails {
    /// Its id.
    pub id: TemplateGroupId,
    /// Its name.
    pub name: Option<String>,
    /// Its description.
    pub description: Option<String>,
    /// When it was created, as Meta writes it (`2025-01-06T23:05:12+0000`).
    pub creation_time: Option<String>,
    /// When it was last updated, in the same format.
    pub update_time: Option<String>,
    /// The first page of its templates (Meta's
    /// `whatsapp_business_templates`), with their components.
    pub templates: Vec<Template>,
}

impl From<TemplateGroup> for TemplateGroupDetails {
    fn from(group: TemplateGroup) -> Self {
        let TemplateGroup {
            id,
            name,
            description,
            creation_time,
            update_time,
            whatsapp_business_account: _,
            whatsapp_business_templates,
            ..
        } = group;
        Self {
            id,
            name,
            description,
            creation_time,
            update_time,
            templates: whatsapp_business_templates
                .map(|page| page.data.into_iter().map(Template::from).collect())
                .unwrap_or_default(),
        }
    }
}

/// A group body in Meta's JSON, read into the library's `T`: each template
/// id a Meta id (`field[i]`, checked by `ids`), the library's limits
/// (`check`), then no key of the request dropped (the creation's rule,
/// [`crate::request::meta_object`]).
fn group_body<T>(
    body: &Value,
    ids: impl Fn(&T) -> Vec<(&'static str, &[TemplateId])>,
    check: impl Fn(&T) -> Result<(), meta_whatsapp_rs::core::error::ValidationError>,
) -> Result<T, ServiceError>
where
    T: DeserializeOwned + Serialize,
{
    let object = T::deserialize(body).map_err(|_| ServiceError::invalid("body"))?;
    for (field, list) in ids(&object) {
        for (i, id) in list.iter().enumerate() {
            graph_id("id", id.as_str())
                .map_err(|_| ServiceError::invalid(format!("{field}[{i}]")))?;
        }
    }
    check(&object).map_err(|invalid| ServiceError::invalid(invalid.field))?;
    let sent = serde_json::to_value(&object).map_err(|_| ServiceError::internal())?;
    match dropped_key(body, &sent, "") {
        Some(path) => Err(ServiceError::invalid(path)),
        None => Ok(object),
    }
}

/// A group's creation in Meta's JSON (`name`, `description`,
/// `whatsapp_business_templates`, `templates/template-groups`), checked
/// before any request.
///
/// # Errors
///
/// `422 invalid_request` on `body`, on `whatsapp_business_templates[i]`
/// (not a Meta id), on `name` or `description` (Meta's limits: 256 and 512
/// characters), or on a dropped key's path.
pub fn group_creation(body: &Value) -> Result<TemplateGroupCreate, ServiceError> {
    group_body(
        body,
        |group: &TemplateGroupCreate| {
            vec![(
                "whatsapp_business_templates",
                group.whatsapp_business_templates.as_slice(),
            )]
        },
        TemplateGroupCreate::validate,
    )
}

/// A group's update in Meta's JSON (`name`, `description`,
/// `add_templates`, `remove_templates`), checked before any request.
///
/// # Errors
///
/// `422 invalid_request` on `body`, on `add_templates[i]` or
/// `remove_templates[i]`, on `name` or `description`, on `update` (nothing
/// to change), or on a dropped key's path.
pub fn group_update(body: &Value) -> Result<TemplateGroupUpdate, ServiceError> {
    group_body(
        body,
        |update: &TemplateGroupUpdate| {
            vec![
                ("add_templates", update.add_templates.as_slice()),
                ("remove_templates", update.remove_templates.as_slice()),
            ]
        },
        TemplateGroupUpdate::validate,
    )
}

/// A group id from a path: a Meta id, else `422` on `group_id`.
fn group_id(id: &str) -> Result<TemplateGroupId, ServiceError> {
    graph_id("group_id", id)?;
    Ok(TemplateGroupId::new(id))
}

/// The group `id`, if it is the WABA's (D118): `GET /{group_id}` (the
/// library's `Templates::get_group`), whose `whatsapp_business_account`
/// must be `waba`, read with its own token. What every operation on one
/// group id checks before it acts, and what the analytics of groups
/// (roadmap M5g2) check for each id they ask about.
///
/// # Errors
///
/// `404 not_found`, without Meta's code or text: Meta refusing the id, or a
/// group (or node) not naming the WABA. Meta's failure otherwise, counted
/// in `observer`.
pub(crate) async fn owned_group_of(
    authz: &Authorizer,
    observer: &dyn Observer,
    waba: &OwnedWaba,
    id: &TemplateGroupId,
) -> Result<TemplateGroup, ServiceError> {
    let group = waba
        .client()
        .templates(waba.waba_id().clone())
        .get_group(id)
        .await;
    let group = match group {
        Ok(group) => group,
        Err(error) => {
            let api = waba.failed(authz, &error).await;
            if error.graph().is_some() {
                observer.graph_error(api.code());
            }
            return Err(object_failed(api, &error));
        }
    };
    let owner = group
        .whatsapp_business_account
        .as_ref()
        .map(|waba| &waba.id);
    // The node answered is the one asked for: an id Meta resolves to
    // another node (an alias, a redirect) is not acted on as the group.
    if group.id == *id && owner == Some(waba.waba_id()) {
        Ok(group)
    } else {
        tracing::warn!("refused a template group that is not the WABA's");
        Err(ServiceError::not_found())
    }
}

/// Whether every one of `ids` is a group of `waba` ([`owned_group_of`] for
/// each, in order, stopping at the first that is not).
///
/// # Errors
///
/// As [`owned_group_of`].
pub(crate) async fn all_groups_in_waba(
    authz: &Authorizer,
    observer: &dyn Observer,
    waba: &OwnedWaba,
    ids: &[TemplateGroupId],
) -> Result<(), ServiceError> {
    for id in ids {
        owned_group_of(authz, observer, waba, id).await?;
    }
    Ok(())
}

impl Templates<'_> {
    /// The group `id`, if it is the WABA's: `GET /{group_id}` (the
    /// library's `Templates::get_group`), whose `whatsapp_business_account`
    /// must be the path's WABA (see the [module](self)).
    ///
    /// # Errors
    ///
    /// `404 not_found`, without Meta's code or text: Meta refusing the id,
    /// or a group (or node) not naming the WABA. Meta's failure otherwise,
    /// with its `details`.
    async fn owned_group(&self, id: &TemplateGroupId) -> Result<TemplateGroup, ServiceError> {
        owned_group_of(self.authz, self.observer, self.waba, id).await
    }

    /// Create `group` (read by [`group_creation`]) in the WABA, its
    /// templates found among the WABA's first: `POST
    /// /{waba_id}/template_groups`, the library's
    /// `Templates::create_group`, not replayed on a timeout. The new
    /// group's id.
    ///
    /// # Errors
    ///
    /// `404 not_found` when a template is not the WABA's, and nothing
    /// created. Meta's refusal or failure, with its `details` (a name in
    /// use, a template in another group, are Meta's).
    pub async fn create_group(
        &self,
        group: &TemplateGroupCreate,
    ) -> Result<TemplateGroupId, ServiceError> {
        self.all_in_waba(self.waba, &group.whatsapp_business_templates, None)
            .await?;
        let created = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .create_group(group)
            .await;
        match created {
            Ok(created) => Ok(created.id),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The group `id`, with the first page of its templates, if it is the
    /// WABA's.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `group_id`, before any request. `404
    /// not_found` for a group not the WABA's. Meta's failure, with its
    /// `details`.
    pub async fn group(&self, id: &str) -> Result<TemplateGroupDetails, ServiceError> {
        let id = group_id(id)?;
        Ok(TemplateGroupDetails::from(self.owned_group(&id).await?))
    }

    /// Change the group `id` with `update` (read by [`group_update`]), if it
    /// is the WABA's, the templates it adds found among the WABA's first:
    /// `POST /{group_id}`, the library's `Templates::update_group`, not
    /// replayed on a timeout. An update is the caller's audit event
    /// (`template_group_updated`).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `group_id`, before any request. `404
    /// not_found` for a group, or a template added, not the WABA's, and
    /// nothing changed. Meta's refusal or failure, with its `details`.
    pub async fn update_group(
        &self,
        id: &str,
        update: &TemplateGroupUpdate,
    ) -> Result<(), ServiceError> {
        let id = group_id(id)?;
        self.owned_group(&id).await?;
        self.all_in_waba(self.waba, &update.add_templates, None)
            .await?;
        let updated = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .update_group(&id, update)
            .await;
        match updated {
            Ok(()) => {
                self.audit("template_group_updated");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Delete the group `id` (its templates stay), if it is the WABA's:
    /// `DELETE /{group_id}`, the library's `Templates::delete_group`. A
    /// deletion is the caller's audit event (`template_group_deleted`).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `group_id`, before any request. `404
    /// not_found` for a group not the WABA's, and nothing deleted. Meta's
    /// refusal or failure, with its `details`.
    pub async fn delete_group(&self, id: &str) -> Result<(), ServiceError> {
        let id = group_id(id)?;
        self.owned_group(&id).await?;
        let deleted = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .delete_group(&id)
            .await;
        match deleted {
            Ok(()) => {
                self.audit("template_group_deleted");
                Ok(())
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

    /// templates/template-groups' bodies: read with no key dropped, each
    /// id a Meta id (strings or Meta's integers), the library's limits.
    /// Decisive: the id check before the library's (which would fail to
    /// write a non-digit id), and the comparison.
    #[test]
    fn group_bodies_are_read_then_checked() {
        let create = json!({"name": "Black Friday 2024",
            "description": "US-based Black Friday sale 2024 templates.",
            "whatsapp_business_templates": [278_077_987_957_091_u64, "1945418102598215"]});
        let group = group_creation(&create).unwrap();
        assert_eq!(group.whatsapp_business_templates.len(), 2);
        let mut with = create.clone();
        with["whatsapp_business_templates"] = json!(["1", "x"]);
        assert_eq!(
            field(&group_creation(&with).unwrap_err()),
            "whatsapp_business_templates[1]"
        );
        with["whatsapp_business_templates"] = json!(["1"]);
        with["descriptio"] = json!("x");
        assert_eq!(field(&group_creation(&with).unwrap_err()), "descriptio");
        let mut long = create.clone();
        long["name"] = json!("x".repeat(257));
        assert_eq!(field(&group_creation(&long).unwrap_err()), "name");
        assert_eq!(
            field(&group_creation(&json!({"name": "x"})).unwrap_err()),
            "body"
        );
        let update = json!({"add_templates": [844_006_344_365_270_u64],
                            "remove_templates": ["1035843174854974"]});
        assert_eq!(group_update(&update).unwrap().remove_templates.len(), 1);
        assert_eq!(field(&group_update(&json!({})).unwrap_err()), "update");
        assert_eq!(
            field(&group_update(&json!({"remove_templates": ["0123"]})).unwrap_err()),
            "remove_templates[0]"
        );
        assert_eq!(field(&group_id("90205a").unwrap_err()), "group_id");
    }
}
