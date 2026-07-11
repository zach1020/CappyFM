use std::{
    collections::HashMap,
    fmt,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::warn;

const MIN_SCRIPT_WORDS: usize = 70;
const MAX_SCRIPT_WORDS: usize = 110;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoicePreset {
    LateNight,
    Radio,
    Cozy,
    Hype,
    Dry,
}

impl VoicePreset {
    pub const ALL: [Self; 5] = [
        Self::LateNight,
        Self::Radio,
        Self::Cozy,
        Self::Hype,
        Self::Dry,
    ];

    pub fn provider_voice(self) -> &'static str {
        match self {
            Self::LateNight => "onyx",
            Self::Radio => "cedar",
            Self::Cozy => "shimmer",
            Self::Hype => "coral",
            Self::Dry => "echo",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::LateNight => "smooth, warm, restrained",
            Self::Radio => "bright, traditional FM delivery",
            Self::Cozy => "soft and suited to study sessions",
            Self::Hype => "energetic without shouting",
            Self::Dry => "deadpan comedy delivery",
        }
    }
}

impl fmt::Display for VoicePreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LateNight => "late-night",
            Self::Radio => "radio",
            Self::Cozy => "cozy",
            Self::Hype => "hype",
            Self::Dry => "dry",
        })
    }
}

impl FromStr for VoicePreset {
    type Err = DjError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "late-night" | "latenight" => Ok(Self::LateNight),
            "radio" => Ok(Self::Radio),
            "cozy" => Ok(Self::Cozy),
            "hype" => Ok(Self::Hype),
            "dry" => Ok(Self::Dry),
            _ => Err(DjError::InvalidSetting),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonalityLevel {
    Chill,
    Quirky,
    Unhinged,
}

impl fmt::Display for PersonalityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Chill => "chill",
            Self::Quirky => "quirky",
            Self::Unhinged => "unhinged",
        })
    }
}

impl FromStr for PersonalityLevel {
    type Err = DjError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "chill" => Ok(Self::Chill),
            "quirky" => Ok(Self::Quirky),
            "unhinged" => Ok(Self::Unhinged),
            _ => Err(DjError::InvalidSetting),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TalkFrequency {
    Off,
    Less,
    Normal,
    More,
}

impl TalkFrequency {
    fn gap(self, sequence: usize) -> usize {
        match self {
            Self::Off => usize::MAX,
            Self::Less => 5 + (sequence % 3),
            Self::Normal => 2 + (sequence % 3),
            Self::More => 2 + (sequence % 2),
        }
    }
}

impl fmt::Display for TalkFrequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Less => "less",
            Self::Normal => "normal",
            Self::More => "more",
        })
    }
}

