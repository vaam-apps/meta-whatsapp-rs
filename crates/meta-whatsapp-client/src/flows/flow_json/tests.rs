//! The builder's local checks, one test per documented limit (each failing
//! case names the path Meta's validator would point at), the dynamic
//! values, and the typed create and upload calls. The round trip of Meta's
//! examples is `tests/flow_json.rs`.

use http::Method;
use meta_whatsapp_core::Error;
use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::flows::{CreateFlow, FlowCategory};
use crate::{Client, RetryPolicy};

/// One terminal screen with a `Footer` that completes: valid.
fn base() -> FlowJson {
    FlowJson::builder("7.3")
        .screen(end_screen("END"))
        .build_unchecked()
}

fn end_screen(id: &str) -> Screen {
    Screen::new(id)
        .terminal(true)
        .child(Footer::new("Done", Action::complete()))
}

/// `base()` with `children` before its `Footer`.
fn with(children: impl IntoIterator<Item = Component>) -> FlowJson {
    let mut f = base();
    let footer = f.screens[0].layout.children.pop().unwrap();
    f.screens[0].layout.children.extend(children);
    f.screens[0].layout.children.push(footer);
    f
}

/// The field and reason of the validation error `f` gets.
#[track_caller]
fn refused(f: &FlowJson) -> (String, String) {
    let e = f.validate().expect_err("accepted");
    (e.field, e.reason)
}

#[track_caller]
fn refused_at(f: &FlowJson, field: &str) {
    let (got, reason) = refused(f);
    assert_eq!(got, field, "{reason}");
}

const C0: &str = "screens[0].layout.children[0]";

fn chars(n: usize) -> String {
    "x".repeat(n)
}

fn options(n: usize) -> Vec<DataSourceItem> {
    (0..n)
        .map(|i| DataSourceItem::new(i.to_string(), format!("Option {i}")))
        .collect()
}

#[test]
fn the_base_flow_is_valid() {
    base().validate().unwrap();
}

#[test]
fn version_is_major_dot_minor() {
    for bad in ["", "7", "v7.3", "7.3.1", "7.x"] {
        let mut f = base();
        f.version = bad.into();
        refused_at(&f, "version");
    }
}

#[test]
fn data_channel_uri_is_refused_from_version_3() {
    let mut f = base();
    f.version = "2.1".into();
    f.data_channel_uri = Some("https://example.com".into());
    f.validate().unwrap();
    f.version = "3.0".into();
    refused_at(&f, "data_channel_uri");
}

#[test]
fn screens_are_one_to_a_hundred() {
    let mut f = base();
    f.screens.clear();
    refused_at(&f, "screens");
    let names: Vec<String> = (0..MAX_SCREENS)
        .map(|i| {
            // Letters only: screen ids take no digits.
            let a = char::from(b'A' + u8::try_from(i / 26).unwrap());
            let b = char::from(b'A' + u8::try_from(i % 26).unwrap());
            format!("S_{a}{b}")
        })
        .collect();
    let mut f = FlowJson::new("7.3");
    f.screens = names.iter().map(|n| end_screen(n)).collect();
    f.validate().unwrap();
    f.screens.push(end_screen("ONE_TOO_MANY"));
    refused_at(&f, "screens");
}

#[test]
fn screen_ids_are_letters_and_underscores_unique_and_not_success() {
    for bad in ["", "SCREEN_1", "my-screen", "SUCCESS", "success", "Success"] {
        let mut f = base();
        f.screens[0].id = bad.into();
        refused_at(&f, "screens[0].id");
    }
    let mut f = base();
    f.screens.push(end_screen("END"));
    refused_at(&f, "screens[1].id");
}

#[test]
fn a_flow_needs_a_terminal_screen_that_can_succeed() {
    let mut f = base();
    f.screens[0].terminal = None;
    refused_at(&f, "screens");
    let mut f = base();
    f.screens[0].success = Some(false);
    refused_at(&f, "screens");
    f.screens.push(end_screen("OTHER"));
    f.validate().unwrap();
}

