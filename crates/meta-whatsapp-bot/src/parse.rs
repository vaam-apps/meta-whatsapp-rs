//! [`CommandParser`]: which text is a command. The default is
//! [`PrefixParser`] (`/name args`, `!name args`, …).

use std::fmt;
use std::sync::Arc;

use crate::command::Args;

/// A text message read as a command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParsedCommand {
    /// The prefix matched (empty for a parser without one).
    pub prefix: String,
    /// The command name as the parser gives it (the default
    /// [`PrefixParser`] lowercases it); the bot looks it up as is.
    pub name: String,
    /// The arguments.
    pub args: Args,
}

impl ParsedCommand {
    /// A parsed command.
    pub fn new(prefix: impl Into<String>, name: impl Into<String>, args: Args) -> Self {
        Self {
            prefix: prefix.into(),
            name: name.into(),
            args,
        }
    }
}

/// Decides which text messages are commands, splits them, and decides how
/// names compare. Replace [`PrefixParser`] for another syntax.
///
/// The bot compares names exactly as the parser gives them: a typed name
/// from [`Self::parse`], a registered name or alias from
/// [`Self::normalize`]. A parser that folds case in one must fold it in
/// the other.
pub trait CommandParser: Send + Sync + fmt::Debug + 'static {
    /// `Some` when `text` is a command. The bot then looks the name up; an
    /// unknown name makes the message a plain one (listeners get it, or
    /// the unknown-command handler when there is one).
    fn parse(&self, text: &str) -> Option<ParsedCommand>;

    /// The prefix the help text writes before command names.
    fn help_prefix(&self) -> &str;

    /// The form a registered name or alias is matched in, the same form
    /// [`Self::parse`] gives a typed one. The default keeps it as written
    /// (case-sensitive names).
    fn normalize(&self, name: &str) -> String {
        name.to_owned()
    }
}

impl<T: CommandParser + ?Sized> CommandParser for Arc<T> {
    fn parse(&self, text: &str) -> Option<ParsedCommand> {
        (**self).parse(text)
    }

    fn help_prefix(&self) -> &str {
        (**self).help_prefix()
    }

    fn normalize(&self, name: &str) -> String {
        (**self).normalize(name)
    }
}

/// Commands are `<prefix><name> <args…>`: a prefix (default `/`, the one
/// Meta's command menu sends), the name up to the first whitespace, then
/// [`Args`]. Leading whitespace is ignored; a prefix with nothing after it
/// is not a command. With several prefixes the longest that matches wins.
///
/// Names are case-insensitive by default: typed and registered names are
/// both lowercased. [`Self::ignore_case`] with `false` keeps them as
/// written.
///
/// An empty prefix makes the first word of every text message a candidate
/// command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixParser {
    prefixes: Vec<String>,
    ignore_case: bool,
}

impl Default for PrefixParser {
    fn default() -> Self {
        Self::new(["/"])
    }
}

impl PrefixParser {
    /// Accept each of `prefixes`, e.g. `["/", "!"]`. The first one given is
    /// the one the help text shows. Names are case-insensitive.
    pub fn new<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let prefixes: Vec<String> = prefixes.into_iter().map(Into::into).collect();
        Self {
            prefixes,
            ignore_case: true,
        }
    }

    /// Whether names compare case-insensitively (default: yes).
    #[must_use]
    pub fn ignore_case(mut self, ignore_case: bool) -> Self {
        self.ignore_case = ignore_case;
        self
    }

    /// The prefixes, in the order given.
    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }
}

impl CommandParser for PrefixParser {
    fn parse(&self, text: &str) -> Option<ParsedCommand> {
        let text = text.trim_start();
        let prefix = self
            .prefixes
            .iter()
            .filter(|p| text.starts_with(p.as_str()))
            .max_by_key(|p| p.len())?;
        let rest = &text[prefix.len()..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() {
            return None;
        }
        Some(ParsedCommand::new(
            prefix.clone(),
            self.normalize(name),
            Args::parse(&rest[end..]),
        ))
    }

    fn help_prefix(&self) -> &str {
        self.prefixes.first().map_or("", String::as_str)
    }

    fn normalize(&self, name: &str) -> String {
        if self.ignore_case {
            name.to_lowercase()
        } else {
            name.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_names_and_arguments() {
        let p = PrefixParser::new(["/", "!", "!!"]);
        let c = p.parse("/imagine cars racing on Mars").unwrap();
        assert_eq!((c.prefix.as_str(), c.name.as_str()), ("/", "imagine"));
        assert_eq!(c.args.as_slice(), ["cars", "racing", "on", "Mars"]);
        // The longest prefix wins; the name is lowercased.
        let c = p.parse("!!Ping").unwrap();
        assert_eq!((c.prefix.as_str(), c.name.as_str()), ("!!", "ping"));
        assert_eq!(p.parse("  !ping").unwrap().name, "ping");
        assert!(p.parse("/").is_none());
        assert!(p.parse("/ ping").is_none());
        assert!(p.parse("ping").is_none());
        assert!(p.parse("").is_none());
        assert_eq!(p.help_prefix(), "/");
        assert_eq!(p.normalize("PiNg"), "ping");
    }

    /// A parser of its own keeps names as written unless it says so.
    #[derive(Debug)]
    struct Exact;

    impl CommandParser for Exact {
        fn parse(&self, text: &str) -> Option<ParsedCommand> {
            Some(ParsedCommand::new("", text, Args::default()))
        }
        fn help_prefix(&self) -> &'static str {
            ""
        }
    }

    #[test]
    fn case_folding_is_the_parsers_option() {
        let p = PrefixParser::new(["/"]).ignore_case(false);
        assert_eq!(p.parse("/Ping x").unwrap().name, "Ping");
        assert_eq!(p.normalize("Ping"), "Ping");
        assert_eq!(Exact.normalize("Ping"), "Ping");
        assert_eq!(Arc::new(Exact).normalize("Ping"), "Ping");
    }

    #[test]
    fn an_empty_prefix_makes_the_first_word_a_candidate() {
        let p = PrefixParser::new([""]);
        let c = p.parse("menu today").unwrap();
        assert_eq!((c.name.as_str(), c.args.raw()), ("menu", "today"));
    }

    #[test]
    fn a_multibyte_prefix_splits_on_char_boundaries() {
        let p = PrefixParser::new(["¿"]);
        let c = p.parse("¿ayuda ya").unwrap();
        assert_eq!((c.name.as_str(), c.args.raw()), ("ayuda", "ya"));
    }
}
