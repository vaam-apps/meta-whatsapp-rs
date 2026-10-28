//! Components (`flows/guides/components`, `flows/guides/media_upload`), by
//! `type`.
//!
//! Each component is a struct holding the properties Meta documents, with
//! their wire names (`on-click-action`, `helper-text`, …), plus a flattened
//! `extra` map for any other property, so a property Meta adds later is
//! kept as sent. A component type this version does not know, or a known one
//! whose properties do not fit its struct, parses into
//! [`Component::Unknown`], verbatim.
//!
//! Build one with `new` (its required properties) and the setters of its
//! optional ones: `TextInput::new("email", "Email").input_type(InputType::Email)`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use super::actions::Action;
use super::dynamic::{Dynamic, TextContent};
use super::tagged::tagged_union;
use crate::flows::types::wire_enum;

/// A struct of Flow JSON properties: required ones, optional ones (each with
/// a setter), and `extra` for the rest.
macro_rules! flow_object {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            required {
                $( $(#[$rmeta:meta])* $rfield:ident: $rty:ty = $rwire:literal, )*
            }
            optional {
                $( $(#[$ometa:meta])* $ofield:ident: $oty:ty = $owire:literal, )*
            }
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            $(
                $(#[$rmeta])*
                #[serde(rename = $rwire)]
                pub $rfield: $rty,
            )*
            $(
                $(#[$ometa])*
                #[serde(rename = $owire, default, skip_serializing_if = "Option::is_none")]
                pub $ofield: Option<$oty>,
            )*
            /// Properties this version does not model, kept as sent.
            #[serde(flatten)]
            pub extra: Map<String, Value>,
        }

        impl $name {
            /// From its required properties; the optional ones are unset.
            pub fn new($( $rfield: impl Into<$rty> ),*) -> Self {
                Self {
                    $( $rfield: $rfield.into(), )*
                    $( $ofield: None, )*
                    extra: Map::new(),
                }
            }

            $(
                #[doc = concat!("Set `", $owire, "`.")]
                #[must_use]
                pub fn $ofield(mut self, value: impl Into<$oty>) -> Self {
                    self.$ofield = Some(value.into());
                    self
                }
            )*

            /// Set a property this version does not model (it is sent as
            /// given, next to the modelled ones).
            #[must_use]
            pub fn extra(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
                self.extra.insert(key.into(), value.into());
                self
            }
        }
    };
}

tagged_union! {
    /// A Flow JSON component, by its `type`.
    pub enum Component by "type" {
        /// `TextHeading`: the page's top-level title.
        TextHeading(TextHeading) = "TextHeading",
        /// `TextSubheading`.
        TextSubheading(TextSubheading) = "TextSubheading",
        /// `TextBody`.
        TextBody(TextBody) = "TextBody",
        /// `TextCaption`.
        TextCaption(TextCaption) = "TextCaption",
        /// `RichText` (Flow JSON 5.1+): Markdown.
        RichText(RichText) = "RichText",
        /// `TextInput`: one line of input.
        TextInput(TextInput) = "TextInput",
        /// `TextArea`: several lines of input.
        TextArea(TextArea) = "TextArea",
        /// `CheckboxGroup`: pick several options.
        CheckboxGroup(CheckboxGroup) = "CheckboxGroup",
        /// `RadioButtonsGroup`: pick one option.
        RadioButtonsGroup(RadioButtonsGroup) = "RadioButtonsGroup",
        /// `Dropdown`: pick one option from a list.
        Dropdown(Dropdown) = "Dropdown",
        /// `ChipsSelector` (Flow JSON 6.3+): pick several chips.
        ChipsSelector(ChipsSelector) = "ChipsSelector",
        /// `Footer`: the screen's button.
        Footer(Footer) = "Footer",
        /// `OptIn`: a checkbox with a "Read more" action.
        OptIn(OptIn) = "OptIn",
        /// `EmbeddedLink`: a tappable text.
        EmbeddedLink(EmbeddedLink) = "EmbeddedLink",
        /// `DatePicker`: one date.
        DatePicker(DatePicker) = "DatePicker",
        /// `CalendarPicker` (Flow JSON 6.1+): a date or a range of dates.
        CalendarPicker(CalendarPicker) = "CalendarPicker",
        /// `Image`.
        Image(Image) = "Image",
        /// `ImageCarousel` (Flow JSON 7.1+).
        ImageCarousel(ImageCarousel) = "ImageCarousel",
        /// `If` (Flow JSON 4.0+): render one of two lists of components.
        If(If) = "If",
        /// `Switch` (Flow JSON 4.0+): render the list of components keyed
        /// by a value.
        Switch(Switch) = "Switch",
        /// `NavigationList` (Flow JSON 6.2+): a list of items leading to
        /// other screens.
        NavigationList(NavigationList) = "NavigationList",
        /// `PhotoPicker` (Flow JSON 4.0+): upload photos.
        PhotoPicker(PhotoPicker) = "PhotoPicker",
        /// `DocumentPicker` (Flow JSON 4.0+): upload documents.
        DocumentPicker(DocumentPicker) = "DocumentPicker",
        /// `Form`: the input container, required around inputs before Flow
        /// JSON 4.0 and optional since.
        Form(Form) = "Form",
    }
}

wire_enum! {
    /// `font-weight` of `TextBody` and `TextCaption`.
    pub enum FontWeight {
        /// `bold`.
        Bold = "bold",
        /// `italic`.
        Italic = "italic",
        /// `bold_italic`.
        BoldItalic = "bold_italic",
        /// `normal`.
        Normal = "normal",
    }
}

wire_enum! {
    /// `input-type` of `TextInput`.
    pub enum InputType {
        /// `text`.
        Text = "text",
        /// `number`.
        Number = "number",
        /// `email`.
        Email = "email",
        /// `password`.
        Password = "password",
        /// `passcode`.
        Passcode = "passcode",
        /// `phone`.
        Phone = "phone",
    }
}

wire_enum! {
    /// `label-variant` of `TextInput` and `TextArea` (Flow JSON 7.0+, named
    /// in the changelog).
    pub enum LabelVariant {
        /// `large`: the label above the field, up to four lines.
        Large = "large",
    }
}

wire_enum! {
    /// `media-size` of the selection components and `NavigationList`.
    pub enum MediaSize {
        /// `regular`.
        Regular = "regular",
        /// `large`.
        Large = "large",
    }
}

wire_enum! {
    /// `mode` of `CalendarPicker`.
    pub enum CalendarMode {
        /// `single`: one date.
        Single = "single",
        /// `range`: a start and an end date.
        Range = "range",
    }
}

wire_enum! {
    /// A day of `CalendarPicker`'s `include-days`.
    pub enum Weekday {
        /// `Mon`.
        Mon = "Mon",
        /// `Tue`.
        Tue = "Tue",
        /// `Wed`.
        Wed = "Wed",
        /// `Thu`.
        Thu = "Thu",
        /// `Fri`.
        Fri = "Fri",
        /// `Sat`.
        Sat = "Sat",
        /// `Sun`.
        Sun = "Sun",
    }
}

wire_enum! {
    /// `scale-type` of `Image` and `ImageCarousel`.
    pub enum ScaleType {
        /// `cover`: clipped to fill the container.
        Cover = "cover",
        /// `contain`: fitted inside it.
        Contain = "contain",
    }
}

wire_enum! {
    /// `photo-source` of `PhotoPicker`.
    pub enum PhotoSource {
        /// `camera_gallery`: either (the default).
        CameraGallery = "camera_gallery",
        /// `camera`: take a photo only.
        Camera = "camera",
        /// `gallery`: pick from the gallery only.
        Gallery = "gallery",
    }
}

flow_object! {
    /// `TextHeading`. At most 80 characters, not blank.
    pub struct TextHeading {
        required {
            /// `text`.
            text: String = "text",
        }
        optional {
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
        }
    }
}

flow_object! {
    /// `TextSubheading`. At most 80 characters, not blank.
    pub struct TextSubheading {
        required {
            /// `text`.
            text: String = "text",
        }
        optional {
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
        }
    }
}

flow_object! {
    /// `TextBody`. At most 4096 characters, not blank.
    pub struct TextBody {
        required {
            /// `text`: a string, or a list of them with `markdown`.
            text: TextContent = "text",
        }
        optional {
            /// `font-weight`.
            font_weight: Dynamic<FontWeight> = "font-weight",
            /// `strikethrough`.
            strikethrough: Dynamic<bool> = "strikethrough",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `markdown` (Flow JSON 5.1+, default false).
            markdown: bool = "markdown",
        }
    }
}

flow_object! {
    /// `TextCaption`. At most 409 characters, not blank.
    pub struct TextCaption {
        required {
            /// `text`: a string, or a list of them with `markdown`.
            text: TextContent = "text",
        }
        optional {
            /// `font-weight`.
            font_weight: Dynamic<FontWeight> = "font-weight",
            /// `strikethrough`.
            strikethrough: Dynamic<bool> = "strikethrough",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `markdown` (Flow JSON 5.1+, default false).
            markdown: bool = "markdown",
        }
    }
}

flow_object! {
    /// `RichText` (Flow JSON 5.1+). Until 6.2 it must be alone on its
    /// screen; from 6.3 a `Footer` may join it.
    pub struct RichText {
        required {
            /// `text`: Markdown, one string or a list of lines.
            text: TextContent = "text",
        }
        optional {
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
        }
    }
}

flow_object! {
    /// `TextInput`. Label at most 20 characters, helper text 80, error 30.
    pub struct TextInput {
        required {
            /// `name`: the key of its value in `${form.…}`.
            name: String = "name",
            /// `label`.
            label: String = "label",
        }
        optional {
            /// `input-type`.
            input_type: InputType = "input-type",
            /// `pattern` (Flow JSON 6.2+): a raw regular expression; needs
            /// `helper-text`.
            pattern: String = "pattern",
            /// `required`.
            required: Dynamic<bool> = "required",
            /// `min-chars`.
            min_chars: Dynamic<u32> = "min-chars",
            /// `max-chars` (default 80).
            max_chars: Dynamic<u32> = "max-chars",
            /// `helper-text`.
            helper_text: String = "helper-text",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `init-value` (Flow JSON 4.0+, outside a `Form` only).
            init_value: String = "init-value",
            /// `error-message` (Flow JSON 4.0+, outside a `Form` only).
            error_message: String = "error-message",
            /// `label-variant` (Flow JSON 7.0+).
            label_variant: LabelVariant = "label-variant",
        }
    }
}

flow_object! {
    /// `TextArea`. Label at most 20 characters, helper text 80.
    pub struct TextArea {
        required {
            /// `name`.
            name: String = "name",
            /// `label`.
            label: String = "label",
        }
        optional {
            /// `required`.
            required: Dynamic<bool> = "required",
            /// `max-length` (default 600).
            max_length: Dynamic<u32> = "max-length",
            /// `helper-text`.
            helper_text: String = "helper-text",
            /// `enabled`.
            enabled: Dynamic<bool> = "enabled",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `init-value` (Flow JSON 4.0+, outside a `Form` only).
            init_value: String = "init-value",
            /// `error-message` (Flow JSON 4.0+, outside a `Form` only).
            error_message: String = "error-message",
            /// `label-variant` (Flow JSON 7.0+).
            label_variant: LabelVariant = "label-variant",
        }
    }
}

flow_object! {
    /// One option of a `data-source` (`CheckboxGroup`, `RadioButtonsGroup`,
    /// `Dropdown`, `ChipsSelector`).
    pub struct DataSourceItem {
        required {
            /// `id`: the value the selection yields.
            id: String = "id",
            /// `title`.
            title: String = "title",
        }
        optional {
            /// `description`.
            description: String = "description",
            /// `metadata`.
            metadata: String = "metadata",
            /// `enabled`.
            enabled: bool = "enabled",
            /// `image` (Flow JSON 5.0+): base64.
            image: String = "image",
            /// `alt-text` (Flow JSON 5.0+).
            alt_text: String = "alt-text",
            /// `color` (Flow JSON 5.0+): a six-digit hex colour.
            color: String = "color",
            /// `on-select-action` (Flow JSON 6.0+): `update_data`.
            on_select_action: Action = "on-select-action",
            /// `on-unselect-action` (Flow JSON 6.0+): `update_data`.
            on_unselect_action: Action = "on-unselect-action",
        }
    }
}

flow_object! {
    /// `CheckboxGroup`. Label at most 30 characters; 1 to 20 options, each
    /// title 30, description 300, metadata 20.
    pub struct CheckboxGroup {
        required {
            /// `name`.
            name: String = "name",
            /// `data-source`: the options, or a reference to them.
            data_source: Dynamic<Vec<DataSourceItem>> = "data-source",
        }
        optional {
            /// `label` (required from Flow JSON 4.0).
            label: String = "label",
            /// `min-selected-items`.
            min_selected_items: Dynamic<u32> = "min-selected-items",
            /// `max-selected-items`.
            max_selected_items: Dynamic<u32> = "max-selected-items",
            /// `enabled`.
            enabled: Dynamic<bool> = "enabled",
            /// `required`.
            required: Dynamic<bool> = "required",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `on-select-action`: `data_exchange`, or `update_data` (6.0+).
            on_select_action: Action = "on-select-action",
            /// `description` (Flow JSON 4.0+).
            description: String = "description",
            /// `init-value` (Flow JSON 4.0+, outside a `Form` only).
            init_value: Dynamic<Vec<String>> = "init-value",
            /// `error-message` (Flow JSON 4.0+, outside a `Form` only).
            error_message: String = "error-message",
            /// `media-size` (Flow JSON 5.0+).
            media_size: Dynamic<MediaSize> = "media-size",
        }
    }
}

/// A property Meta documents both as one string and as a list of them
/// (`RadioButtonsGroup`'s `init-value`): either parses, and serializes back
/// as it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    /// One string (a value, or a reference such as `${data.selected}`).
    One(String),
    /// A list of strings.
    Many(Vec<String>),
}

impl From<&str> for OneOrMany {
    fn from(value: &str) -> Self {
        Self::One(value.to_owned())
    }
}

impl From<String> for OneOrMany {
    fn from(value: String) -> Self {
        Self::One(value)
    }
}

impl From<Vec<String>> for OneOrMany {
    fn from(value: Vec<String>) -> Self {
        Self::Many(value)
    }
}

flow_object! {
    /// `RadioButtonsGroup`. Label at most 30 characters; 1 to 20 options,
    /// each title 30, description 300, metadata 20.
    pub struct RadioButtonsGroup {
        required {
            /// `name`.
            name: String = "name",
            /// `data-source`: the options, or a reference to them.
            data_source: Dynamic<Vec<DataSourceItem>> = "data-source",
        }
        optional {
            /// `label` (required from Flow JSON 4.0).
            label: String = "label",
            /// `enabled`.
            enabled: Dynamic<bool> = "enabled",
            /// `required`.
            required: Dynamic<bool> = "required",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `on-select-action`: `data_exchange`, or `update_data` (6.0+).
            on_select_action: Action = "on-select-action",
            /// `description` (Flow JSON 4.0+).
            description: String = "description",
            /// `init-value` (Flow JSON 4.0+, outside a `Form` only): the
            /// selected option's id, or a reference. The component table
            /// says an array of strings, the Flow JSON guide's `init-values`
            /// table a string: both parse, and come back as sent.
            init_value: OneOrMany = "init-value",
            /// `error-message` (Flow JSON 4.0+, outside a `Form` only).
            error_message: String = "error-message",
            /// `media-size` (Flow JSON 5.0+).
            media_size: Dynamic<MediaSize> = "media-size",
        }
    }
}

flow_object! {
    /// `Dropdown`. Label at most 20 characters; 1 to 200 options (100 when
    /// any has an image), each title 30, description 300, metadata 20.
    pub struct Dropdown {
        required {
            /// `name`.
            name: String = "name",
            /// `label`.
            label: String = "label",
            /// `data-source`: the options, or a reference to them.
            data_source: Dynamic<Vec<DataSourceItem>> = "data-source",
        }
        optional {
            /// `required`.
            required: Dynamic<bool> = "required",
            /// `enabled`.
            enabled: Dynamic<bool> = "enabled",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `on-select-action`: `data_exchange`, or `update_data` (6.0+).
            on_select_action: Action = "on-select-action",
            /// `init-value` (outside a `Form` only).
            init_value: String = "init-value",
            /// `error-message` (outside a `Form` only).
            error_message: String = "error-message",
        }
    }
}

flow_object! {
    /// `ChipsSelector` (Flow JSON 6.3+). Label at most 80 characters,
    /// description 300; 2 to 20 options.
    pub struct ChipsSelector {
        required {
            /// `name`.
            name: String = "name",
            /// `label`.
            label: String = "label",
            /// `data-source`: the options, or a reference to them.
            data_source: Dynamic<Vec<DataSourceItem>> = "data-source",
        }
        optional {
            /// `min-selected-items`.
            min_selected_items: Dynamic<u32> = "min-selected-items",
            /// `max-selected-items`.
            max_selected_items: Dynamic<u32> = "max-selected-items",
            /// `enabled`.
            enabled: Dynamic<bool> = "enabled",
            /// `required`.
            required: Dynamic<bool> = "required",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `description`.
            description: String = "description",
            /// `init-value` (outside a `Form` only).
            init_value: Dynamic<Vec<String>> = "init-value",
            /// `error-message` (outside a `Form` only).
            error_message: String = "error-message",
            /// `on-select-action` (Flow JSON 7.1+ at the component's root).
            on_select_action: Action = "on-select-action",
            /// `on-unselect-action` (Flow JSON 7.1+ at the component's
            /// root); without it `on-select-action` handles both.
            on_unselect_action: Action = "on-unselect-action",
        }
    }
}

flow_object! {
    /// `Footer`. Label at most 35 characters, captions 15; either
    /// `center-caption` alone or `left-caption` with `right-caption`. One
    /// per screen; a terminal screen needs one.
    pub struct Footer {
        required {
            /// `label`.
            label: String = "label",
            /// `on-click-action`.
            on_click_action: Action = "on-click-action",
        }
        optional {
            /// `left-caption`.
            left_caption: String = "left-caption",
            /// `center-caption`.
            center_caption: String = "center-caption",
            /// `right-caption`.
            right_caption: String = "right-caption",
            /// `enabled`.
            enabled: Dynamic<bool> = "enabled",
        }
    }
}

flow_object! {
    /// `OptIn`. Label at most 120 characters; at most 5 per screen.
    pub struct OptIn {
        required {
            /// `name`.
            name: String = "name",
            /// `label`.
            label: String = "label",
        }
        optional {
            /// `required`.
            required: Dynamic<bool> = "required",
            /// `on-click-action` ("Read more"): `data_exchange`,
            /// `navigate`, or `open_url` (6.0+).
            on_click_action: Action = "on-click-action",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `init-value` (Flow JSON 4.0+, outside a `Form` only).
            init_value: Dynamic<bool> = "init-value",
        }
    }
}

flow_object! {
    /// `EmbeddedLink`. Text at most 25 characters, not blank; at most 2 per
    /// screen.
    pub struct EmbeddedLink {
        required {
            /// `text`.
            text: String = "text",
            /// `on-click-action`: `data_exchange`, `navigate`, or `open_url`
            /// (6.0+).
            on_click_action: Action = "on-click-action",
        }
        optional {
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
        }
    }
}

flow_object! {
    /// `DatePicker`. Label at most 40 characters, helper text and error 80.
    /// From Flow JSON 5.0 dates are `YYYY-MM-DD`; before, timestamps in
    /// milliseconds as strings.
    pub struct DatePicker {
        required {
            /// `name`.
            name: String = "name",
            /// `label`.
            label: String = "label",
        }
        optional {
            /// `min-date`.
            min_date: String = "min-date",
            /// `max-date`.
            max_date: String = "max-date",
            /// `unavailable-dates`.
            unavailable_dates: Dynamic<Vec<String>> = "unavailable-dates",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `helper-text`.
            helper_text: String = "helper-text",
            /// `enabled` (default true).
            enabled: Dynamic<bool> = "enabled",
            /// `on-select-action`: `data_exchange` only.
            on_select_action: Action = "on-select-action",
            /// `init-value` (Flow JSON 4.0+, outside a `Form` only).
            init_value: String = "init-value",
            /// `error-message` (Flow JSON 4.0+, outside a `Form` only).
            error_message: String = "error-message",
        }
    }
}

/// A `CalendarPicker` property that takes one value in `single` mode and a
/// `{"start-date": …, "end-date": …}` pair in `range` mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CalendarValue<T> {
    /// The `single` mode value.
    Single(T),
    /// The `range` mode pair.
    Range(DateRange<T>),
}

/// `{"start-date": …, "end-date": …}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateRange<T> {
    /// `start-date`.
    #[serde(rename = "start-date")]
    pub start_date: T,
    /// `end-date`.
    #[serde(rename = "end-date")]
    pub end_date: T,
    /// Properties this version does not model, kept as sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl<T> DateRange<T> {
    /// The pair `start`, `end`.
    pub fn new(start: impl Into<T>, end: impl Into<T>) -> Self {
        Self {
            start_date: start.into(),
            end_date: end.into(),
            extra: Map::new(),
        }
    }
}

impl From<&str> for CalendarValue<String> {
    fn from(value: &str) -> Self {
        Self::Single(value.to_owned())
    }
}

impl From<String> for CalendarValue<String> {
    fn from(value: String) -> Self {
        Self::Single(value)
    }
}

impl From<bool> for CalendarValue<Dynamic<bool>> {
    fn from(value: bool) -> Self {
        Self::Single(Dynamic::Static(value))
    }
}

impl<T> From<DateRange<T>> for CalendarValue<T> {
    fn from(value: DateRange<T>) -> Self {
        Self::Range(value)
    }
}

flow_object! {
    /// `CalendarPicker` (Flow JSON 6.1+). Title at most 80 characters,
    /// description 300, label 40, helper text and error 80. Dates are
    /// `YYYY-MM-DD`.
    pub struct CalendarPicker {
        required {
            /// `name`.
            name: String = "name",
            /// `label` (a pair in `range` mode).
            label: CalendarValue<String> = "label",
        }
        optional {
            /// `title` (`range` mode only).
            title: String = "title",
            /// `description` (`range` mode only).
            description: String = "description",
            /// `helper-text` (a pair in `range` mode).
            helper_text: CalendarValue<String> = "helper-text",
            /// `required` (default false; a pair in `range` mode).
            required: CalendarValue<Dynamic<bool>> = "required",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `enabled` (default true).
            enabled: Dynamic<bool> = "enabled",
            /// `mode` (default `single`).
            mode: Dynamic<CalendarMode> = "mode",
            /// `min-date`.
            min_date: String = "min-date",
            /// `max-date`.
            max_date: String = "max-date",
            /// `unavailable-dates`.
            unavailable_dates: Dynamic<Vec<String>> = "unavailable-dates",
            /// `include-days` (default every day).
            include_days: Dynamic<Vec<Weekday>> = "include-days",
            /// `min-days` (`range` mode only).
            min_days: Dynamic<u32> = "min-days",
            /// `max-days` (`range` mode only).
            max_days: Dynamic<u32> = "max-days",
            /// `on-select-action`: `data_exchange` only.
            on_select_action: Action = "on-select-action",
            /// `init-value` (a pair in `range` mode; outside a `Form` only).
            init_value: CalendarValue<String> = "init-value",
            /// `error-message` (a pair in `range` mode; outside a `Form`
            /// only).
            error_message: CalendarValue<String> = "error-message",
        }
    }
}

flow_object! {
    /// `Image`. At most 3 per screen; JPEG or PNG, base64.
    pub struct Image {
        required {
            /// `src`: base64 of the image, or a reference.
            src: String = "src",
        }
        optional {
            /// `width`.
            width: Dynamic<u32> = "width",
            /// `height`.
            height: Dynamic<u32> = "height",
            /// `scale-type` (default `contain`).
            scale_type: ScaleType = "scale-type",
            /// `aspect-ratio` (default 1).
            aspect_ratio: Dynamic<Number> = "aspect-ratio",
            /// `alt-text`.
            alt_text: String = "alt-text",
        }
    }
}

flow_object! {
    /// One image of an `ImageCarousel`.
    pub struct CarouselImage {
        required {
            /// `src`: base64 of the image.
            src: String = "src",
            /// `alt-text`.
            alt_text: String = "alt-text",
        }
        optional {}
    }
}

flow_object! {
    /// `ImageCarousel` (Flow JSON 7.1+). 1 to 3 images; at most 2 per
    /// screen and 3 per Flow.
    pub struct ImageCarousel {
        required {
            /// `images`, or a reference to them.
            images: Dynamic<Vec<CarouselImage>> = "images",
        }
        optional {
            /// `aspect-ratio`: `4:3` (default) or `16:9`.
            aspect_ratio: String = "aspect-ratio",
            /// `scale-type` (default `contain`).
            scale_type: ScaleType = "scale-type",
        }
    }
}

flow_object! {
    /// `If` (Flow JSON 4.0+). `then` is not empty; a `Footer` in one branch
    /// needs one in the other; nested at most 3 deep.
    pub struct If {
        required {
            /// `condition`: a boolean expression with at least one reference.
            condition: String = "condition",
            /// `then`: rendered when the condition holds.
            then: Vec<Component> = "then",
        }
        optional {
            /// `else`: rendered otherwise.
            else_: Vec<Component> = "else",
        }
    }
}

flow_object! {
    /// `Switch` (Flow JSON 4.0+). `cases` is not empty.
    pub struct Switch {
        required {
            /// `value`: evaluated at run time, usually a reference.
            value: String = "value",
            /// `cases`: the components rendered for each value.
            cases: BTreeMap<String, Vec<Component>> = "cases",
        }
        optional {}
    }
}

flow_object! {
    /// The `main-content` of a `NavigationList` item.
    pub struct NavigationItemMainContent {
        required {
            /// `title`.
            title: String = "title",
        }
        optional {
            /// `description`.
            description: String = "description",
            /// `metadata`.
            metadata: String = "metadata",
        }
    }
}

flow_object! {
    /// The `end` add-on of a `NavigationList` item.
    pub struct NavigationItemEnd {
        required {}
        optional {
            /// `title`.
            title: String = "title",
            /// `description`.
            description: String = "description",
            /// `metadata`.
            metadata: String = "metadata",
        }
    }
}

impl Default for NavigationItemEnd {
    fn default() -> Self {
        Self::new()
    }
}

flow_object! {
    /// The `start` add-on of a `NavigationList` item.
    pub struct NavigationItemStart {
        required {
            /// `image`: base64.
            image: String = "image",
        }
        optional {
            /// `alt-text`.
            alt_text: String = "alt-text",
        }
    }
}

flow_object! {
    /// One item of a `NavigationList`.
    pub struct NavigationItem {
        required {
            /// `main-content`.
            main_content: NavigationItemMainContent = "main-content",
        }
        optional {
            /// `id` (named in the guide's dynamic example).
            id: String = "id",
            /// `end`.
            end: NavigationItemEnd = "end",
            /// `start`.
            start: NavigationItemStart = "start",
            /// `badge` (one item per list at most).
            badge: String = "badge",
            /// `tags`.
            tags: Vec<String> = "tags",
            /// `on-click-action`: `data_exchange` or `navigate`; not with a
            /// component-level one.
            on_click_action: Action = "on-click-action",
        }
    }
}

flow_object! {
    /// `NavigationList` (Flow JSON 6.2+). Alone on its screen (two at most),
    /// not on a terminal screen.
    pub struct NavigationList {
        required {
            /// `name`.
            name: String = "name",
            /// `list-items`, or a reference to them.
            list_items: Dynamic<Vec<NavigationItem>> = "list-items",
        }
        optional {
            /// `label`.
            label: String = "label",
            /// `description`.
            description: String = "description",
            /// `media-size` (default `regular`).
            media_size: Dynamic<MediaSize> = "media-size",
            /// `on-click-action`: `data_exchange` or `navigate`, for every
            /// item (or set one on each item instead).
            on_click_action: Action = "on-click-action",
        }
    }
}

/// `error-message` of the media pickers: one message, or one per media id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MediaErrorMessage {
    /// One message for the component (or a reference).
    Text(String),
    /// A message per media id.
    PerMedia(BTreeMap<String, String>),
}