#[test]
fn success_is_for_terminal_screens_only() {
    let f = FlowJson::builder("7.3")
        .screen(
            Screen::new("START")
                .success(true)
                .child(Footer::new("Next", Action::navigate("END"))),
        )
        .screen(end_screen("END"))
        .build_unchecked();
    refused_at(&f, "screens[0].success");
}

#[test]
fn refresh_on_back_needs_a_data_endpoint() {
    let mut f = base();
    f.screens[0].refresh_on_back = Some(true);
    refused_at(&f, "screens[0].refresh_on_back");
    f.data_api_version = Some("3.0".into());
    f.routing_model = Some([("END".to_owned(), vec![])].into());
    f.validate().unwrap();
}

/// START → MIDDLE → END, with a data endpoint.
fn endpoint_flow() -> FlowJsonBuilder {
    FlowJson::builder("7.3")
        .data_api_version("3.0")
        .screen(Screen::new("START").child(Footer::new("Next", Action::data_exchange())))
        .screen(Screen::new("MIDDLE").child(Footer::new("Next", Action::data_exchange())))
        .screen(end_screen("END"))
}

#[test]
fn an_endpoint_flow_needs_a_routing_model_covering_every_screen() {
    refused_at(&endpoint_flow().build_unchecked(), "routing_model");
    endpoint_flow()
        .route("START", ["MIDDLE"])
        .route("MIDDLE", ["END"])
        .build()
        .unwrap();
    let missing = endpoint_flow().route("START", ["END"]).build_unchecked();
    let (field, reason) = refused(&missing);
    assert_eq!(field, "routing_model");
    assert!(reason.contains("MIDDLE"), "{reason}");
}

#[test]
fn routes_name_screens_go_forward_and_branch_at_most_ten_ways() {
    let f = endpoint_flow()
        .route("START", ["MIDDLE"])
        .route("MIDDLE", ["END"])
        .route("ELSEWHERE", ["END"])
        .build_unchecked();
    refused_at(&f, "routing_model.ELSEWHERE");
    let f = endpoint_flow()
        .route("START", ["MIDDLE", "NOWHERE"])
        .route("MIDDLE", ["END"])
        .build_unchecked();
    refused_at(&f, "routing_model.START[1]");
    let f = endpoint_flow()
        .route("START", ["START"])
        .route("MIDDLE", ["END"])
        .build_unchecked();
    refused_at(&f, "routing_model.START[0]");
    let f = endpoint_flow()
        .route("START", ["MIDDLE"])
        .route("MIDDLE", ["END", "START"])
        .build_unchecked();
    refused_at(&f, "routing_model.MIDDLE[1]");

    let names: Vec<String> = (0..=MAX_BRANCHES_PER_SCREEN)
        .map(|i| format!("END_{}", char::from(b'A' + u8::try_from(i).unwrap())))
        .collect();
    let mut b = FlowJson::builder("7.3")
        .data_api_version("3.0")
        .screen(Screen::new("START").child(Footer::new("Next", Action::data_exchange())));
    for n in &names {
        b = b.screen(end_screen(n));
    }
    let ten = b
        .clone()
        .route("START", &names[..MAX_BRANCHES_PER_SCREEN])
        .route(names[MAX_BRANCHES_PER_SCREEN].clone(), Vec::<String>::new());
    ten.build().unwrap();
    refused_at(
        &b.route("START", &names).build_unchecked(),
        "routing_model.START",
    );
}

#[test]
fn a_screen_holds_at_most_fifty_components() {
    let f = with((0..MAX_COMPONENTS_PER_SCREEN - 1).map(|_| TextBody::new("x").into()));
    f.validate().unwrap();
    let f = with((0..MAX_COMPONENTS_PER_SCREEN).map(|_| TextBody::new("x").into()));
    refused_at(&f, "screens[0].layout.children");
}

