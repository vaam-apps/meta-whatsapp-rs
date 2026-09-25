//! Building blocks shared by several fields: the business phone number
//! `metadata` and the WhatsApp user `contacts` entries.
//!
//! Identity follows `business-scoped-user-ids` (BSUID): `user_id` is present
//! in every messages webhook since April 2026, while `wa_id` (the phone
//! number) and `profile` may be absent once a user adopts a username. Key a
//! user by [`Contact::user_id`] when there is one; never by phone alone.

use serde::{Deserialize, Serialize};
use meta_whatsapp_core::ids::{PhoneNumberId, UserId, WaId};

/// `value.metadata`: which business phone number the change is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    /// The business number as displayed, digits only (e.g. `15550783881`).
    pub display_phone_number: String,
    /// The business phone number id; what every send call is addressed to.
    pub phone_number_id: PhoneNumberId,
}

/// `profile` of a [`Contact`]. Both parts are optional: calls, echoes and
/// history webhooks may carry only a `username`, or no profile at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name as set in the WhatsApp client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Username, only when the user adopted one (`business-scoped-user-ids`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// One `contacts[]` entry: the WhatsApp user a message, status, call or
/// preference change is about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// Profile; defaults to empty when Meta omits the object.
    #[serde(default)]
    pub profile: Profile,
    /// Phone number (`wa_id`). Omitted for username adopters whose number
    /// Meta may not share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// Business-scoped user id (BSUID), e.g. `US.13491208655302741918`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    /// Parent BSUID (`US.ENT.…`), only for portfolios enrolled in parent BSUIDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<UserId>,
    /// Identity key hash, only with the identity change check enabled
    /// (`identity-change`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_key_hash: Option<String>,
}

impl Contact {
    /// Profile name, if Meta sent one.
    pub fn name(&self) -> Option<&str> {
        self.profile.name.as_deref()
    }

    /// Username, if the user adopted one.
    pub fn username(&self) -> Option<&str> {
        self.profile.username.as_deref()
    }
}

/// Phone numbers appear with and without `+` across Meta's pages
/// (`text` reference: `from` is `+16505551234` in the table and
/// `16505551234` in the example), so compare them without it.
pub(crate) fn same_phone(a: &str, b: &str) -> bool {
    a.trim_start_matches('+') == b.trim_start_matches('+')
}

/// Find the contact an item refers to: by BSUID first (always present since
/// 2026), then by phone number. Never guesses from position: a batch may
/// carry several contacts, and a wrong attribution in an inbox is worse than
/// none.
pub(crate) fn find_contact<'a>(
    contacts: &'a [Contact],
    user_ids: &[Option<&UserId>],
    phones: &[Option<&str>],
) -> Option<&'a Contact> {
    for user_id in user_ids.iter().flatten() {
        if let Some(c) = contacts
            .iter()
            .find(|c| c.user_id.as_ref() == Some(*user_id))
        {
            return Some(c);
        }
    }
    for phone in phones.iter().flatten() {
        if let Some(c) = contacts.iter().find(|c| {
            c.wa_id
                .as_ref()
                .is_some_and(|wa| same_phone(wa.as_str(), phone))
        }) {
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_without_wa_id_or_profile_parses() {
        let c: Contact = serde_json::from_str(r#"{"user_id":"US.1"}"#).unwrap();
        assert_eq!(c.user_id.as_ref().map(UserId::as_str), Some("US.1"));
        assert!(c.wa_id.is_none() && c.name().is_none() && c.username().is_none());
    }

    #[test]
    fn contacts_match_by_bsuid_before_phone() {
        let by_phone = Contact {
            wa_id: Some("16505551234".into()),
            ..Contact::default()
        };
        let by_user = Contact {
            user_id: Some("US.1".into()),
            ..Contact::default()
        };
        let contacts = [by_phone.clone(), by_user.clone()];
        let user = UserId::new("US.1");
        assert_eq!(
            find_contact(&contacts, &[Some(&user)], &[Some("+16505551234")]),
            Some(&by_user)
        );
        assert_eq!(
            find_contact(&contacts, &[None], &[Some("+16505551234")]),
            Some(&by_phone)
        );
        assert_eq!(find_contact(&contacts, &[None], &[Some("1")]), None);
    }
}
