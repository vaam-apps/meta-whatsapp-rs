//! Roadmap L18's decisive test: every Flow JSON example of Meta's pages
//! round-trips through the typed model as the same JSON value, and the main
//! ones come out of the builder identical.
//!
//! Fixtures (`tests/fixtures/flows/flow_json/<page>/`) are the JSON examples
//! of `flows/guides/<page>` (`error-codes` is `flows/reference/error-codes`),
//! copied by their kind: `flow-*` a whole Flow JSON, `component-*` one
//! component, `action-*` one action, `data-*` a screen's data model,
//! `sample-*` an error-code sample. Where a page's example is not JSON as
//! printed, the fixture is the JSON it shows: `"screens": [...]` is
//! `"screens": []`, a `"on-click-action": {…}` fragment is its value, the
//! `{"data": …}` wrapper of a data model is dropped, Markdown's `\|` in a
//! `RichText` table is `|`, and the `flow_json` strings of `flowsapi` and
//! `flows-templates` are decoded (the latter's shell-escaped `\'` read as
//! `'`).

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use meta_whatsapp_client::flows::flow_json::{
    Action, CarouselImage, ChipsSelector, Component, DataField, DataItems, DataSourceItem, Dynamic,
    FlowJson, Footer, ImageCarousel, RadioButtonsGroup, ScaleType, Screen, TextHeading,
    UpdateDataAction,
};
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flows/flow_json")
}

fn load(rel: &str) -> Value {
    let path = fixtures().join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Every fixture of one page, as `(file name, value)`.
fn page(name: &str) -> Vec<(String, Value)> {
    let mut files: Vec<_> = std::fs::read_dir(fixtures().join(name))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|f| {
            Path::new(f)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|f| {
            let value = load(&format!("{name}/{f}"));
            (f, value)
        })
        .collect()
}

/// Parse `value` as `T`, serialize it back (to a value and through text,
/// as it is uploaded) and compare.
fn round_trip<T: DeserializeOwned + Serialize>(name: &str, value: &Value) -> T {
    let typed: T = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(&serde_json::to_value(&typed).unwrap(), value, "{name}");
    let text = serde_json::to_string(&typed).unwrap();
    assert_eq!(
        &serde_json::from_str::<Value>(&text).unwrap(),
        value,
        "{name}"
    );
    typed
}

/// Fails when a documented example used something the model does not know
/// (an unknown component or action, or a property left in `extra`): the
/// round trip alone would still pass, since unknown things are kept whole.
struct Modelled<'a>(&'a str);

