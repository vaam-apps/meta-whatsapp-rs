//! The block list (docs/design/server.md, section 4.2; roadmap M5g), as
//! services an API adapter calls, with the tenant's WABA token, over the
//! library's `client::block_users`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`BlockedUsers::list`] | `GET /{pn}/block_users?limit=…[&after=…]` |
//! | [`BlockedUsers::block`] | `POST /{pn}/block_users` |
//! | [`BlockedUsers::unblock`] | `DELETE /{pn}/block_users` (the same body) |
//!
//! Pages: `block-users`, `reference/whatsapp-business-phone-number/block-api`.
//! Limits, checked before any request: 1 to 1,000 people a request
//! (`block-users`, "Limitations"), each a phone number (E.164 with its
//! `+`, as a send's) and/or a business-scoped user id (BSUID); a group, or
//! a parent BSUID (`CC.ENT.…`), is refused, as Meta documents it fails.
//! The 24-hour rule (the person must have messaged the number in the last
//! 24 hours) and the 64,000-entry cap are Meta's. The list's page size is
//! the service's to bound (1 to [`LIST_PAGE_MAX`]): the reference
//! documents none (D143).
//!
//! **The block list is a number's own**, and the services open only on a
//! number of the [`Caller`]'s own tenant (step 4 of the authorization
//! order, [`crate::authz`]); a person is no object of Meta's to check
//! against the number, so there is nothing else to look for.
//!
//! **Blocking acts on a person**, so a block and an unblock are the
//! caller's audit events ([`crate::audit::contacts_audit`]:
//! `users_blocked`, `users_unblocked`), with how many people the request
//! named and how many Meta blocked and refused, and an `unknown` outcome
//! when the call may have gone through unanswered (a timeout, or Meta's
//! `139100`, which says some were not blocked and not which). **Never
//! who**: the event holds no phone number and no BSUID, and neither does
//! any log of the service.
//!
//! Meta answers a person it refused with its error's code: the service
//! answers that code and the error's kind, **never Meta's text**. Nor does
//! a failed call answer or log Meta's texts (`details` included): they may
//! quote the person, as an OTP's or a PIN's may quote a code
//! ([`OwnedNumber::failed_withholding_texts`]).

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::block_users::{
    BlockUsersResponse, BlockedUser, FailedUser, ListBlockedUsers, MAX_USERS_PER_REQUEST,
    UnblockUsersResponse, UserOutcome,
};
use meta_whatsapp_rs::core::GraphApiError;
use meta_whatsapp_rs::core::ids::{UserId, WaId};
use meta_whatsapp_rs::core::recipient::Recipient;

