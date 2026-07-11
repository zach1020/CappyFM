use std::{collections::HashMap, fmt, str::FromStr, sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::RwLock;

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
}

impl Default for DjSessionSettings {
    fn default() -> Self {
        Self {
            voice: VoicePreset::LateNight,
            personality: PersonalityLevel::Quirky,
            frequency: TalkFrequency::Normal,
            tracks_since_segment: 0,
            segments_spoken: 0,
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
    gain: f32,
}

impl OpenAiTtsProvider {
    pub fn new(api_key: String, model: String, gain: f32) -> Result<Self, TtsError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| TtsError::Request(error.to_string()))?;
        Ok(Self {
            client,
            api_key,
            model,
            gain: gain.clamp(1.0, 1.5),
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
                "response_format": "pcm",
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
            bytes: pcm_to_wav_with_gain(&bytes, self.gain)?,
            content_type: "audio/wav",
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
            if context.session_opening {
                "session opening"
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
                "instructions": "You write radio-DJ copy for CappyFM, a quirky capybara music host. Use only supplied playback context. Never invent facts. Never mention or imply access to Discord conversation. Do not claim to hear the room. Be warm, witty, musically literate, and conversational. Vary the structure and pacing. Write 70 to 110 words, use at most one capybara joke, and no emojis. Return only the spoken script.",
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
                client: reqwest::Client::new(),
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
                    std::env::var("CAPPY_DJ_GAIN")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1.18),
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
        context: DjContext<'_>,
        force: bool,
    ) -> Result<Option<DjSegment>, DjError> {
        let settings = self.settings(guild_id).await;
        if settings.frequency == TalkFrequency::Off {
            return Ok(None);
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
        let audio_uri = if self.cache.get(&key).await.is_some() {
            Some(format!("{}/audio/{}.wav", self.public_audio_base, key))
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
                    Some(format!("{}/audio/{}.wav", self.public_audio_base, key))
                }
                Err(_) => None,
            }
        };
        {
            let mut session = self.session_mut(guild_id).await;
            session.tracks_since_segment = 0;
            session.segments_spoken += 1;
        }
        Ok(Some(DjSegment { script, audio_uri }))
    }
}

fn template_script(context: &DjContext<'_>) -> String {
    if context.session_opening {
        format!(
            "You're tuned to CappyFM, where the capybara has the aux and the queue is officially awake. We are opening this session with {} by {}, a selection from {}. Settle in, adjust Cappy to a comfortable personal volume in Discord, and let the music do its work. I will check back after a few songs with a recap and the next turn in the queue. For now, paws off the dial and ears on the opening track.",
            context.title, context.artist, context.requester,
        )
    } else {
        let recap = context
            .previous_track
            .map(|track| format!("Coming off {track}. "))
            .unwrap_or_default();
        match context.personality {
            PersonalityLevel::Chill => format!(
                "{recap}That gives us a good place to breathe before the queue moves forward. Up next is {} by {}, requested by {}. No invented trivia, no dramatic weather report, just a clean handoff and a little room for the last track to linger. Get comfortable, let the transition land, and keep the volume where it feels right for you. Cappy will return after a few more songs to take stock of where this session has traveled.",
                context.title, context.artist, context.requester,
            ),
            PersonalityLevel::Quirky => format!(
                "{recap}The queue now pivots toward {} by {}, requested by {}. That is a confident little turn, and the capybara behind the console approves of the trajectory. I am keeping the commentary honest: what played, what is next, and precisely zero facts pulled from a suspiciously damp hat. Let this one take over the room at whatever personal volume suits you. I will resurface after another handful of tracks with the next chapter of our highly organized musical wandering.",
                context.title, context.artist, context.requester,
            ),
            PersonalityLevel::Unhinged => format!(
                "{recap}Now {} has placed {} by {} directly in our path, and retreat is neither necessary nor particularly stylish. The queue has made its decision. The lights are imaginary, the turntables are under strict paw supervision, and the transition is cleared for launch. Keep your own Discord volume civilized while Cappy sends this selection into orbit. I will be back after a few songs to inspect the musical consequences, summarize the journey, and announce whatever excellent decision comes next.",
                context.requester, context.title, context.artist,
            ),
        }
    }
}

fn pcm_to_wav_with_gain(pcm: &[u8], requested_gain: f32) -> Result<Vec<u8>, TtsError> {
    if !pcm.chunks_exact(2).remainder().is_empty() || pcm.len() > (u32::MAX - 44) as usize {
        return Err(TtsError::Request("invalid PCM response".to_owned()));
    }

    let peak = pcm
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]) as i32)
        .map(i32::abs)
        .max()
        .unwrap_or(0);
    let clipping_safe_gain = if peak == 0 {
        requested_gain
    } else {
        (i16::MAX as f32 / peak as f32).min(requested_gain)
    };

    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(pcm.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&24_000_u32.to_le_bytes());
    wav.extend_from_slice(&48_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in pcm.chunks_exact(2) {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as f32;
        let amplified = (value * clipping_safe_gain).round() as i16;
        wav.extend_from_slice(&amplified.to_le_bytes());
    }
    Ok(wav)
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
        "i heard you",
        "monthly listeners",
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

    fn test_service() -> DjService {
        DjService {
            writer: Arc::new(TemplateWriter),
            tts: Arc::new(DisabledTtsProvider),
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
                    personality: PersonalityLevel::Quirky,
                },
                false,
            )
            .await
            .expect("opening should succeed");
        assert!(segment.is_some());
    }

    #[test]
    fn template_can_recap_supplied_playback_context() {
        let script = template_script(&DjContext {
            title: "Next Song",
            artist: "Next Artist",
            requester: "listener",
            previous_track: Some("Last Song by Last Artist"),
            session_opening: false,
            personality: PersonalityLevel::Chill,
        });
        assert!(script.contains("Coming off Last Song by Last Artist"));
        assert!(validate_script(&script).is_ok());
    }

    #[test]
    fn speech_gain_builds_clipping_safe_wav_audio() {
        let pcm = [1_000_i16, -2_000_i16, 30_000_i16]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        let wav = pcm_to_wav_with_gain(&pcm, 1.18).expect("valid PCM");
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let samples = wav[44..]
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        assert!(samples[0] > 1_000);
        assert_eq!(samples[2], i16::MAX);
    }
}
