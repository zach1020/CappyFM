use std::fmt;

use serde::{Deserialize, Serialize};
use strsim::normalized_levenshtein;
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MusicProvider {
    YouTube,
    SoundCloud,
    Spotify,
    AppleMusic,
    Search,
}

impl fmt::Display for MusicProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::YouTube => "YouTube",
            Self::SoundCloud => "SoundCloud",
            Self::Spotify => "Spotify",
            Self::AppleMusic => "Apple Music",
            Self::Search => "YouTube search",
        })
    }
}

impl MusicProvider {
    pub fn database_name(self) -> &'static str {
        match self {
            Self::YouTube | Self::Search => "youtube",
            Self::SoundCloud => "soundcloud",
            Self::Spotify => "spotify",
            Self::AppleMusic => "apple_music",
        }
    }

    pub fn needs_playable_match(self) -> bool {
        matches!(self, Self::Spotify | Self::AppleMusic)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClassificationError {
    #[error("unsupported URL")]
    UnsupportedUrl,
}

pub fn classify_input(input: &str) -> Result<MusicProvider, ClassificationError> {
    if !input.contains("://") {
        return Ok(MusicProvider::Search);
    }
    let url = Url::parse(input).map_err(|_| ClassificationError::UnsupportedUrl)?;
    if url.scheme() != "https" {
        return Err(ClassificationError::UnsupportedUrl);
    }
    let host = url
        .host_str()
        .map(|host| host.to_ascii_lowercase())
        .ok_or(ClassificationError::UnsupportedUrl)?;
    let path = url.path().to_ascii_lowercase();

    if matches!(
        host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
    ) {
        Ok(MusicProvider::YouTube)
    } else if matches!(
        host.as_str(),
        "soundcloud.com" | "www.soundcloud.com" | "on.soundcloud.com"
    ) {
        Ok(MusicProvider::SoundCloud)
    } else if host == "open.spotify.com"
        && ["/track/", "/album/", "/playlist/"]
            .iter()
            .any(|kind| path.contains(kind))
    {
        Ok(MusicProvider::Spotify)
    } else if host == "music.apple.com"
        && ["/album/", "/playlist/"]
            .iter()
            .any(|kind| path.contains(kind))
    {
        Ok(MusicProvider::AppleMusic)
    } else {
        Err(ClassificationError::UnsupportedUrl)
    }
}

#[derive(Debug, Clone)]
pub struct CandidateMetadata<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: Option<&'a str>,
    pub duration_ms: Option<u64>,
    pub isrc: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentVersion {
    Explicit,
    Clean,
    Unknown,
}

/// Spotify and Apple Music matching defaults to explicit unless the source
/// metadata clearly identifies a clean/edit version.
pub fn preferred_content_version(
    provider: MusicProvider,
    title: &str,
    album: Option<&str>,
) -> ContentVersion {
    let combined = format!("{title} {}", album.unwrap_or_default());
    match infer_content_version(&combined) {
        ContentVersion::Unknown if provider.needs_playable_match() => ContentVersion::Explicit,
        version => version,
    }
}

pub fn infer_content_version(value: &str) -> ContentVersion {
    let normalized = normalize(value);
    if ["clean", "radio edit", "edited", "censored"]
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        ContentVersion::Clean
    } else if ["explicit", "uncensored", "dirty"]
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        ContentVersion::Explicit
    } else {
        ContentVersion::Unknown
    }
}

pub fn score_content_version(
    metadata_score: f64,
    preferred: ContentVersion,
    candidate: ContentVersion,
) -> Option<f64> {
    match (preferred, candidate) {
        (ContentVersion::Explicit, ContentVersion::Clean)
        | (ContentVersion::Clean, ContentVersion::Explicit) => None,
        (ContentVersion::Explicit, ContentVersion::Explicit)
        | (ContentVersion::Clean, ContentVersion::Clean) => Some((metadata_score + 0.15).min(1.0)),
        _ => Some(metadata_score),
    }
}