use crate::audit::{ContactFacts, Outcome, Subject, contacts_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::{ServiceError, kind_code};
use crate::messages::is_e164;
use crate::observe::Observer;

/// Largest page of the block list the service asks for: its own bound
/// (D143), the reference documents none.
pub const LIST_PAGE_MAX: u32 = 100;

/// The audit actions (docs/design/server.md, D58 and D143).
const BLOCKED: &str = "users_blocked";
const UNBLOCKED: &str = "users_unblocked";

/// Meta's code for "failed to block/unblock some users".
const PARTIAL_FAILURE: i64 = 139_100;

// ─── The request's checks ────────────────────────────────────────────────

/// A person named by a request: a phone number, a BSUID, or both (Meta
/// then uses the phone number).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Person {
    /// E.164 **with** its `+`.
    pub phone: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`).
    pub user_id: Option<String>,
}

/// The library's recipients for `people`, checked.
///
/// # Errors
///
/// `422 invalid_request` on `users` (none, or more than 1,000), on
/// `users[i]` (neither a phone nor a BSUID), on `users[i].phone` (not
/// strict E.164 with its `+`) or on `users[i].user_id` (not a standard
/// BSUID: a parent BSUID is not one). Never quotes a value.
pub fn people(people: Vec<Person>) -> Result<Vec<Recipient>, ServiceError> {
    if people.is_empty() || people.len() > MAX_USERS_PER_REQUEST {
        return Err(ServiceError::invalid("users"));
    }
    people
        .into_iter()
        .enumerate()
        .map(|(i, person)| {
            let phone = match person.phone {
                Some(phone) if is_e164(&phone) => Some(phone),
                Some(_) => return Err(ServiceError::invalid(format!("users[{i}].phone"))),
                None => None,
            };
            let user = match person.user_id {
                Some(id) => {
                    let id = UserId::new(id);
                    if !id.is_bsuid() {
                        return Err(ServiceError::invalid(format!("users[{i}].user_id")));
                    }
                    Some(id)
                }
                None => None,
            };
            match (phone, user) {
                (Some(phone), None) => Ok(Recipient::Phone(phone)),
                (None, Some(user)) => Ok(Recipient::User(user)),
                (Some(phone), Some(user)) => Ok(Recipient::PhoneAndUser { phone, user }),
                (None, None) => Err(ServiceError::invalid(format!("users[{i}]"))),
            }
        })
        .collect()
}

// ─── The answers ─────────────────────────────────────────────────────────

/// A person the operation succeeded for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// What the request sent: the phone number, or the BSUID if only that.
    pub input: Option<String>,
    /// The phone number as WhatsApp knows it (absent for a BSUID).
    pub wa_id: Option<String>,
    /// The BSUID, when one was sent.
    pub user_id: Option<String>,
}

impl From<UserOutcome> for Done {
    fn from(user: UserOutcome) -> Self {
        Self {
            input: user.input,
            wa_id: user.wa_id.map(WaId::into_inner),
            user_id: user.user_id.map(UserId::into_inner),
        }
    }
}

/// Why Meta refused a person: its code and the service's kind for it,
/// never its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    /// Meta's error code (`131047`: the person has not messaged in 24
    /// hours; `131021`: the number itself).
    pub code: i64,
    /// The service's error code for it (`ErrorKind`'s), to branch on.
    pub kind: &'static str,
}

impl From<&GraphApiError> for Reason {
    fn from(error: &GraphApiError) -> Self {
        Self {
            code: error.code,
            kind: kind_code(error.kind()),
        }
    }
}

/// A person Meta refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// What the request sent.
    pub input: Option<String>,
    /// The phone number as WhatsApp knows it (may be absent).
    pub wa_id: Option<String>,
    /// The BSUID, when one was sent.
    pub user_id: Option<String>,
    /// Why.
    pub reasons: Vec<Reason>,
}

impl From<FailedUser> for Refused {
    fn from(user: FailedUser) -> Self {
        Self {
            reasons: user.errors.iter().map(Reason::from).collect(),
            input: user.input,
            wa_id: user.wa_id.map(WaId::into_inner),
            user_id: user.user_id.map(UserId::into_inner),
        }
    }
}

/// What a block or an unblock came to, person by person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcomes {
    /// The people it succeeded for.
    pub done: Vec<Done>,
    /// The people Meta refused.
    pub refused: Vec<Refused>,
    /// Meta's code when it answered a partial failure next to the lists
    /// (`139100`).
    pub error_code: Option<i64>,
}

impl From<BlockUsersResponse> for Outcomes {
    fn from(response: BlockUsersResponse) -> Self {
        Self {
            done: response
                .block_users
                .added_users
                .into_iter()
                .map(Done::from)
                .collect(),
            refused: response
                .block_users
                .failed_users
                .into_iter()
                .map(Refused::from)
                .collect(),
            error_code: response.error.map(|e| e.code),
        }
    }
}

impl From<UnblockUsersResponse> for Outcomes {
    fn from(response: UnblockUsersResponse) -> Self {
        Self {
            done: response
                .block_users
                .removed_users
                .into_iter()
                .map(Done::from)
                .collect(),
            refused: response
                .block_users
                .failed_users
                .into_iter()
                .map(Refused::from)
                .collect(),
            error_code: response.error.map(|e| e.code),
        }
    }
}

/// A person on the block list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// The phone number (absent when the person has a username).
    pub wa_id: Option<String>,
    /// The BSUID.
    pub user_id: Option<String>,
    /// The parent BSUID, when the portfolio is enrolled for them.
    pub parent_user_id: Option<String>,
}

impl From<BlockedUser> for Blocked {
    fn from(user: BlockedUser) -> Self {
        Self {
            wa_id: user.wa_id.map(WaId::into_inner),
            user_id: user.user_id.map(UserId::into_inner),
            parent_user_id: user.parent_user_id.map(UserId::into_inner),
        }
    }
}

/// A page of the block list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedPage {
    /// The people.
    pub users: Vec<Blocked>,
    /// Meta's cursor for the next page, when Meta says one follows.
    pub next_after: Option<String>,
}

// ─── The operations ──────────────────────────────────────────────────────

/// The block list operations on a number of the caller's tenant: an
/// [`OwnedNumber`] its [`Authorizer`] made from that tenant's binding, for
/// a [`Caller`] it made, both admitted ([`Self::new`]), with the number's
/// WABA token. Cheap: a few references.
pub struct BlockedUsers<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for BlockedUsers<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockedUsers")
            .field("caller", self.caller)
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> BlockedUsers<'a> {
    /// The operations on `number` for `caller`, once `authz` admits both
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
            tracing::warn!("refused the block list services on another tenant's number");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
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

    /// The error of a failed Graph call, **without Meta's texts**: they may
    /// quote the person (a phone number, a BSUID), so neither the answer's
    /// `details` nor the debug log line holds them
    /// ([`OwnedNumber::failed_withholding_texts`]).
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self
            .number
            .failed_withholding_texts(self.authz, error)
            .await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    fn block_users(&self) -> meta_whatsapp_rs::client::block_users::BlockUsers {
        self.number
            .client()
            .block_users(self.number.phone_number_id().clone())
    }

    /// The caller's audit event `action` for a request that named `users`.
    fn audit(&self, action: &'static str, facts: ContactFacts, outcome: Outcome) {
        contacts_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            &facts,
            outcome,
        );
    }

    /// The error of a change that failed, audited as `action` with an
    /// unknown outcome when it may have gone through, or Meta says some
    /// of the people were not changed without saying which.
    async fn change_failed(
        &self,
        action: &'static str,
        users: usize,
        error: &Error,
    ) -> ServiceError {
        let failed = self.failed(error).await;
        let partial = failed.graph().is_some_and(|g| g.code == PARTIAL_FAILURE);
        if failed.may_have_been_sent() || partial {
            self.audit(
                action,
                ContactFacts {
                    users: users as u64,
                    ..ContactFacts::default()
                },
                Outcome::Unanswered,
            );
        }
        failed
    }

    /// A page of the block list.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `limit`, before any request. Meta's
    /// refusal or failure, with its `details`.
    pub async fn list(
        &self,
        limit: Option<u32>,
        after: Option<String>,
    ) -> Result<BlockedPage, ServiceError> {
        if limit.is_some_and(|limit| !(1..=LIST_PAGE_MAX).contains(&limit)) {
            return Err(ServiceError::invalid("limit"));
        }
        let listed = self
            .block_users()
            .list(&ListBlockedUsers {
                limit,
                after,
                before: None,
            })
            .await;
        match listed {
            Ok(page) => {
                let next_after = page.next_cursor().map(str::to_owned);
                Ok(BlockedPage {
                    users: page.data.into_iter().map(Blocked::from).collect(),
                    next_after,
                })
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Block `users` (checked by [`people`]): `POST /{pn}/block_users`, not
    /// replayed on a timeout. The caller's audit event `users_blocked`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`).
    pub async fn block(&self, users: &[Recipient]) -> Result<Outcomes, ServiceError> {
        match self.block_users().block(users).await {
            Ok(response) => {
                let outcomes = Outcomes::from(response);
                self.audit(BLOCKED, facts(users.len(), &outcomes), Outcome::Answered);
                Ok(outcomes)
            }
            Err(error) => Err(self.change_failed(BLOCKED, users.len(), &error).await),
        }
    }

    /// Unblock `users` (checked by [`people`]): `DELETE /{pn}/block_users`
    /// with the same body, not replayed on a timeout. The caller's audit
    /// event `users_unblocked`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn unblock(&self, users: &[Recipient]) -> Result<Outcomes, ServiceError> {
        match self.block_users().unblock(users).await {
            Ok(response) => {
                let outcomes = Outcomes::from(response);
                self.audit(UNBLOCKED, facts(users.len(), &outcomes), Outcome::Answered);
                Ok(outcomes)
            }
            Err(error) => Err(self.change_failed(UNBLOCKED, users.len(), &error).await),
        }
    }
}