impl FromStr for TalkFrequency {
    type Err = DjError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "on" | "normal" => Ok(Self::Normal),
            "less" => Ok(Self::Less),
            "more" => Ok(Self::More),
            _ => Err(DjError::InvalidSetting),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DjSessionSettings {
    pub voice: VoicePreset,
    pub personality: PersonalityLevel,
    pub frequency: TalkFrequency,
    pub tracks_since_segment: usize,
    pub segments_spoken: usize,
    pub opening_pending: bool,
    pub tts_backoff_until: Option<Instant>,
}

impl Default for DjSessionSettings {
    fn default() -> Self {
        Self {
            voice: VoicePreset::LateNight,
            personality: PersonalityLevel::Quirky,
            frequency: TalkFrequency::Normal,
            tracks_since_segment: 0,
            segments_spoken: 0,
            opening_pending: true,
            tts_backoff_until: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DjContext<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub requester: &'a str,
    pub previous_track: Option<&'a str>,
    pub session_opening: bool,
    pub radio_session: bool,
    pub personality: PersonalityLevel,
}

#[derive(Debug, Clone)]
pub struct DjSegment {
    pub script: String,
    pub audio_uri: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TtsRequest {
    pub text: String,
    pub voice_preset: VoicePreset,
}

#[derive(Debug, Clone)]
pub struct TtsAudio {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

#[derive(Debug, Clone)]
pub struct VoiceDescriptor {
    pub preset: VoicePreset,
    pub provider_voice: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Error)]
pub enum TtsError {
    #[error("TTS is disabled")]
    Disabled,
    #[error("TTS request failed: {0}")]
    Request(String),
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    async fn synthesize(&self, request: TtsRequest) -> Result<TtsAudio, TtsError>;
    fn available_voices(&self) -> Vec<VoiceDescriptor>;
}

pub struct DisabledTtsProvider;

#[async_trait]
impl TtsProvider for DisabledTtsProvider {
    async fn synthesize(&self, _request: TtsRequest) -> Result<TtsAudio, TtsError> {
        Err(TtsError::Disabled)
    }

    fn available_voices(&self) -> Vec<VoiceDescriptor> {
        voice_descriptors()
    }
}

pub struct OpenAiTtsProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
}

impl OpenAiTtsProvider {
    pub fn new(api_key: String, model: String) -> Result<Self, TtsError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| TtsError::Request(error.to_string()))?;
        Ok(Self {
            client,
            api_key,
            model,
        })
    }
}

#[async_trait]
impl TtsProvider for OpenAiTtsProvider {
    async fn synthesize(&self, request: TtsRequest) -> Result<TtsAudio, TtsError> {
        let response = self
            .client
            .post("https://api.openai.com/v1/audio/speech")
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "input": request.text,
                "voice": request.voice_preset.provider_voice(),
                "response_format": "mp3",
                "speed": 1.0
            }))
            .send()
            .await
            .map_err(|error| TtsError::Request(error.to_string()))?;
        if response.status() != StatusCode::OK {
            return Err(TtsError::Request(format!("HTTP {}", response.status())));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| TtsError::Request(error.to_string()))?;
        Ok(TtsAudio {
            bytes: bytes.to_vec(),
            content_type: "audio/mpeg",
        })
    }

    fn available_voices(&self) -> Vec<VoiceDescriptor> {
        voice_descriptors()
    }
}

#[async_trait]
trait DjWriter: Send + Sync {
    async fn write(&self, context: &DjContext<'_>) -> Result<String, DjError>;
}

struct TemplateWriter;

#[async_trait]
impl DjWriter for TemplateWriter {
    async fn write(&self, context: &DjContext<'_>) -> Result<String, DjError> {
        Ok(template_script(context))
    }
}

struct OpenAiWriter {
    client: reqwest::Client,
    api_key: String,
    model: String,
}

#[async_trait]
impl DjWriter for OpenAiWriter {
    async fn write(&self, context: &DjContext<'_>) -> Result<String, DjError> {
        let prompt = format!(
            "Segment: {}\nPrevious track: {}\nTrack: {} by {}\nRequester: {}\nPersonality: {}",
            if context.session_opening && context.radio_session {
                "radio session opening; explicitly say this is a radio session"
            } else if context.session_opening {
                "session opening"
            } else if context.radio_session {
                "radio transition"
            } else {
                "requested track intro"
            },
            context.previous_track.unwrap_or("not supplied"),
            context.title,
            context.artist,
            context.requester,
            context.personality
        );
        let response = self
            .client
            .post("https://api.openai.com/v1/responses")
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "instructions": "You write radio-DJ copy as Cappy, a quirky capybara music host. Talk only about the supplied song, artist, previous track, request, or a playful music-related tangent. If a sourced fact is not supplied, do not invent one. Never discuss Discord, an app, volume, controls, the bot, the queue, system behavior, or future commentary. Never mention or imply access to conversation, and do not claim to hear the room. Avoid repeating the same framing, promises, or catchphrases. Be warm, musically literate, and willing to become amusingly unhinged when the personality calls for it. Vary structure and pacing. Write 70 to 110 words, use at most one capybara joke, and no emojis. Return only the spoken script.",
                "input": prompt,
                "max_output_tokens": 240
            }))
            .send()
            .await
            .map_err(|error| DjError::Writer(error.to_string()))?;
        if !response.status().is_success() {
            return Err(DjError::Writer(format!("HTTP {}", response.status())));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|error| DjError::Writer(error.to_string()))?;
        body.get("output")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.iter().find_map(|item| item.get("content")))
            .and_then(serde_json::Value::as_array)
            .and_then(|content| content.iter().find_map(|item| item.get("text")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| DjError::Writer("response did not contain text".to_owned()))
    }
}