impl From<&str> for MediaErrorMessage {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

flow_object! {
    /// `PhotoPicker` (Flow JSON 4.0+). Label at most 80 characters,
    /// description 300. One per screen, and not with a `DocumentPicker`.
    pub struct PhotoPicker {
        required {
            /// `name`.
            name: String = "name",
            /// `label`.
            label: String = "label",
        }
        optional {
            /// `description`.
            description: String = "description",
            /// `photo-source` (default `camera_gallery`).
            photo_source: PhotoSource = "photo-source",
            /// `max-file-size-kb`: 1 to 25600 (the default).
            max_file_size_kb: u32 = "max-file-size-kb",
            /// `min-uploaded-photos`: 0 (the default) to 30.
            min_uploaded_photos: u32 = "min-uploaded-photos",
            /// `max-uploaded-photos`: 1 to 30 (the default).
            max_uploaded_photos: u32 = "max-uploaded-photos",
            /// `enabled` (default true).
            enabled: Dynamic<bool> = "enabled",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `error-message`.
            error_message: MediaErrorMessage = "error-message",
        }
    }
}

flow_object! {
    /// `DocumentPicker` (Flow JSON 4.0+). Label at most 80 characters,
    /// description 300. One per screen, and not with a `PhotoPicker`.
    pub struct DocumentPicker {
        required {
            /// `name`.
            name: String = "name",
            /// `label`.
            label: String = "label",
        }
        optional {
            /// `description`.
            description: String = "description",
            /// `max-file-size-kb`: 1 to 25600 (the default).
            max_file_size_kb: u32 = "max-file-size-kb",
            /// `min-uploaded-documents`: 0 (the default) to 30.
            min_uploaded_documents: u32 = "min-uploaded-documents",
            /// `max-uploaded-documents`: 1 to 30 (the default).
            max_uploaded_documents: u32 = "max-uploaded-documents",
            /// `allowed-mime-types`.
            allowed_mime_types: Vec<String> = "allowed-mime-types",
            /// `enabled` (default true).
            enabled: Dynamic<bool> = "enabled",
            /// `visible` (default true).
            visible: Dynamic<bool> = "visible",
            /// `error-message`.
            error_message: MediaErrorMessage = "error-message",
        }
    }
}

flow_object! {
    /// `Form`: wraps inputs (required before Flow JSON 4.0).
    pub struct Form {
        required {
            /// `name`.
            name: String = "name",
            /// `children`.
            children: Vec<Component> = "children",
        }
        optional {
            /// `init-values`: input name to initial value, or a reference.
            init_values: Dynamic<Map<String, Value>> = "init-values",
            /// `error-messages`: input name to message, or a reference.
            error_messages: Dynamic<Map<String, Value>> = "error-messages",
        }
    }
}