impl Modelled<'_> {
    fn extra(&self, at: &str, extra: &Map<String, Value>) {
        assert!(
            extra.is_empty(),
            "{}: {at} has unmodelled properties {:?}",
            self.0,
            extra.keys().collect::<Vec<_>>()
        );
    }

    fn flow(&self, f: &FlowJson) {
        self.extra("the document", &f.extra);
        for s in &f.screens {
            self.screen(s);
        }
    }

    fn screen(&self, s: &Screen) {
        self.extra(&s.id, &s.extra);
        self.extra(&s.id, &s.layout.extra);
        for field in s.data.iter().flat_map(BTreeMap::values) {
            self.data(field);
        }
        self.components(&s.layout.children);
    }

    fn data(&self, f: &DataField) {
        self.extra("a data field", &f.extra);
        assert!(f.kind.is_some(), "{}: a data field without type", self.0);
        if let Some(DataItems::Schema(items)) = &f.items {
            self.data(items);
        }
        for p in f.properties.iter().flat_map(BTreeMap::values) {
            self.data(p);
        }
    }

    fn components(&self, cs: &[Component]) {
        for c in cs {
            self.component(c);
        }
    }

    fn items(&self, source: &Dynamic<Vec<DataSourceItem>>) {
        for item in source.as_static().into_iter().flatten() {
            self.extra("an option", &item.extra);
            self.actions([&item.on_select_action, &item.on_unselect_action]);
        }
    }

    fn actions<'b>(&self, actions: impl IntoIterator<Item = &'b Option<Action>>) {
        for a in actions.into_iter().flatten() {
            self.action(a);
        }
    }

    fn action(&self, a: &Action) {
        let extra = match a {
            Action::Navigate(n) => {
                self.extra("navigate.next", &n.next.extra);
                &n.extra
            }
            Action::Complete(c) => &c.extra,
            Action::DataExchange(d) => &d.extra,
            Action::UpdateData(u) => &u.extra,
            Action::OpenUrl(o) => &o.extra,
            Action::Unknown { raw } => panic!("{}: unknown action {raw}", self.0),
            _ => panic!("{}: an action this test does not walk", self.0),
        };
        self.extra("an action", extra);
    }

    fn component(&self, c: &Component) {
        let tag = c.tag().unwrap_or("?").to_owned();
        let extra = match c {
            Component::TextHeading(x) => &x.extra,
            Component::TextSubheading(x) => &x.extra,
            Component::TextBody(x) => &x.extra,
            Component::TextCaption(x) => &x.extra,
            Component::RichText(x) => &x.extra,
            Component::TextInput(x) => &x.extra,
            Component::TextArea(x) => &x.extra,
            Component::CheckboxGroup(x) => {
                self.items(&x.data_source);
                self.actions([&x.on_select_action]);
                &x.extra
            }
            Component::RadioButtonsGroup(x) => {
                self.items(&x.data_source);
                self.actions([&x.on_select_action]);
                &x.extra
            }
            Component::Dropdown(x) => {
                self.items(&x.data_source);
                self.actions([&x.on_select_action]);
                &x.extra
            }
            Component::ChipsSelector(x) => {
                self.items(&x.data_source);
                self.actions([&x.on_select_action, &x.on_unselect_action]);
                &x.extra
            }
            Component::Footer(x) => {
                self.action(&x.on_click_action);
                &x.extra
            }
            Component::OptIn(x) => {
                self.actions([&x.on_click_action]);
                &x.extra
            }
            Component::EmbeddedLink(x) => {
                self.action(&x.on_click_action);
                &x.extra
            }
            Component::DatePicker(x) => {
                self.actions([&x.on_select_action]);
                &x.extra
            }
            Component::CalendarPicker(x) => {
                self.actions([&x.on_select_action]);
                &x.extra
            }
            Component::Image(x) => &x.extra,
            Component::ImageCarousel(x) => {
                for image in x.images.as_static().into_iter().flatten() {
                    self.extra("an image", &image.extra);
                }
                &x.extra
            }
            Component::If(x) => {
                self.components(&x.then);
                self.components(x.else_.as_deref().unwrap_or_default());
                &x.extra
            }
            Component::Switch(x) => {
                for case in x.cases.values() {
                    self.components(case);
                }
                &x.extra
            }
            Component::NavigationList(x) => {
                self.actions([&x.on_click_action]);
                for item in x.list_items.as_static().into_iter().flatten() {
                    self.extra("a list item", &item.extra);
                    self.extra("main-content", &item.main_content.extra);
                    if let Some(end) = &item.end {
                        self.extra("end", &end.extra);
                    }
                    if let Some(start) = &item.start {
                        self.extra("start", &start.extra);
                    }
                    self.actions([&item.on_click_action]);
                }
                &x.extra
            }
            Component::PhotoPicker(x) => &x.extra,
            Component::DocumentPicker(x) => &x.extra,
            Component::Form(x) => {
                self.components(&x.children);
                &x.extra
            }
            Component::Unknown { raw } => panic!("{}: unknown component {raw}", self.0),
            _ => panic!("{}: a component this test does not walk: {tag}", self.0),
        };
        self.extra(&tag, extra);
    }
}

