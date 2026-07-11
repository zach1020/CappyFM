use std::fmt;

pub const PRIVACY_RESPONSE: &str = "🔒 CappyFM does not monitor Discord conversation. It immediately ignores messages that do not start with `cappy!` or `cap!`. It stores only music-related activity such as requested tracks, likes, skips, session settings, and listening history. Ordinary chat is never sent to AI.";

pub const HELP_RESPONSE: &str = "**CappyFM** — the capybara has the aux.\n\n`cap!play <URL or search>` — YouTube, SoundCloud, Spotify, or Apple Music\n`cap!queue` — show up next\n`cap!now` — show the current track\n`cap!pause` / `cap!resume` — control playback\n`cap!skip` — skip the current track\n`cap!stop` — stop and clear the queue\n`cap!leave` — disconnect\n`cap!privacy` — explain the privacy boundary\n\nSpotify and Apple Music provide metadata; playback is matched to YouTube. Both prefixes work.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandName {
    Help,
    Privacy,
    Play,
    Queue,
    Skip,
    Stop,
    Now,
    Pause,
    Resume,
    Leave,
    Unknown,
}

impl fmt::Display for CommandName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Help => "help",
            Self::Privacy => "privacy",
            Self::Play => "play",
            Self::Queue => "queue",
            Self::Skip => "skip",
            Self::Stop => "stop",
            Self::Now => "now",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Leave => "leave",
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
        let name = match raw_name.to_ascii_lowercase().as_str() {
            "help" => CommandName::Help,
            "privacy" => CommandName::Privacy,
            "play" => CommandName::Play,
            "queue" => CommandName::Queue,
            "skip" => CommandName::Skip,
            "stop" => CommandName::Stop,
            "now" | "np" => CommandName::Now,
            "pause" => CommandName::Pause,
            "resume" => CommandName::Resume,
            "leave" => CommandName::Leave,
            _ => CommandName::Unknown,
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
        assert_eq!(parsed.name, CommandName::Play);
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
