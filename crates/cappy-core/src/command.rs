use std::fmt;

pub const PRIVACY_RESPONSE: &str = "🔒 CappyFM does not monitor Discord conversation. It immediately ignores messages that do not start with `cappy!`, `capy!`, or `cap!`. It stores only music-related activity such as requested tracks, likes, skips, session settings, and listening history. Ordinary chat is never sent to AI.";

pub const HELP_RESPONSE: &str = "**CappyFM** — the capybara has the aux.\n\n`cap!play <URL or search>` — queue music\n`cap!queue` / `cap!now` / `cap!requested` — inspect playback\n`cap!remove <position>` / `cap!move <from> <to>` / `cap!shuffle` / `cap!undo`\n`cap!pause` / `cap!resume` / `cap!skip` / `cap!clear` / `cap!stop`\n`cap!radio [vibe|off]` — continuous music radio\n`cap!vibe [description]` / `cap!surprise` / `cap!crate` / `cap!similar`\n`cap!like` / `cap!dislike` / `cap!favorites`\n`cap!why` / `cap!fact` / `cap!history` / `cap!stats`\n`cap!volume <0-100>` — shared output; use Discord User Volume for private levels\n`cap!voice [list|preset|preview preset]` — AI-generated DJ voice\n`cap!personality <chill|quirky|unhinged|roast>`\n`cap!talk <off|on|less|normal|more>` / `cap!shutup`\n`cap!settings` / `cap!health` / `cap!privacy`\n\nUse `cap!help radio`, `cap!help dj`, or `cap!help admin` for focused help. Spotify and Apple Music are metadata sources matched to YouTube playback. The `cap!`, `capy!`, and `cappy!` prefixes all work.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandName {
    Help,
    Privacy,
    Play,
    Queue,
    Skip,
    Stop,
    Clear,
    Remove,
    Move,
    Shuffle,
    Undo,
    Requested,
    Now,
    Pause,
    Resume,
    Leave,
    Volume,
    Voice,
    Personality,
    Talk,
    Shutup,
    Intro,
    Radio,
    Session,
    Vibe,
    Surprise,
    Crate,
    Similar,
    Why,
    Fact,
    Like,
    Dislike,
    Favorites,
    History,
    Stats,
    Memory,
    Settings,
    Health,
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
            Self::Clear => "clear",
            Self::Remove => "remove",
            Self::Move => "move",
            Self::Shuffle => "shuffle",
            Self::Undo => "undo",
            Self::Requested => "requested",
            Self::Now => "now",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Leave => "leave",
            Self::Volume => "volume",
            Self::Voice => "voice",
            Self::Personality => "personality",
            Self::Talk => "talk",
            Self::Shutup => "shutup",
            Self::Intro => "intro",
            Self::Radio => "radio",
            Self::Session => "session",
            Self::Vibe => "vibe",
            Self::Surprise => "surprise",
            Self::Crate => "crate",
            Self::Similar => "similar",
            Self::Why => "why",
            Self::Fact => "fact",
            Self::Like => "like",
            Self::Dislike => "dislike",
            Self::Favorites => "favorites",
            Self::History => "history",
            Self::Stats => "stats",
            Self::Memory => "memory",
            Self::Settings => "settings",
            Self::Health => "health",
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
            "clear" => CommandName::Clear,
            "remove" => CommandName::Remove,
            "move" => CommandName::Move,
            "shuffle" => CommandName::Shuffle,
            "undo" => CommandName::Undo,
            "requested" => CommandName::Requested,
            "now" | "np" => CommandName::Now,
            "pause" => CommandName::Pause,
            "resume" => CommandName::Resume,
            "leave" => CommandName::Leave,
            "volume" => CommandName::Volume,
            "voice" => CommandName::Voice,
            "personality" => CommandName::Personality,
            "talk" => CommandName::Talk,
            "shutup" => CommandName::Shutup,
            "intro" => CommandName::Intro,
            "radio" => CommandName::Radio,
            "session" => CommandName::Session,
            "vibe" => CommandName::Vibe,
            "surprise" => CommandName::Surprise,
            "crate" => CommandName::Crate,
            "similar" => CommandName::Similar,
            "why" => CommandName::Why,
            "fact" => CommandName::Fact,
            "like" => CommandName::Like,
            "dislike" => CommandName::Dislike,
            "favorites" => CommandName::Favorites,
            "history" => CommandName::History,
            "stats" => CommandName::Stats,
            "memory" => CommandName::Memory,
            "settings" | "config" => CommandName::Settings,
            "health" => CommandName::Health,
            _ => CommandName::Unknown,
        };

        Some(ParsedCommand { name, arguments })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> PrefixParser {
        PrefixParser::new(["cappy!", "capy!", "cap!"])
    }

    #[test]
    fn all_prefixes_parse_identically() {
        assert_eq!(parser().parse("cap!help"), parser().parse("cappy!help"));
        assert_eq!(parser().parse("cap!help"), parser().parse("capy!help"));
    }

    #[test]
    fn prefix_and_command_are_case_insensitive() {
        assert_eq!(parser().parse("CaP!HeLp").unwrap().name, CommandName::Help);
    }

    #[test]
    fn clear_command_is_recognized() {
        assert_eq!(
            parser().parse("cap!clear").unwrap().name,
            CommandName::Clear
        );
    }

    #[test]
    fn undo_command_is_recognized() {
        assert_eq!(parser().parse("cap!undo").unwrap().name, CommandName::Undo);
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
        assert!(parser().parse("capyy!help").is_none());
    }
}