/// Round-trips every fixture of a documentation page by kind, and checks
/// the model knew everything in it. Returns how many of each kind.
fn check_page(name: &str) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for (file, value) in page(name) {
        let label = format!("{name}/{file}");
        let kind = if file.starts_with("flow-") {
            let f: FlowJson = round_trip(&label, &value);
            Modelled(&label).flow(&f);
            if !f.screens.is_empty() {
                f.validate().unwrap_or_else(|e| panic!("{label}: {e}"));
            }
            "flow"
        } else if file.starts_with("component-") {
            let c: Component = round_trip(&label, &value);
            Modelled(&label).component(&c);
            "component"
        } else if file.starts_with("action-") {
            let a: Action = round_trip(&label, &value);
            Modelled(&label).action(&a);
            "action"
        } else if file.starts_with("data-") {
            let d: BTreeMap<String, DataField> = round_trip(&label, &value);
            for field in d.values() {
                Modelled(&label).data(field);
            }
            "data"
        } else {
            panic!("{label}: unknown fixture kind");
        };
        *counts.entry(kind).or_default() += 1;
    }
    counts
}

#[test]
fn every_flow_json_example_round_trips_through_the_typed_model() {
    // flows/guides/flowjson: the decisive page. The counts pin the corpus,
    // so a fixture that stops being read fails here.
    assert_eq!(
        check_page("flowjson"),
        BTreeMap::from([("action", 2), ("component", 21), ("data", 1), ("flow", 3)])
    );
    assert_eq!(
        check_page("components"),
        BTreeMap::from([("action", 1), ("component", 18), ("data", 1), ("flow", 2)])
    );
    assert_eq!(check_page("media_upload"), BTreeMap::from([("action", 4)]));
    assert_eq!(check_page("flowsapi"), BTreeMap::from([("flow", 1)]));
    assert_eq!(check_page("flows-templates"), BTreeMap::from([("flow", 1)]));
}

/// The error-code samples are deliberately wrong (and often elided), so
/// they are not all Flow JSON. Each one either parses and serializes back
/// unchanged, or is refused: never silently altered. A wrong property inside
/// a component does not fail parsing (the component is kept whole as
/// `Unknown`); a wrong document skeleton does.
#[test]
fn error_code_samples_round_trip_or_are_refused_and_fail_validation() {
    let mut outcomes = BTreeMap::new();
    for (file, value) in page("error-codes") {
        let label = format!("error-codes/{file}");
        let outcome = if value.get("screens").is_some() {
            match try_round_trip::<FlowJson>(&label, &value) {
                Some(f) if f.validate().is_ok() => "document, locally accepted",
                Some(_) => "document, locally refused",
                None => "refused",
            }
        } else if value.get("type").is_some() {
            try_round_trip::<Component>(&label, &value).map_or("refused", |_| "component")
        } else if value.get("layout").is_some() {
            try_round_trip::<Screen>(&label, &value).map_or("refused", |_| "screen")
        } else {
            "refused"
        };
        outcomes.insert(file, outcome);
    }
    let got: BTreeMap<&str, Vec<&str>> =
        outcomes
            .iter()
            .fold(BTreeMap::new(), |mut acc, (file, outcome)| {
                acc.entry(*outcome).or_default().push(file.as_str());
                acc
            });
    assert_eq!(
        got,
        BTreeMap::from([
            // Elided: no `version`, or a layout without its `type`.
            (
                "refused",
                vec![
                    "sample-05.json",
                    "sample-07.json",
                    "sample-08.json",
                    "sample-09.json",
                    "sample-10.json",
                    "sample-11.json",
                    "sample-52.json",
                    "sample-64.json",
                    "sample-65.json",
                    "sample-67.json",
                    "sample-93.json",
                    "sample-94.json",
                    "sample-95.json",
                    "sample-96.json",
                ]
            ),
            (
                "component",
                vec!["sample-03.json", "sample-35.json", "sample-36.json"]
            ),
            ("screen", vec!["sample-60.json"]),
            (
                "document, locally refused",
                vec![
                    "sample-06.json",
                    "sample-41.json",
                    "sample-51.json",
                    "sample-53.json",
                    "sample-62.json",
                    "sample-68.json",
                    "sample-71.json",
                    "sample-72.json",
                    "sample-75.json",
                    "sample-81.json",
                    "sample-82.json",
                    "sample-90.json",
                ]
            ),
            // Its error is a global reference into a data model's schema
            // (`${screen.ROOT.data.products.items…}`): references are Meta's
            // to check, not the local checks'.
            ("document, locally accepted", vec!["sample-61.json"]),
        ])
    );
}