/// Scores a playable candidate using the MVP's documented weights. An exact
/// ISRC match wins outright; otherwise missing fields simply contribute zero.
pub fn candidate_score(target: &CandidateMetadata<'_>, candidate: &CandidateMetadata<'_>) -> f64 {
    if target
        .isrc
        .zip(candidate.isrc)
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
    {
        return 1.0;
    }

    let title = music_similarity(target.title, candidate.title);
    let artist = music_similarity(target.artist, candidate.artist).max(
        if normalize(candidate.title).contains(&normalize(target.artist)) {
            0.8
        } else {
            0.0
        },
    );
    let duration = target
        .duration_ms
        .zip(candidate.duration_ms)
        .map(|(left, right)| {
            let maximum = left.max(right).max(1) as f64;
            (1.0 - left.abs_diff(right) as f64 / maximum).max(0.0)
        })
        .unwrap_or(0.0);
    let album = target
        .album
        .zip(candidate.album)
        .map(|(left, right)| music_similarity(left, right))
        .unwrap_or(0.0);

    0.40 * title + 0.30 * artist + 0.15 * duration + 0.05 * album
}

fn music_similarity(left: &str, right: &str) -> f64 {
    let left = normalize(left);
    let right = normalize(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left == right {
        return 1.0;
    }
    let containment: f64 = if left.contains(&right) || right.contains(&left) {
        0.9
    } else {
        0.0
    };
    containment.max(normalized_levenshtein(&left, &right))
}

pub fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|token| !matches!(*token, "official" | "audio" | "video" | "lyrics" | "hd"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_urls_are_classified() {
        assert_eq!(
            classify_input("Burial Archangel").unwrap(),
            MusicProvider::Search
        );
        assert_eq!(
            classify_input("https://soundcloud.com/artist/track").unwrap(),
            MusicProvider::SoundCloud
        );
        assert_eq!(
            classify_input("https://open.spotify.com/track/abc").unwrap(),
            MusicProvider::Spotify
        );
        assert_eq!(
            classify_input("https://music.apple.com/us/album/name/123?i=456").unwrap(),
            MusicProvider::AppleMusic
        );
    }

    #[test]
    fn unsafe_and_unknown_urls_are_rejected() {
        assert_eq!(
            classify_input("http://127.0.0.1/private"),
            Err(ClassificationError::UnsupportedUrl)
        );
        assert_eq!(
            classify_input("https://example.com/audio.mp3"),
            Err(ClassificationError::UnsupportedUrl)
        );
    }

    #[test]
    fn exact_isrc_overrides_fuzzy_scoring() {
        let target = CandidateMetadata {
            title: "Completely Different",
            artist: "Someone",
            album: None,
            duration_ms: Some(100_000),
            isrc: Some("GB-AAA-01-00001"),
        };
        let candidate = CandidateMetadata {
            title: "Other",
            artist: "Other",
            album: None,
            duration_ms: Some(300_000),
            isrc: Some("gb-aaa-01-00001"),
        };
        assert_eq!(candidate_score(&target, &candidate), 1.0);
    }

    #[test]
    fn artist_in_video_title_and_duration_produce_a_confident_match() {
        let target = CandidateMetadata {
            title: "Archangel",
            artist: "Burial",
            album: Some("Untrue"),
            duration_ms: Some(240_000),
            isrc: None,
        };
        let candidate = CandidateMetadata {
            title: "Burial - Archangel (Official Audio)",
            artist: "Hyperdub",
            album: None,
            duration_ms: Some(239_000),
            isrc: None,
        };
        assert!(candidate_score(&target, &candidate) >= 0.70);
    }

    #[test]
    fn provider_links_default_to_explicit_and_reject_clean_candidates() {
        let preferred = preferred_content_version(MusicProvider::Spotify, "Track", None);
        assert_eq!(preferred, ContentVersion::Explicit);
        assert_eq!(
            score_content_version(0.9, preferred, ContentVersion::Clean),
            None
        );
        assert_eq!(
            score_content_version(0.75, preferred, ContentVersion::Explicit),
            Some(0.9)
        );
    }

    #[test]
    fn clearly_labeled_clean_source_is_respected() {
        assert_eq!(
            preferred_content_version(MusicProvider::AppleMusic, "Song (Radio Edit)", None),
            ContentVersion::Clean
        );
    }
}
