//! The Flows management API: `/{WABA_ID}/flows` and `/{FLOW_ID}/…`.
//!
//! Source: `flows/guides/flowsapi`; state rules from `flows/guides/lifecycle`.

use bytes::Bytes;
use futures::Stream;
use wa_core::Result;
use wa_core::ids::{FlowId, WabaId};
use wa_core::paging::Page;
use wa_core::transport::Multipart;

use super::types::{
    CreateFlow, CreatedFlow, FlowAsset, FlowDetails, FlowJsonUpload, FlowPreview, ListFlowAssets,
    ListFlows, UpdateFlow, validate_flow_json_len,
};
use crate::Client;
use crate::request::{paginate_or_error, reject_cursors};

/// Every field `GET /{FLOW_ID}` documents except `preview` (which has its own
/// call, [`Flow::preview`], because asking for it mints a link) and the
/// `data_channel_uri` field deprecated since Graph v19.0 in favour of
/// `endpoint_uri`.
const DETAIL_FIELDS: &str = "id,name,status,categories,validation_errors,json_version,\
data_api_version,endpoint_uri,whatsapp_business_account,application";

/// Flows owned by one WhatsApp Business Account. See [`Client::flows`].
#[derive(Debug, Clone)]
pub struct Flows {
    client: Client,
    waba_id: WabaId,
}

/// One Flow, by id. See [`Client::flow`].
#[derive(Debug, Clone)]
pub struct Flow {
    client: Client,
    flow_id: FlowId,
}

impl Client {
    /// [`Flows`] API for `waba_id`: create and list Flows.
    pub fn flows(&self, waba_id: impl Into<WabaId>) -> Flows {
        Flows {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }

    /// [`Flow`] API for one Flow: read, edit, upload JSON, publish,
    /// deprecate, delete. Flow ids are global, so no WABA id is needed.
    pub fn flow(&self, flow_id: impl Into<FlowId>) -> Flow {
        Flow {
            client: self.clone(),
            flow_id: flow_id.into(),
        }
    }
}

impl Flows {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// [`Flow`] API for a Flow of this account.
    pub fn flow(&self, flow_id: impl Into<FlowId>) -> Flow {
        self.client.flow(flow_id)
    }

    /// Create a Flow (`POST /{WABA_ID}/flows`). It starts as a draft unless
    /// [`CreateFlow::publish`] is set; Flow JSON problems come back in
    /// [`CreatedFlow::validation_errors`] rather than as an error.
    ///
    /// Not retried on timeouts: a replay would create a second Flow.
    pub async fn create(&self, flow: &CreateFlow) -> Result<CreatedFlow> {
        flow.validate()?;
        self.client
            .post_at(&[self.waba_id.as_str(), "flows"])
            .json(flow)
            .context("create flow response")
            .send()
            .await
    }

    /// One page of this account's Flows (`GET /{WABA_ID}/flows`), with the
    /// default fields. The next page: `ListFlows::new().after(..)` with
    /// this page's [`Page::next_cursor`].
    pub async fn list(&self, query: &ListFlows) -> Result<Page<FlowDetails>> {
        self.client
            .get_at(&[self.waba_id.as_str(), "flows"])
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .context("list flows response")
            .send()
            .await
    }

    /// Every Flow of this account, following cursors page by page. The
    /// stream manages them itself: a query with `after` or `before` set is
    /// refused (the stream's single item is that validation error).
    pub fn list_stream(
        &self,
        query: &ListFlows,
    ) -> impl Stream<Item = Result<FlowDetails>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref()).map(|()| {
                self.client
                    .get_at(&[self.waba_id.as_str(), "flows"])
                    .context("list flows response")
            }),
        )
    }
}

