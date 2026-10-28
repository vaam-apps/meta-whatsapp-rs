//! The local checks of [`FlowJson::validate`](super::FlowJson::validate).
//!
//! Each rule is one the docs state as a hard limit: the component pages'
//! "Limits and restrictions" tables (`flows/guides/components`,
//! `flows/guides/media_upload`) and the static validation errors of
//! `flows/reference/error-codes`. Limits the docs describe as soft
//! (`NavigationList` content that is truncated or not rendered past its
//! limit) are not checked. Only static values are checked: a property bound
//! to data (`${data.x}`) is Meta's to check when the data arrives.
//!
//! Lengths are counted in Unicode scalar values (`char`s). Where a count
//! could be read two ways, the reading that refuses less is used, so a
//! local check never refuses what Meta would accept: components per screen
//! are counted at the top level of the layout, the per-screen maxima along
//! the components one render can show (the larger branch of an `If` or
//! `Switch`, not their sum), and a list text's limit per line.

use std::collections::BTreeSet;

use meta_whatsapp_core::error::ValidationError;

use super::components::{
    CalendarValue, Component, DataSourceItem, Footer, If, MediaSize, NavigationList, Switch,
};
use super::dynamic::{Dynamic, TextContent, is_expression};
use super::{Action, FlowJson, Screen};

/// Most screens in one Flow (`MAX_SCREENS_NUMBER`).
pub const MAX_SCREENS: usize = 100;
/// Most components in one screen (`flows/guides/components`), counted at
/// the top level of its layout.
pub const MAX_COMPONENTS_PER_SCREEN: usize = 50;
/// Most screens one screen may lead to in the routing model.
pub const MAX_BRANCHES_PER_SCREEN: usize = 10;
/// The screen id Meta reserves (`flows/guides/flowjson` spells it
/// `SUCCESS`, the error codes `success`; both are refused, in any case).
pub const RESERVED_SCREEN_ID: &str = "SUCCESS";

/// Per-screen maxima (`MAX_*_PER_SCREEN` and the component pages).
const MAX_FOOTERS: usize = 1;
const MAX_EMBEDDED_LINKS: usize = 2;
const MAX_IMAGES: usize = 3;
const MAX_OPT_INS: usize = 5;
const MAX_NAVIGATION_LISTS: usize = 2;
const MAX_IMAGE_CAROUSELS: usize = 2;
const MAX_IMAGE_CAROUSELS_PER_FLOW: usize = 3;
/// `If` components nest at most this deep.
const MAX_IF_DEPTH: usize = 3;
/// `max-file-size-kb`'s range.
const MAX_FILE_SIZE_KB: u32 = 25600;
/// `max-uploaded-…`'s upper bound (and `min-uploaded-…`'s).
const MAX_UPLOADS: u32 = 30;

type Check = Result<(), ValidationError>;

/// Picks the components one per-screen maximum counts.
type Matcher = fn(&Component) -> bool;

fn err(field: impl Into<String>, reason: impl Into<String>) -> ValidationError {
    ValidationError::new(field, reason)
}

/// What every check needs to know about the document.
struct Ctx<'a> {
    /// `version` as `(major, minor)`.
    version: (u32, u32),
    has_endpoint: bool,
    screen_ids: BTreeSet<&'a str>,
}

impl Ctx<'_> {
    fn at_least(&self, major: u32, minor: u32) -> bool {
        self.version >= (major, minor)
    }
}

/// Where a component sits.
#[derive(Clone, Copy)]
struct Place<'a> {
    screen_id: &'a str,
    terminal: bool,
    if_depth: usize,
}

