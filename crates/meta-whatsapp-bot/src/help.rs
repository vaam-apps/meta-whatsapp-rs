//! The help text: the visible commands grouped by category
//! ([`HelpSection`]), formatted by a [`HelpFormatter`]. The default,
//! [`CategoryHelp`], writes each category in bold and one line per command.

use std::fmt;
use std::sync::Arc;

use crate::command::CommandInfo;

/// One section of the help text: a category and its visible commands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HelpSection {
    /// The category.
    pub category: String,
    /// Its commands that are not hidden, in registration order, each with
    /// only its subcommands that are not hidden.
    pub commands: Vec<CommandInfo>,
}

impl HelpSection {
    /// A section.
    pub fn new(category: impl Into<String>, commands: Vec<CommandInfo>) -> Self {
        Self {
            category: category.into(),
            commands,
        }
    }
}

/// Formats the help text a help command sends (`BotBuilder::help_command_with`).
pub trait HelpFormatter: Send + Sync + fmt::Debug + 'static {
    /// The text for `sections` (never empty ones), writing `prefix` (the
    /// parser's help prefix) before command names. It is split into
    /// messages at the text limit after.
    fn format(&self, sections: &[HelpSection], prefix: &str) -> String;
}

impl<T: HelpFormatter + ?Sized> HelpFormatter for Arc<T> {
    fn format(&self, sections: &[HelpSection], prefix: &str) -> String {
        (**self).format(sections, prefix)
    }
}

/// The default [`HelpFormatter`]: each category in bold, then one line per
/// command, `/name, /alias usage — description`, each followed by one line
/// per subcommand, `/name sub, /name alias usage — description` (under the
/// command's name, with the subcommand's aliases); a blank line between
/// categories.
#[derive(Debug, Clone, Copy, Default)]
pub struct CategoryHelp;

/// One help line: each of `names` after `prefix`, then the usage and the
/// description of `command`.
fn line(text: &mut String, prefix: &str, names: &[String], command: &CommandInfo) {
    text.push('\n');
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            text.push_str(", ");
        }
        text.push_str(prefix);
        text.push_str(name);
    }
    if let Some(usage) = &command.usage {
        text.push(' ');
        text.push_str(usage);
    }
    if let Some(description) = &command.description {
        text.push_str(" — ");
        text.push_str(description);
    }
}

impl HelpFormatter for CategoryHelp {
    fn format(&self, sections: &[HelpSection], prefix: &str) -> String {
        sections
            .iter()
            .map(|section| {
                let mut text = format!("*{}*", section.category);
                for command in &section.commands {
                    let names: Vec<String> = std::iter::once(&command.name)
                        .chain(&command.aliases)
                        .cloned()
                        .collect();
                    line(&mut text, prefix, &names, command);
                    for sub in &command.subcommands {
                        let names: Vec<String> = std::iter::once(&sub.name)
                            .chain(&sub.aliases)
                            .map(|word| format!("{} {word}", command.name))
                            .collect();
                        line(&mut text, prefix, &names, sub);
                    }
                }
                text
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// The commands that are not hidden, grouped by category in the order
/// categories first appear, each with its subcommands that are not hidden.
pub(crate) fn sections(commands: &[CommandInfo]) -> Vec<HelpSection> {
    let mut sections: Vec<HelpSection> = Vec::new();
    for info in commands.iter().filter(|c| !c.hidden) {
        let mut info = info.clone();
        info.subcommands.retain(|sub| !sub.hidden);
        match sections.iter_mut().find(|s| s.category == info.category) {
            Some(section) => section.commands.push(info),
            None => sections.push(HelpSection::new(info.category.clone(), vec![info])),
        }
    }
    sections
}

/// What a context knows about the bot's commands (empty for a context
/// built by hand).
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    pub(crate) commands: Vec<CommandInfo>,
    pub(crate) help_prefix: String,
}