#[test]
fn per_screen_maxima() {
    let link = || EmbeddedLink::new("Terms", Action::open_url("https://example.com")).into();
    let image = || Image::new("iVBORw0KGgo=").into();
    let opt_in = |i: usize| OptIn::new(format!("o{i}"), "I agree").into();
    with([link(), link()]).validate().unwrap();
    refused_at(
        &with([link(), link(), link()]),
        "screens[0].layout.children",
    );
    with([image(), image(), image()]).validate().unwrap();
    refused_at(&with((0..4).map(|_| image())), "screens[0].layout.children");
    with((0..5).map(opt_in)).validate().unwrap();
    refused_at(&with((0..6).map(opt_in)), "screens[0].layout.children");
    let footer = Footer::new("Again", Action::complete()).into();
    refused_at(&with([footer]), "screens[0].layout.children");
}

#[test]
fn a_terminal_screen_needs_a_footer_and_one_in_both_if_branches_counts_once() {
    let mut f = base();
    f.screens[0].layout.children.clear();
    refused_at(&f, "screens[0].layout.children");

    let footer = || Component::from(Footer::new("Done", Action::complete()));
    let mut f = base();
    f.screens[0].layout.children = vec![
        If::new("${data.ok}", vec![footer()])
            .else_(vec![footer()])
            .into(),
    ];
    f.validate().unwrap();
    f.screens[0].layout.children.push(footer());
    refused_at(&f, "screens[0].layout.children");
}

#[test]
fn text_limits_apply_to_static_text_only() {
    with([TextHeading::new(chars(80)).into()])
        .validate()
        .unwrap();
    refused_at(
        &with([TextHeading::new(chars(81)).into()]),
        &format!("{C0}.text"),
    );
    refused_at(
        &with([TextHeading::new("  ").into()]),
        &format!("{C0}.text"),
    );
    refused_at(
        &with([TextSubheading::new(chars(81)).into()]),
        &format!("{C0}.text"),
    );
    refused_at(
        &with([TextBody::new(chars(4097)).into()]),
        &format!("{C0}.text"),
    );
    with([TextBody::new(chars(4096)).into()])
        .validate()
        .unwrap();
    refused_at(
        &with([TextCaption::new(chars(410)).into()]),
        &format!("{C0}.text"),
    );
    refused_at(
        &with([TextCaption::new(["ok", &chars(410)]).markdown(true).into()]),
        &format!("{C0}.text[1]"),
    );
    // A reference is checked when its data arrives, by Meta.
    let long_ref = data_ref(&chars(200));
    with([TextHeading::new(long_ref).into()])
        .validate()
        .unwrap();
    // Characters, not bytes.
    with([TextHeading::new("é".repeat(80)).into()])
        .validate()
        .unwrap();
}

#[test]
fn text_input_limits() {
    let input = || TextInput::new("email", "Email");
    with([input().into()]).validate().unwrap();
    refused_at(
        &with([TextInput::new("e", chars(21)).into()]),
        &format!("{C0}.label"),
    );
    refused_at(
        &with([input().helper_text(chars(81)).into()]),
        &format!("{C0}.helper-text"),
    );
    refused_at(
        &with([input().error_message(chars(31)).into()]),
        &format!("{C0}.error-message"),
    );
    refused_at(
        &with([input().pattern("^a+$").into()]),
        &format!("{C0}.helper-text"),
    );
    with([input().pattern("^a+$").helper_text("Only a").into()])
        .validate()
        .unwrap();
    refused_at(
        &with([input().min_chars(5).max_chars(4).into()]),
        &format!("{C0}.min-chars"),
    );
    with([input().min_chars(5).max_chars(Dynamic::data("max")).into()])
        .validate()
        .unwrap();
    refused_at(
        &with([TextArea::new("a", chars(21)).into()]),
        &format!("{C0}.label"),
    );
}

