//! The `calling` object of `GET`/`POST /{phone-number-id}/settings`.
//!
//! Docs: `calling/call-settings` (the full object, call hours, audio,
//! voicemail, restrictions), `calling/reference#configure-or-update-calling-settings`,
//! and `calling/sip` for the `sip` sub-object that page links to.
//!
//! One type serves both directions so a `GET` → edit → `POST` round trip
//! keeps every value, including values this crate does not know yet (every
//! enum has an `Other(String)` catch-all). Response-only data (restrictions,
//! the SIP password, the app id of a SIP server) is never serialized back.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{Date, Month, OffsetDateTime};
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::MediaId;

/// Most holiday overrides in `call_hours.holiday_schedule`.
pub const MAX_HOLIDAY_OVERRIDES: usize = 20;
/// Most opening windows per weekday (and per holiday date).
pub const MAX_WINDOWS_PER_DAY: usize = 2;
/// Longest voicemail `timeout_seconds`.
pub const VOICEMAIL_TIMEOUT_MAX_SECONDS: u8 = 30;
/// Most SIP servers per phone number ("Each phone number can have only one
/// SIP server configured", `calling/sip`).
pub const MAX_SIP_SERVERS: usize = 1;
/// Longest key or value in `request_uri_user_params`, in characters.
pub const SIP_URI_PARAM_MAX_CHARS: usize = 128;

/// Calling API settings of a business phone number. Every field is
/// optional: in a `POST`, a `None` field is left as it is; in a `GET`, it
/// was not returned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallingSettings {
    /// Calling on or off for the number. Calling needs a messaging limit of
    /// at least 2,000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<FeatureStatus>,
    /// Whether WhatsApp shows the call button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_icon_visibility: Option<CallIconVisibility>,
    /// Countries whose users see the call button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_icons: Option<CallIcons>,
    /// Opening hours for user-initiated calls. A `POST` replaces the whole
    /// object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_hours: Option<CallHours>,
    /// Whether callers are prompted to grant call permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_permission_status: Option<FeatureStatus>,
    /// SIP signalling instead of Graph calls/webhooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sip: Option<SipSettings>,
    /// Extra audio codecs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioSettings>,
    /// Voicemail for missed or rejected user-initiated calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voicemail: Option<Voicemail>,
    /// Restrictions Meta imposed after negative feedback (response only).
    #[serde(default, skip_serializing)]
    pub restrictions: Option<CallingRestrictions>,
}

impl CallingSettings {
    /// Check the documented limits before a `POST`.
    pub(super) fn validate(&self) -> Result<()> {
        if let Some(hours) = &self.call_hours {
            hours.validate()?;
        }
        if let Some(sip) = &self.sip {
            sip.validate()?;
        }
        if let Some(voicemail) = &self.voicemail {
            voicemail.validate()?;
        }
        Ok(())
    }
}

/// `ENABLED` / `DISABLED`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FeatureStatus {
    /// `ENABLED`.
    Enabled,
    /// `DISABLED`.
    Disabled,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `call_icon_visibility`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CallIconVisibility {
    /// `DEFAULT`: call button shown, unsolicited calls allowed.
    Default,
    /// `DISABLE_ALL`: call button and external entry points hidden; users
    /// cannot place unsolicited calls. Call-button messages still work.
    DisableAll,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `call_icons`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallIcons {
    /// Show the call button only to users with phone numbers registered in
    /// these countries. `Some(vec![])` sends `[]`: no restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restrict_to_user_countries: Option<Vec<String>>,
}

/// `call_hours`. Outside these hours users are offered to chat or request a
/// callback instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHours {
    /// When disabled the business counts as open 24/7.
    pub status: FeatureStatus,
    /// IANA-style timezone id, e.g. `America/Manaus`.
    pub timezone_id: String,
    /// Weekly windows: at least one; at most two per day, not overlapping,
    /// each opening before it closes.
    #[serde(default)]
    pub weekly_operating_hours: Vec<WeeklyHours>,
    /// Up to 20 overrides. Leaving it out of a `POST` **deletes** the
    /// existing holiday schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holiday_schedule: Option<Vec<HolidayHours>>,
}

