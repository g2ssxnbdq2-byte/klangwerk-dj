//! Schlanker Client für die Spotify-Endpunkte, die das Programm nutzt.
//!
//! Stand Development Mode (Februar 2026): Suche liefert höchstens 10 Treffer,
//! Wiedergabe-Endpunkte bleiben verfügbar, der App-Besitzer braucht Premium.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use url::Url;

use super::auth::{StoredToken, TokenEndpoint, TokenStore};
use crate::error::{Error, Result};
use crate::http::{send_with_retry, Reply, RetryPolicy};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub artists: Vec<Artist>,
}

impl Track {
    pub fn artist_names(&self) -> String {
        self.artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn label(&self) -> String {
        format!("{} – {}", self.artist_names(), self.name)
    }
}

#[derive(Debug, Deserialize)]
pub struct CurrentlyPlaying {
    #[serde(default)]
    pub is_playing: bool,
    pub progress_ms: Option<u64>,
    pub item: Option<Track>,
    #[serde(default)]
    pub currently_playing_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: Option<String>,
    #[serde(default)]
    pub is_active: bool,
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    pub volume_percent: Option<u32>,
}

#[derive(Deserialize)]
struct DevicesResponse {
    #[serde(default)]
    devices: Vec<Device>,
}

#[derive(Deserialize)]
struct SearchResponse {
    tracks: Option<Paging>,
}

#[derive(Deserialize)]
struct Paging {
    #[serde(default)]
    items: Vec<Option<Track>>,
}

pub struct Spotify {
    http: reqwest::Client,
    api_base: Url,
    accounts_base: Url,
    client_id: String,
    store: TokenStore,
    token: Mutex<Option<StoredToken>>,
    retry: RetryPolicy,
}

impl Spotify {
    pub fn new(
        http: reqwest::Client,
        api_base: Url,
        accounts_base: Url,
        client_id: String,
        store: TokenStore,
    ) -> Self {
        Self {
            http,
            api_base,
            accounts_base,
            client_id,
            store,
            token: Mutex::new(None),
            retry: RetryPolicy::default(),
        }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn accounts_base(&self) -> &Url {
        &self.accounts_base
    }

    pub fn token_path(&self) -> &std::path::Path {
        self.store.path()
    }

    pub fn token_endpoint(&self) -> TokenEndpoint<'_> {
        TokenEndpoint {
            http: &self.http,
            accounts_base: &self.accounts_base,
            client_id: &self.client_id,
            retry: &self.retry,
        }
    }

    pub async fn store_token(&self, token: StoredToken) -> Result<()> {
        self.store.save(&token)?;
        *self.token.lock().await = Some(token);
        Ok(())
    }

    async fn access_token(&self, force_refresh: bool) -> Result<String> {
        let mut guard = self.token.lock().await;
        if guard.is_none() {
            *guard = self.store.load()?;
        }
        let refresh_token = match guard.as_ref() {
            None => return Err(Error::NotLoggedIn),
            Some(t) if force_refresh || t.expires_soon() => Some(t.refresh_token.clone()),
            Some(_) => None,
        };
        if let Some(refresh_token) = refresh_token {
            let fresh = self.token_endpoint().refresh(&refresh_token).await?;
            self.store.save(&fresh)?;
            *guard = Some(fresh);
        }
        guard
            .as_ref()
            .map(|t| t.access_token.clone())
            .ok_or(Error::NotLoggedIn)
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
        idempotent: bool,
    ) -> Result<Reply> {
        let url = self
            .api_base
            .join(path)
            .map_err(|e| Error::Config(format!("Spotify-Pfad {path}: {e}")))?;
        let mut refreshed = false;
        loop {
            let token = self.access_token(refreshed).await?;
            let reply = send_with_retry("Spotify", &self.retry, idempotent, || {
                let builder = self
                    .http
                    .request(method.clone(), url.clone())
                    .bearer_auth(&token)
                    .query(query)
                    .timeout(Duration::from_secs(20));
                match body {
                    Some(b) => builder.json(b),
                    None if method != Method::GET => builder.body(Vec::<u8>::new()),
                    None => builder,
                }
            })
            .await?;
            if reply.status == StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                continue;
            }
            return check(reply);
        }
    }

    /// `None`, wenn gerade nichts läuft.
    pub async fn currently_playing(&self) -> Result<Option<CurrentlyPlaying>> {
        let reply = self
            .call(
                Method::GET,
                "me/player/currently-playing",
                &[("additional_types", "track")],
                None,
                true,
            )
            .await?;
        if reply.status == StatusCode::NO_CONTENT || reply.body.is_empty() {
            return Ok(None);
        }
        parse(&reply).map(Some)
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        let reply = self
            .call(Method::GET, "me/player/devices", &[], None, true)
            .await?;
        Ok(parse::<DevicesResponse>(&reply)?.devices)
    }