fn try_round_trip<T: DeserializeOwned + Serialize>(label: &str, value: &Value) -> Option<T> {
    let typed: T = serde_json::from_value(value.clone()).ok()?;
    assert_eq!(&serde_json::to_value(&typed).unwrap(), value, "{label}");
    Some(typed)
}

/// Most components have no JSON example in the Markdown pages (Meta renders
/// them as previews), so the decisive test never sees their wire names.
/// Here every property of each component's table in
/// `flows/guides/components` and `flows/guides/media_upload` (and the
/// changelog's `label-variant` and `ChipsSelector` root actions) is set
/// once, with its documented name: each must land in a typed field, none
/// in `extra`, and the object must come back unchanged.
#[test]
fn every_documented_property_of_every_component_is_modelled() {
    let update = json!({"name": "update_data", "payload": {"x": "${form.a}"}});
    let exchange = json!({"name": "data_exchange", "payload": {}});
    let option = json!({"id": "a", "title": "A", "description": "d", "metadata": "m",
        "enabled": true, "image": "iVBO", "alt-text": "alt", "color": "#ff0000",
        "on-select-action": update, "on-unselect-action": update});
    let range = |v: Value| json!({"start-date": v, "end-date": v});
    let components = [
        json!({"type": "TextHeading", "text": "t", "visible": true}),
        json!({"type": "TextSubheading", "text": "t", "visible": "${data.v}"}),
        json!({"type": "TextBody", "text": "t", "font-weight": "bold", "strikethrough": false,
            "visible": true, "markdown": false}),
        json!({"type": "TextCaption", "text": ["t"], "font-weight": "${data.fw}",
            "strikethrough": true, "visible": true, "markdown": true}),
        json!({"type": "RichText", "text": ["# T"], "visible": true}),
        json!({"type": "TextInput", "name": "n", "label": "L", "input-type": "passcode",
            "pattern": "^[0-9]+$", "required": true, "min-chars": 1, "max-chars": "${data.max}",
            "helper-text": "h", "visible": true, "init-value": "i", "error-message": "e",
            "label-variant": "large"}),
        json!({"type": "TextArea", "name": "n", "label": "L", "required": true,
            "max-length": 600, "helper-text": "h", "enabled": true, "visible": true,
            "init-value": "i", "error-message": "e", "label-variant": "large"}),
        json!({"type": "CheckboxGroup", "name": "n", "label": "L", "data-source": [option],
            "min-selected-items": 1, "max-selected-items": 2, "enabled": true,
            "required": true, "visible": true, "on-select-action": exchange,
            "description": "d", "init-value": ["a"], "error-message": "e",
            "media-size": "large"}),
        json!({"type": "RadioButtonsGroup", "name": "n", "label": "L", "data-source": [option],
            "enabled": true, "required": true, "visible": true, "on-select-action": exchange,
            "description": "d", "init-value": "a", "error-message": "e",
            "media-size": "regular"}),
        json!({"type": "Dropdown", "name": "n", "label": "L", "data-source": "${data.opts}",
            "required": true, "enabled": true, "visible": true,
            "on-select-action": exchange, "init-value": "a", "error-message": "e"}),
        json!({"type": "ChipsSelector", "name": "n", "label": "L",
            "data-source": [{"id": "a", "title": "A", "enabled": true,
                "on-select-action": update, "on-unselect-action": update}],
            "min-selected-items": 1, "max-selected-items": 2, "enabled": true,
            "required": true, "visible": true, "description": "d", "init-value": ["a"],
            "error-message": "e", "on-select-action": update, "on-unselect-action": update}),
        json!({"type": "Footer", "label": "Go", "left-caption": "l", "right-caption": "r",
            "enabled": true, "on-click-action": {"name": "complete", "payload": {}}}),
        json!({"type": "Footer", "label": "Go", "center-caption": "c",
            "on-click-action": {"name": "navigate", "next": {"type": "screen", "name": "B"},
                "payload": {}}}),
        json!({"type": "OptIn", "name": "n", "label": "L", "required": true,
            "on-click-action": {"name": "open_url", "url": "https://example.com"},
            "visible": true, "init-value": true}),
        json!({"type": "EmbeddedLink", "text": "t", "visible": true,
            "on-click-action": exchange}),
        json!({"type": "DatePicker", "name": "n", "label": "L", "min-date": "2024-01-01",
            "max-date": "2024-12-31", "unavailable-dates": ["2024-06-01"], "visible": true,
            "helper-text": "h", "enabled": true, "on-select-action": exchange,
            "init-value": "2024-03-01", "error-message": "e"}),
        json!({"type": "CalendarPicker", "name": "n", "title": "T", "description": "d",
            "label": range(json!("L")), "helper-text": range(json!("h")),
            "required": range(json!(true)), "visible": true, "enabled": true, "mode": "range",
            "min-date": "2024-01-01", "max-date": "2024-12-31",
            "unavailable-dates": ["2024-06-01"], "include-days": ["Mon", "Fri"],
            "min-days": 1, "max-days": 7, "on-select-action": exchange,
            "init-value": range(json!("2024-03-01")), "error-message": range(json!("e"))}),
        json!({"type": "CalendarPicker", "name": "n", "label": "L", "helper-text": "h",
            "required": "${data.r}", "mode": "single", "init-value": "2024-03-01",
            "error-message": "e"}),
        json!({"type": "Image", "src": "iVBO", "width": 100, "height": 50,
            "scale-type": "cover", "aspect-ratio": 1.5, "alt-text": "alt"}),
        json!({"type": "ImageCarousel", "images": [{"src": "iVBO", "alt-text": "alt"}],
            "aspect-ratio": "16:9", "scale-type": "contain"}),
        json!({"type": "If", "condition": "${data.a}",
            "then": [{"type": "TextBody", "text": "t"}],
            "else": [{"type": "TextBody", "text": "f"}]}),
        json!({"type": "Switch", "value": "${data.k}",
            "cases": {"a": [{"type": "TextBody", "text": "t"}]}}),
        json!({"type": "NavigationList", "name": "n", "label": "L", "description": "d",
            "media-size": "regular", "on-click-action": exchange,
            "list-items": [{"id": "a",
                "main-content": {"title": "T", "description": "d", "metadata": "m"},
                "end": {"title": "T", "description": "d", "metadata": "m"},
                "start": {"image": "iVBO", "alt-text": "alt"},
                "badge": "New", "tags": ["x"]}]}),
        json!({"type": "NavigationList", "name": "n",
            "list-items": [{"main-content": {"title": "T"}, "on-click-action": exchange}]}),
        json!({"type": "PhotoPicker", "name": "n", "label": "L", "description": "d",
            "photo-source": "camera", "max-file-size-kb": 1024, "min-uploaded-photos": 1,
            "max-uploaded-photos": 3, "enabled": true, "visible": "${data.v}",
            "error-message": "e"}),
        json!({"type": "DocumentPicker", "name": "n", "label": "L", "description": "d",
            "max-file-size-kb": 1024, "min-uploaded-documents": 0,
            "max-uploaded-documents": 3, "allowed-mime-types": ["application/pdf"],
            "enabled": true, "visible": true, "error-message": {"media_1": "e"}}),
        json!({"type": "Form", "name": "f", "init-values": {"n": "v"},
            "error-messages": "${data.errors}",
            "children": [{"type": "TextInput", "name": "n", "label": "L"}]}),
    ];
    let mut tags = std::collections::BTreeSet::new();
    for value in &components {
        let label = format!("{}", value["type"]);
        let c: Component = round_trip(&label, value);
        Modelled(&label).component(&c);
        tags.insert(c.tag().unwrap().to_owned());
    }
    // Every variant of `Component` but `Unknown`.
    assert_eq!(tags.len(), 24, "{tags:?}");
}