impl CallHours {
    fn validate(&self) -> Result<()> {
        if self.weekly_operating_hours.is_empty() {
            return Err(ValidationError::new(
                "calling.call_hours.weekly_operating_hours",
                "must not be empty",
            )
            .into());
        }
        let weekly: Vec<_> = self
            .weekly_operating_hours
            .iter()
            .map(|w| (w.day_of_week.clone(), w.open_time, w.close_time))
            .collect();
        validate_windows("calling.call_hours.weekly_operating_hours", &weekly)?;
        if let Some(holidays) = &self.holiday_schedule {
            let field = "calling.call_hours.holiday_schedule";
            if holidays.len() > MAX_HOLIDAY_OVERRIDES {
                return Err(ValidationError::new(
                    field,
                    format!(
                        "at most {MAX_HOLIDAY_OVERRIDES} overrides, got {}",
                        holidays.len()
                    ),
                )
                .into());
            }
            let windows: Vec<_> = holidays
                .iter()
                .map(|h| (h.date, h.start_time, h.end_time))
                .collect();
            validate_windows(field, &windows)?;
        }
        Ok(())
    }
}

/// Each window opens before it closes; per key (weekday or date) at most
/// two windows, and they do not overlap (touching is fine).
fn validate_windows<K: PartialEq + fmt::Debug>(
    field: &str,
    windows: &[(K, ClockTime, ClockTime)],
) -> Result<()> {
    for (i, (key, open, close)) in windows.iter().enumerate() {
        if open >= close {
            return Err(ValidationError::new(
                format!("{field}[{i}]"),
                format!("opens at {open} but closes at {close}; it must open first"),
            )
            .into());
        }
        let same_key: Vec<_> = windows.iter().filter(|(k, _, _)| k == key).collect();
        if same_key.len() > MAX_WINDOWS_PER_DAY {
            return Err(ValidationError::new(
                field,
                format!("at most {MAX_WINDOWS_PER_DAY} windows for {key:?}"),
            )
            .into());
        }
        for (j, (other_key, other_open, other_close)) in windows.iter().enumerate() {
            if j != i && other_key == key && open < other_close && other_open < close {
                return Err(ValidationError::new(
                    format!("{field}[{i}]"),
                    format!("overlaps entry {j} on {key:?}"),
                )
                .into());
            }
        }
    }
    Ok(())
}

/// An entry of `weekly_operating_hours`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeeklyHours {
    /// Day.
    pub day_of_week: DayOfWeek,
    /// Opening time.
    pub open_time: ClockTime,
    /// Closing time.
    pub close_time: ClockTime,
}

/// An entry of `holiday_schedule`. The guide's table calls the times
/// `open_time`/`close_time`; every example uses `start_time`/`end_time`,
/// which is what is sent (example wins, see the `meta-docs` skill).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayHours {
    /// The day overridden, `YYYY-MM-DD` on the wire.
    #[serde(with = "ymd")]
    pub date: Date,
    /// Opening time.
    pub start_time: ClockTime,
    /// Closing time.
    pub end_time: ClockTime,
}

/// Day of the week.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DayOfWeek {
    /// `MONDAY`.
    Monday,
    /// `TUESDAY`.
    Tuesday,
    /// `WEDNESDAY`.
    Wednesday,
    /// `THURSDAY`.
    Thursday,
    /// `FRIDAY`.
    Friday,
    /// `SATURDAY`.
    Saturday,
    /// `SUNDAY`.
    Sunday,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// A time of day in call hours, `"HHMM"` on the wire (`"1130"` = 11:30).
///
/// One sample on the reference page writes `"04:00"` and the tables type
/// the field as an integer, so `"HH:MM"` and a number (`400`) are accepted
/// when reading; `"HHMM"` is always written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClockTime {
    hour: u8,
    minute: u8,
}

impl ClockTime {
    /// A time of day; `hour` 0–23, `minute` 0–59.
    pub fn new(hour: u8, minute: u8) -> Result<Self> {
        if hour > 23 || minute > 59 {
            return Err(ValidationError::new(
                "time",
                format!("{hour:02}:{minute:02} is not a time of day"),
            )
            .into());
        }
        Ok(Self { hour, minute })
    }

    /// Hour, 0–23.
    pub fn hour(self) -> u8 {
        self.hour
    }

    /// Minute, 0–59.
    pub fn minute(self) -> u8 {
        self.minute
    }

    fn parse(s: &str) -> Option<Self> {
        let digits: String = s.trim().chars().filter(|c| *c != ':').collect();
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let hour = digits[..2].parse().ok()?;
        let minute = digits[2..].parse().ok()?;
        Self::new(hour, minute).ok()
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}{:02}", self.hour, self.minute)
    }
}