#[test]
fn selection_limits() {
    let group = |n| CheckboxGroup::new("c", options(n)).label("Pick");
    with([group(20).into()]).validate().unwrap();
    refused_at(&with([group(21).into()]), &format!("{C0}.data-source"));
    refused_at(&with([group(0).into()]), &format!("{C0}.data-source"));
    refused_at(
        &with([group(1).label(chars(31)).into()]),
        &format!("{C0}.label"),
    );
    refused_at(
        &with([group(2).min_selected_items(3).max_selected_items(2).into()]),
        &format!("{C0}.min-selected-items"),
    );
    let long_title = vec![DataSourceItem::new("1", chars(31))];
    refused_at(
        &with([CheckboxGroup::new("c", long_title).into()]),
        &format!("{C0}.data-source[0].title"),
    );
    let long_meta = vec![DataSourceItem::new("1", "t").metadata(chars(21))];
    refused_at(
        &with([RadioButtonsGroup::new("r", long_meta).into()]),
        &format!("{C0}.data-source[0].metadata"),
    );
    refused_at(
        &with([RadioButtonsGroup::new("r", options(21)).into()]),
        &format!("{C0}.data-source"),
    );
    with([CheckboxGroup::new("c", Dynamic::data("opts")).into()])
        .validate()
        .unwrap();

    with([Dropdown::new("d", "Pick", options(200)).into()])
        .validate()
        .unwrap();
    refused_at(
        &with([Dropdown::new("d", "Pick", options(201)).into()]),
        &format!("{C0}.data-source"),
    );
    let mut pictured = options(101);
    pictured[0].image = Some("iVBORw0KGgo=".into());
    refused_at(
        &with([Dropdown::new("d", "Pick", pictured).into()]),
        &format!("{C0}.data-source"),
    );
    refused_at(
        &with([Dropdown::new("d", chars(21), options(1)).into()]),
        &format!("{C0}.label"),
    );

    with([ChipsSelector::new("c", "Pick", options(2)).into()])
        .validate()
        .unwrap();
    refused_at(
        &with([ChipsSelector::new("c", "Pick", options(1)).into()]),
        &format!("{C0}.data-source"),
    );
}

#[test]
fn footer_limits() {
    let footer = |f: Footer| -> FlowJson {
        let mut flow = base();
        flow.screens[0].layout.children = vec![f.into()];
        flow
    };
    footer(Footer::new(chars(35), Action::complete()))
        .validate()
        .unwrap();
    refused_at(
        &footer(Footer::new(chars(36), Action::complete())),
        &format!("{C0}.label"),
    );
    let done = || Footer::new("Done", Action::complete());
    refused_at(
        &footer(done().center_caption(chars(16))),
        &format!("{C0}.center-caption"),
    );
    footer(done().left_caption("a").right_caption("b"))
        .validate()
        .unwrap();
    refused_at(
        &footer(
            done()
                .left_caption("a")
                .right_caption("b")
                .center_caption("c"),
        ),
        &format!("{C0}.center-caption"),
    );
    refused_at(
        &footer(done().left_caption("a")),
        &format!("{C0}.right-caption"),
    );
    refused_at(
        &footer(done().right_caption("b")),
        &format!("{C0}.left-caption"),
    );
}

#[test]
fn opt_in_embedded_link_and_date_limits() {
    refused_at(
        &with([OptIn::new("o", chars(121)).into()]),
        &format!("{C0}.label"),
    );
    let link = |t: String| EmbeddedLink::new(t, Action::open_url("https://example.com")).into();
    with([link(chars(25))]).validate().unwrap();
    refused_at(&with([link(chars(26))]), &format!("{C0}.text"));
    refused_at(&with([link(" ".into())]), &format!("{C0}.text"));
    refused_at(
        &with([DatePicker::new("d", chars(41)).into()]),
        &format!("{C0}.label"),
    );
    refused_at(
        &with([DatePicker::new("d", "When").error_message(chars(81)).into()]),
        &format!("{C0}.error-message"),
    );
    let range =
        CalendarPicker::new("c", DateRange::new("Start", chars(41))).mode(CalendarMode::Range);
    refused_at(&with([range.into()]), &format!("{C0}.label.end-date"));
    refused_at(
        &with([CalendarPicker::new("c", "When").title(chars(81)).into()]),
        &format!("{C0}.title"),
    );
}

