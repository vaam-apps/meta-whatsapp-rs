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
/// unchanged, or is refused: never silently altered. Those that parse as a
/// document are each refused by local validation, as Meta refuses them.
#[test]
fn error_code_samples_round_trip_or_are_refused_and_fail_validation() {
    let (mut parsed, mut accepted) = (Vec::new(), Vec::new());
    for (file, value) in page("error-codes") {
        let label = format!("error-codes/{file}");
        let attempt = if value.get("screens").is_some() {
            try_round_trip::<FlowJson>(&label, &value).map(|f| {
                if f.validate().is_ok() {
                    accepted.push(file.clone());
                }
            })
        } else if value.get("type").is_some() {
            try_round_trip::<Component>(&label, &value).map(drop)
        } else if value.get("layout").is_some() {
            try_round_trip::<Screen>(&label, &value).map(drop)
        } else {
            None
        };
        if attempt.is_some() {
            parsed.push(file);
        }
    }
    assert_eq!(
        parsed,
        [
            "sample-06.json",
            "sample-51.json",
            "sample-60.json",
            "sample-61.json",
            "sample-62.json",
            "sample-68.json",
            "sample-75.json",
            "sample-81.json",
            "sample-82.json",
            "sample-90.json",
        ]
    );
}

fn try_round_trip<T: DeserializeOwned + Serialize>(label: &str, value: &Value) -> Option<T> {
    let typed: T = serde_json::from_value(value.clone()).ok()?;
    assert_eq!(&serde_json::to_value(&typed).unwrap(), value, "{label}");
    Some(typed)
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