#[test]
fn the_flowsapi_example_comes_out_of_the_builder() {
    let built = FlowJson::builder("5.0")
        .screen(
            Screen::new("WELCOME_SCREEN")
                .title("Welcome")
                .terminal(true)
                .success(true)
                .empty_data()
                .child(TextHeading::new("Hello World"))
                .child(Footer::new("Complete", Action::complete())),
        )
        .build()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&built).unwrap(),
        load("flowsapi/flow-create-request.json")
    );
}

#[test]
fn the_chips_selector_example_comes_out_of_the_builder() {
    let options = [
        ("room_layout", "🏡 Room layouts"),
        ("lighting", "💡 Lighting"),
        ("renovation", "🛠️ Renovation"),
        ("furnitures", "📐 Room layouts"),
    ]
    .map(|(id, title)| DataSourceItem::new(id, title));
    let built = FlowJson::builder("6.3")
        .screen(
            Screen::new("DEMO_SCREEN")
                .terminal(true)
                .title("Demo screen")
                .child(
                    ChipsSelector::new("chips", "Personalize your experience", options.to_vec())
                        .description(
                            "Choose your interests to get personalized design ideas and solution",
                        )
                        .max_selected_items(2),
                )
                .child(Footer::new("Continue", Action::complete())),
        )
        .build()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&built).unwrap(),
        load("components/flow-chips-selector.json")
    );
}

