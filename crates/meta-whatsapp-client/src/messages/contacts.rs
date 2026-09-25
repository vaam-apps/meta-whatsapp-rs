//! Contact cards for `contacts` messages (`messages/contacts-messages`).
//!
//! The `type` labels (`Home`, `Office`, `Landline`, …) are free strings: the
//! reference schema (`reference/whatsapp-business-phone-number/message-api`)
//! lists only `HOME`/`WORK`, but the page's own example sends `Office`,
//! `Pop-Up`, `Landline`, `Mobile` and `Company (FB)`. We follow the example.

use serde::Serialize;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::WaId;

use super::validate::{self, Check};

/// "Each message can include information for up to 257 contacts"
/// (`messages/contacts-messages`).
pub(crate) const MAX_CONTACTS: usize = 257;

/// One contact card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Contact {
    /// Postal addresses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<ContactAddress>,
    /// Birthday, `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub birthday: Option<String>,
    /// Email addresses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub emails: Vec<ContactEmail>,
    /// Name; `formatted_name` is required and shown in the message.
    pub name: ContactName,
    /// Organization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<ContactOrg>,
    /// Phone numbers. Include `wa_id` to get "Message"/"Save contact"
    /// buttons instead of "Invite to WhatsApp".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phones: Vec<ContactPhone>,
    /// Websites.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<ContactUrl>,
}

impl Contact {
    /// A card with only a formatted name.
    pub fn new(formatted_name: impl Into<String>) -> Self {
        Self {
            addresses: Vec::new(),
            birthday: None,
            emails: Vec::new(),
            name: ContactName::new(formatted_name),
            org: None,
            phones: Vec::new(),
            urls: Vec::new(),
        }
    }

    /// Add a phone number.
    #[must_use]
    pub fn phone(mut self, phone: ContactPhone) -> Self {
        self.phones.push(phone);
        self
    }

    /// Add an email address.
    #[must_use]
    pub fn email(mut self, email: ContactEmail) -> Self {
        self.emails.push(email);
        self
    }

    /// Add a postal address.
    #[must_use]
    pub fn address(mut self, address: ContactAddress) -> Self {
        self.addresses.push(address);
        self
    }

    /// Add a website.
    #[must_use]
    pub fn url(mut self, url: ContactUrl) -> Self {
        self.urls.push(url);
        self
    }

    /// Set the organization.
    #[must_use]
    pub fn org(mut self, org: ContactOrg) -> Self {
        self.org = Some(org);
        self
    }

    /// Set the birthday (`YYYY-MM-DD`).
    #[must_use]
    pub fn birthday(mut self, birthday: impl Into<String>) -> Self {
        self.birthday = Some(birthday.into());
        self
    }

    pub(crate) fn validate(&self, path: &str) -> Check {
        // "Required. Contact's formatted name."
        validate::non_empty(
            &format!("{path}.name.formatted_name"),
            &self.name.formatted_name,
        )?;
        if let Some(birthday) = &self.birthday {
            // "Must be in YYYY-MM-DD format." The shape check is needed
            // because `time`'s `[year]` accepts a leading sign
            // (`+1999-01-23`); the parse rejects impossible dates (02-30).
            let format = time::macros::format_description!("[year]-[month]-[day]");
            if !is_yyyy_mm_dd(birthday) || time::Date::parse(birthday, &format).is_err() {
                return Err(ValidationError::new(
                    format!("{path}.birthday"),
                    "must be a valid date in YYYY-MM-DD format",
                ));
            }
        }
        Ok(())
    }
}

/// Exactly four digits, `-`, two digits, `-`, two digits.
fn is_yyyy_mm_dd(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// `name` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactName {
    /// Full name as displayed. Required.
    pub formatted_name: String,
    /// First name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    /// Last name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// Middle name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub middle_name: Option<String>,
    /// Suffix, e.g. `Esq.`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// Prefix, e.g. `Dr.`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

impl ContactName {
    /// Name with only `formatted_name`.
    pub fn new(formatted_name: impl Into<String>) -> Self {
        Self {
            formatted_name: formatted_name.into(),
            first_name: None,
            last_name: None,
            middle_name: None,
            suffix: None,
            prefix: None,
        }
    }
}

/// `addresses[]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContactAddress {
    /// Street and number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    /// City.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// Two-letter state code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Postal code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    /// Country name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// ISO two-letter country code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    /// Label, e.g. `Home`, `Office`.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// `emails[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactEmail {
    /// Address.
    pub email: String,
    /// Label, e.g. `Work`.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl ContactEmail {
    /// Email with a label.
    pub fn new(email: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            kind: Some(kind.into()),
        }
    }
}

/// `org` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContactOrg {
    /// Company.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    /// Department.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    /// Job title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// `phones[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactPhone {
    /// Phone number.
    pub phone: String,
    /// Label, e.g. `Mobile`.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The contact's WhatsApp id; enables the "Message" button.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
}

impl ContactPhone {
    /// Phone with a label.
    pub fn new(phone: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            phone: phone.into(),
            kind: Some(kind.into()),
            wa_id: None,
        }
    }

    /// Set the WhatsApp id.
    #[must_use]
    pub fn wa_id(mut self, wa_id: impl Into<WaId>) -> Self {
        self.wa_id = Some(wa_id.into());
        self
    }
}

/// `urls[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactUrl {
    /// URL.
    pub url: String,
    /// Label, e.g. `Company`.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl ContactUrl {
    /// URL with a label.
    pub fn new(url: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            kind: Some(kind.into()),
        }
    }
}
