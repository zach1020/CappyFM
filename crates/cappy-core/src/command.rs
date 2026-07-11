use std::fmt;

pub const PRIVACY_RESPONSE: &str = "🔒 CappyFM does not monitor Discord conversation. It immediately ignores messages that do not start with `cappy!` or `cap!`. It stores only music-related activity such as requested tracks, likes, skips, session settings, and listening history. Ordinary chat is never sent to AI.";

pub const HELP_RESPONSE: &str = "**CappyFM** — the capybara has the aux.\n\nFor now:\n`cap!help` — show this message\n`cap!privacy` — explain the privacy boundary\n\nBoth `cap!` and `cappy!` work. Playback commands are coming in the next milestone.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandName {
    Help,
    Privacy,
    Unknown,
}

impl fmt::Display for CommandName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Help => "help",
            Self::Privacy => "privacy",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand<'a> {
    pub name: CommandName,
    pub arguments: &'a str,
}

#[derive(Debug, Clone)]
pub struct PrefixParser {
    prefixes: Vec<String>,
}

impl PrefixParser {
    pub fn new(prefixes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut prefixes: Vec<String> = prefixes
            .into_iter()
            .map(Into::into)
            .map(|prefix| prefix.to_lowercase())
            .collect();
        prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
        Self { prefixes }
    }

    /// Returns immediately for ordinary chat. Callers must not log or retain `content`
    /// before this privacy gate has accepted it.
    pub fn parse<'a>(&self, content: &'a str) -> Option<ParsedCommand<'a>> {
        let prefix_len = self.prefixes.iter().find_map(|prefix| {
            content
                .get(..prefix.len())
                .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
                .map(str::len)
        })?;

        let command_text = content.get(prefix_len..)?.trim_start();
        let split_at = command_text
            .find(char::is_whitespace)
            .unwrap_or(command_text.len());
        let raw_name = &command_text[..split_at];
        let arguments = command_text[split_at..].trim_start();
        let name = if raw_name.eq_ignore_ascii_case("help") {
            CommandName::Help
        } else if raw_name.eq_ignore_ascii_case("privacy") {
            CommandName::Privacy
        } else {
            CommandName::Unknown
        };

        Some(ParsedCommand { name, arguments })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> PrefixParser {
        PrefixParser::new(["cappy!", "cap!"])
    }

    #[test]
    fn both_prefixes_parse_identically() {
        assert_eq!(parser().parse("cap!help"), parser().parse("cappy!help"));
    }

    #[test]
    fn prefix_and_command_are_case_insensitive() {
        assert_eq!(parser().parse("CaP!HeLp").unwrap().name, CommandName::Help);
    }

    #[test]
    fn optional_whitespace_and_original_argument_case_are_preserved() {
        let parsed = parser().parse("cap!   play Burial Archangel").unwrap();
        assert_eq!(parsed.name, CommandName::Unknown);
        assert_eq!(parsed.arguments, "Burial Archangel");
    }

    #[test]
    fn ordinary_chat_is_rejected_at_the_first_gate() {
        let sensitive_chat = "I had a difficult day and this is private";
        assert!(parser().parse(sensitive_chat).is_none());
    }

    #[test]
    fn similar_but_invalid_prefix_is_rejected() {
        assert!(parser().parse("capy!help").is_none());
    }
}