fn parse_version(v: &str) -> Option<(u32, u32)> {
    let (major, minor) = v.split_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(major) || !digits(minor) {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

pub(super) fn flow(f: &FlowJson) -> Check {
    let Some(version) = parse_version(&f.version) else {
        return Err(err(
            "version",
            format!("must be {{major}}.{{minor}}, got {:?}", f.version),
        ));
    };
    if f.data_channel_uri.is_some() && version >= (3, 0) {
        return Err(err(
            "data_channel_uri",
            "is not supported from Flow JSON 3.0; set the Flow's endpoint_uri instead",
        ));
    }
    if f.screens.is_empty() {
        return Err(err("screens", "at least one screen is required"));
    }
    if f.screens.len() > MAX_SCREENS {
        return Err(err(
            "screens",
            format!("has {} screens; at most {MAX_SCREENS}", f.screens.len()),
        ));
    }
    let mut screen_ids = BTreeSet::new();
    for (i, s) in f.screens.iter().enumerate() {
        screen_id(&format!("screens[{i}].id"), &s.id)?;
        if !screen_ids.insert(s.id.as_str()) {
            return Err(err(
                format!("screens[{i}].id"),
                format!("duplicate screen id {:?}", s.id),
            ));
        }
    }
    let terminals: Vec<&Screen> = f
        .screens
        .iter()
        .filter(|s| s.terminal == Some(true))
        .collect();
    if terminals.is_empty() {
        return Err(err("screens", "at least one screen must be terminal"));
    }
    if terminals.iter().all(|s| s.success == Some(false)) {
        return Err(err(
            "screens",
            "at least one terminal screen must have success true (the default)",
        ));
    }
    let ctx = Ctx {
        version,
        has_endpoint: f.data_api_version.is_some(),
        screen_ids,
    };
    routing_model(f, &ctx)?;
    let mut carousels = 0;
    for (i, s) in f.screens.iter().enumerate() {
        screen(&format!("screens[{i}]"), s, &ctx)?;
        carousels += count(&s.layout.children, &|c| {
            matches!(c, Component::ImageCarousel(_))
        });
    }
    if carousels > MAX_IMAGE_CAROUSELS_PER_FLOW {
        return Err(err(
            "screens",
            format!(
                "has {carousels} ImageCarousel components; at most {MAX_IMAGE_CAROUSELS_PER_FLOW} per Flow"
            ),
        ));
    }
    Ok(())
}

fn screen_id(path: &str, id: &str) -> Check {
    if id.is_empty() {
        return Err(err(path, "must not be empty"));
    }
    if !id.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_') {
        return Err(err(path, "must contain only letters and underscores"));
    }
    if id.eq_ignore_ascii_case(RESERVED_SCREEN_ID) {
        return Err(err(path, "SUCCESS is reserved"));
    }
    Ok(())
}

fn routing_model(f: &FlowJson, ctx: &Ctx<'_>) -> Check {
    let Some(model) = &f.routing_model else {
        if ctx.has_endpoint {
            return Err(err(
                "routing_model",
                "is required with data_api_version (a Flow with a data endpoint)",
            ));
        }
        return Ok(());
    };
    if ctx.has_endpoint
        && let Some(missing) = f
            .screens
            .iter()
            .find(|s| !model.contains_key(&s.id) && !model.values().any(|to| to.contains(&s.id)))
    {
        return Err(err(
            "routing_model",
            format!("is missing screen {:?}", missing.id),
        ));
    }
    for (from, targets) in model {
        let path = format!("routing_model.{from}");
        if !ctx.screen_ids.contains(from.as_str()) {
            return Err(err(path, "is not a screen of this Flow"));
        }
        if targets.len() > MAX_BRANCHES_PER_SCREEN {
            return Err(err(
                path,
                format!(
                    "has {} routes; at most {MAX_BRANCHES_PER_SCREEN}",
                    targets.len()
                ),
            ));
        }
        for (j, to) in targets.iter().enumerate() {
            let path = format!("routing_model.{from}[{j}]");
            if !ctx.screen_ids.contains(to.as_str()) {
                return Err(err(path, format!("{to:?} is not a screen of this Flow")));
            }
            if to == from {
                return Err(err(path, "a screen cannot route to itself"));
            }
            if model.get(to).is_some_and(|back| back.contains(from)) {
                return Err(err(
                    path,
                    format!("{to} also routes back to {from}; give forward routes only"),
                ));
            }
        }
    }
    Ok(())
}

fn screen(path: &str, s: &Screen, ctx: &Ctx<'_>) -> Check {
    let terminal = s.terminal == Some(true);
    if s.success.is_some() && !terminal {
        return Err(err(
            format!("{path}.success"),
            "can only be set on a terminal screen",
        ));
    }
    if s.refresh_on_back == Some(true) && !ctx.has_endpoint {
        return Err(err(
            format!("{path}.refresh_on_back"),
            "can be true only with data_api_version (a Flow with a data endpoint)",
        ));
    }
    let children = &s.layout.children;
    let cpath = format!("{path}.layout.children");
    if children.len() > MAX_COMPONENTS_PER_SCREEN {
        return Err(err(
            cpath,
            format!(
                "has {} components; at most {MAX_COMPONENTS_PER_SCREEN} per screen",
                children.len()
            ),
        ));
    }
    let limits: [(&str, usize, Matcher); 8] = [
        ("Footer", MAX_FOOTERS, |c| matches!(c, Component::Footer(_))),
        ("EmbeddedLink", MAX_EMBEDDED_LINKS, |c| {
            matches!(c, Component::EmbeddedLink(_))
        }),
        ("Image", MAX_IMAGES, |c| matches!(c, Component::Image(_))),
        ("OptIn", MAX_OPT_INS, |c| matches!(c, Component::OptIn(_))),
        ("NavigationList", MAX_NAVIGATION_LISTS, |c| {
            matches!(c, Component::NavigationList(_))
        }),
        ("ImageCarousel", MAX_IMAGE_CAROUSELS, |c| {
            matches!(c, Component::ImageCarousel(_))
        }),
        ("PhotoPicker", 1, |c| matches!(c, Component::PhotoPicker(_))),
        ("PhotoPicker or DocumentPicker", 1, |c| {
            matches!(c, Component::PhotoPicker(_) | Component::DocumentPicker(_))
        }),
    ];
    for (name, max, pred) in limits {
        let n = count(children, &pred);
        if n > max {
            return Err(err(
                cpath,
                format!("has {n} {name} components; at most {max} per screen"),
            ));
        }
    }
    if children
        .iter()
        .any(|c| matches!(c, Component::NavigationList(_)))
    {
        if terminal {
            return Err(err(cpath, "NavigationList cannot be on a terminal screen"));
        }
        if let Some(i) = children
            .iter()
            .position(|c| !matches!(c, Component::NavigationList(_)))
        {
            return Err(err(
                format!("{cpath}[{i}]"),
                "NavigationList cannot share its screen with other components",
            ));
        }
    }
    if terminal && count(children, &|c| matches!(c, Component::Footer(_))) == 0 {
        return Err(err(cpath, "a terminal screen needs a Footer"));
    }
    let place = Place {
        screen_id: &s.id,
        terminal,
        if_depth: 0,
    };
    for (i, c) in children.iter().enumerate() {
        component(&format!("{cpath}[{i}]"), c, place, ctx)?;
    }
    Ok(())
}

/// How many components `pred` matches along the components one render can
/// show: an `If` or `Switch` adds its larger branch.
fn count(children: &[Component], pred: &dyn Fn(&Component) -> bool) -> usize {
    children
        .iter()
        .map(|c| {
            if pred(c) {
                return 1;
            }
            match c {
                Component::If(i) => {
                    count(&i.then, pred).max(count(i.else_.as_deref().unwrap_or_default(), pred))
                }
                Component::Switch(s) => s
                    .cases
                    .values()
                    .map(|case| count(case, pred))
                    .max()
                    .unwrap_or(0),
                Component::Form(f) => count(&f.children, pred),
                _ => 0,
            }
        })
        .sum()
}

fn has_footer(children: &[Component]) -> bool {
    count(children, &|c| matches!(c, Component::Footer(_))) > 0
}

/// A static string is at most `max` characters (and, with `not_blank`, not
/// blank). A reference or expression is not checked.
fn text(path: &str, value: &str, max: usize, not_blank: bool) -> Check {
    if is_expression(value) {
        return Ok(());
    }
    if not_blank && value.trim().is_empty() {
        return Err(err(path, "must not be empty or blank"));
    }
    let n = value.chars().count();
    if n > max {
        return Err(err(path, format!("is {n} characters; at most {max}")));
    }
    Ok(())
}

fn opt_text(path: &str, value: Option<&String>, max: usize) -> Check {
    value.map_or(Ok(()), |v| text(path, v, max, false))
}

fn text_content(path: &str, value: &TextContent, max: usize) -> Check {
    match value {
        TextContent::Text(t) => text(path, t, max, true),
        TextContent::Lines(lines) => {
            if lines.iter().all(|l| l.trim().is_empty()) {
                return Err(err(path, "must not be empty or blank"));
            }
            for (i, line) in lines.iter().enumerate() {
                text(&format!("{path}[{i}]"), line, max, false)?;
            }
            Ok(())
        }
    }
}

fn calendar_text(path: &str, value: Option<&CalendarValue<String>>, max: usize) -> Check {
    match value {
        None => Ok(()),
        Some(CalendarValue::Single(v)) => text(path, v, max, false),
        Some(CalendarValue::Range(r)) => {
            text(&format!("{path}.start-date"), &r.start_date, max, false)?;
            text(&format!("{path}.end-date"), &r.end_date, max, false)
        }
    }
}

fn min_le_max(
    path: &str,
    (min_name, min): (&str, Option<u32>),
    (max_name, max): (&str, Option<u32>),
) -> Check {
    if let (Some(lo), Some(hi)) = (min, max)
        && lo > hi
    {
        return Err(err(
            format!("{path}.{min_name}"),
            format!("{lo} is greater than {max_name} {hi}"),
        ));
    }
    Ok(())
}

fn static_u32(v: Option<&Dynamic<u32>>) -> Option<u32> {
    v.and_then(Dynamic::as_static).copied()
}

fn range(path: &str, value: Option<u32>, lo: u32, hi: u32) -> Check {
    match value {
        Some(v) if v < lo || v > hi => Err(err(path, format!("{v} is outside {lo}..={hi}"))),
        _ => Ok(()),
    }
}

/// The item limits of `CheckboxGroup`, `RadioButtonsGroup` and `Dropdown`
/// (title 30, description 300, metadata 20). `ChipsSelector`'s page gives
/// none for its options.
const ITEM_LIMITS: Option<(usize, usize, usize)> = Some((30, 300, 20));

/// The options of a selection component: `min..=max` of them when static,
/// each within `item_limits` (title, description, metadata) when given.
fn data_source(
    path: &str,
    source: &Dynamic<Vec<DataSourceItem>>,
    (min, max): (usize, usize),
    item_limits: Option<(usize, usize, usize)>,
    place: Place<'_>,
    ctx: &Ctx<'_>,
) -> Check {
    let Dynamic::Static(items) = source else {
        return Ok(());
    };
    if items.len() < min || items.len() > max {
        return Err(err(
            path,
            format!("has {} options; {min} to {max} allowed", items.len()),
        ));
    }
    for (i, item) in items.iter().enumerate() {
        let p = format!("{path}[{i}]");
        if let Some((title, description, metadata)) = item_limits {
            text(&format!("{p}.title"), &item.title, title, false)?;
            opt_text(
                &format!("{p}.description"),
                item.description.as_ref(),
                description,
            )?;
            opt_text(&format!("{p}.metadata"), item.metadata.as_ref(), metadata)?;
        }
        opt_action(
            &format!("{p}.on-select-action"),
            item.on_select_action.as_ref(),
            false,
            place,
            ctx,
        )?;
        opt_action(
            &format!("{p}.on-unselect-action"),
            item.on_unselect_action.as_ref(),
            false,
            place,
            ctx,
        )?;
    }
    Ok(())
}

fn opt_action(
    path: &str,
    action: Option<&Action>,
    on_click: bool,
    place: Place<'_>,
    ctx: &Ctx<'_>,
) -> Check {
    action.map_or(Ok(()), |a| self::action(path, a, on_click, place, ctx))
}

fn action(path: &str, a: &Action, on_click: bool, place: Place<'_>, ctx: &Ctx<'_>) -> Check {
    match a {
        Action::Navigate(n) => {
            let target = n.next.name.as_str();
            if !ctx.screen_ids.contains(target) {
                return Err(err(
                    format!("{path}.next.name"),
                    format!("{target:?} is not a screen of this Flow"),
                ));
            }
            if target == place.screen_id {
                return Err(err(
                    format!("{path}.next.name"),
                    "a screen cannot navigate to itself",
                ));
            }
        }
        Action::Complete(_) if on_click && !place.terminal => {
            return Err(err(path, "complete can only be used on a terminal screen"));
        }
        Action::DataExchange(_) if !ctx.has_endpoint => {
            return Err(err(
                path,
                "data_exchange needs data_api_version (a Flow with a data endpoint)",
            ));
        }
        Action::UpdateData(_) | Action::OpenUrl(_) if !ctx.at_least(6, 0) => {
            return Err(err(path, "needs Flow JSON 6.0 or later"));
        }
        _ => {}
    }
    Ok(())
}

fn branch(path: &str, children: &[Component], place: Place<'_>, ctx: &Ctx<'_>) -> Check {
    for (i, c) in children.iter().enumerate() {
        component(&format!("{path}[{i}]"), c, place, ctx)?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // one arm per component, each a few checks
fn component(path: &str, c: &Component, place: Place<'_>, ctx: &Ctx<'_>) -> Check {
    let p = |field: &str| format!("{path}.{field}");
    match c {
        Component::TextHeading(t) => text(&p("text"), &t.text, 80, true),
        Component::TextSubheading(t) => text(&p("text"), &t.text, 80, true),
        Component::TextBody(t) => text_content(&p("text"), &t.text, 4096),
        Component::TextCaption(t) => text_content(&p("text"), &t.text, 409),
        Component::TextInput(t) => {
            text(&p("label"), &t.label, 20, false)?;
            opt_text(&p("helper-text"), t.helper_text.as_ref(), 80)?;
            opt_text(&p("error-message"), t.error_message.as_ref(), 30)?;
            if t.pattern.is_some() && t.helper_text.is_none() {
                return Err(err(p("helper-text"), "is required with pattern"));
            }
            min_le_max(
                path,
                ("min-chars", static_u32(t.min_chars.as_ref())),
                ("max-chars", static_u32(t.max_chars.as_ref())),
            )
        }
        Component::TextArea(t) => {
            text(&p("label"), &t.label, 20, false)?;
            opt_text(&p("helper-text"), t.helper_text.as_ref(), 80)
        }
        Component::CheckboxGroup(g) => {
            opt_text(&p("label"), g.label.as_ref(), 30)?;
            data_source(
                &p("data-source"),
                &g.data_source,
                (1, 20),
                ITEM_LIMITS,
                place,
                ctx,
            )?;
            opt_action(
                &p("on-select-action"),
                g.on_select_action.as_ref(),
                false,
                place,
                ctx,
            )?;
            min_le_max(
                path,
                (
                    "min-selected-items",
                    static_u32(g.min_selected_items.as_ref()),
                ),
                (
                    "max-selected-items",
                    static_u32(g.max_selected_items.as_ref()),
                ),
            )
        }
        Component::RadioButtonsGroup(g) => {
            opt_text(&p("label"), g.label.as_ref(), 30)?;
            data_source(
                &p("data-source"),
                &g.data_source,
                (1, 20),
                ITEM_LIMITS,
                place,
                ctx,
            )?;
            opt_action(
                &p("on-select-action"),
                g.on_select_action.as_ref(),
                false,
                place,
                ctx,
            )
        }
        Component::Dropdown(d) => {
            text(&p("label"), &d.label, 20, false)?;
            let with_images = d
                .data_source
                .as_static()
                .is_some_and(|items| items.iter().any(|i| i.image.is_some()));
            let max = if with_images { 100 } else { 200 };
            data_source(
                &p("data-source"),
                &d.data_source,
                (1, max),
                ITEM_LIMITS,
                place,
                ctx,
            )?;
            opt_action(
                &p("on-select-action"),
                d.on_select_action.as_ref(),
                false,
                place,
                ctx,
            )
        }
        Component::ChipsSelector(g) => {
            text(&p("label"), &g.label, 80, false)?;
            opt_text(&p("description"), g.description.as_ref(), 300)?;
            data_source(&p("data-source"), &g.data_source, (2, 20), None, place, ctx)?;
            opt_action(
                &p("on-select-action"),
                g.on_select_action.as_ref(),
                false,
                place,
                ctx,
            )?;
            opt_action(
                &p("on-unselect-action"),
                g.on_unselect_action.as_ref(),
                false,
                place,
                ctx,
            )?;
            min_le_max(
                path,
                (
                    "min-selected-items",
                    static_u32(g.min_selected_items.as_ref()),
                ),
                (
                    "max-selected-items",
                    static_u32(g.max_selected_items.as_ref()),
                ),
            )
        }
        Component::Footer(f) => footer(path, f, place, ctx),
        Component::OptIn(o) => {
            text(&p("label"), &o.label, 120, false)?;
            opt_action(
                &p("on-click-action"),
                o.on_click_action.as_ref(),
                true,
                place,
                ctx,
            )
        }
        Component::EmbeddedLink(l) => {
            text(&p("text"), &l.text, 25, true)?;
            action(&p("on-click-action"), &l.on_click_action, true, place, ctx)
        }
        Component::DatePicker(d) => {
            text(&p("label"), &d.label, 40, false)?;
            opt_text(&p("helper-text"), d.helper_text.as_ref(), 80)?;
            opt_text(&p("error-message"), d.error_message.as_ref(), 80)?;
            opt_action(
                &p("on-select-action"),
                d.on_select_action.as_ref(),
                false,
                place,
                ctx,
            )
        }
        Component::CalendarPicker(d) => {
            opt_text(&p("title"), d.title.as_ref(), 80)?;
            opt_text(&p("description"), d.description.as_ref(), 300)?;
            calendar_text(&p("label"), Some(&d.label), 40)?;
            calendar_text(&p("helper-text"), d.helper_text.as_ref(), 80)?;
            calendar_text(&p("error-message"), d.error_message.as_ref(), 80)?;
            min_le_max(
                path,
                ("min-days", static_u32(d.min_days.as_ref())),
                ("max-days", static_u32(d.max_days.as_ref())),
            )?;
            opt_action(
                &p("on-select-action"),
                d.on_select_action.as_ref(),
                false,
                place,
                ctx,
            )
        }
        Component::ImageCarousel(c) => match &c.images {
            Dynamic::Static(images) if images.is_empty() || images.len() > 3 => Err(err(
                p("images"),
                format!("has {} images; 1 to 3 allowed", images.len()),
            )),
            _ => Ok(()),
        },
        Component::If(i) => if_component(path, i, place, ctx),
        Component::Switch(s) => switch(path, s, place, ctx),
        Component::NavigationList(n) => navigation_list(path, n, place, ctx),
        Component::PhotoPicker(m) => {
            text(&p("label"), &m.label, 80, false)?;
            opt_text(&p("description"), m.description.as_ref(), 300)?;
            media(
                path,
                m.max_file_size_kb,
                ("min-uploaded-photos", m.min_uploaded_photos),
                ("max-uploaded-photos", m.max_uploaded_photos),
            )
        }
        Component::DocumentPicker(m) => {
            text(&p("label"), &m.label, 80, false)?;
            opt_text(&p("description"), m.description.as_ref(), 300)?;
            media(
                path,
                m.max_file_size_kb,
                ("min-uploaded-documents", m.min_uploaded_documents),
                ("max-uploaded-documents", m.max_uploaded_documents),
            )
        }
        Component::Form(f) => branch(&p("children"), &f.children, place, ctx),
        Component::RichText(_) | Component::Image(_) | Component::Unknown { .. } => Ok(()),
    }
}

fn footer(path: &str, f: &Footer, place: Place<'_>, ctx: &Ctx<'_>) -> Check {
    let p = |field: &str| format!("{path}.{field}");
    text(&p("label"), &f.label, 35, false)?;
    opt_text(&p("left-caption"), f.left_caption.as_ref(), 15)?;
    opt_text(&p("center-caption"), f.center_caption.as_ref(), 15)?;
    opt_text(&p("right-caption"), f.right_caption.as_ref(), 15)?;
    let (left, center, right) = (
        f.left_caption.is_some(),
        f.center_caption.is_some(),
        f.right_caption.is_some(),
    );
    if center && (left || right) {
        return Err(err(
            p("center-caption"),
            "cannot be set with left-caption or right-caption",
        ));
    }
    if left != right {
        let (missing, present) = if left {
            ("right-caption", "left-caption")
        } else {
            ("left-caption", "right-caption")
        };
        return Err(err(p(missing), format!("is required with {present}")));
    }
    action(&p("on-click-action"), &f.on_click_action, true, place, ctx)
}

fn if_component(path: &str, i: &If, place: Place<'_>, ctx: &Ctx<'_>) -> Check {
    let p = |field: &str| format!("{path}.{field}");
    let depth = place.if_depth + 1;
    if depth > MAX_IF_DEPTH {
        return Err(err(
            path,
            format!("If components nest at most {MAX_IF_DEPTH} deep"),
        ));
    }
    if i.condition.trim().is_empty() {
        return Err(err(p("condition"), "must not be empty"));
    }
    if i.then.is_empty() {
        return Err(err(p("then"), "must contain at least one component"));
    }
    let else_ = i.else_.as_deref().unwrap_or_default();
    let (then_footer, else_footer) = (has_footer(&i.then), has_footer(else_));
    if (then_footer || else_footer) && depth > 1 {
        return Err(err(path, "a Footer can only be in a first-level If"));
    }
    if then_footer && i.else_.is_none() {
        return Err(err(
            p("else"),
            "is required (with a Footer) when then has a Footer",
        ));
    }
    if then_footer != else_footer {
        let missing = if then_footer { "else" } else { "then" };
        return Err(err(p(missing), "needs a Footer: the other branch has one"));
    }
    let inner = Place {
        if_depth: depth,
        ..place
    };
    branch(&p("then"), &i.then, inner, ctx)?;
    branch(&p("else"), else_, inner, ctx)
}

fn switch(path: &str, s: &Switch, place: Place<'_>, ctx: &Ctx<'_>) -> Check {
    if s.cases.is_empty() {
        return Err(err(format!("{path}.cases"), "must not be empty"));
    }
    let with_footer = s.cases.values().filter(|c| has_footer(c)).count();
    if with_footer > 0
        && with_footer < s.cases.len()
        && let Some((key, _)) = s.cases.iter().find(|(_, c)| !has_footer(c))
    {
        return Err(err(
            format!("{path}.cases.{key}"),
            "needs a Footer: another case has one",
        ));
    }
    for (key, case) in &s.cases {
        branch(&format!("{path}.cases.{key}"), case, place, ctx)?;
    }
    Ok(())
}

fn navigation_list(path: &str, n: &NavigationList, place: Place<'_>, ctx: &Ctx<'_>) -> Check {
    let p = |field: &str| format!("{path}.{field}");
    opt_action(
        &p("on-click-action"),
        n.on_click_action.as_ref(),
        true,
        place,
        ctx,
    )?;
    let Dynamic::Static(items) = &n.list_items else {
        return Ok(());
    };
    let large = matches!(n.media_size, Some(Dynamic::Static(MediaSize::Large)));
    let mut badges = 0;
    for (i, item) in items.iter().enumerate() {
        let ip = format!("{path}.list-items[{i}]");
        match (&n.on_click_action, &item.on_click_action) {
            (Some(_), Some(_)) => {
                return Err(err(
                    format!("{ip}.on-click-action"),
                    "cannot be set both on the list and on its items",
                ));
            }
            (None, None) => {
                return Err(err(
                    format!("{ip}.on-click-action"),
                    "is required on each item when the list has none",
                ));
            }
            (None, Some(a)) => action(&format!("{ip}.on-click-action"), a, true, place, ctx)?,
            (Some(_), None) => {}
        }
        if item.end.is_some() && large {
            return Err(err(
                format!("{ip}.end"),
                "cannot be used with media-size large",
            ));
        }
        badges += usize::from(item.badge.is_some());
        if badges > 1 {
            return Err(err(
                format!("{ip}.badge"),
                "only one item per list may have a badge",
            ));
        }
    }
    Ok(())
}

fn media(
    path: &str,
    max_file_size_kb: Option<u32>,
    (min_name, min): (&str, Option<u32>),
    (max_name, max): (&str, Option<u32>),
) -> Check {
    range(
        &format!("{path}.max-file-size-kb"),
        max_file_size_kb,
        1,
        MAX_FILE_SIZE_KB,
    )?;
    range(&format!("{path}.{min_name}"), min, 0, MAX_UPLOADS)?;
    range(&format!("{path}.{max_name}"), max, 1, MAX_UPLOADS)?;
    // The documented defaults (0 and 30) when unset.
    min_le_max(
        path,
        (min_name, Some(min.unwrap_or(0))),
        (max_name, Some(max.unwrap_or(MAX_UPLOADS))),
    )
}
