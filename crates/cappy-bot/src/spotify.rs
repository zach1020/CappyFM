use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use axum::{
    Router,
    extract::{Query, State},
    response::Html,
    routing::get,
};
use lavalink_rs::model::track::{TrackData, TrackInfo};
use rand::distr::{Alphanumeric, SampleString};
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, oneshot};
use url::Url;

const SPOTIFY_ACCOUNTS_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";
const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
const PLAYLIST_PAGE_SIZE: usize = 50;

#[derive(Debug, Error)]
pub enum SpotifyError {
    #[error("Spotify rejected the saved authorization; run the login flow again")]
    AuthorizationExpired,
    #[error("Spotify only exposes playlists owned by or shared with the authorized user")]
    PlaylistForbidden,
    #[error("Spotify playlist not found")]
    PlaylistNotFound,
    #[error("Spotify request failed: {0}")]
    Request(String),
}

#[derive(Debug)]
pub struct SpotifyPlaylist {
    pub name: String,
    pub tracks: Vec<TrackData>,
}

#[derive(Debug, Clone)]
struct SpotifyCredentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

#[derive(Debug, Clone)]
struct CachedAccessToken {
    value: String,
    expires_at: Instant,
}

#[derive(Clone)]
pub struct SpotifyClient {
    http: Client,
    credentials: SpotifyCredentials,
    access_token: Arc<RwLock<Option<CachedAccessToken>>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistSummary {
    name: String,
}

#[derive(Debug, Deserialize)]
struct PlaylistItemsPage {
    items: Vec<PlaylistItem>,
    total: usize,
}

#[derive(Debug, Deserialize)]
struct PlaylistItem {
    #[serde(default)]
    item: Option<SpotifyTrack>,
    #[serde(default)]
    track: Option<SpotifyTrack>,
    #[serde(default)]
    is_local: bool,
}

#[derive(Debug, Deserialize)]
struct SpotifyTrack {
    id: Option<String>,
    name: String,
    duration_ms: u64,
    #[serde(default)]
    explicit: bool,
    #[serde(default)]
    is_local: bool,
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    artists: Vec<SpotifyArtist>,
    album: Option<SpotifyAlbum>,
    external_ids: Option<SpotifyExternalIds>,
    external_urls: Option<SpotifyExternalUrls>,
}

#[derive(Debug, Deserialize)]
struct SpotifyArtist {
    name: String,
}

#[derive(Debug, Deserialize)]
struct SpotifyAlbum {
    name: String,
    #[serde(default)]
    images: Vec<SpotifyImage>,
}

#[derive(Debug, Deserialize)]
struct SpotifyImage {
    url: String,
}

#[derive(Debug, Deserialize)]
struct SpotifyExternalIds {
    isrc: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpotifyExternalUrls {
    spotify: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

type OAuthResultSender = oneshot::Sender<Result<String, String>>;

#[derive(Clone)]
struct OAuthCallbackState {
    expected_state: String,
    sender: Arc<Mutex<Option<OAuthResultSender>>>,
}

impl SpotifyClient {
    pub fn from_environment() -> Option<Self> {
        let client_id = nonempty_env("SPOTIFY_CLIENT_ID")?;
        let client_secret = nonempty_env("SPOTIFY_CLIENT_SECRET")?;
        let refresh_token = nonempty_env("SPOTIFY_REFRESH_TOKEN")
            .or_else(|| fs::read_to_string(refresh_token_path()).ok())?
            .trim()
            .to_owned();
        if refresh_token.is_empty() {
            return None;
        }
        Some(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(12))
                .build()
                .ok()?,
            credentials: SpotifyCredentials {
                client_id,
                client_secret,
                refresh_token,
            },
            access_token: Arc::default(),
        })
    }

    pub async fn load_owned_playlist(
        &self,
        playlist_url: &str,
        maximum_tracks: usize,
    ) -> Result<SpotifyPlaylist, SpotifyError> {
        let playlist_id = playlist_id(playlist_url)
            .ok_or_else(|| SpotifyError::Request("invalid Spotify playlist URL".to_owned()))?;
        let access_token = self.access_token().await?;
        let summary_url = format!("{SPOTIFY_API_BASE}/playlists/{playlist_id}?fields=name");
        let summary: PlaylistSummary = self.authorized_json(&summary_url, &access_token).await?;

        let mut tracks = Vec::new();
        let mut offset = 0;
        while tracks.len() < maximum_tracks {
            let url = format!(
                "{SPOTIFY_API_BASE}/playlists/{playlist_id}/items?limit={PLAYLIST_PAGE_SIZE}&offset={offset}"
            );
            let page: PlaylistItemsPage = self.authorized_json(&url, &access_token).await?;
            let page_len = page.items.len();
            tracks.extend(
                page.items
                    .into_iter()
                    .filter_map(PlaylistItem::into_track_data)
                    .take(maximum_tracks.saturating_sub(tracks.len())),
            );
            offset += page_len;
            if page_len == 0 || offset >= page.total {
                break;
            }
        }

        Ok(SpotifyPlaylist {
            name: summary.name,
            tracks,
        })
    }