    /// Titelsuche; Spotify erlaubt im Development Mode höchstens 10 Treffer pro Anfrage.
    pub async fn search_tracks(&self, query: &str, limit: u8) -> Result<Vec<Track>> {
        let limit = limit.clamp(1, 10).to_string();
        let reply = self
            .call(
                Method::GET,
                "search",
                &[("q", query), ("type", "track"), ("limit", limit.as_str())],
                None,
                true,
            )
            .await?;
        let found: SearchResponse = parse(&reply)?;
        Ok(found
            .tracks
            .map(|p| p.items.into_iter().flatten().collect())
            .unwrap_or_default())
    }

    /// Hängt einen Titel an die Warteschlange an. Nicht idempotent: wird bei
    /// unklarem Ausgang nicht wiederholt, damit kein Titel doppelt landet.
    pub async fn add_to_queue(&self, uri: &str, device_id: Option<&str>) -> Result<()> {
        let mut query = vec![("uri", uri)];
        if let Some(d) = device_id {
            query.push(("device_id", d));
        }
        self.call(Method::POST, "me/player/queue", &query, None, false)
            .await?;
        Ok(())
    }

    /// Startet die Wiedergabe mit den angegebenen Titeln.
    pub async fn play(&self, uris: &[String], device_id: Option<&str>) -> Result<()> {
        let query: Vec<(&str, &str)> = device_id.map(|d| ("device_id", d)).into_iter().collect();
        let body = json!({ "uris": uris });
        self.call(Method::PUT, "me/player/play", &query, Some(&body), true)
            .await?;
        Ok(())
    }
}

fn parse<T: DeserializeOwned>(reply: &Reply) -> Result<T> {
    serde_json::from_slice(&reply.body).map_err(|e| Error::Unexpected {
        service: "Spotify",
        message: e.to_string(),
    })
}

fn check(reply: Reply) -> Result<Reply> {
    if reply.status.is_success() {
        return Ok(reply);
    }
    let body = reply.json().unwrap_or_default();
    let reason = body["error"]["reason"].as_str().unwrap_or_default();
    let message = body["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| reply.text_lossy(200));
    let status = reply.status.as_u16();
    Err(match (status, reason) {
        (_, "NO_ACTIVE_DEVICE") => Error::NoActiveDevice,
        (_, "PREMIUM_REQUIRED") => Error::PremiumRequired,
        (429, "QUOTA_EXCEEDED") => Error::QuotaExceeded,
        (401, _) => Error::LoginExpired,
        (404, _) if message.to_lowercase().contains("no active device") => Error::NoActiveDevice,
        _ => Error::Api {
            service: "Spotify",
            status,
            message,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status: StatusCode::from_u16(status).expect("status"),
            headers: HeaderMap::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn maps_spotify_error_reasons() {
        let no_device = r#"{"error":{"status":404,"message":"Player command failed: No active device found","reason":"NO_ACTIVE_DEVICE"}}"#;
        assert!(matches!(
            check(reply(404, no_device)),
            Err(Error::NoActiveDevice)
        ));
        let premium = r#"{"error":{"status":403,"message":"Player command failed: Premium required","reason":"PREMIUM_REQUIRED"}}"#;
        assert!(matches!(
            check(reply(403, premium)),
            Err(Error::PremiumRequired)
        ));
        let quota =
            r#"{"error":{"status":429,"message":"Too many requests","reason":"QUOTA_EXCEEDED"}}"#;
        assert!(matches!(
            check(reply(429, quota)),
            Err(Error::QuotaExceeded)
        ));
        assert!(matches!(
            check(reply(500, "kaputt")),
            Err(Error::Api { status: 500, .. })
        ));
        assert!(check(reply(204, "")).is_ok());
    }

    #[test]
    fn parses_currently_playing_and_search() {
        let cp: CurrentlyPlaying = serde_json::from_str(
            r#"{"is_playing":true,"progress_ms":1000,"currently_playing_type":"track",
                "item":{"uri":"spotify:track:1","name":"Song","duration_ms":200000,"artists":[{"name":"A"},{"name":"B"}]}}"#,
        )
        .expect("json");
        assert_eq!(cp.item.expect("item").label(), "A, B – Song");

        let s: SearchResponse = serde_json::from_str(
            r#"{"tracks":{"items":[null,{"uri":"spotify:track:2","name":"X"}]}}"#,
        )
        .expect("json");
        let items: Vec<Track> = s
            .tracks
            .expect("tracks")
            .items
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].duration_ms, 0);
    }
}
