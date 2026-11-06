//! Pre-verified numbers (roadmap M3f, decisions D203 and D204; Meta's
//! `embedded-signup/pre-verified-numbers`): numbers the deployment
//! verified ahead of time in a pool on its own business portfolio, which a
//! merchant then picks in Embedded Signup without typing a one-time code.
//!
//! The pool is the deployment's, not a tenant's, so its routes are the
//! operator's (admin key): adding a number, requesting and checking its
//! code, listing and reading the pool, deleting a number, and reserving
//! one for a tenant. A tenant reaches a number only through the launch
//! (`POST /v1/signup/sessions`, `pre_verified_phone_ids`), which offers
//! only the ids reserved for it. Every call is the library's
//! (`Business::add_pre_verified_number`, `Client::pre_verified_phone_number`)
//! with the pool's token (`business_management`), never a tenant's.
//!
//! No legal act: no pre-verified endpoint accepts terms. Deleting a number
//! and reserving one are explicit, audited operator actions no other call
//! makes. Not offered: sharing a number with a partner
//! (`share_pre_verified_number`) and reading who has one (`partners`),
//! which are Multi-Partner Solution matters (D205).

use meta_whatsapp_rs::client::embedded_signup::LaunchOptions;
use meta_whatsapp_rs::client::phone_numbers::{
    CodeMethod, CodeVerificationStatus, VerificationCode,
};
use meta_whatsapp_rs::client::waba::{
    CreatedPreVerifiedNumber, ListPreVerifiedNumbers, PreVerifiedNumber,
};
use meta_whatsapp_rs::core::ids::{BusinessId, PreVerifiedPhoneNumberId};
use meta_whatsapp_rs::core::paging::Page;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::Expiry;
use meta_whatsapp_rs::{Client, Error};
use serde::{Deserialize, Serialize};

use super::{ErrorCode, ServiceError, Signup, Signups};
use crate::authz::{AdminCaller, Authorizer};
use crate::model::TenantId;
use crate::observe::Observer;

/// The key/value namespace of the reservations (decision D203; a new
/// stored name, pending the owner's confirmation before the first
/// release): a pre-verified number's id, the tenant it is reserved for.
pub(super) const RESERVATION_NAMESPACE: &str = "wa.es.preverified";

/// The most ids a launch offers: a modest bound on the page's picker.
pub const MAX_LAUNCH_IDS: usize = 20;

/// The deployment's own business portfolio and the system user token that
/// manages its pool. `Debug` shows no token.
#[derive(Clone)]
pub struct PartnerPool {
    pub(super) business_id: BusinessId,
    pub(super) token: AccessToken,
}

impl std::fmt::Debug for PartnerPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartnerPool")
            .field("business_id", &self.business_id)
            .finish_non_exhaustive()
    }
}

impl PartnerPool {
    /// The deployment's own business portfolio.
    pub(crate) fn business_id(&self) -> &BusinessId {
        &self.business_id
    }

    /// The system user token that manages it.
    pub(crate) fn token(&self) -> &AccessToken {
        &self.token
    }
}

/// The tenant a number is reserved for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Reservation {
    pub tenant_id: String,
}

/// `POST /v1/admin/pre-verified-numbers`.
#[derive(Debug, Clone)]
pub struct PreVerifiedAdd {
    /// The number, E.164 or formatted (Meta normalizes it).
    pub phone_number: String,
}

/// A code request: how it is delivered, and in which language.
#[derive(Debug, Clone)]
pub struct PreVerifiedCode {
    /// `SMS` or `VOICE`.
    pub method: CodeMethod,
    /// The language, e.g. `en_US`.
    pub language: String,
}

/// `GET /v1/admin/pre-verified-numbers`' filters.
#[derive(Debug, Clone, Default)]
pub struct PreVerifiedList {
    /// Only the number written this way.
    pub phone_number: Option<String>,
    /// Only the numbers in this state.
    pub status: Option<CodeVerificationStatus>,
    /// The page size.
    pub limit: Option<u32>,
    /// The cursor of the next page.
    pub after: Option<String>,
}

fn pool_id(id: &str) -> Result<PreVerifiedPhoneNumberId, ServiceError> {
    PreVerifiedPhoneNumberId::new(id).map_err(|_| ServiceError::invalid("id"))
}