#[test]
fn if_and_switch_rules() {
    let text = || Component::from(TextBody::new("x"));
    let footer = || Component::from(Footer::new("Done", Action::complete()));
    let screen_of = |children: Vec<Component>| {
        let mut f = base();
        f.screens[0].layout.children = children;
        f
    };
    refused_at(
        &with([If::new("${data.a}", Vec::<Component>::new()).into()]),
        &format!("{C0}.then"),
    );
    refused_at(
        &with([If::new(" ", vec![text()]).into()]),
        &format!("{C0}.condition"),
    );
    let nest = |depth: usize| {
        let mut c = If::new("${data.a}", vec![text()]);
        for _ in 1..depth {
            c = If::new("${data.a}", vec![c.into()]);
        }
        Component::from(c)
    };
    with([nest(3)]).validate().unwrap();
    refused_at(&with([nest(4)]), &format!("{C0}.then[0].then[0].then[0]"));
    refused_at(
        &screen_of(vec![If::new("${data.a}", vec![footer()]).into()]),
        &format!("{C0}.else"),
    );
    refused_at(
        &screen_of(vec![
            If::new("${data.a}", vec![footer()])
                .else_(vec![text()])
                .into(),
        ]),
        &format!("{C0}.else"),
    );
    let nested_footer = If::new(
        "${data.a}",
        vec![
            If::new("${data.b}", vec![footer()])
                .else_(vec![footer()])
                .into(),
        ],
    )
    .else_(vec![footer()]);
    refused_at(
        &screen_of(vec![nested_footer.into()]),
        &format!("{C0}.then[0]"),
    );

    refused_at(
        &with([Switch::new("${data.k}", std::collections::BTreeMap::new()).into()]),
        &format!("{C0}.cases"),
    );
    let cases = [
        ("a".to_owned(), vec![footer()]),
        ("b".to_owned(), vec![text()]),
    ];
    refused_at(
        &screen_of(vec![Switch::new("${data.k}", cases).into()]),
        &format!("{C0}.cases.b"),
    );
    let cases = [
        ("a".to_owned(), vec![footer()]),
        ("b".to_owned(), vec![footer()]),
    ];
    screen_of(vec![Switch::new("${data.k}", cases).into()])
        .validate()
        .unwrap();
}

#[test]
fn navigation_list_rules() {
    let item = |title: &str| NavigationItem::new(NavigationItemMainContent::new(title));
    let list = |items: Vec<NavigationItem>| {
        NavigationList::new("n", items).on_click_action(Action::navigate("END"))
    };
    let flow_of = |list: NavigationList| {
        FlowJson::builder("7.3")
            .screen(Screen::new("MENU").child(list))
            .screen(end_screen("END"))
            .build_unchecked()
    };
    let c0 = "screens[0].layout.children[0]";
    flow_of(list(vec![item("a"), item("b")]))
        .validate()
        .unwrap();
    let mut both = vec![item("a")];
    both[0].on_click_action = Some(Action::navigate("END"));
    refused_at(
        &flow_of(list(both)),
        &format!("{c0}.list-items[0].on-click-action"),
    );
    refused_at(
        &flow_of(NavigationList::new("n", vec![item("a")])),
        &format!("{c0}.list-items[0].on-click-action"),
    );
    refused_at(
        &flow_of(list(vec![item("a").badge("New"), item("b").badge("Hot")])),
        &format!("{c0}.list-items[1].badge"),
    );
    refused_at(
        &flow_of(
            list(vec![item("a").end(NavigationItemEnd::new().title("$9"))])
                .media_size(MediaSize::Large),
        ),
        &format!("{c0}.list-items[0].end"),
    );
    let mut f = flow_of(list(vec![item("a")]));
    f.screens[0].layout.children.push(TextBody::new("x").into());
    refused_at(&f, "screens[0].layout.children[1]");
    let mut f = flow_of(list(vec![item("a")]));
    f.screens[0].terminal = Some(true);
    let (field, reason) = refused(&f);
    assert_eq!(field, "screens[0].layout.children");
    assert!(reason.contains("NavigationList"), "{reason}");
}

