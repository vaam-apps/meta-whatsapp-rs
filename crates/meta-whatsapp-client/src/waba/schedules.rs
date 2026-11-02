//! Meta's campaign schedules on a WABA
//! (`reference/whatsapp-business-account/schedules-api`).

use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::TemplateId;
use meta_whatsapp_core::open_enum;
use meta_whatsapp_core::paging::Page;
use serde::{Deserialize, Serialize};

use super::Waba;
use super::activity::ListWabaRecords;

open_enum! {
    #![case_insensitive]
    /// A campaign schedule's `status` (`WhatsAppBusinessCampaignScheduleStatus`).
    pub enum ScheduleStatus {
        /// Sent.
        Completed => "COMPLETED",
        /// Failed.
        Failed => "FAILED",
        /// Waiting for its time.
        Scheduled => "SCHEDULED",
        /// Being sent.
        Sending => "SENDING",
    }
}

/// A campaign schedule (`WhatsAppBusinessCampaignSchedule`). Every field
/// is optional in the reference; Meta's defaults are `name`,
/// `description`, `delivery_time` and `status` (not `id`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CampaignSchedule {
    /// The schedule's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// When it is delivered, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_time: Option<i64>,
    /// Its description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ScheduleStatus>,
}

/// Body of `POST /{WABA_ID}/schedules` (`ScheduleCreateRequest`): every
/// field is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct NewSchedule {
    /// The template (HSM) to send.
    pub hsm_id: TemplateId,
    /// The audience to send it to.
    pub audience_id: String,
    /// "WABA customer status ID (phone number)", as the reference has it.
    pub waba_cs_id: String,
    /// A description.
    pub description: String,
    /// A name.
    pub name: String,
    /// When to deliver, in Unix seconds.
    pub delivery_time: i64,
}

impl NewSchedule {
    /// A schedule with every required field.
    pub fn new(
        hsm_id: TemplateId,
        audience_id: impl Into<String>,
        waba_cs_id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        delivery_time: i64,
    ) -> Self {
        Self {
            hsm_id,
            audience_id: audience_id.into(),
            waba_cs_id: waba_cs_id.into(),
            description: description.into(),
            name: name.into(),
            delivery_time,
        }
    }

    /// Every string is set, and the delivery time is a positive Unix time
    /// (the reference states no other limit).
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] naming the field.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        for (field, value) in [
            ("hsm_id", self.hsm_id.as_str()),
            ("audience_id", self.audience_id.as_str()),
            ("waba_cs_id", self.waba_cs_id.as_str()),
            ("description", self.description.as_str()),
            ("name", self.name.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ValidationError::new(field, "required"));
            }
        }
        if self.delivery_time <= 0 {
            return Err(ValidationError::new(
                "delivery_time",
                "must be a Unix time in seconds",
            ));
        }
        Ok(())
    }
}

/// The answer to [`Waba::create_schedule`] (`ScheduleCreateResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedSchedule {
    /// The new schedule's id.
    pub id: String,
}

impl Waba {
    /// `GET /{WABA_ID}/schedules`, one page: the WABA's campaign schedules
    /// (default fields `name`, `description`, `delivery_time`, `status`;
    /// add `id` to get theirs). The next page: the same query with `after`
    /// set to this page's [`Page::next_cursor`].
    pub async fn schedules(&self, query: &ListWabaRecords) -> Result<Page<CampaignSchedule>> {
        self.records_page("schedules", "WABA schedules", query)?
            .send()
            .await
    }