impl Serialize for ClockTime {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Num(u16),
            Str(String),
        }
        let text = match Raw::deserialize(d)? {
            Raw::Num(n) => format!("{n:04}"),
            Raw::Str(s) => s,
        };
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid time of day `{text}`")))
    }
}

/// `YYYY-MM-DD` for [`HolidayHours::date`].
mod ymd {
    use super::{Date, Deserialize, Deserializer, Month, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `with` contract passes `&T`
    pub(super) fn serialize<S: Serializer>(d: &Date, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&format_args!(
            "{:04}-{:02}-{:02}",
            d.year(),
            u8::from(d.month()),
            d.day()
        ))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Date, D::Error> {
        let s = String::deserialize(d)?;
        parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid YYYY-MM-DD date `{s}`")))
    }

    fn parse(s: &str) -> Option<Date> {
        let mut parts = s.trim().splitn(3, '-');
        let year = parts.next()?.parse::<i32>().ok()?;
        let month = Month::try_from(parts.next()?.parse::<u8>().ok()?).ok()?;
        let day = parts.next()?.parse::<u8>().ok()?;
        Date::from_calendar_date(year, month, day).ok()
    }
}

/// `sip` (`calling/sip`). While SIP is enabled, the Graph calling endpoints
/// and call webhooks stop working for the number.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SipSettings {
    /// SIP on or off (default off). Turning it off keeps `servers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<FeatureStatus>,
    /// Whether call webhooks are delivered for SIP calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_delivery: Option<FeatureStatus>,
    /// At most one server per number (the array is for forward
    /// compatibility). `Some(vec![])` sends `[]`, which deletes the
    /// server your app configured. A `GET` may list one per app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servers: Option<Vec<SipServer>>,
}

impl SipSettings {
    fn validate(&self) -> Result<()> {
        let Some(servers) = &self.servers else {
            return Ok(());
        };
        if servers.len() > MAX_SIP_SERVERS {
            return Err(ValidationError::new(
                "calling.sip.servers",
                format!("at most {MAX_SIP_SERVERS} SIP server per phone number"),
            )
            .into());
        }
        for (i, server) in servers.iter().enumerate() {
            if server.hostname.trim().is_empty() {
                return Err(ValidationError::new(
                    format!("calling.sip.servers[{i}].hostname"),
                    "must not be empty",
                )
                .into());
            }
            for (key, value) in &server.request_uri_user_params {
                if key.chars().count() > SIP_URI_PARAM_MAX_CHARS
                    || value.chars().count() > SIP_URI_PARAM_MAX_CHARS
                {
                    return Err(ValidationError::new(
                        format!("calling.sip.servers[{i}].request_uri_user_params"),
                        format!(
                            "keys and values are at most {SIP_URI_PARAM_MAX_CHARS} characters (`{key}`)"
                        ),
                    )
                    .into());
                }
            }
        }
        Ok(())
    }
}

/// A SIP server. TLS is mandatory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SipServer {
    /// Host name.
    pub hostname: String,
    /// TLS port; Meta's default is 5061.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_opt_u16"
    )]
    pub port: Option<u16>,
    /// Parameters added to the user part of the request URI (e.g. `tgrp`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_uri_user_params: BTreeMap<String, String>,
    /// App that configured this server (response only).
    #[serde(default, skip_serializing, deserialize_with = "lenient_opt_string")]
    pub app_id: Option<String>,
    /// Meta-generated digest password, only with
    /// `include_sip_credentials=true` (response only, redacted in `Debug`).
    #[serde(default, skip_serializing)]
    pub sip_user_password: Option<SipPassword>,
}

/// The SIP digest password Meta generates. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SipPassword(String);

impl SipPassword {
    /// Read the password. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SipPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SipPassword([REDACTED])")
    }
}

/// `audio`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSettings {
    /// Codecs besides Opus (always on). `Some(vec![])` sends `[]`: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_codecs: Option<Vec<AudioCodec>>,
}

/// An additional audio codec.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AudioCodec {
    /// `PCMA` (G.711 A-law).
    Pcma,
    /// `PCMU` (G.711 µ-law).
    Pcmu,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `voicemail`. Needs calling enabled; turn call hours off while using it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voicemail {
    /// On or off (default off).
    pub status: FeatureStatus,
    /// What starts a voicemail; at least one when enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<VoicemailTrigger>,
    /// Announcement; required when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<VoicemailAudio>,
}