#[test]
fn media_picker_rules() {
    let photo = || PhotoPicker::new("photo", "Add a photo");
    with([photo().into()]).validate().unwrap();
    refused_at(
        &with([photo().max_file_size_kb(0u32).into()]),
        &format!("{C0}.max-file-size-kb"),
    );
    refused_at(
        &with([photo().max_file_size_kb(25601u32).into()]),
        &format!("{C0}.max-file-size-kb"),
    );
    refused_at(
        &with([photo().min_uploaded_photos(31u32).into()]),
        &format!("{C0}.min-uploaded-photos"),
    );
    refused_at(
        &with([photo()
            .min_uploaded_photos(3u32)
            .max_uploaded_photos(2u32)
            .into()]),
        &format!("{C0}.min-uploaded-photos"),
    );
    refused_at(
        &with([photo().into(), photo().into()]),
        "screens[0].layout.children",
    );
    let doc = DocumentPicker::new("doc", "Add a document");
    refused_at(
        &with([photo().into(), doc.clone().into()]),
        "screens[0].layout.children",
    );
    refused_at(
        &with([doc.max_uploaded_documents(0u32).into()]),
        &format!("{C0}.max-uploaded-documents"),
    );
    refused_at(
        &with([PhotoPicker::new("p", chars(81)).into()]),
        &format!("{C0}.label"),
    );
}

#[test]
fn image_carousel_limits() {
    let carousel = |n: usize| {
        ImageCarousel::new(
            (0..n)
                .map(|i| CarouselImage::new("iVBORw0KGgo=", format!("image {i}")))
                .collect::<Vec<_>>(),
        )
    };
    with([carousel(3).into()]).validate().unwrap();
    refused_at(&with([carousel(4).into()]), &format!("{C0}.images"));
    refused_at(&with([carousel(0).into()]), &format!("{C0}.images"));
    refused_at(
        &with([carousel(1).into(), carousel(1).into(), carousel(1).into()]),
        "screens[0].layout.children",
    );
    let two = || end_screen("A").child(carousel(1)).child(carousel(1));
    let f = FlowJson::builder("7.3")
        .screen(two())
        .screen(Screen {
            id: "B".into(),
            ..two()
        })
        .build_unchecked();
    refused_at(&f, "screens");
}

#[test]
fn action_rules() {
    let footer_to = |a: Action| {
        FlowJson::builder("7.3")
            .screen(Screen::new("START").child(Footer::new("Go", a)))
            .screen(end_screen("END"))
            .build_unchecked()
    };
    footer_to(Action::navigate("END")).validate().unwrap();
    let f0 = "screens[0].layout.children[0].on-click-action";
    refused_at(
        &footer_to(Action::navigate("NOWHERE")),
        &format!("{f0}.next.name"),
    );
    refused_at(
        &footer_to(Action::navigate("START")),
        &format!("{f0}.next.name"),
    );
    refused_at(&footer_to(Action::complete()), f0);
    refused_at(&footer_to(Action::data_exchange()), f0);

    let update = vec![DataSourceItem::new("1", "One").on_select_action(Action::update_data())];
    let mut f = with([RadioButtonsGroup::new("r", update).label("Pick").into()]);
    f.validate().unwrap();
    f.version = "5.1".into();
    refused_at(&f, &format!("{C0}.data-source[0].on-select-action"));
    let mut f = with([EmbeddedLink::new("Terms", Action::open_url("https://example.com")).into()]);
    f.version = "5.1".into();
    refused_at(&f, &format!("{C0}.on-click-action"));
}

