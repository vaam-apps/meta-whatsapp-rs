//! [`CommandParser`]: which text is a command. The default is
//! [`PrefixParser`] (`/name args`, `!name args`, …).

use std::fmt;

use crate::command::Args;

/// A text message read as a command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParsedCommand {
    /// The prefix matched (empty for a parser without one).
    pub prefix: String,
    /// The command name as typed; the bot lowercases it to look it up.
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

/// Decides which text messages are commands and splits them. Replace
/// [`PrefixParser`] for another syntax.
pub trait CommandParser: Send + Sync + fmt::Debug + 'static {
    /// `Some` when `text` is a command. The bot then looks the name up; an
    /// unknown name makes the message a plain one (listeners get it).
    fn parse(&self, text: &str) -> Option<ParsedCommand>;

    /// The prefix the help text writes before command names.
    fn help_prefix(&self) -> &str;
}

/// Commands are `<prefix><name> <args…>`: a prefix (default `/`, the one
/// Meta's command menu sends), the name up to the first whitespace, then
/// [`Args`]. Leading whitespace is ignored; a prefix with nothing after it
/// is not a command. With several prefixes the longest that matches wins.
///
/// An empty prefix makes the first word of every text message a candidate
/// command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixParser {
    /// Longest first.
    prefixes: Vec<String>,
}

impl Default for PrefixParser {
    fn default() -> Self {
        Self::new(["/"])
    }
}

impl PrefixParser {
    /// Accept each of `prefixes`, e.g. `["/", "!"]`. The first one given is
    /// the one the help text shows.
    pub fn new<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let prefixes: Vec<String> = prefixes.into_iter().map(Into::into).collect();
        Self { prefixes }
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
            name,
            Args::parse(&rest[end..]),
        ))
    }

    fn help_prefix(&self) -> &str {
        self.prefixes.first().map_or("", String::as_str)
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
        // The longest prefix wins.
        let c = p.parse("!!Ping").unwrap();
        assert_eq!((c.prefix.as_str(), c.name.as_str()), ("!!", "Ping"));
        assert_eq!(p.parse("  !ping").unwrap().name, "ping");
        assert!(p.parse("/").is_none());
        assert!(p.parse("/ ping").is_none());
        assert!(p.parse("ping").is_none());
        assert!(p.parse("").is_none());
        assert_eq!(p.help_prefix(), "/");
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