#[derive(Clone, Default)]
pub struct AudioCache(Arc<RwLock<HashMap<String, TtsAudio>>>);

impl AudioCache {
    pub async fn get(&self, key: &str) -> Option<TtsAudio> {
        self.0.read().await.get(key).cloned()
    }

    async fn insert(&self, key: String, audio: TtsAudio) {
        self.0.write().await.insert(key, audio);
    }
}

#[derive(Clone)]
pub struct DjService {
    writer: Arc<dyn DjWriter>,
    tts: Arc<dyn TtsProvider>,
    sessions: Arc<RwLock<HashMap<u64, DjSessionSettings>>>,
    cache: AudioCache,
    public_audio_base: String,
}

impl DjService {
    pub fn from_env(public_audio_base: String) -> Self {
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|key| !key.is_empty());
        let writer: Arc<dyn DjWriter> = match api_key.as_ref() {
            Some(key) => Arc::new(OpenAiWriter {
                client: reqwest::Client::builder()
                    .timeout(Duration::from_secs(8))
                    .build()
                    .unwrap_or_else(|_| unreachable!("static HTTP client configuration")),
                api_key: key.clone(),
                model: std::env::var("OPENAI_TEXT_MODEL")
                    .unwrap_or_else(|_| "gpt-5.4-nano".to_owned()),
            }),
            None => Arc::new(TemplateWriter),
        };
        let tts: Arc<dyn TtsProvider> = match api_key {
            Some(key) => Arc::new(
                OpenAiTtsProvider::new(
                    key,
                    std::env::var("OPENAI_TTS_MODEL").unwrap_or_else(|_| "tts-1".to_owned()),
                )
                .unwrap_or_else(|_| unreachable!("static HTTP client configuration")),
            ),
            None => Arc::new(DisabledTtsProvider),
        };
        Self {
            writer,
            tts,
            sessions: Arc::default(),
            cache: AudioCache::default(),
            public_audio_base: public_audio_base.trim_end_matches('/').to_owned(),
        }
    }

    pub fn audio_cache(&self) -> AudioCache {
        self.cache.clone()
    }

    pub async fn settings(&self, guild_id: u64) -> DjSessionSettings {
        self.sessions
            .read()
            .await
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn set_voice(&self, guild_id: u64, voice: VoicePreset) {
        self.session_mut(guild_id).await.voice = voice;
    }

    pub async fn set_personality(&self, guild_id: u64, personality: PersonalityLevel) {
        self.session_mut(guild_id).await.personality = personality;
    }

    pub async fn set_frequency(&self, guild_id: u64, frequency: TalkFrequency) {
        self.session_mut(guild_id).await.frequency = frequency;
    }

    pub async fn mark_session_ended(&self, guild_id: u64) {
        let mut session = self.session_mut(guild_id).await;
        session.tracks_since_segment = 0;
        session.segments_spoken = 0;
        session.opening_pending = true;
    }

    async fn session_mut(
        &self,
        guild_id: u64,
    ) -> tokio::sync::RwLockMappedWriteGuard<'_, DjSessionSettings> {
        tokio::sync::RwLockWriteGuard::map(self.sessions.write().await, |sessions| {
            sessions.entry(guild_id).or_default()
        })
    }

    pub async fn create_intro(
        &self,
        guild_id: u64,
        mut context: DjContext<'_>,
        force: bool,
    ) -> Result<Option<DjSegment>, DjError> {
        let settings = self.settings(guild_id).await;
        if settings.frequency == TalkFrequency::Off {
            return Ok(None);
        }
        if !force {
            context.session_opening |= settings.opening_pending;
        }
        if !force
            && !context.session_opening
            && settings.tracks_since_segment < settings.frequency.gap(settings.segments_spoken)
        {
            self.session_mut(guild_id).await.tracks_since_segment += 1;
            return Ok(None);
        }

        let candidate = self
            .writer
            .write(&context)
            .await
            .unwrap_or_else(|_| template_script(&context));
        let script = if validate_script(&candidate).is_ok() {
            candidate
        } else {
            template_script(&context)
        };
        validate_script(&script)?;
        let key = cache_key(settings.voice, &script);
        let tts_backoff_active = settings
            .tts_backoff_until
            .is_some_and(|until| Instant::now() < until);
        let audio_uri = if self.cache.get(&key).await.is_some() {
            Some(format!("{}/audio/{}.mp3", self.public_audio_base, key))
        } else if tts_backoff_active {
            None
        } else {
            match self
                .tts
                .synthesize(TtsRequest {
                    text: script.clone(),
                    voice_preset: settings.voice,
                })
                .await
            {
                Ok(audio) => {
                    self.cache.insert(key.clone(), audio).await;
                    self.session_mut(guild_id).await.tts_backoff_until = None;
                    Some(format!("{}/audio/{}.mp3", self.public_audio_base, key))
                }
                Err(error) => {
                    warn!(error = %error, "DJ speech synthesis failed");
                    self.session_mut(guild_id).await.tts_backoff_until =
                        Some(Instant::now() + Duration::from_secs(60));
                    None
                }
            }
        };
        {
            let mut session = self.session_mut(guild_id).await;
            session.tracks_since_segment = 0;
            session.segments_spoken += 1;
            if audio_uri.is_some() {
                session.opening_pending = false;
            }
        }
        Ok(Some(DjSegment { script, audio_uri }))
    }
}