impl Voicemail {
    fn validate(&self) -> Result<()> {
        if self.status != FeatureStatus::Enabled {
            return Ok(());
        }
        if self.triggers.is_empty() {
            return Err(ValidationError::new(
                "calling.voicemail.triggers",
                "at least one trigger is required when voicemail is enabled",
            )
            .into());
        }
        let default = self.audio.as_ref().map(|a| &a.default);
        if default
            .and_then(|d| d.announcement_media_id.as_ref())
            .is_none()
        {
            return Err(ValidationError::new(
                "calling.voicemail.audio.default.announcement_media_id",
                "is required when voicemail is enabled",
            )
            .into());
        }
        let timeout = default.and_then(|d| d.timeout_seconds);
        if self.triggers.contains(&VoicemailTrigger::Timeout) && timeout.is_none() {
            return Err(ValidationError::new(
                "calling.voicemail.audio.default.timeout_seconds",
                "is required with the TIMEOUT trigger (without it Meta disables the trigger)",
            )
            .into());
        }
        if timeout.is_some_and(|t| t > VOICEMAIL_TIMEOUT_MAX_SECONDS) {
            return Err(ValidationError::new(
                "calling.voicemail.audio.default.timeout_seconds",
                format!("must be between 0 and {VOICEMAIL_TIMEOUT_MAX_SECONDS}"),
            )
            .into());
        }
        Ok(())
    }
}

/// A voicemail trigger.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VoicemailTrigger {
    /// `REJECT`: you reject the call.
    Reject,
    /// `TIMEOUT`: you neither accept nor reject within `timeout_seconds`.
    Timeout,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `voicemail.audio`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicemailAudio {
    /// Configuration for every user.
    pub default: VoicemailAudioConfig,
}

/// `voicemail.audio.default`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicemailAudioConfig {
    /// Media uploaded with `use_case=call_voicemail_announcement` (OGG/Opus,
    /// under 60 s). Sent as a JSON number, as documented, when it is all
    /// digits.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "media_id_as_number",
        deserialize_with = "lenient_opt_media_id"
    )]
    pub announcement_media_id: Option<MediaId>,
    /// Ring time before voicemail starts, 0–30 s (`TIMEOUT` trigger).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u8>,
}

/// `restrictions` (response only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CallingRestrictions {
    /// Current restrictions. The table names it `restriction_list`, the
    /// example `restrictions_list`; both are read.
    #[serde(default, alias = "restriction_list")]
    pub restrictions_list: Vec<CallingRestriction>,
}

/// One calling restriction.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallingRestriction {
    /// What is restricted.
    #[serde(rename = "type")]
    pub kind: RestrictionType,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// When it lifts.
    #[serde(default, with = "wa_core::timestamp::unix_option")]
    pub expiration: Option<OffsetDateTime>,
}

/// Kind of calling restriction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RestrictionType {
    /// `RESTRICTED_BUSINESS_INITIATED_CALLING`.
    RestrictedBusinessInitiatedCalling,
    /// `RESTRICTED_USER_INITIATED_CALLING`.
    RestrictedUserInitiatedCalling,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

#[allow(clippy::ref_option)] // serde's `serialize_with` contract passes `&Option<T>`
fn media_id_as_number<S: Serializer>(
    id: &Option<MediaId>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match id {
        Some(id) => match id.as_str().parse::<u64>() {
            Ok(n) => s.serialize_u64(n),
            Err(_) => s.serialize_str(id.as_str()),
        },
        None => s.serialize_none(),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StrOrNum {
    Str(String),
    Num(serde_json::Number),
}

impl StrOrNum {
    fn into_string(self) -> String {
        match self {
            Self::Str(s) => s,
            Self::Num(n) => n.to_string(),
        }
    }
}

fn lenient_opt_media_id<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<MediaId>, D::Error> {
    Ok(Option::<StrOrNum>::deserialize(d)?.map(|v| MediaId::new(v.into_string())))
}

fn lenient_opt_string<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Ok(Option::<StrOrNum>::deserialize(d)?.map(StrOrNum::into_string))
}

fn lenient_opt_u16<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<u16>, D::Error> {
    match Option::<StrOrNum>::deserialize(d)? {
        None => Ok(None),
        Some(v) => {
            let s = v.into_string();
            s.trim()
                .parse::<u16>()
                .map(Some)
                .map_err(|_| serde::de::Error::custom(format!("invalid port `{s}`")))
        }
    }
}