#[test]
fn dynamic_values_parse_as_what_was_sent() {
    let parse = |v: Value| serde_json::from_value::<Dynamic<bool>>(v).unwrap();
    assert_eq!(parse(json!(true)), Dynamic::Static(true));
    assert_eq!(parse(json!("${data.v}")), Dynamic::data("v"));
    assert_eq!(
        parse(json!("`${form.age} > 18`")),
        Dynamic::expression("`${form.age} > 18`")
    );
    // Not a boolean nor a reference: kept as text for Meta to report.
    assert_eq!(parse(json!("yes")), Dynamic::Expression("yes".into()));
    assert!(serde_json::from_value::<Dynamic<bool>>(json!(3)).is_err());

    let weight = |v: Value| serde_json::from_value::<Dynamic<FontWeight>>(v).unwrap();
    assert_eq!(weight(json!("bold")), Dynamic::Static(FontWeight::Bold));
    assert_eq!(weight(json!("${data.fw}")), Dynamic::data("fw"));
    assert_eq!(
        weight(json!("heavy")),
        Dynamic::Static(FontWeight::Unknown("heavy".into()))
    );
    for v in [json!(true), json!("${data.v}"), json!("yes")] {
        assert_eq!(serde_json::to_value(parse(v.clone())).unwrap(), v);
    }
    assert_eq!(
        serde_json::to_value(Dynamic::<u32>::form("n")).unwrap(),
        json!("${form.n}")
    );
    assert_eq!(screen_data_ref("A", "b"), "${screen.A.data.b}");
    assert_eq!(screen_form_ref("A", "b"), "${screen.A.form.b}");
    assert!(!is_expression("`"));
    assert!(!is_expression("plain"));
}

#[test]
fn a_known_component_with_a_wrong_property_type_is_an_error_not_dropped() {
    let e =
        serde_json::from_value::<Component>(json!({"type": "TextHeading", "text": 3})).unwrap_err();
    assert!(e.to_string().contains("TextHeading"), "{e}");
    let e =
        serde_json::from_value::<Component>(json!({"type": "Footer", "label": "x"})).unwrap_err();
    assert!(e.to_string().contains("on-click-action"), "{e}");
}

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

#[tokio::test]
async fn create_sends_the_typed_flow_json_as_its_string() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"id": "1", "success": true, "validation_errors": []}),
    );
    let flow = base();
    let create = CreateFlow::new("typed", [FlowCategory::Other])
        .flow_json_typed(&flow)
        .unwrap();
    client(&t)
        .flows(meta_whatsapp_core::ids::WabaId::new("102290129340398").unwrap())
        .create(&create)
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/102290129340398/flows");
    let body = req.json().unwrap();
    assert_eq!(body["name"], "typed");
    let sent: Value = serde_json::from_str(body["flow_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        sent,
        json!({"version": "7.3", "screens": [{"id": "END", "terminal": true, "layout": {
            "type": "SingleColumnLayout",
            "children": [{"type": "Footer", "label": "Done",
                "on-click-action": {"name": "complete", "payload": {}}}]}}]})
    );
    assert_eq!(t.remaining(), 0);

    let mut bad = base();
    bad.screens[0].id = "SUCCESS".into();
    let e = CreateFlow::new("typed", [FlowCategory::Other])
        .flow_json_typed(&bad)
        .unwrap_err();
    assert_eq!(e.field, "screens[0].id");
}

#[tokio::test]
async fn upload_sends_the_typed_flow_json_and_refuses_an_invalid_one_locally() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true, "validation_errors": []}));
    let flow = base();
    client(&t)
        .flow("FLOW_ID")
        .upload_flow_json_typed(&flow)
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/FLOW_ID/assets");
    assert!(matches!(req.body, RecordedBody::Multipart(_)));
    let (filename, content_type, data) = req.multipart_field("file").unwrap();
    assert_eq!(
        (filename, content_type),
        (Some("flow.json"), Some("application/json"))
    );
    assert_eq!(&data[..], flow.to_json_string().unwrap().as_bytes());
    assert_eq!(
        &req.multipart_field("asset_type").unwrap().2[..],
        b"FLOW_JSON"
    );
    assert_eq!(t.remaining(), 0);

    let t = ScriptedTransport::new();
    let mut bad = base();
    bad.screens[0].layout.children.clear();
    let err = client(&t)
        .flow("FLOW_ID")
        .upload_flow_json_typed(&bad)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Validation(ref v) if v.field == "screens[0].layout.children"),
        "{err:?}"
    );
    assert!(t.requests().is_empty());
}