    async fn access_token(&self) -> Result<String, SpotifyError> {
        if let Some(cached) = self.access_token.read().await.as_ref()
            && cached.expires_at > Instant::now() + Duration::from_secs(30)
        {
            return Ok(cached.value.clone());
        }

        let body = form_body(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &self.credentials.refresh_token),
        ]);
        let response = self
            .http
            .post(SPOTIFY_ACCOUNTS_TOKEN_URL)
            .basic_auth(
                &self.credentials.client_id,
                Some(&self.credentials.client_secret),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|error| SpotifyError::Request(error.to_string()))?;
        if response.status() == StatusCode::BAD_REQUEST
            || response.status() == StatusCode::UNAUTHORIZED
        {
            return Err(SpotifyError::AuthorizationExpired);
        }
        let response = response
            .error_for_status()
            .map_err(|error| SpotifyError::Request(error.to_string()))?
            .json::<TokenResponse>()
            .await
            .map_err(|error| SpotifyError::Request(error.to_string()))?;
        let expires_at = Instant::now() + Duration::from_secs(response.expires_in.max(60));
        *self.access_token.write().await = Some(CachedAccessToken {
            value: response.access_token.clone(),
            expires_at,
        });
        Ok(response.access_token)
    }

    async fn authorized_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<T, SpotifyError> {
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| SpotifyError::Request(error.to_string()))?;
        match response.status() {
            StatusCode::UNAUTHORIZED => return Err(SpotifyError::AuthorizationExpired),
            StatusCode::FORBIDDEN => return Err(SpotifyError::PlaylistForbidden),
            StatusCode::NOT_FOUND => return Err(SpotifyError::PlaylistNotFound),
            _ => {}
        }
        response
            .error_for_status()
            .map_err(|error| SpotifyError::Request(error.to_string()))?
            .json::<T>()
            .await
            .map_err(|error| SpotifyError::Request(error.to_string()))
    }
}

impl PlaylistItem {
    fn into_track_data(self) -> Option<TrackData> {
        if self.is_local {
            return None;
        }
        let track = self.item.or(self.track)?;
        if track.is_local || track.item_type != "track" {
            return None;
        }
        let identifier = track.id?;
        let author = track
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if author.is_empty() {
            return None;
        }
        let album_name = track.album.as_ref().map(|album| album.name.clone());
        let artwork_url = track
            .album
            .as_ref()
            .and_then(|album| album.images.first())
            .map(|image| image.url.clone());
        Some(TrackData {
            encoded: String::new(),
            info: TrackInfo {
                identifier,
                is_seekable: true,
                author,
                length: track.duration_ms,
                is_stream: false,
                position: 0,
                title: track.name,
                uri: track.external_urls.and_then(|urls| urls.spotify),
                artwork_url,
                isrc: track.external_ids.and_then(|ids| ids.isrc),
                source_name: "spotify".to_owned(),
            },
            plugin_info: Some(serde_json::json!({
                "albumName": album_name,
                "explicit": track.explicit,
            })),
            user_data: None,
        })
    }
}

pub fn is_playlist_url(input: &str) -> bool {
    playlist_id(input).is_some()
}