/// An audit event's counts.
fn facts(users: usize, outcomes: &Outcomes) -> ContactFacts {
    ContactFacts {
        users: users as u64,
        done: outcomes.done.len() as u64,
        failed: outcomes.refused.len() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone(phone: &str) -> Person {
        Person {
            phone: Some(phone.to_owned()),
            user_id: None,
        }
    }

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    /// The people of a request: one to 1,000, each strict E.164 with its
    /// `+` and/or a standard BSUID, a parent BSUID refused. Decisive: each
    /// clause, and the field named with its position.
    #[test]
    fn a_request_names_one_to_a_thousand_people_by_phone_or_bsuid() {
        let ok = people(vec![
            phone("+16505551234"),
            Person {
                phone: None,
                user_id: Some("US.13491208655302741918".to_owned()),
            },
            Person {
                phone: Some("+16505551234".to_owned()),
                user_id: Some("US.13491208655302741918".to_owned()),
            },
        ])
        .unwrap();
        assert!(matches!(ok[0], Recipient::Phone(_)));
        assert!(matches!(ok[1], Recipient::User(_)));
        assert!(matches!(ok[2], Recipient::PhoneAndUser { .. }));
        assert_eq!(field(&people(Vec::new()).unwrap_err()), "users");
        assert_eq!(
            field(&people(vec![phone("+16505551234"); 1001]).unwrap_err()),
            "users"
        );
        assert!(people(vec![phone("+16505551234"); 1000]).is_ok());
        assert_eq!(
            field(&people(vec![phone("+16505551234"), phone("16505551234")]).unwrap_err()),
            "users[1].phone",
            "digits only: no `+`"
        );
        assert_eq!(
            field(&people(vec![Person::default()]).unwrap_err()),
            "users[0]"
        );
        for bad in [
            "US.ENT.11815799212886844830",
            "13491208655302741918",
            "us.1",
        ] {
            let person = Person {
                phone: None,
                user_id: Some(bad.to_owned()),
            };
            assert_eq!(
                field(&people(vec![person]).unwrap_err()),
                "users[0].user_id",
                "{bad}"
            );
        }
    }
}