fn template_script(context: &DjContext<'_>) -> String {
    if context.session_opening && context.radio_session {
        format!(
            "This radio session begins with {} by {}. The selected vibe has pointed us here, so this track gets the first word and the responsibility of drawing the musical map. There is no invented biography hiding behind the curtain, only the title, the artist, and the direction chosen for this station. Consider the signal officially live. The opening selection may now establish the atmosphere, make its argument, and decide what sort of strange musical neighborhood we have entered.",
            context.title, context.artist,
        )
    } else if context.session_opening {
        format!(
            "We begin with {} by {}, requested by {}. A first song carries a peculiar responsibility: it has to open the door, establish the atmosphere, and convince every song waiting behind it that the night has standards. This one has accepted the assignment without filing an appeal. The title is on the marquee, the artist has the floor, and all unnecessary speeches have been escorted away. Let the opening selection make its own introduction from here.",
            context.title, context.artist, context.requester,
        )
    } else {
        let recap = context
            .previous_track
            .map(|track| format!("Coming off {track}. "))
            .unwrap_or_default();
        let lead = if context.radio_session {
            format!(
                "Radio now turns toward {} by {}.",
                context.title, context.artist
            )
        } else {
            format!(
                "Next comes {} by {}, requested by {}.",
                context.title, context.artist, context.requester
            )
        };
        match context.personality {
            PersonalityLevel::Chill => format!(
                "{recap}{lead} The previous selection gets a moment to leave its outline behind while this title steps into focus. There is no need to manufacture a grand theory about the connection; sometimes two songs simply meet at the border and exchange a quiet nod. That is enough. Let the new track establish its own shape, choose its own pace, and carry this stretch of listening wherever it intends to go, without asking the transition to explain more than the music itself can say.",
            ),
            PersonalityLevel::Quirky => format!(
                "{recap}{lead} The title has arrived wearing the expression of someone who knows exactly why it was invited, which is more confidence than most of us bring to a Tuesday. No imaginary statistics or suspicious folklore are required here. We have an artist, a song, and a perfectly respectable musical handoff. The capybara has examined the paperwork, stamped it with one damp paw, and declared this selection ready to become the entire point for the next few minutes.",
            ),
            PersonalityLevel::Unhinged => format!(
                "{recap}{lead} The title has entered the building like it owns several legally questionable fog machines. Nobody panic. The song has credentials, the artist has been named, and the transition ritual may proceed beneath the ancient laws of rhythm and extremely confident pointing. I have released one ceremonial capybara into the imaginary control room as a witness. It understands nothing about audio engineering, but its posture is impeccable. Enough bureaucracy. Let the music commence before the paperwork develops consciousness and demands a producer credit.",
            ),
        }
    }
}