#[test]
fn the_image_carousel_example_comes_out_of_the_builder() {
    let fixture = load("components/flow-image-carousel.json");
    let src = |i: usize| {
        fixture["screens"][0]["layout"]["children"][0]["images"][i]["src"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let images = vec![
        CarouselImage::new(src(0), "Landscape image"),
        CarouselImage::new(src(1), "Square image"),
        CarouselImage::new(src(2), "Portrait image"),
    ];
    let built = FlowJson::builder("7.1")
        .screen(
            Screen::new("DEMO_SCREEN")
                .terminal(true)
                .title("Demo screen")
                .child(ImageCarousel::new(images).scale_type(ScaleType::Cover))
                .child(Footer::new("Continue", Action::complete())),
        )
        .build()
        .unwrap();
    assert_eq!(serde_json::to_value(&built).unwrap(), fixture);
}

#[test]
fn the_update_data_example_comes_out_of_the_builder() {
    let fixture = load("flowjson/flow-update-data-radio-buttons.json");
    // The `__example__` values are mock data, not structure: taken from the
    // fixture rather than retyped.
    let example = |field: &str| fixture["screens"][0]["data"][field]["__example__"].clone();
    let option =
        || DataField::object([("id", DataField::string()), ("title", DataField::string())]);
    let radio = |name: &str, label: &str, source: &str| {
        RadioButtonsGroup::new(name, Dynamic::data(source)).label(label)
    };
    let built = FlowJson::builder("6.0")
        .screen(
            Screen::new("ADDRESS_SELECTION")
                .child(radio("select_country", "Select country:", "countries"))
                .child(
                    radio("select_states", "Select state:", "states")
                        .visible(Dynamic::data("state_visibility")),
                )
                .child(
                    radio("pincode", "Select pincode:", "pincode")
                        .visible(Dynamic::data("pincode_visibility")),
                )
                .child(Footer::new("Complete", Action::complete()))
                .title("Address selection")
                .terminal(true)
                .success(true)
                .data_field(
                    "countries",
                    DataField::array(option()).example(example("countries")),
                )
                .data_field("states", DataField::array(option()).example(json!([])))
                .data_field("pincode", DataField::array(option()).example(json!([])))
                .data_field("state_visibility", DataField::boolean().example(false))
                .data_field("pincode_visibility", DataField::boolean().example(false)),
        )
        .build()
        .unwrap();
    assert_eq!(serde_json::to_value(&built).unwrap(), fixture);

    // The example's `update_data` actions sit inside mock data, so they are
    // plain JSON there; they parse as actions all the same.
    let usa = &example("countries")[0];
    let action: Action = serde_json::from_value(usa["on-unselect-action"].clone()).unwrap();
    assert_eq!(
        action,
        UpdateDataAction::new()
            .payload("state_visibility", false)
            .payload("pincode_visibility", false)
            .into()
    );
}

#[test]
fn unknown_components_actions_and_properties_are_kept_as_sent() {
    let value = json!({
        "version": "9.9",
        "future_top_level": {"a": 1},
        "screens": [{
            "id": "ONLY",
            "terminal": true,
            "future_screen_flag": true,
            "layout": {
                "type": "SingleColumnLayout",
                "future_layout_prop": [1, 2],
                "children": [
                    {"type": "HologramPicker", "name": "h", "depth": 3},
                    {"no_type_at_all": true},
                    {"type": "TextHeading", "text": "Hi", "future-glow": "soft"},
                    {"type": "Footer", "label": "Go", "on-click-action":
                        {"name": "teleport", "where": "moon"}},
                    {"type": "EmbeddedLink", "text": "Terms", "on-click-action":
                        {"name": "open_url", "url": "https://example.com", "target": "_blank"}}
                ]
            }
        }]
    });
    let flow: FlowJson = round_trip("synthetic", &value);
    assert_eq!(flow.extra["future_top_level"], json!({"a": 1}));
    assert_eq!(flow.screens[0].extra["future_screen_flag"], json!(true));
    assert_eq!(
        flow.screens[0].layout.extra["future_layout_prop"],
        json!([1, 2])
    );
    let children = &flow.screens[0].layout.children;
    assert!(
        matches!(&children[0], Component::Unknown { raw } if raw["depth"] == 3),
        "{:?}",
        children[0]
    );
    assert_eq!(children[0].tag(), Some("HologramPicker"));
    assert!(matches!(&children[1], Component::Unknown { .. }));
    assert_eq!(children[1].tag(), None);
    let Component::TextHeading(heading) = &children[2] else {
        panic!("{:?}", children[2]);
    };
    assert_eq!(heading.extra["future-glow"], json!("soft"));
    let Component::Footer(footer) = &children[3] else {
        panic!("{:?}", children[3]);
    };
    assert!(matches!(&footer.on_click_action, Action::Unknown { raw } if raw["where"] == "moon"));
    let Component::EmbeddedLink(link) = &children[4] else {
        panic!("{:?}", children[4]);
    };
    let Action::OpenUrl(open) = &link.on_click_action else {
        panic!("{:?}", link.on_click_action);
    };
    assert_eq!(open.extra["target"], json!("_blank"));
}

#[test]
fn a_documented_example_reads_as_typed_values() {
    let flow: FlowJson =
        serde_json::from_value(load("components/flow-chips-selector.json")).unwrap();
    let Component::ChipsSelector(chips) = &flow.screens[0].layout.children[0] else {
        panic!("not a ChipsSelector");
    };
    assert_eq!(chips.max_selected_items, Some(Dynamic::Static(2)));
    assert_eq!(chips.data_source.as_static().map(Vec::len), Some(4));

    let c: Component =
        serde_json::from_value(load("flowjson/component-equality-booleans.json")).unwrap();
    let Component::TextBody(body) = c else {
        panic!("not a TextBody");
    };
    assert_eq!(
        body.visible,
        Some(Dynamic::Expression("`${form.accept} != true`".into()))
    );

    let flow: FlowJson =
        serde_json::from_value(load("flowjson/flow-update-data-radio-buttons.json")).unwrap();
    let Component::RadioButtonsGroup(states) = &flow.screens[0].layout.children[1] else {
        panic!("not a RadioButtonsGroup");
    };
    assert_eq!(states.data_source, Dynamic::data("states"));
    assert_eq!(states.visible, Some(Dynamic::data("state_visibility")));
}