impl Flow {
    /// The id this API is scoped to.
    pub fn id(&self) -> &FlowId {
        &self.flow_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Details of the Flow (`GET /{FLOW_ID}`), with every documented field
    /// except `preview`.
    pub async fn get(&self) -> Result<FlowDetails> {
        self.client
            .get_at(&[self.flow_id.as_str()])
            .query("fields", DETAIL_FIELDS)
            .context("flow details response")
            .send()
            .await
    }

    /// The Flow's web preview link (`GET /{FLOW_ID}?fields=preview.invalidate(..)`).
    ///
    /// Links last 30 days. `invalidate = true` revokes the current link and
    /// returns a new one — use it when a link was shared too widely.
    pub async fn preview(&self, invalidate: bool) -> Result<FlowPreview> {
        #[derive(serde::Deserialize)]
        struct PreviewResponse {
            preview: FlowPreview,
        }
        let response: PreviewResponse = self
            .client
            .get_at(&[self.flow_id.as_str()])
            .query("fields", format!("preview.invalidate({invalidate})"))
            .context("flow preview response")
            .send()
            .await?;
        Ok(response.preview)
    }

    /// Change the Flow's name, categories or endpoint (`POST /{FLOW_ID}`).
    ///
    /// Only drafts can be changed (`flows/guides/lifecycle`); Meta rejects
    /// edits to published Flows. Replayed on transient errors: setting the
    /// same values twice has the same effect as once.
    pub async fn update(&self, update: &UpdateFlow) -> Result<()> {
        update.validate()?;
        self.client
            .post_at(&[self.flow_id.as_str()])
            .json(update)
            .idempotent(true)
            .context("update flow response")
            .send_success()
            .await
    }

    /// Replace the Flow JSON (`POST /{FLOW_ID}/assets`, multipart, as the docs
    /// require: `file` = `flow.json` as `application/json`, `name` =
    /// `flow.json`, `asset_type` = `FLOW_JSON`).
    ///
    /// `flow_json` is the JSON text (at most 10 MB). The upload is stored
    /// even when Meta finds problems in it; those come back in
    /// [`FlowJsonUpload::validation_errors`]. Replayed on transient errors:
    /// uploading the same file twice leaves the same asset.
    pub async fn upload_flow_json(&self, flow_json: impl Into<Bytes>) -> Result<FlowJsonUpload> {
        let flow_json = flow_json.into();
        validate_flow_json_len("file", flow_json.len())?;
        let form = Multipart::new()
            .file("file", "flow.json", "application/json", flow_json)
            .text("name", "flow.json")
            .text("asset_type", "FLOW_JSON");
        self.client
            .post_at(&[self.flow_id.as_str(), "assets"])
            .multipart(form)
            .idempotent(true)
            .context("upload flow json response")
            .send()
            .await
    }

    /// One page of the Flow's assets (`GET /{FLOW_ID}/assets`). The next
    /// page: `ListFlowAssets::new().after(..)` with this page's
    /// [`Page::next_cursor`].
    pub async fn assets(&self, query: &ListFlowAssets) -> Result<Page<FlowAsset>> {
        self.client
            .get_at(&[self.flow_id.as_str(), "assets"])
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .context("flow assets response")
            .send()
            .await
    }

    /// Every asset of the Flow, following cursors. The stream manages them
    /// itself: a query with `after` or `before` set is refused (the
    /// stream's single item is that validation error).
    pub fn assets_stream(
        &self,
        query: &ListFlowAssets,
    ) -> impl Stream<Item = Result<FlowAsset>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref()).map(|()| {
                self.client
                    .get_at(&[self.flow_id.as_str(), "assets"])
                    .context("flow assets response")
            }),
        )
    }

    /// Publish the Flow (`POST /{FLOW_ID}/publish`). Irreversible: a
    /// published Flow can no longer be edited or deleted, only deprecated.
    /// Meta refuses while validation errors or publishing checks
    /// (`flows/guides/healthmonitoring`) are outstanding.
    pub async fn publish(&self) -> Result<()> {
        self.client
            .post_at(&[self.flow_id.as_str(), "publish"])
            .context("publish flow response")
            .send_success()
            .await
    }

    /// Deprecate a published Flow (`POST /{FLOW_ID}/deprecate`): it can no
    /// longer be sent or opened. Irreversible.
    pub async fn deprecate(&self) -> Result<()> {
        self.client
            .post_at(&[self.flow_id.as_str(), "deprecate"])
            .context("deprecate flow response")
            .send_success()
            .await
    }

    /// Delete the Flow (`DELETE /{FLOW_ID}`). Only drafts can be deleted.
    pub async fn delete(&self) -> Result<()> {
        self.client
            .delete_at(&[self.flow_id.as_str()])
            .context("delete flow response")
            .send_success()
            .await
    }
}
