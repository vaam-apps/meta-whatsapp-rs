//! Actions (`flows/guides/flowjson`, § Actions): what a component does when
//! tapped, selected or unselected, told apart by `name`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::tagged::tagged_union;

/// An action payload: keys to values, each value a literal or a reference
/// (`${form.first_name}`).
pub type Payload = Map<String, Value>;

tagged_union! {
    /// An action (`on-click-action`, `on-select-action`,
    /// `on-unselect-action`), by its `name`.
    ///
    /// Which components accept which actions is in each component's docs:
    /// `complete` only on a `Footer` of a terminal screen, `open_url` (Flow
    /// JSON 6.0+) only on `EmbeddedLink` and `OptIn`, `update_data` (6.0+)
    /// on selections.
    pub enum Action by "name" {
        /// `navigate`: open another screen, its `payload` becoming that
        /// screen's `${data.…}`.
        Navigate(NavigateAction) = "navigate",
        /// `complete`: end the Flow; the payload reaches the business in the
        /// response message webhook.
        Complete(CompleteAction) = "complete",
        /// `data_exchange`: send the payload to the Flow's data endpoint.
        DataExchange(DataExchangeAction) = "data_exchange",
        /// `update_data` (Flow JSON 6.0+): update this screen's data at once.
        UpdateData(UpdateDataAction) = "update_data",
        /// `open_url` (Flow JSON 6.0+): open a link in the device's browser.
        OpenUrl(OpenUrlAction) = "open_url",
    }
}

/// The `navigate` action's target: `{"type": "screen", "name": …}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NextScreen {
    /// `type`; `screen` is the only documented value.
    #[serde(rename = "type")]
    pub kind: String,
    /// The target screen's id.
    pub name: String,
    /// Properties this version does not model, kept as sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl NextScreen {
    /// The screen `id`.
    pub fn screen(id: impl Into<String>) -> Self {
        Self {
            kind: "screen".to_owned(),
            name: id.into(),
            extra: Map::new(),
        }
    }
}

/// `navigate`. Build it with [`NavigateAction::new`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigateAction {
    /// Where to go.
    pub next: NextScreen,
    /// Data for the next screen. Meta says not to use `navigate` on a
    /// terminal screen's `Footer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Payload>,
    /// Properties this version does not model, kept as sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl NavigateAction {
    /// Navigate to the screen `screen_id`, with an empty payload.
    pub fn new(screen_id: impl Into<String>) -> Self {
        Self {
            next: NextScreen::screen(screen_id),
            payload: Some(Payload::new()),
            extra: Map::new(),
        }
    }

    /// Add `key` to the payload.
    #[must_use]
    pub fn payload(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.payload
            .get_or_insert_with(Payload::new)
            .insert(key.into(), value.into());
        self
    }
}

macro_rules! payload_action {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            /// The payload (keys to literals or references).
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub payload: Option<Payload>,
            /// Properties this version does not model, kept as sent.
            #[serde(flatten)]
            pub extra: Map<String, Value>,
        }

        impl $name {
            /// The action with an empty payload (`"payload": {}`).
            pub fn new() -> Self {
                Self { payload: Some(Payload::new()), extra: Map::new() }
            }

            /// Add `key` to the payload.
            #[must_use]
            pub fn payload(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
                self.payload
                    .get_or_insert_with(Payload::new)
                    .insert(key.into(), value.into());
                self
            }
        }
    };
}

payload_action! {
    /// `complete`. Its payload comes back in the response message webhook:
    /// Meta asks to keep it to what the user entered, and small.
    CompleteAction
}

payload_action! {
    /// `data_exchange`. Needs a data endpoint (`data_api_version`).
    DataExchangeAction
}

payload_action! {
    /// `update_data` (Flow JSON 6.0+). Each payload key names a field of
    /// this screen's data.
    UpdateDataAction
}

/// `open_url` (Flow JSON 6.0+). It takes no payload, only `url`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenUrlAction {
    /// The link to open.
    pub url: String,
    /// Properties this version does not model, kept as sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl OpenUrlAction {
    /// Open `url`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            extra: Map::new(),
        }
    }
}

impl Action {
    /// `navigate` to `screen_id` with an empty payload.
    pub fn navigate(screen_id: impl Into<String>) -> Self {
        NavigateAction::new(screen_id).into()
    }

    /// `complete` with an empty payload.
    pub fn complete() -> Self {
        CompleteAction::new().into()
    }

    /// `data_exchange` with an empty payload.
    pub fn data_exchange() -> Self {
        DataExchangeAction::new().into()
    }

    /// `update_data` with an empty payload.
    pub fn update_data() -> Self {
        UpdateDataAction::new().into()
    }

    /// `open_url` to `url`.
    pub fn open_url(url: impl Into<String>) -> Self {
        OpenUrlAction::new(url).into()
    }
}