fn audit_number(action: &'static str, admin: &AdminCaller, id: &PreVerifiedPhoneNumberId) {
    tracing::info!(
        target: "audit",
        action,
        admin_key_id = admin.key_id(),
        pre_verified_id = id.as_str(),
        "admin change"
    );
}

impl Signups {
    /// The pool's client and portfolio, once `admin` is admitted.
    fn pool(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
    ) -> Result<(Client, BusinessId), ServiceError> {
        authz.admit_admin(admin)?;
        let pool = self
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.pool.as_ref())
            .ok_or_else(|| ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED))?;
        Ok((
            authz.client().with_token(pool.token.clone()),
            pool.business_id.clone(),
        ))
    }

    /// `POST /{business}/add_phone_numbers`: add a number to the pool.
    ///
    /// # Errors
    ///
    /// `admin` another [`Authorizer`] made: `403 forbidden`; no pool:
    /// `409 signup_not_configured`; a number that is not one: `422`;
    /// Meta's refusal, as its code says.
    pub async fn pre_verified_add(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        add: PreVerifiedAdd,
        observer: &dyn Observer,
    ) -> Result<CreatedPreVerifiedNumber, ServiceError> {
        let (client, business) = self.pool(authz, admin)?;
        let created = client
            .business(business)
            .add_pre_verified_number(&add.phone_number)
            .await
            .map_err(|error| refused(&error, observer))?;
        audit_number("pre_verified_number_added", admin, &created.id);
        Ok(created)
    }

    /// `GET /{business}/preverified_numbers`: one page of the pool.
    ///
    /// # Errors
    ///
    /// As [`Self::pre_verified_add`].
    pub async fn pre_verified_list(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        list: PreVerifiedList,
        observer: &dyn Observer,
    ) -> Result<Page<PreVerifiedNumber>, ServiceError> {
        let (client, business) = self.pool(authz, admin)?;
        let mut query = ListPreVerifiedNumbers::new().fields([
            "id",
            "phone_number",
            "code_verification_status",
            "code_verification_time",
            "verification_expiry_time",
        ]);
        if let Some(number) = list.phone_number {
            query = query.phone_number(number);
        }
        if let Some(status) = list.status {
            query = query.code_verification_status(status);
        }
        if let Some(limit) = list.limit {
            query = query.limit(limit);
        }
        if let Some(after) = list.after {
            query = query.after(after);
        }
        client
            .business(business)
            .pre_verified_numbers(&query)
            .await
            .map_err(|error| refused(&error, observer))
    }

    /// `GET /{pre-verified id}`: one number.
    ///
    /// # Errors
    ///
    /// As [`Self::pre_verified_add`]; an id that is not digits: `422`.
    pub async fn pre_verified_get(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        id: &str,
        observer: &dyn Observer,
    ) -> Result<PreVerifiedNumber, ServiceError> {
        let (client, _) = self.pool(authz, admin)?;
        let id = pool_id(id)?;
        client
            .pre_verified_phone_number(id)
            .get(&[
                "id",
                "phone_number",
                "code_verification_status",
                "code_verification_time",
                "verification_expiry_time",
            ])
            .await
            .map_err(|error| refused(&error, observer))
    }

    /// `POST /{pre-verified id}/request_code`: send the one-time code.
    ///
    /// # Errors
    ///
    /// As [`Self::pre_verified_get`].
    pub async fn pre_verified_request_code(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        id: &str,
        code: PreVerifiedCode,
        observer: &dyn Observer,
    ) -> Result<(), ServiceError> {
        let (client, _) = self.pool(authz, admin)?;
        let id = pool_id(id)?;
        client
            .pre_verified_phone_number(id.clone())
            .request_code(code.method, &code.language)
            .await
            .map_err(|error| refused(&error, observer))?;
        audit_number("pre_verified_code_requested", admin, &id);
        Ok(())
    }

    /// `POST /{pre-verified id}/verify_code`: check the code the number
    /// received. The code is in no log, error or audit event.
    ///
    /// # Errors
    ///
    /// As [`Self::pre_verified_get`]; a code that is not digits: `422` on
    /// `code`.
    pub async fn pre_verified_verify_code(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        id: &str,
        code: String,
        observer: &dyn Observer,
    ) -> Result<(), ServiceError> {
        let (client, _) = self.pool(authz, admin)?;
        let id = pool_id(id)?;
        let code = VerificationCode::new(code).map_err(|_| ServiceError::invalid("code"))?;
        client
            .pre_verified_phone_number(id.clone())
            .verify_code(&code)
            .await
            .map_err(|error| refused(&error, observer))?;
        audit_number("pre_verified_code_verified", admin, &id);
        Ok(())
    }

    /// `DELETE /{pre-verified id}`: remove a number from the pool, and its
    /// reservation. An explicit operator action.
    ///
    /// # Errors
    ///
    /// As [`Self::pre_verified_get`].
    pub async fn pre_verified_delete(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        id: &str,
        observer: &dyn Observer,
    ) -> Result<(), ServiceError> {
        let (client, _) = self.pool(authz, admin)?;
        let id = pool_id(id)?;
        client
            .pre_verified_phone_number(id.clone())
            .delete()
            .await
            .map_err(|error| refused(&error, observer))?;
        self.reservations.delete(id.as_str()).await?;
        audit_number("pre_verified_number_deleted", admin, &id);
        Ok(())
    }

    /// Reserve a pool number for `tenant`: the launch offers it to that
    /// tenant's signup only (decision D203). Asks Meta nothing; replaces a
    /// reservation for another tenant.
    ///
    /// # Errors
    ///
    /// `admin` another [`Authorizer`] made: `403 forbidden`; no pool: `409
    /// signup_not_configured`; an id that is not digits: `422`; no such
    /// tenant: `404 not_found`.
    pub async fn pre_verified_reserve(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        id: &str,
        tenant: &TenantId,
    ) -> Result<(), ServiceError> {
        self.pool(authz, admin)?;
        let id = pool_id(id)?;
        authz
            .records()
            .tenant(tenant)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        self.reservations
            .put(
                id.as_str(),
                &Reservation {
                    tenant_id: tenant.as_str().to_owned(),
                },
                Expiry::Never,
            )
            .await?;
        tracing::info!(
            target: "audit",
            action = "pre_verified_number_reserved",
            admin_key_id = admin.key_id(),
            tenant = tenant.as_str(),
            pre_verified_id = id.as_str(),
            "admin change"
        );
        Ok(())
    }

    /// Release a reservation. Idempotent.
    ///
    /// # Errors
    ///
    /// As [`Self::pre_verified_reserve`], but for the tenant.
    pub async fn pre_verified_release(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        id: &str,
    ) -> Result<(), ServiceError> {
        self.pool(authz, admin)?;
        let id = pool_id(id)?;
        self.reservations.delete(id.as_str()).await?;
        audit_number("pre_verified_number_released", admin, &id);
        Ok(())
    }
}