pub fn validate_script(script: &str) -> Result<(), DjError> {
    let word_count = script.split_whitespace().count();
    if word_count < MIN_SCRIPT_WORDS {
        return Err(DjError::ScriptTooShort);
    }
    if word_count > MAX_SCRIPT_WORDS {
        return Err(DjError::ScriptTooLong);
    }
    let lowercase = script.to_ascii_lowercase();
    if [
        "as an ai",
        "discord conversation",
        "discord",
        "i heard you",
        "monthly listeners",
        "volume",
        "the bot",
        "this app",
        "the queue",
    ]
    .iter()
    .any(|phrase| lowercase.contains(phrase))
    {
        return Err(DjError::ProhibitedClaim);
    }
    Ok(())
}

fn cache_key(voice: VoicePreset, script: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(voice.to_string());
    hash.update([0]);
    hash.update(script);
    hex::encode(hash.finalize())
}

fn voice_descriptors() -> Vec<VoiceDescriptor> {
    VoicePreset::ALL
        .into_iter()
        .map(|preset| VoiceDescriptor {
            preset,
            provider_voice: preset.provider_voice(),
            description: preset.description(),
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum DjError {
    #[error("invalid DJ setting")]
    InvalidSetting,
    #[error("DJ script is shorter than 70 words")]
    ScriptTooShort,
    #[error("DJ script exceeds 110 words")]
    ScriptTooLong,
    #[error("DJ script contains a prohibited claim")]
    ProhibitedClaim,
    #[error("DJ writer failed: {0}")]
    Writer(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticTtsProvider;

    #[async_trait]
    impl TtsProvider for StaticTtsProvider {
        async fn synthesize(&self, _request: TtsRequest) -> Result<TtsAudio, TtsError> {
            Ok(TtsAudio {
                bytes: vec![1, 2, 3],
                content_type: "audio/mpeg",
            })
        }

        fn available_voices(&self) -> Vec<VoiceDescriptor> {
            voice_descriptors()
        }
    }

    fn test_service() -> DjService {
        DjService {
            writer: Arc::new(TemplateWriter),
            tts: Arc::new(DisabledTtsProvider),
            sessions: Arc::default(),
            cache: AudioCache::default(),
            public_audio_base: "http://bot:8080".to_owned(),
        }
    }

    fn test_service_with_audio() -> DjService {
        DjService {
            writer: Arc::new(TemplateWriter),
            tts: Arc::new(StaticTtsProvider),
            sessions: Arc::default(),
            cache: AudioCache::default(),
            public_audio_base: "http://bot:8080".to_owned(),
        }
    }

    #[test]
    fn validator_enforces_conversational_length_range() {
        assert!(validate_script(&vec!["word"; 69].join(" ")).is_err());
        assert!(validate_script(&vec!["word"; 70].join(" ")).is_ok());
        assert!(validate_script(&vec!["word"; 110].join(" ")).is_ok());
        assert!(validate_script(&vec!["word"; 111].join(" ")).is_err());
    }

    #[test]
    fn validator_rejects_privacy_and_unsupported_claims() {
        assert!(validate_script("I analyzed your Discord conversation.").is_err());
        assert!(validate_script("I heard you asking for this one.").is_err());
    }

    #[test]
    fn all_product_voice_presets_are_available() {
        assert_eq!(VoicePreset::ALL.len(), 5);
        assert!(
            VoicePreset::ALL
                .iter()
                .all(|preset| !preset.provider_voice().is_empty())
        );
    }

    #[tokio::test]
    async fn shutup_is_scoped_to_one_session() {
        let service = test_service();
        service.set_frequency(1, TalkFrequency::Off).await;
        assert_eq!(service.settings(1).await.frequency, TalkFrequency::Off);
        assert_eq!(service.settings(2).await.frequency, TalkFrequency::Normal);
    }

    #[test]
    fn normal_frequency_rotates_between_two_and_four_songs() {
        let gaps: Vec<_> = (0..6)
            .map(|sequence| TalkFrequency::Normal.gap(sequence))
            .collect();
        assert_eq!(gaps, vec![2, 3, 4, 2, 3, 4]);
    }

    #[tokio::test]
    async fn session_opening_always_speaks_when_enabled() {
        let service = test_service();
        let segment = service
            .create_intro(
                1,
                DjContext {
                    title: "Song",
                    artist: "Artist",
                    requester: "listener",
                    previous_track: None,
                    session_opening: true,
                    radio_session: false,
                    personality: PersonalityLevel::Quirky,
                },
                false,
            )
            .await
            .expect("opening should succeed");
        assert!(segment.is_some());
        assert!(service.settings(1).await.tts_backoff_until.is_some());
    }

    #[tokio::test]
    async fn ended_session_makes_the_next_track_an_opening() {
        let service = test_service_with_audio();
        let context = || DjContext {
            title: "Song",
            artist: "Artist",
            requester: "listener",
            previous_track: None,
            session_opening: false,
            radio_session: false,
            personality: PersonalityLevel::Quirky,
        };
        service
            .create_intro(1, context(), false)
            .await
            .expect("first opening should succeed");
        assert!(!service.settings(1).await.opening_pending);
        service.mark_session_ended(1).await;
        assert!(service.settings(1).await.opening_pending);
        assert!(
            service
                .create_intro(1, context(), false)
                .await
                .expect("reopened session should succeed")
                .is_some()
        );
    }

    #[test]
    fn template_can_recap_supplied_playback_context() {
        let script = template_script(&DjContext {
            title: "Next Song",
            artist: "Next Artist",
            requester: "listener",
            previous_track: Some("Last Song by Last Artist"),
            session_opening: false,
            radio_session: false,
            personality: PersonalityLevel::Chill,
        });
        assert!(script.contains("Coming off Last Song by Last Artist"));
        assert!(validate_script(&script).is_ok());
    }

    #[test]
    fn radio_opening_identifies_the_session_and_transition_stays_music_focused() {
        let opening = template_script(&DjContext {
            title: "Opening Song",
            artist: "Opening Artist",
            requester: "radio",
            previous_track: None,
            session_opening: true,
            radio_session: true,
            personality: PersonalityLevel::Quirky,
        });
        assert!(opening.to_ascii_lowercase().contains("radio session"));
        assert!(validate_script(&opening).is_ok());

        let transition = template_script(&DjContext {
            title: "Next Song",
            artist: "Next Artist",
            requester: "radio",
            previous_track: Some("Previous Song by Previous Artist"),
            session_opening: false,
            radio_session: true,
            personality: PersonalityLevel::Unhinged,
        });
        assert!(!transition.contains("requested by radio"));
        assert!(validate_script(&transition).is_ok());
    }
}