    /// All schedules, following cursors. The stream manages them itself: a
    /// query with `after` or `before` set is refused (the stream's single
    /// item is that validation error).
    pub fn schedules_stream(
        &self,
        query: &ListWabaRecords,
    ) -> impl Stream<Item = Result<CampaignSchedule>> + Send + 'static + use<> {
        self.records_stream("schedules", "WABA schedules", query)
    }

    /// `POST /{WABA_ID}/schedules`: schedule a campaign. Checked first
    /// ([`NewSchedule::validate`]), with no request on a refusal. **Never
    /// replayed** on a timeout or a transient error: a second schedule
    /// would send the campaign twice; only an error proving Meta refused
    /// it is resent ([`crate::RetryPolicy`]).
    pub async fn create_schedule(&self, schedule: &NewSchedule) -> Result<CreatedSchedule> {
        schedule.validate()?;
        self.client
            .post_at(&[self.waba_id.as_str(), "schedules"])
            .json(schedule)
            .context("create schedule response")
            .send()
            .await
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::error::TransportError;
    use meta_whatsapp_core::ids::WabaId;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use meta_whatsapp_core::{Error, ErrorKind};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{Client, RetryPolicy};

    fn client(t: &ScriptedTransport, retry: RetryPolicy) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(retry)
            .build()
            .unwrap()
    }

    fn waba(t: &ScriptedTransport) -> Waba {
        client(t, RetryPolicy::NONE).waba(WabaId::new("102290129340398").unwrap())
    }

    fn schedule() -> NewSchedule {
        NewSchedule::new(
            TemplateId::new("1234567890").unwrap(),
            "9876543210",
            "106540352242922",
            "Summer sale",
            "The summer sale announcement",
            1_767_225_600,
        )
    }

    /// `WhatsAppBusinessCampaignSchedulesConnection` with every field (no
    /// example on the page; values ours).
    fn schedules_example() -> serde_json::Value {
        json!({
            "data": [
                {"id": "111", "delivery_time": 1_767_225_600, "description": "The summer sale announcement", "name": "Summer sale", "status": "SCHEDULED"},
                {"name": "Spring sale", "status": "COMPLETED"}
            ],
            "paging": {"cursors": {"before": "QVFI", "after": "QVFJ"}}
        })
    }

    #[tokio::test]
    async fn schedules_send_the_documented_query_and_parse_every_field() {
        let t = ScriptedTransport::new();
        t.push_json(200, schedules_example());
        let page = waba(&t)
            .schedules(
                &ListWabaRecords::new()
                    .fields(["id", "name", "description", "delivery_time", "status"])
                    .limit(10)
                    .after("QVFJ"),
            )
            .await
            .unwrap();
        assert_eq!(page.data[0].id.as_deref(), Some("111"));
        assert_eq!(page.data[0].delivery_time, Some(1_767_225_600));
        assert_eq!(page.data[0].status, Some(ScheduleStatus::Scheduled));
        assert_eq!(page.data[1].status, Some(ScheduleStatus::Completed));
        assert_eq!(
            serde_json::to_value(&page.data).unwrap(),
            schedules_example()["data"]
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/102290129340398/schedules");
        assert_eq!(
            req.query("fields").as_deref(),
            Some("id,name,description,delivery_time,status")
        );
        assert_eq!(req.query("limit").as_deref(), Some("10"));
        assert_eq!(req.query("after").as_deref(), Some("QVFJ"));
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn schedules_stream_follows_cursors_and_refuses_the_callers() {
        let t = ScriptedTransport::new();
        let w = waba(&t);
        let refused: Vec<_> = w
            .schedules_stream(&ListWabaRecords::new().after("x"))
            .collect()
            .await;
        assert!(
            matches!(&refused[..], [Err(Error::Validation(_))]),
            "{refused:?}"
        );
        assert!(matches!(
            w.schedules(&ListWabaRecords::new().limit(101)).await,
            Err(Error::Validation(v)) if v.field == "limit"
        ));
        assert!(t.requests().is_empty());
        t.push_json(
            200,
            json!({"data": [{"name": "a"}], "paging": {"cursors": {"after": "c"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"name": "b"}]}));
        let names: Vec<_> = w
            .schedules_stream(&ListWabaRecords::new())
            .map(|s| s.unwrap().name.unwrap())
            .collect()
            .await;
        assert_eq!(names, ["a", "b"]);
        assert_eq!(t.requests()[1].query("after").as_deref(), Some("c"));
        assert_eq!(t.remaining(), 0);
    }

    /// `ScheduleCreateRequest`'s six fields, with its names and types
    /// (`delivery_time` an integer); the `ScheduleCreateResponse`.
    #[tokio::test]
    async fn create_sends_every_documented_field() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "222"}));
        let created = waba(&t).create_schedule(&schedule()).await.unwrap();
        assert_eq!(created.id, "222");
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/102290129340398/schedules");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "hsm_id": "1234567890",
                "audience_id": "9876543210",
                "waba_cs_id": "106540352242922",
                "description": "The summer sale announcement",
                "name": "Summer sale",
                "delivery_time": 1_767_225_600
            }))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn create_is_checked_before_any_request() {
        let t = ScriptedTransport::new();
        let w = waba(&t);
        let blank = |f: fn(&mut NewSchedule)| {
            let mut s = schedule();
            f(&mut s);
            s
        };
        for (s, field) in [
            (
                blank(|s| s.hsm_id = TemplateId::new_unchecked("")),
                "hsm_id",
            ),
            (blank(|s| s.audience_id = " ".into()), "audience_id"),
            (blank(|s| s.waba_cs_id = String::new()), "waba_cs_id"),
            (blank(|s| s.description = String::new()), "description"),
            (blank(|s| s.name = String::new()), "name"),
            (blank(|s| s.delivery_time = 0), "delivery_time"),
        ] {
            let err = w.create_schedule(&s).await.unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == field),
                "{field}: {err:?}"
            );
        }
        assert!(t.requests().is_empty());
    }

    /// A campaign is never scheduled twice by a replay.
    #[tokio::test]
    async fn create_is_never_replayed_after_a_timeout() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let retrying = client(
            &t,
            RetryPolicy {
                max_retries: 3,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            },
        );
        assert!(
            retrying
                .waba(WabaId::new("102290129340398").unwrap())
                .create_schedule(&schedule())
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 1);

        // The page's 403 example.
        let t = ScriptedTransport::new();
        t.push_json(
            403,
            json!({"error": {"message": "Your app doesn't have permission to access this WhatsApp Business Account", "type": "OAuthException", "code": 200, "error_subcode": 1349174, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        );
        let err = waba(&t).create_schedule(&schedule()).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }
}
