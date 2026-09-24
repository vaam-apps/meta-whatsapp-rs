//! Local checks of the limits Meta's pages state, run before any request.
//!
//! Every limit carries a comment naming the page it comes from (relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`).
//! Nothing here is invented: where the docs are ambiguous or contradict
//! each other the check is left out and the gap is noted instead, because
//! a false local rejection has no workaround while a missed one only costs a
//! Graph error.
//!
//! Lengths are counted in Unicode scalar values (`chars()`); the docs say
//! "characters" without defining them.

use std::collections::BTreeSet;

use wa_core::error::ValidationError;

use super::definition::{
    Button, CarouselCard, FlowAction, FlowButton, HeaderComponent, HeaderFormat, OtpButton,
    SupportedApp, TemplateComponent, TemplateDefinition, TemplateEdit,
};
use super::types::{ParameterFormat, TemplateCategory};

type Check = Result<(), ValidationError>;

fn err(field: impl Into<String>, reason: impl Into<String>) -> ValidationError {
    ValidationError::new(field, reason)
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// `value` must be non-empty and at most `max` characters.
fn text(value: &str, max: usize, field: &str) -> Check {
    if value.trim().is_empty() {
        return Err(err(field, "must not be empty"));
    }
    if chars(value) > max {
        return Err(err(field, format!("must be at most {max} characters")));
    }
    Ok(())
}

/// Template name. `templates/template-management#template-name-validation`:
/// `^[a-z0-9_]+$`, at most 512 characters (also `templates/overview#names`).
pub(crate) fn name(value: &str, field: &str) -> Check {
    if value.is_empty() {
        return Err(err(field, "must not be empty"));
    }
    if chars(value) > 512 {
        return Err(err(field, "must be at most 512 characters"));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(err(
            field,
            "may only contain lowercase letters, digits and underscores",
        ));
    }
    Ok(())
}

/// Authentication `code_expiration_minutes`: "Minimum 1, maximum 90"
/// (`templates/authentication-templates/copy-code-button-authentication-templates`,
/// same on the one-tap, zero-tap and template-preview pages).
pub(crate) fn code_expiration_minutes(value: u32, field: &str) -> Check {
    if !(1..=90).contains(&value) {
        return Err(err(field, "must be between 1 and 90"));
    }
    Ok(())
}

/// `message_send_ttl_seconds` per category (`templates/time-to-live`):
/// authentication 30–900, utility 30–43200 (both also accept `-1` = 30
/// days), marketing 43200–2592000. Unknown category: not checked.
///
/// The page's own example sets `120` on a `MARKETING` template, which its
/// table forbids; the table is enforced.
pub(crate) fn ttl(value: Option<i64>, category: Option<&TemplateCategory>, field: &str) -> Check {
    let (Some(v), Some(category)) = (value, category) else {
        return Ok(());
    };
    let ok = match category {
        TemplateCategory::Authentication => v == -1 || (30..=900).contains(&v),
        TemplateCategory::Utility => v == -1 || (30..=43200).contains(&v),
        TemplateCategory::Marketing => (43200..=2_592_000).contains(&v),
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(err(
            field,
            format!("{v} is outside the documented range for {category} templates"),
        ))
    }
}

pub(crate) fn definition(d: &TemplateDefinition) -> Check {
    name(&d.name, "name")?;
    if d.language.trim().is_empty() {
        return Err(err("language", "must not be empty"));
    }
    ttl(
        d.message_send_ttl_seconds,
        Some(&d.category),
        "message_send_ttl_seconds",
    )?;
    // `templates/overview#parameter-formats`: positional when omitted.
    let format = d
        .parameter_format
        .clone()
        .unwrap_or(ParameterFormat::Positional);
    components(
        &d.components,
        Some(&d.category),
        Some(&format),
        "components",
    )
}

pub(crate) fn edit(e: &TemplateEdit) -> Check {
    if e == &TemplateEdit::default() {
        return Err(err("edit", "nothing to change"));
    }
    ttl(
        e.message_send_ttl_seconds,
        e.category.as_ref(),
        "message_send_ttl_seconds",
    )?;
    if let Some(list) = &e.components {
        // The template's current format is unknown here unless given, so
        // either placeholder style is accepted (but not both mixed).
        components(
            list,
            e.category.as_ref(),
            e.parameter_format.as_ref(),
            "components",
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct Counts {
    header: usize,
    body: usize,
    footer: usize,
    buttons: usize,
    carousel: usize,
    lto: usize,
    cpr: usize,
}

/// What the rest of the template changes about a component's limits.
struct Ctx<'a> {
    category: Option<&'a TemplateCategory>,
    format: Option<&'a ParameterFormat>,
    auth: bool,
    lto: bool,
    product_header: bool,
}

fn components(
    list: &[TemplateComponent],
    category: Option<&TemplateCategory>,
    format: Option<&ParameterFormat>,
    path: &str,
) -> Check {
    let mut n = Counts::default();
    let mut has_otp = false;
    let mut product_header = false;
    for c in list {
        match c {
            TemplateComponent::Header(h) => {
                n.header += 1;
                product_header |= h.format == HeaderFormat::Product;
            }
            TemplateComponent::Body(_) => n.body += 1,
            TemplateComponent::Footer(_) => n.footer += 1,
            TemplateComponent::Buttons { buttons } => {
                n.buttons += 1;
                has_otp |= buttons.iter().any(|b| matches!(b, Button::Otp(_)));
            }
            TemplateComponent::Carousel { .. } => n.carousel += 1,
            TemplateComponent::LimitedTimeOffer { .. } => n.lto += 1,
            TemplateComponent::CallPermissionRequest => n.cpr += 1,
            _ => {}
        }
    }
    // `templates/components`: body is the only required component; one
    // header, one body, one footer; buttons live in a single component.
    if n.body != 1 {
        return Err(err(path, "exactly one BODY component is required"));
    }
    for (count, what) in [
        (n.header, "HEADER"),
        (n.footer, "FOOTER"),
        (n.buttons, "BUTTONS"),
        (n.carousel, "CAROUSEL"),
        (n.lto, "LIMITED_TIME_OFFER"),
        (n.cpr, "call_permission_request"),
    ] {
        if count > 1 {
            return Err(err(path, format!("at most one {what} component")));
        }
    }

    let auth = match category {
        Some(c) => *c == TemplateCategory::Authentication,
        None => has_otp,
    };
    if has_otp && !auth {
        // `templates/components#one-time-password-buttons`: OTP buttons are
        // for authentication templates.
        return Err(err(
            path,
            "OTP buttons are only allowed in AUTHENTICATION templates",
        ));
    }
    let is = |c: TemplateCategory| category.is_none_or(|k| *k == c);
    let marketing_or_utility = category
        .is_none_or(|k| matches!(k, TemplateCategory::Marketing | TemplateCategory::Utility));

    if n.lto == 1 {
        // `templates/marketing-templates/limited-time-offer-templates#limitations`.
        if !is(TemplateCategory::Marketing) {
            return Err(err(
                path,
                "limited-time offers require a MARKETING template",
            ));
        }
        if n.footer > 0 {
            return Err(err(
                path,
                "limited-time offer templates cannot have a footer",
            ));
        }
    }
    if n.carousel == 1 && !is(TemplateCategory::Marketing) {
        // `templates/marketing-templates/media-card-carousel-templates`:
        // "carousel cards are only available for marketing template messages".
        return Err(err(path, "carousels require a MARKETING template"));
    }
    if n.cpr == 1 {
        // `templates/marketing-templates/call-permission-request-message-template#limitations`.
        if !marketing_or_utility {
            return Err(err(
                path,
                "call permission requests require a MARKETING or UTILITY template",
            ));
        }
        if n.buttons > 0 || n.carousel > 0 || n.lto > 0 {
            return Err(err(
                path,
                "a call permission request cannot be combined with other interactive components",
            ));
        }
    }

    let ctx = Ctx {
        category,
        format,
        auth,
        lto: n.lto == 1,
        product_header,
    };
    if auth {
        auth_components(list, path)?;
    }
    for (i, c) in list.iter().enumerate() {
        let field = format!("{path}[{i}]");
        component(c, &ctx, &field)?;
    }
    Ok(())
}

/// `templates/authentication-templates/authentication-templates`: preset
/// body text, optional security recommendation and expiry footer, one OTP
/// button; "URLs, media, and emojis are not supported".
fn auth_components(list: &[TemplateComponent], path: &str) -> Check {
    let mut otp_buttons = 0;
    for (i, c) in list.iter().enumerate() {
        let field = format!("{path}[{i}]");
        match c {
            TemplateComponent::Body(b) => {
                if b.text.is_some() || b.example.is_some() {
                    return Err(err(
                        format!("{field}.text"),
                        "authentication templates use Meta's preset body text",
                    ));
                }
            }
            TemplateComponent::Footer(f) => {
                if f.text.is_some() {
                    return Err(err(
                        format!("{field}.text"),
                        "authentication templates use Meta's preset footer text",
                    ));
                }
            }
            TemplateComponent::Buttons { buttons } => {
                for (j, b) in buttons.iter().enumerate() {
                    if matches!(b, Button::Otp(_)) {
                        otp_buttons += 1;
                    } else {
                        return Err(err(
                            format!("{field}.buttons[{j}]"),
                            "authentication templates only take an OTP button",
                        ));
                    }
                }
            }
            TemplateComponent::Other(_) => {}
            _ => {
                return Err(err(
                    field,
                    "authentication templates only take BODY, FOOTER and BUTTONS",
                ));
            }
        }
    }
    if otp_buttons != 1 {
        return Err(err(
            path,
            "authentication templates need exactly one OTP button",
        ));
    }
    Ok(())
}

fn component(c: &TemplateComponent, ctx: &Ctx<'_>, field: &str) -> Check {
    match c {
        TemplateComponent::Header(h) => header(h, ctx, field),
        TemplateComponent::Body(b) => {
            if b.add_security_recommendation == Some(true) && !ctx.auth {
                return Err(err(
                    format!("{field}.add_security_recommendation"),
                    "only for AUTHENTICATION templates",
                ));
            }
            if ctx.auth {
                return Ok(());
            }
            let Some(text) = &b.text else {
                return Err(err(format!("{field}.text"), "required"));
            };
            // `templates/components#body`: 1024. Limited-time offers: 600
            // (`…/limited-time-offer-templates`). Single-product templates:
            // 160 (`catalogs/spm-template-messages`, "Maximum 160").
            let max = if ctx.lto {
                600
            } else if ctx.product_header {
                160
            } else {
                1024
            };
            body_text(
                text,
                b.example.as_ref(),
                ctx.format,
                max,
                &format!("{field}.text"),
            )
        }
        TemplateComponent::Footer(f) => {
            if let Some(minutes) = f.code_expiration_minutes {
                if !ctx.auth {
                    return Err(err(
                        format!("{field}.code_expiration_minutes"),
                        "only for AUTHENTICATION templates",
                    ));
                }
                code_expiration_minutes(minutes, &format!("{field}.code_expiration_minutes"))?;
            }
            if ctx.auth {
                return Ok(());
            }
            let Some(t) = &f.text else {
                return Err(err(format!("{field}.text"), "required"));
            };
            // `templates/components#footer`: 60 characters maximum.
            text(t, 60, &format!("{field}.text"))
        }
        TemplateComponent::Buttons { buttons } => button_list(buttons, ctx, field),
        TemplateComponent::Carousel { cards } => carousel(cards, ctx, field),
        TemplateComponent::LimitedTimeOffer { limited_time_offer } => {
            // `…/limited-time-offer-templates`: "Maximum 16 characters".
            text(
                &limited_time_offer.text,
                16,
                &format!("{field}.limited_time_offer.text"),
            )
        }
        _ => Ok(()),
    }
}

fn header(h: &HeaderComponent, ctx: &Ctx<'_>, field: &str) -> Check {
    match &h.format {
        HeaderFormat::Text => {
            let Some(t) = &h.text else {
                return Err(err(format!("{field}.text"), "required for TEXT headers"));
            };
            // `templates/components#text-header`: 60 characters, 1 parameter.
            text(t, 60, &format!("{field}.text"))?;
            let ex = h.example.as_ref();
            placeholders_match(
                t,
                ex.and_then(|e| e.header_text.as_deref()),
                ex.and_then(|e| e.header_text_named_params.as_deref())
                    .map(|v| {
                        v.iter()
                            .map(|p| (p.param_name.as_str(), p.example.as_str()))
                            .collect()
                    }),
                ctx.format,
                Some(1),
                &format!("{field}.text"),
            )
        }
        f if f.is_media() => {
            if ctx.lto && !matches!(f, HeaderFormat::Image | HeaderFormat::Video) {
                // `…/limited-time-offer-templates`: header "Can be IMAGE, or VIDEO".
                return Err(err(
                    format!("{field}.format"),
                    "limited-time offer headers must be IMAGE or VIDEO",
                ));
            }
            // `templates/components#media-header`: media headers carry the
            // uploaded asset handle as their example.
            let has_handle = h
                .example
                .as_ref()
                .and_then(|e| e.header_handle.as_ref())
                .is_some_and(|v| v.iter().any(|s| !s.trim().is_empty()));
            if has_handle {
                Ok(())
            } else {
                Err(err(
                    format!("{field}.example.header_handle"),
                    "media headers need an uploaded asset handle",
                ))
            }
        }
        HeaderFormat::Location => {
            // `templates/components#location-header`: UTILITY or MARKETING.
            if ctx.category.is_some_and(|c| {
                !matches!(c, TemplateCategory::Utility | TemplateCategory::Marketing)
            }) {
                return Err(err(
                    format!("{field}.format"),
                    "location headers require a UTILITY or MARKETING template",
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn body_text(
    t: &str,
    example: Option<&super::definition::BodyExample>,
    format: Option<&ParameterFormat>,
    max: usize,
    field: &str,
) -> Check {
    text(t, max, field)?;
    // `templates/template-review#parameter-formatting`: "The message
    // template cannot start or end with a parameter".
    let trimmed = t.trim();
    let found = parse_placeholders(t, field)?;
    if let (Some(first), Some(last)) = (found.first(), found.last())
        && (trimmed.starts_with(&first.raw) || trimmed.ends_with(&last.raw))
    {
        return Err(err(field, "body text cannot start or end with a parameter"));
    }
    let positional = example
        .and_then(|e| e.body_text.as_ref())
        .map(|rows| rows.first().map(Vec::as_slice).unwrap_or_default());
    let named = example
        .and_then(|e| e.body_text_named_params.as_deref())
        .map(|v| {
            v.iter()
                .map(|p| (p.param_name.as_str(), p.example.as_str()))
                .collect()
        });
    placeholders_match(t, positional, named, format, None, field)
}

struct Found {
    raw: String,
    kind: Kind,
}

#[derive(PartialEq, Eq)]
enum Kind {
    Positional(u32),
    Named(String),
}

/// Find `{{…}}` placeholders. `templates/overview#parameter-formats`:
/// positional are `{{1}}`, `{{2}}`…; named are lowercase letters and
/// underscores. `templates/template-review#parameter-formatting`: special
/// characters and mismatched braces are rejected.
fn parse_placeholders(t: &str, field: &str) -> Result<Vec<Found>, ValidationError> {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(err(field, "unclosed `{{` placeholder"));
        };
        let inner = &after[..end];
        let kind = if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) {
            match inner.parse::<u32>() {
                Ok(n) => Kind::Positional(n),
                Err(_) => return Err(err(field, format!("invalid placeholder `{{{{{inner}}}}}`"))),
            }
        } else if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            Kind::Named(inner.to_owned())
        } else {
            return Err(err(
                field,
                format!(
                    "invalid placeholder `{{{{{inner}}}}}`: use `{{{{1}}}}` or lowercase letters and underscores"
                ),
            ));
        };
        out.push(Found {
            raw: format!("{{{{{inner}}}}}"),
            kind,
        });
        rest = &after[end + 2..];
    }
    Ok(out)
}

/// Placeholders in `t` agree with the declared format and the examples
/// (`templates/overview#parameter-formats`, `templates/components`: "If this
/// string contains a parameter, you must include the example property").
fn placeholders_match(
    t: &str,
    positional_examples: Option<&[String]>,
    named_examples: Option<Vec<(&str, &str)>>,
    format: Option<&ParameterFormat>,
    max: Option<usize>,
    field: &str,
) -> Check {
    let found = parse_placeholders(t, field)?;
    if found.is_empty() {
        return Ok(());
    }
    let positional: Vec<u32> = found
        .iter()
        .filter_map(|f| match f.kind {
            Kind::Positional(n) => Some(n),
            Kind::Named(_) => None,
        })
        .collect();
    let named: Vec<&str> = found
        .iter()
        .filter_map(|f| match &f.kind {
            Kind::Named(n) => Some(n.as_str()),
            Kind::Positional(_) => None,
        })
        .collect();
    if !positional.is_empty() && !named.is_empty() {
        return Err(err(field, "mixes positional and named placeholders"));
    }
    if !positional.is_empty() {
        if format == Some(&ParameterFormat::Named) {
            return Err(err(field, "positional placeholder in a NAMED template"));
        }
        // First occurrences must read 1, 2, 3, …
        let mut order: Vec<u32> = Vec::new();
        for n in positional {
            if !order.contains(&n) {
                order.push(n);
            }
        }
        if !order.iter().zip(1u32..).all(|(n, want)| *n == want) {
            return Err(err(
                field,
                "positional placeholders must be {{1}}, {{2}}, … in order",
            ));
        }
        if let Some(max) = max
            && order.len() > max
        {
            return Err(err(field, format!("at most {max} placeholder(s)")));
        }
        let examples = positional_examples.unwrap_or_default();
        if examples.len() != order.len() {
            return Err(err(
                field,
                format!(
                    "{} placeholder(s) but {} positional example value(s)",
                    order.len(),
                    examples.len()
                ),
            ));
        }
        return Ok(());
    }
    if format == Some(&ParameterFormat::Positional) {
        return Err(err(
            field,
            "named placeholder in a POSITIONAL template (set parameter_format to NAMED)",
        ));
    }
    let wanted: BTreeSet<&str> = named.into_iter().collect();
    if let Some(max) = max
        && wanted.len() > max
    {
        return Err(err(field, format!("at most {max} placeholder(s)")));
    }
    let examples = named_examples.unwrap_or_default();
    let mut given = BTreeSet::new();
    for (name, _) in &examples {
        if !given.insert(*name) {
            return Err(err(field, format!("duplicate example for `{name}`")));
        }
    }
    if given != wanted {
        return Err(err(
            field,
            "named examples must cover exactly the placeholders in the text",
        ));
    }
    Ok(())
}

fn button_list(buttons: &[Button], ctx: &Ctx<'_>, field: &str) -> Check {
    // `templates/components#buttons`: up to 10 buttons in total.
    if buttons.is_empty() {
        return Err(err(format!("{field}.buttons"), "must not be empty"));
    }
    if buttons.len() > 10 {
        return Err(err(format!("{field}.buttons"), "at most 10 buttons"));
    }
    let count = |f: fn(&Button) -> bool| buttons.iter().filter(|b| f(b)).count();
    // `templates/components`: two URL buttons, one phone number button,
    // one copy code button (also `…/coupon-templates#limitations`).
    if count(|b| matches!(b, Button::Url { .. })) > 2 {
        return Err(err(format!("{field}.buttons"), "at most 2 URL buttons"));
    }
    if count(|b| matches!(b, Button::PhoneNumber { .. })) > 1 {
        return Err(err(
            format!("{field}.buttons"),
            "at most 1 PHONE_NUMBER button",
        ));
    }
    if count(|b| matches!(b, Button::CopyCode { .. })) > 1 {
        return Err(err(
            format!("{field}.buttons"),
            "at most 1 COPY_CODE button",
        ));
    }
    // `templates/components#quick-reply-buttons`: quick replies and other
    // buttons must form two groups ("Quick Reply, URL, Quick Reply" is
    // invalid).
    let groups = buttons
        .windows(2)
        .filter(|w| {
            matches!(w[0], Button::QuickReply { .. }) != matches!(w[1], Button::QuickReply { .. })
        })
        .count();
    if groups > 1 {
        return Err(err(
            format!("{field}.buttons"),
            "quick-reply buttons must be grouped together, before or after the other buttons",
        ));
    }
    for (i, b) in buttons.iter().enumerate() {
        button(b, ctx, &format!("{field}.buttons[{i}]"))?;
    }
    Ok(())
}

fn button(b: &Button, ctx: &Ctx<'_>, field: &str) -> Check {
    let f = |name: &str| format!("{field}.{name}");
    match b {
        // `templates/components#quick-reply-buttons`: label 25 characters.
        Button::QuickReply { text: t } => text(t, 25, &f("text")),
        Button::Url {
            text: t,
            url,
            example,
        } => {
            // `templates/components#url-buttons`: label 25, URL 2000, one
            // variable appended to the end, example required with a variable.
            text(t, 25, &f("text"))?;
            text(url, 2000, &f("url"))?;
            let found = parse_placeholders(url, &f("url"))?;
            if found.is_empty() {
                return Ok(());
            }
            if found.len() > 1 {
                return Err(err(f("url"), "at most one placeholder"));
            }
            if !url.trim_end().ends_with(&found[0].raw) {
                return Err(err(
                    f("url"),
                    "the placeholder must be at the end of the URL",
                ));
            }
            match (&found[0].kind, ctx.format) {
                (Kind::Positional(n), _) if *n != 1 => {
                    return Err(err(f("url"), "the placeholder must be {{1}}"));
                }
                (Kind::Positional(_), Some(ParameterFormat::Named)) => {
                    return Err(err(f("url"), "positional placeholder in a NAMED template"));
                }
                (Kind::Named(_), Some(ParameterFormat::Positional)) => {
                    return Err(err(f("url"), "named placeholder in a POSITIONAL template"));
                }
                _ => {}
            }
            match example.as_deref() {
                Some([one]) => text(one, 2000, &f("example[0]")),
                _ => Err(err(
                    f("example"),
                    "one example value is required when the URL has a placeholder",
                )),
            }
        }
        Button::PhoneNumber {
            text: t,
            phone_number,
        } => {
            // `templates/components#phone-number-buttons`: label 25, number 20.
            text(t, 25, &f("text"))?;
            text(phone_number, 20, &f("phone_number"))
        }
        Button::CopyCode { example } => {
            // `templates/components#copy-code-buttons`: 20; limited-time
            // offers: 15 (`…/limited-time-offer-templates`).
            let max = if ctx.lto { 15 } else { 20 };
            text(example, max, &f("example"))
        }
        Button::Otp(otp) => otp_button(otp, field),
        Button::Flow(flow) => flow_button(flow, field),
        Button::VoiceCall {
            text: t,
            ttl_minutes,
        } => {
            // `calling/call-button-messages-deep-links`: label 20,
            // ttl_minutes 1440–43200 at creation.
            if let Some(t) = t {
                text(t, 20, &f("text"))?;
            }
            if let Some(ttl) = ttl_minutes
                && !(1440..=43200).contains(ttl)
            {
                return Err(err(f("ttl_minutes"), "must be between 1440 and 43200"));
            }
            Ok(())
        }
        Button::RequestContactInfo { text: t } => {
            // `business-scoped-user-ids#using-templates`: utility and
            // marketing templates; label fixed.
            if ctx.category.is_some_and(|c| {
                !matches!(c, TemplateCategory::Utility | TemplateCategory::Marketing)
            }) {
                return Err(err(
                    field,
                    "REQUEST_CONTACT_INFO requires a UTILITY or MARKETING template",
                ));
            }
            match t.as_deref() {
                None | Some("Share Contact Info") => Ok(()),
                Some(_) => Err(err(
                    f("text"),
                    "cannot be customized; omit it or pass \"Share Contact Info\"",
                )),
            }
        }
        _ => Ok(()),
    }
}

/// OTP button limits, from the copy-code, one-tap and zero-tap pages under
/// `templates/authentication-templates/`.
pub(crate) fn otp_button(otp: &OtpButton, field: &str) -> Check {
    let f = |name: &str| format!("{field}.{name}");
    let (label, autofill, apps) = match otp {
        OtpButton::CopyCode { text } => (text, &None, None),
        OtpButton::OneTap {
            text,
            autofill_text,
            supported_apps,
        } => (text, autofill_text, Some(supported_apps)),
        OtpButton::ZeroTap {
            text,
            autofill_text,
            zero_tap_terms_accepted,
            supported_apps,
        } => {
            // zero-tap page: "If set to false, the template will not be created".
            if !zero_tap_terms_accepted {
                return Err(err(
                    f("zero_tap_terms_accepted"),
                    "must be true to create a zero-tap template",
                ));
            }
            (text, autofill_text, Some(supported_apps))
        }
    };
    // Button and autofill labels: "Maximum 25 characters".
    if let Some(t) = label {
        text(t, 25, &f("text"))?;
    }
    if let Some(t) = autofill {
        text(t, 25, &f("autofill_text"))?;
    }
    if let Some(apps) = apps {
        // "define pairs of app package names and signing key hashes for up
        // to 5 apps"; package name and hash are "Required".
        if apps.is_empty() {
            return Err(err(f("supported_apps"), "at least one app is required"));
        }
        if apps.len() > 5 {
            return Err(err(f("supported_apps"), "at most 5 apps"));
        }
        for (i, app) in apps.iter().enumerate() {
            supported_app(app, &format!("{field}.supported_apps[{i}]"))?;
        }
    }
    Ok(())
}

fn supported_app(app: &SupportedApp, field: &str) -> Check {
    let pkg = &app.package_name;
    let pkg_field = format!("{field}.package_name");
    // One-tap/zero-tap pages: at least two segments, each starting with a
    // letter, `[a-zA-Z0-9_]`, at most 224 characters.
    if chars(pkg) > 224 {
        return Err(err(pkg_field, "must be at most 224 characters"));
    }
    let segments: Vec<&str> = pkg.split('.').collect();
    let segment_ok = |s: &&str| {
        s.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    if segments.len() < 2 || !segments.iter().all(segment_ok) {
        return Err(err(
            pkg_field,
            "needs two or more dot-separated segments, each starting with a letter, [a-zA-Z0-9_] only",
        ));
    }
    // "Must be exactly 11 characters", `[a-zA-Z0-9+/=]`.
    let hash = &app.signature_hash;
    if hash.len() != 11
        || !hash
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return Err(err(
            format!("{field}.signature_hash"),
            "must be exactly 11 characters of [a-zA-Z0-9+/=]",
        ));
    }
    Ok(())
}

/// `flows/guides/flows-templates` and the reference `Buttons` schema.
fn flow_button(flow: &FlowButton, field: &str) -> Check {
    if flow.text.trim().is_empty() {
        return Err(err(format!("{field}.text"), "must not be empty"));
    }
    let sources = [
        flow.flow_id.is_some(),
        flow.flow_name.is_some(),
        flow.flow_json.is_some(),
    ]
    .into_iter()
    .filter(|&set| set)
    .count();
    if sources != 1 {
        return Err(err(
            field,
            "exactly one of flow_id, flow_name, flow_json is required",
        ));
    }
    // "navigate_screen: Required if flow_action is navigate". Meta's default
    // action is navigate too, but whether the screen is then required is not
    // stated, so only an explicit `navigate` is checked.
    if flow.flow_action == Some(FlowAction::Navigate) && flow.navigate_screen.is_none() {
        return Err(err(
            format!("{field}.navigate_screen"),
            "required when flow_action is navigate",
        ));
    }
    Ok(())
}

/// Carousels: `templates/marketing-templates/media-card-carousel-templates`
/// and `catalogs/product-card-carousel-template-messages`.
fn carousel(cards: &[CarouselCard], ctx: &Ctx<'_>, field: &str) -> Check {
    // Media cards: "minimum 2, maximum 10", and a send must carry exactly
    // that many. Product cards: "Define only two product cards when you
    // create the template" (a send may then carry up to 10). That reads as
    // guidance rather than a stated rejection, so product carousels get the
    // same 2–10 bound here instead of an exact 2.
    if !(2..=10).contains(&cards.len()) {
        return Err(err(
            format!("{field}.cards"),
            "a carousel needs 2 to 10 cards",
        ));
    }
    // "All cards defined on a template must have the same components."
    let shape = |card: &CarouselCard| -> Vec<String> {
        card.components
            .iter()
            .map(|c| match c {
                TemplateComponent::Header(h) => format!("HEADER:{}", h.format),
                TemplateComponent::Body(_) => "BODY".to_owned(),
                TemplateComponent::Buttons { buttons } => format!(
                    "BUTTONS:{}",
                    buttons
                        .iter()
                        .map(button_kind)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                other => format!("{other:?}"),
            })
            .collect()
    };
    let first = shape(&cards[0]);
    for (i, card) in cards.iter().enumerate() {
        let path = format!("{field}.cards[{i}]");
        if shape(card) != first {
            return Err(err(path, "all cards must have the same components"));
        }
        card_components(&card.components, ctx, &path)?;
    }
    Ok(())
}

fn button_kind(b: &Button) -> &'static str {
    match b {
        Button::QuickReply { .. } => "QUICK_REPLY",
        Button::Url { .. } => "URL",
        Button::PhoneNumber { .. } => "PHONE_NUMBER",
        Button::Spm { .. } => "SPM",
        _ => "OTHER",
    }
}

fn card_components(list: &[TemplateComponent], ctx: &Ctx<'_>, path: &str) -> Check {
    let headers: Vec<&HeaderComponent> = list
        .iter()
        .filter_map(|c| match c {
            TemplateComponent::Header(h) => Some(h),
            _ => None,
        })
        .collect();
    let [card_header] = headers.as_slice() else {
        return Err(err(path, "each card needs exactly one HEADER"));
    };
    let product = card_header.format == HeaderFormat::Product;
    if !product
        && !matches!(
            card_header.format,
            HeaderFormat::Image | HeaderFormat::Video
        )
    {
        return Err(err(
            format!("{path}.components"),
            "card headers are IMAGE, VIDEO or PRODUCT",
        ));
    }
    for (i, c) in list.iter().enumerate() {
        let field = format!("{path}.components[{i}]");
        match c {
            TemplateComponent::Header(h) => header(h, ctx, &field)?,
            TemplateComponent::Body(b) => {
                let Some(t) = &b.text else {
                    return Err(err(format!("{field}.text"), "required"));
                };
                // Media card page: "the card body text limit of 160 characters".
                body_text(
                    t,
                    b.example.as_ref(),
                    ctx.format,
                    160,
                    &format!("{field}.text"),
                )?;
            }
            TemplateComponent::Buttons { buttons } => {
                // Media cards: "up to two buttons", quick reply / phone / URL.
                // Product cards: "a single View button or URL button".
                let (max, allowed): (usize, fn(&Button) -> bool) = if product {
                    (1, |b| matches!(b, Button::Spm { .. } | Button::Url { .. }))
                } else {
                    (2, |b| {
                        matches!(
                            b,
                            Button::QuickReply { .. }
                                | Button::Url { .. }
                                | Button::PhoneNumber { .. }
                        )
                    })
                };
                if buttons.len() > max {
                    return Err(err(
                        format!("{field}.buttons"),
                        format!("at most {max} button(s) per card"),
                    ));
                }
                if let Some(j) = buttons.iter().position(|b| !allowed(b)) {
                    return Err(err(
                        format!("{field}.buttons[{j}]"),
                        "button type not allowed on this kind of card",
                    ));
                }
                button_list(buttons, ctx, &field)?;
            }
            _ => {
                return Err(err(field, "cards take HEADER, BODY and BUTTONS only"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::definition::{BodyComponent, FooterComponent};

    fn def(components: Vec<TemplateComponent>) -> TemplateDefinition {
        let mut d = TemplateDefinition::new("t", "en_US", TemplateCategory::Marketing);
        d.components = components;
        d
    }

    fn field_of(d: &TemplateDefinition) -> String {
        definition(d).unwrap_err().field
    }

    #[test]
    fn names_follow_the_documented_regex_and_length() {
        assert!(name("order_confirmation_2", "name").is_ok());
        assert!(name("Order", "name").is_err());
        assert!(name("order confirmation", "name").is_err());
        assert!(name("", "name").is_err());
        assert!(name(&"a".repeat(512), "name").is_ok());
        assert!(name(&"a".repeat(513), "name").is_err());
    }

    #[test]
    fn body_is_required_and_unique() {
        assert_eq!(field_of(&def(vec![])), "components");
        assert_eq!(
            field_of(&def(vec![
                TemplateComponent::body("a b"),
                TemplateComponent::body("c d")
            ])),
            "components"
        );
    }

    #[test]
    fn text_lengths() {
        assert!(definition(&def(vec![TemplateComponent::body("x".repeat(1024))])).is_ok());
        assert_eq!(
            field_of(&def(vec![TemplateComponent::body("x".repeat(1025))])),
            "components[0].text"
        );
        assert_eq!(
            field_of(&def(vec![
                TemplateComponent::header_text("h".repeat(61)),
                TemplateComponent::body("b")
            ])),
            "components[0].text"
        );
        assert_eq!(
            field_of(&def(vec![
                TemplateComponent::body("b"),
                TemplateComponent::footer("f".repeat(61))
            ])),
            "components[1].text"
        );
    }

    #[test]
    fn positional_placeholders_need_sequential_numbers_and_examples() {
        let ok = TemplateComponent::body_positional("Hi {{1}}, order {{2}}.", ["a", "b"]);
        assert!(definition(&def(vec![ok])).is_ok());
        let gap = TemplateComponent::body_positional("Hi {{1}}, order {{3}}.", ["a", "b"]);
        assert!(definition(&def(vec![gap])).is_err());
        let missing = TemplateComponent::body_positional("Hi {{1}}, order {{2}}.", ["a"]);
        assert!(definition(&def(vec![missing])).is_err());
        let none = TemplateComponent::body("Hi {{1}}, welcome.");
        assert!(definition(&def(vec![none])).is_err());
        let dangling = TemplateComponent::body_positional("Your code is {{1}}", ["a"]);
        assert!(
            definition(&def(vec![dangling])).is_err(),
            "cannot end with a parameter"
        );
        let bad = TemplateComponent::body_positional("Hi {{#1}} there.", ["a"]);
        assert!(definition(&def(vec![bad])).is_err());
        let unclosed = TemplateComponent::body("Hi {{1 there.");
        assert!(definition(&def(vec![unclosed])).is_err());
    }

    #[test]
    fn named_placeholders_need_named_format_and_matching_examples() {
        let body = TemplateComponent::body_named("Hi {{first_name}}!", [("first_name", "Pablo")]);
        assert!(
            definition(&def(vec![body.clone()])).is_err(),
            "default format is positional"
        );
        let named = def(vec![body]).parameter_format(ParameterFormat::Named);
        assert!(definition(&named).is_ok());
        let wrong = def(vec![TemplateComponent::body_named(
            "Hi {{first_name}}!",
            [("last_name", "x")],
        )])
        .parameter_format(ParameterFormat::Named);
        assert!(definition(&wrong).is_err());
        let upper = def(vec![TemplateComponent::body_named(
            "Hi {{First}}!",
            [("First", "x")],
        )])
        .parameter_format(ParameterFormat::Named);
        assert!(definition(&upper).is_err());
    }

    #[test]
    fn header_text_takes_one_parameter() {
        let two = TemplateComponent::Header(HeaderComponent {
            format: HeaderFormat::Text,
            text: Some("{{1}} and {{2}}".into()),
            example: Some(crate::templates::HeaderExample {
                header_text: Some(vec!["a".into(), "b".into()]),
                ..Default::default()
            }),
        });
        assert_eq!(
            field_of(&def(vec![two, TemplateComponent::body("b")])),
            "components[0].text"
        );
    }

    #[test]
    fn media_headers_need_a_handle() {
        let h = TemplateComponent::Header(HeaderComponent {
            format: HeaderFormat::Image,
            text: None,
            example: None,
        });
        assert_eq!(
            field_of(&def(vec![h, TemplateComponent::body("b")])),
            "components[0].example.header_handle"
        );
    }

    #[test]
    fn button_counts_labels_and_grouping() {
        let many =
            TemplateComponent::buttons((0..11).map(|i| Button::quick_reply(format!("q{i}"))));
        assert!(definition(&def(vec![TemplateComponent::body("b"), many])).is_err());
        let urls = TemplateComponent::buttons([
            Button::url("a", "https://a"),
            Button::url("b", "https://b"),
            Button::url("c", "https://c"),
        ]);
        assert!(definition(&def(vec![TemplateComponent::body("b"), urls])).is_err());
        let split = TemplateComponent::buttons([
            Button::quick_reply("a"),
            Button::url("b", "https://b"),
            Button::quick_reply("c"),
        ]);
        assert_eq!(
            field_of(&def(vec![TemplateComponent::body("b"), split])),
            "components[1].buttons"
        );
        let grouped = TemplateComponent::buttons([
            Button::url("b", "https://b"),
            Button::phone_number("p", "15550051310"),
            Button::quick_reply("a"),
            Button::quick_reply("c"),
        ]);
        assert!(definition(&def(vec![TemplateComponent::body("b"), grouped])).is_ok());
        let long = TemplateComponent::buttons([Button::quick_reply("x".repeat(26))]);
        assert_eq!(
            field_of(&def(vec![TemplateComponent::body("b"), long])),
            "components[1].buttons[0].text"
        );
        let phone = TemplateComponent::buttons([Button::phone_number("p", "1".repeat(21))]);
        assert!(definition(&def(vec![TemplateComponent::body("b"), phone])).is_err());
        let code = TemplateComponent::buttons([Button::copy_code("x".repeat(21))]);
        assert!(definition(&def(vec![TemplateComponent::body("b"), code])).is_err());
    }

    #[test]
    fn url_variable_goes_at_the_end_with_an_example() {
        let ok = Button::url_with_example("Shop", "https://x.com/shop?promo={{1}}", "summer");
        assert!(
            definition(&def(vec![
                TemplateComponent::body("b"),
                TemplateComponent::buttons([ok])
            ]))
            .is_ok()
        );
        let middle = Button::url_with_example("Shop", "https://x.com/{{1}}/shop", "a");
        assert!(
            definition(&def(vec![
                TemplateComponent::body("b"),
                TemplateComponent::buttons([middle])
            ]))
            .is_err()
        );
        let no_example = Button::url("Shop", "https://x.com/{{1}}");
        assert_eq!(
            field_of(&def(vec![
                TemplateComponent::body("b"),
                TemplateComponent::buttons([no_example])
            ])),
            "components[1].buttons[0].example"
        );
    }

    #[test]
    fn otp_buttons_only_in_authentication_and_their_limits() {
        let otp = Button::Otp(OtpButton::CopyCode { text: None });
        assert!(
            definition(&def(vec![
                TemplateComponent::body("b"),
                TemplateComponent::buttons([otp])
            ]))
            .is_err()
        );

        let auth = |otp: OtpButton| {
            let mut d = TemplateDefinition::new("a", "en_US", TemplateCategory::Authentication);
            d.components = vec![
                TemplateComponent::Body(BodyComponent {
                    add_security_recommendation: Some(true),
                    ..BodyComponent::default()
                }),
                TemplateComponent::Footer(FooterComponent {
                    text: None,
                    code_expiration_minutes: Some(10),
                }),
                TemplateComponent::buttons([Button::Otp(otp)]),
            ];
            d
        };
        assert!(definition(&auth(OtpButton::CopyCode { text: None })).is_ok());
        let app = SupportedApp::new("com.example.luckyshrub", "K8a/AINcGX7");
        assert!(
            definition(&auth(OtpButton::OneTap {
                text: None,
                autofill_text: None,
                supported_apps: vec![app.clone()]
            }))
            .is_ok()
        );
        assert!(
            definition(&auth(OtpButton::OneTap {
                text: None,
                autofill_text: None,
                supported_apps: vec![]
            }))
            .is_err()
        );
        assert!(
            definition(&auth(OtpButton::OneTap {
                text: None,
                autofill_text: None,
                supported_apps: vec![app.clone(); 6]
            }))
            .is_err()
        );
        for (pkg, hash) in [
            ("example", "K8a/AINcGX7"),
            ("com.1example", "K8a/AINcGX7"),
            ("com.exa-mple", "K8a/AINcGX7"),
            ("com.example", "K8a/AINcGX"),
            ("com.example", "K8a/AINcGX7!"),
        ] {
            assert!(
                definition(&auth(OtpButton::OneTap {
                    text: None,
                    autofill_text: None,
                    supported_apps: vec![SupportedApp::new(pkg, hash)]
                }))
                .is_err(),
                "{pkg} / {hash}"
            );
        }
        assert!(
            definition(&auth(OtpButton::ZeroTap {
                text: None,
                autofill_text: None,
                zero_tap_terms_accepted: false,
                supported_apps: vec![app.clone()]
            }))
            .is_err()
        );
        let mut d = auth(OtpButton::CopyCode { text: None });
        d.components[1] = TemplateComponent::Footer(FooterComponent {
            text: None,
            code_expiration_minutes: Some(91),
        });
        assert!(definition(&d).is_err());
        let mut d = auth(OtpButton::CopyCode { text: None });
        d.components[0] = TemplateComponent::body("custom text");
        assert!(definition(&d).is_err(), "auth body text is preset");
        let mut d = auth(OtpButton::CopyCode { text: None });
        d.components.insert(0, TemplateComponent::header_text("h"));
        assert!(definition(&d).is_err(), "no header in auth templates");
    }

    #[test]
    fn ttl_ranges_by_category() {
        let c = |c| Some(c);
        assert!(ttl(Some(900), c(&TemplateCategory::Authentication), "t").is_ok());
        assert!(ttl(Some(901), c(&TemplateCategory::Authentication), "t").is_err());
        assert!(ttl(Some(29), c(&TemplateCategory::Authentication), "t").is_err());
        assert!(ttl(Some(-1), c(&TemplateCategory::Utility), "t").is_ok());
        assert!(ttl(Some(43201), c(&TemplateCategory::Utility), "t").is_err());
        assert!(ttl(Some(120), c(&TemplateCategory::Marketing), "t").is_err());
        assert!(ttl(Some(-1), c(&TemplateCategory::Marketing), "t").is_err());
        assert!(ttl(Some(43200), c(&TemplateCategory::Marketing), "t").is_ok());
    }

    #[test]
    fn limited_time_offer_rules() {
        let base = |extra: Vec<TemplateComponent>| {
            let mut v = vec![
                TemplateComponent::limited_time_offer("Expiring offer!", Some(true)),
                TemplateComponent::body("Good news, use it."),
            ];
            v.extend(extra);
            def(v)
        };
        assert!(definition(&base(vec![])).is_ok());
        assert!(definition(&base(vec![TemplateComponent::footer("f")])).is_err());
        let long = def(vec![
            TemplateComponent::limited_time_offer("x".repeat(17), None),
            TemplateComponent::body("b"),
        ]);
        assert!(definition(&long).is_err());
        let code = base(vec![TemplateComponent::buttons([Button::copy_code(
            "x".repeat(16),
        )])]);
        assert!(definition(&code).is_err(), "LTO offer code max 15");
        let mut utility = base(vec![]);
        utility.category = TemplateCategory::Utility;
        assert!(definition(&utility).is_err());
    }

    #[test]
    fn call_permission_request_rules() {
        let mut d = def(vec![
            TemplateComponent::body("Can we call you?"),
            TemplateComponent::call_permission_request(),
        ]);
        assert!(definition(&d).is_ok());
        d.components
            .push(TemplateComponent::buttons([Button::quick_reply("x")]));
        assert!(definition(&d).is_err());
    }

    #[test]
    fn carousel_rules() {
        let card = |handle: &str| {
            CarouselCard::new([
                TemplateComponent::header_image(handle),
                TemplateComponent::buttons([Button::quick_reply("More")]),
            ])
        };
        let ok = def(vec![
            TemplateComponent::body("Rare plants for sale."),
            TemplateComponent::carousel([card("h1"), card("h2")]),
        ]);
        assert!(definition(&ok).is_ok());
        let one = def(vec![
            TemplateComponent::body("Rare plants for sale."),
            TemplateComponent::carousel([card("h1")]),
        ]);
        assert!(definition(&one).is_err());
        let uneven = def(vec![
            TemplateComponent::body("Rare plants for sale."),
            TemplateComponent::carousel([
                card("h1"),
                CarouselCard::new([TemplateComponent::header_image("h2")]),
            ]),
        ]);
        assert_eq!(field_of(&uneven), "components[1].cards[1]");
    }

    #[test]
    fn voice_call_and_contact_info_buttons() {
        let vc =
            TemplateComponent::buttons([Button::voice_call(Some("Call Now".into()), Some(100))]);
        assert!(definition(&def(vec![TemplateComponent::body("b"), vc])).is_err());
        let ci = TemplateComponent::buttons([Button::RequestContactInfo {
            text: Some("Share".into()),
        }]);
        assert!(definition(&def(vec![TemplateComponent::body("b"), ci])).is_err());
    }

    #[test]
    fn edit_needs_something_and_checks_components() {
        assert!(edit(&TemplateEdit::default()).is_err());
        assert!(edit(&TemplateEdit::category(TemplateCategory::Marketing)).is_ok());
        assert!(edit(&TemplateEdit::components(vec![])).is_err());
        // Either placeholder style when the format is unknown.
        assert!(
            edit(&TemplateEdit::components(vec![
                TemplateComponent::body_named("Hi {{name}}!", [("name", "x")])
            ]))
            .is_ok()
        );
    }
}