/// Meta's refusal as the service answers it: its code, never its text or
/// details; told to `observer` when Meta answered.
fn refused(error: &Error, observer: &dyn Observer) -> ServiceError {
    let api = ServiceError::from_library_withholding_texts(error);
    if error.graph().is_some() {
        observer.graph_error(api.code());
    }
    api
}

impl Signup<'_> {
    /// `options` offering `ids`, once each is reserved for the caller's
    /// tenant: another tenant's, an unreserved one and a malformed one are
    /// the same `422 invalid_request` on `pre_verified_phone_ids`, so a
    /// tenant learns nothing of the pool.
    pub(super) async fn offer_pre_verified(
        &self,
        options: LaunchOptions,
        ids: &[String],
    ) -> Result<LaunchOptions, ServiceError> {
        if ids.is_empty() {
            return Ok(options);
        }
        let invalid = || ServiceError::invalid("pre_verified_phone_ids");
        if ids.len() > MAX_LAUNCH_IDS || !self.deployment.manages_pre_verified_pool() {
            return Err(invalid());
        }
        let mut checked = Vec::with_capacity(ids.len());
        for id in ids {
            let id = PreVerifiedPhoneNumberId::new(id.as_str()).map_err(|_| invalid())?;
            let reserved = self
                .signups
                .reservations
                .get(id.as_str())
                .await?
                .is_some_and(|(reservation, ..)| reservation.tenant_id == self.tenant().as_str());
            if !reserved || checked.contains(&id) {
                return Err(invalid());
            }
            checked.push(id);
        }
        Ok(options.pre_verified_phone_ids(checked))
    }
}