pub async fn run_login_flow() -> Result<()> {
    let client_id =
        nonempty_env("SPOTIFY_CLIENT_ID").context("SPOTIFY_CLIENT_ID is empty in .env")?;
    let client_secret =
        nonempty_env("SPOTIFY_CLIENT_SECRET").context("SPOTIFY_CLIENT_SECRET is empty in .env")?;
    let redirect_uri =
        nonempty_env("SPOTIFY_REDIRECT_URI").unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_owned());
    let redirect = Url::parse(&redirect_uri).context("SPOTIFY_REDIRECT_URI is invalid")?;
    if redirect.scheme() != "http"
        || redirect.host_str() != Some("127.0.0.1")
        || redirect.path() != "/callback"
    {
        bail!("SPOTIFY_REDIRECT_URI must use http://127.0.0.1:<port>/callback");
    }
    let port = redirect
        .port_or_known_default()
        .context("SPOTIFY_REDIRECT_URI needs a port")?;
    let oauth_state = Alphanumeric.sample_string(&mut rand::rng(), 40);
    let mut authorization = Url::parse(SPOTIFY_AUTHORIZE_URL)?;
    authorization.query_pairs_mut().extend_pairs([
        ("client_id", client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri.as_str()),
        ("scope", "playlist-read-private"),
        ("state", oauth_state.as_str()),
        ("show_dialog", "true"),
    ]);

    let (sender, receiver) = oneshot::channel();
    let callback_state = OAuthCallbackState {
        expected_state: oauth_state,
        sender: Arc::new(Mutex::new(Some(sender))),
    };
    let app = Router::new()
        .route("/callback", get(oauth_callback))
        .with_state(callback_state);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("could not listen for Spotify on port {port}"))?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    println!("\nOpen this URL in your browser to authorize your Spotify playlists:\n");
    println!("{authorization}\n");
    println!("Waiting up to five minutes for Spotify to redirect back...");

    let code = tokio::time::timeout(Duration::from_secs(300), receiver)
        .await
        .context("Spotify login timed out")?
        .context("Spotify callback closed unexpectedly")?
        .map_err(anyhow::Error::msg)?;
    server.abort();

    let http = Client::builder().timeout(Duration::from_secs(12)).build()?;
    let body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
    ]);
    let token = http
        .post(SPOTIFY_ACCOUNTS_TOKEN_URL)
        .basic_auth(&client_id, Some(&client_secret))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;
    let refresh_token = token
        .refresh_token
        .context("Spotify did not return a refresh token")?;
    write_refresh_token(&refresh_token_path(), &refresh_token)?;
    println!(
        "Spotify authorization saved. CappyFM can now read playlists you own or collaborate on."
    );
    Ok(())
}

async fn oauth_callback(
    State(state): State<OAuthCallbackState>,
    Query(callback): Query<OAuthCallback>,
) -> Html<&'static str> {
    let result = if let Some(error) = callback.error {
        Err(format!("Spotify denied authorization: {error}"))
    } else if callback.state.as_deref() != Some(state.expected_state.as_str()) {
        Err("Spotify callback state did not match".to_owned())
    } else {
        callback
            .code
            .ok_or_else(|| "Spotify callback did not include a code".to_owned())
    };
    if let Some(sender) = state.sender.lock().await.take() {
        let _ = sender.send(result);
    }
    Html("Spotify authorization received. You can close this window and return to CappyFM.")
}

fn playlist_id(input: &str) -> Option<String> {
    let url = Url::parse(input).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("open.spotify.com") {
        return None;
    }
    let mut segments = url.path_segments()?;
    if segments.next()? != "playlist" {
        return None;
    }
    let id = segments.next()?;
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(id.to_owned())
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn refresh_token_path() -> PathBuf {
    std::env::var_os("SPOTIFY_REFRESH_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if Path::new("/app/data").is_dir() {
                PathBuf::from("/app/data/spotify-refresh-token")
            } else {
                PathBuf::from("data/spotify-refresh-token")
            }
        })
}

fn write_refresh_token(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("could not write {}", path.display()))?;
    file.write_all(value.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn form_body(values: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(values.iter().copied());
    serializer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_strict_spotify_playlist_urls() {
        assert_eq!(
            playlist_id("https://open.spotify.com/playlist/abc123?si=hello").as_deref(),
            Some("abc123")
        );
        assert!(playlist_id("https://open.spotify.com/track/abc123").is_none());
        assert!(playlist_id("https://example.com/playlist/abc123").is_none());
    }

    #[test]
    fn playlist_item_becomes_metadata_only_track() {
        let item: PlaylistItem = serde_json::from_value(serde_json::json!({
            "is_local": false,
            "item": {
                "id": "spotify-id",
                "name": "Example Song",
                "duration_ms": 123000,
                "explicit": true,
                "is_local": false,
                "type": "track",
                "artists": [{"name": "Example Artist"}],
                "album": {"name": "Example Album", "images": [{"url": "https://example.com/art.jpg"}]},
                "external_ids": {"isrc": "USABC1234567"},
                "external_urls": {"spotify": "https://open.spotify.com/track/spotify-id"}
            }
        }))
        .unwrap();
        let track = item.into_track_data().unwrap();
        assert!(track.encoded.is_empty());
        assert_eq!(track.info.title, "Example Song");
        assert_eq!(track.info.author, "Example Artist");
        assert_eq!(track.info.isrc.as_deref(), Some("USABC1234567"));
        assert_eq!(track.plugin_info.unwrap()["explicit"], true);
    }
}
