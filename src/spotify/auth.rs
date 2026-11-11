//! OAuth 2.0 Authorization Code Flow mit PKCE (RFC 7636), ohne Client-Secret.
//!
//! Der Rückruf kommt entweder über einen kurzlebigen lokalen Server auf der
//! Redirect-Adresse an oder wird von Hand eingefügt (für Rechner ohne Browser).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, warn};
use url::Url;

use crate::error::{Error, Result};
use crate::http::{send_with_retry, RetryPolicy};

/// Nur was das Programm braucht: Wiedergabe lesen und Titel einreihen.
pub const SCOPES: &str =
    "user-read-playback-state user-read-currently-playing user-modify-playback-state";

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

pub fn random_string(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| UNRESERVED[rng.gen_range(0..UNRESERVED.len())] as char)
        .collect()
}

pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn authorize_url(
    accounts_base: &Url,
    client_id: &str,
    redirect_uri: &Url,
    challenge: &str,
    state: &str,
) -> Result<Url> {
    let mut url = accounts_base
        .join("authorize")
        .map_err(|e| Error::Config(format!("Spotify-Anmeldeadresse: {e}")))?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri.as_str())
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", challenge)
        .append_pair("scope", SCOPES)
        .append_pair("state", state);
    Ok(url)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

/// Liest `code` und `state` aus einer vollständigen Adresse oder aus Pfad+Query.
pub fn parse_callback(input: &str) -> Result<Callback> {
    let input = input.trim();
    let parsed = if input.starts_with('/') {
        Url::parse("http://127.0.0.1/").and_then(|base| base.join(input))
    } else {
        Url::parse(input)
    };
    let url =
        parsed.map_err(|_| Error::Auth("Die eingefügte Adresse ist keine gültige URL.".into()))?;

    let (mut code, mut state, mut error) = (None, None, None);
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(Error::Auth(if e == "access_denied" {
            "Der Zugriff wurde in Spotify abgelehnt.".into()
        } else {
            format!("Spotify meldet: {e}")
        }));
    }
    match (code, state) {
        (Some(code), Some(state)) if !code.is_empty() => Ok(Callback { code, state }),
        _ => Err(Error::Auth(
            "In der Adresse fehlen „code“ oder „state“.".into(),
        )),
    }
}

/// Wartet auf den Rückruf von Spotify und gibt den Autorisierungscode zurück.
pub async fn wait_for_code(
    redirect_uri: &Url,
    expected_state: &str,
    timeout: Duration,
) -> Result<String> {
    let listener = bind_listener(redirect_uri).await;
    let path = redirect_uri.path().to_string();

    // Eigener Thread statt tokio::io::stdin: dessen blockierendes Lesen würde
    // das Programmende aufhalten, bis jemand Enter drückt.
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let text = line.trim();
            if text.contains("code=") || text.contains("error=") {
                let _ = tx.send(text.to_string());
                return;
            }
            if !text.is_empty() {
                eprintln!("Das ist nicht die Weiterleitungsadresse (sie enthält „code=“). Bitte erneut einfügen.");
            }
        }
    });

    let from_browser = async {
        match listener {
            Some(l) => serve_callback(l, &path, expected_state).await,
            None => std::future::pending::<Result<String>>().await,
        }
    };
    let pasted = async {
        match rx.await {
            Ok(text) => text,
            Err(_) => std::future::pending::<String>().await,
        }
    };

    let target = tokio::select! {
        r = from_browser => r?,
        text = pasted => text,
        _ = tokio::time::sleep(timeout) => {
            return Err(Error::Auth("Zeitüberschreitung: Es kam keine Antwort von Spotify.".into()));
        }
    };

    let callback = parse_callback(&target)?;
    if callback.state != expected_state {
        return Err(Error::Auth(
            "Sicherheitsprüfung fehlgeschlagen (state passt nicht). Bitte die Anmeldung neu starten.".into(),
        ));
    }
    Ok(callback.code)
}

async fn bind_listener(redirect: &Url) -> Option<TcpListener> {
    let host = redirect
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = redirect.port_or_known_default()?;
    let addr = match host.as_str() {
        "127.0.0.1" | "localhost" => format!("127.0.0.1:{port}"),
        "::1" => format!("[::1]:{port}"),
        _ => {
            debug!("Redirect-Adresse ist nicht lokal; warte nur auf eingefügte Adresse");
            return None;
        }
    };
    match TcpListener::bind(&addr).await {
        Ok(listener) => Some(listener),
        Err(e) => {
            warn!(
                "{addr} ist nicht verfügbar ({e}); bitte die Adresse nach der Anmeldung einfügen."
            );
            None
        }
    }
}

const PAGE_OK: &str = "<!doctype html><meta charset=\"utf-8\"><title>klangwerk-dj</title>\
<p style=\"font:16px system-ui;margin:3em\">Anmeldung abgeschlossen. Du kannst dieses Fenster schließen.</p>";
const PAGE_FAIL: &str = "<!doctype html><meta charset=\"utf-8\"><title>klangwerk-dj</title>\
<p style=\"font:16px system-ui;margin:3em\">Die Anmeldung wurde nicht abgeschlossen. Details stehen im Terminal.</p>";
const PAGE_IGNORED: &str = "<!doctype html><meta charset=\"utf-8\"><title>klangwerk-dj</title>\
<p style=\"font:16px system-ui;margin:3em\">Das ist nicht die Antwort von Spotify. klangwerk-dj wartet weiter auf die Anmeldung.</p>";

/// Ist das die Antwort von Spotify auf *diese* Anmeldung? Dann trägt sie
/// unseren `state` und entweder `code` oder `error`.
fn is_spotify_answer(target: &str, expected_state: &str) -> bool {
    let query = target.split_once('?').map_or("", |(_, q)| q);
    let (mut state_ok, mut has_result) = (false, false);
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match &*key {
            "state" => state_ok = value == expected_state,
            "code" | "error" => has_result |= !value.is_empty(),
            _ => {}
        }
    }
    state_ok && has_result
}

/// Wartet auf den Rückruf. Fremde Aufrufe der Adresse (Vorschau des Browsers,
/// eine andere Webseite, ein Aufruf ohne oder mit falschem `state`) werden
/// abgewiesen und brechen die Anmeldung nicht ab.
async fn serve_callback(listener: TcpListener, path: &str, expected_state: &str) -> Result<String> {
    loop {
        let (mut sock, _) = listener
            .accept()
            .await
            .map_err(|e| Error::Auth(format!("Rückruf-Server: {e}")))?;
        let mut buf = vec![0u8; 8192];
        let mut filled = 0;
        while filled < buf.len() {
            match tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf[filled..])).await
            {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => {
                    filled += n;
                    if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        let head = String::from_utf8_lossy(&buf[..filled]);
        let target = head.lines().next().and_then(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("GET"), Some(t)) => Some(t.to_string()),
                _ => None,
            }
        });
        match target {
            Some(t) if t.split('?').next() == Some(path) => {
                if !is_spotify_answer(&t, expected_state) {
                    debug!("Aufruf der Rückruf-Adresse ohne gültige Antwort ignoriert");
                    let _ = respond(&mut sock, "400 Bad Request", PAGE_IGNORED).await;
                    continue;
                }
                let page = if t.contains("code=") {
                    PAGE_OK
                } else {
                    PAGE_FAIL
                };
                let _ = respond(&mut sock, "200 OK", page).await;
                return Ok(t);
            }
            _ => {
                let _ = respond(&mut sock, "404 Not Found", "").await;
            }
        }
    }
}

async fn respond(sock: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(msg.as_bytes()).await?;
    sock.shutdown().await
}

// ---------------------------------------------------------------------------
// Tokens

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix-Zeit in Sekunden.
    pub expires_at: u64,
    #[serde(default)]
    pub scope: String,
}

impl StoredToken {
    pub fn expires_soon(&self) -> bool {
        now_secs() + 60 >= self.expires_at
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
}

pub struct TokenEndpoint<'a> {
    pub http: &'a reqwest::Client,
    pub accounts_base: &'a Url,
    pub client_id: &'a str,
    pub retry: &'a RetryPolicy,
}

impl TokenEndpoint<'_> {
    pub async fn exchange(
        &self,
        code: &str,
        redirect_uri: &Url,
        verifier: &str,
    ) -> Result<StoredToken> {
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", self.client_id),
            ("code_verifier", verifier),
        ];
        self.request(&form, None).await
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<StoredToken> {
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id),
        ];
        self.request(&form, Some(refresh_token)).await
    }

    async fn request(
        &self,
        form: &[(&str, &str)],
        previous_refresh: Option<&str>,
    ) -> Result<StoredToken> {
        let url = self
            .accounts_base
            .join("api/token")
            .map_err(|e| Error::Config(format!("Spotify-Token-Adresse: {e}")))?;
        let reply = send_with_retry("Spotify-Anmeldung", self.retry, false, || {
            self.http
                .post(url.clone())
                .form(form)
                .timeout(Duration::from_secs(20))
        })
        .await?;

        if !reply.status.is_success() {
            let body = reply.json().unwrap_or_default();
            let code = body["error"].as_str().unwrap_or_default();
            let description = body["error_description"].as_str().unwrap_or_default();
            if code == "invalid_grant" && previous_refresh.is_some() {
                return Err(Error::LoginExpired);
            }
            return Err(Error::Auth(
                format!("Spotify hat die Token-Anfrage abgelehnt: {code} {description}")
                    .trim()
                    .to_string(),
            ));
        }

        let token: TokenResponse =
            serde_json::from_slice(&reply.body).map_err(|e| Error::Unexpected {
                service: "Spotify-Anmeldung",
                message: e.to_string(),
            })?;
        let refresh_token = token
            .refresh_token
            .or_else(|| previous_refresh.map(str::to_string))
            .ok_or_else(|| Error::Auth("Spotify hat kein Refresh-Token geliefert.".into()))?;
        Ok(StoredToken {
            access_token: token.access_token,
            refresh_token,
            expires_at: now_secs() + token.expires_in,
            scope: token.scope.unwrap_or_default(),
        })
    }
}

/// Speichert das Token als JSON, nur für den eigenen Benutzer lesbar.
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("spotify-token.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<StoredToken>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| Error::Unexpected {
                    service: "Token-Datei",
                    message: format!("{}: {e}", self.path.display()),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(&self.path, e)),
        }
    }

    pub fn save(&self, token: &StoredToken) -> Result<()> {
        let data = serde_json::to_vec_pretty(token).map_err(|e| Error::Unexpected {
            service: "Token-Datei",
            message: e.to_string(),
        })?;
        save_private(&self.path, &data)
    }

    pub fn delete(&self) -> Result<bool> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::io(&self.path, e)),
        }
    }
}

/// Schreibt eine Datei atomar (temporäre Datei + Umbenennen), unter Unix mit Rechten 600
/// in einem Ordner mit Rechten 700. Unter Windows schützt das Benutzerprofil die Datei.
pub(crate) fn save_private(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Config(format!("Ungültiger Pfad: {}", path.display())))?;
    create_private_dir(dir).map_err(|e| Error::io(dir, e))?;
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    write_private(&tmp, data).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let _ = std::fs::remove_file(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_uses_only_unreserved_characters() {
        let v = random_string(128);
        assert_eq!(v.len(), 128);
        assert!(v.bytes().all(|b| UNRESERVED.contains(&b)));
    }

    #[test]
    fn authorize_url_contains_pkce_parameters() {
        let base = Url::parse("https://accounts.spotify.com/").expect("url");
        let redirect = Url::parse("http://127.0.0.1:8888/callback").expect("url");
        let url = authorize_url(&base, "abc", &redirect, "xyz", "st").expect("url");
        let q: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let get = |k: &str| q.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(url.path(), "/authorize");
        assert_eq!(get("code_challenge_method"), Some("S256"));
        assert_eq!(get("code_challenge"), Some("xyz"));
        assert_eq!(get("redirect_uri"), Some("http://127.0.0.1:8888/callback"));
        assert_eq!(get("scope"), Some(SCOPES));
    }

    #[test]
    fn parses_full_url_and_path() {
        let full = parse_callback("http://127.0.0.1:8888/callback?code=AQB&state=s1").expect("cb");
        assert_eq!(
            full,
            Callback {
                code: "AQB".into(),
                state: "s1".into()
            }
        );
        let path = parse_callback("/callback?state=s2&code=X%2FY").expect("cb");
        assert_eq!(
            path,
            Callback {
                code: "X/Y".into(),
                state: "s2".into()
            }
        );
    }

    #[test]
    fn reports_denied_access() {
        let err = parse_callback("http://127.0.0.1:8888/callback?error=access_denied&state=s")
            .unwrap_err();
        assert!(err.to_string().contains("abgelehnt"));
        assert!(parse_callback("http://127.0.0.1:8888/callback?state=s").is_err());
    }

    #[tokio::test]
    async fn local_server_receives_callback() {
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            probe.local_addr().expect("addr").port()
        };
        let redirect = Url::parse(&format!("http://127.0.0.1:{port}/callback")).expect("url");
        let waiter = tokio::spawn(async move {
            wait_for_code(&redirect, "st4te", Duration::from_secs(10)).await
        });
        let mut stream = None;
        for _ in 0..50 {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut stream = stream.expect("Rückruf-Server erreichbar");
        stream
            .write_all(b"GET /callback?code=c0de&state=st4te HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.expect("read");
        assert!(answer.starts_with("HTTP/1.1 200"));
        assert_eq!(waiter.await.expect("join").expect("code"), "c0de");
    }

    #[tokio::test]
    async fn local_server_ignores_foreign_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server =
            tokio::spawn(async move { serve_callback(listener, "/callback", "st4te").await });
        for (target, status) in [
            ("/callback", "400"),
            ("/callback?code=boese&state=falsch", "400"),
            ("/callback?state=st4te", "400"),
            ("/favicon.ico", "404"),
            ("/callback?code=c0de&state=st4te", "200"),
        ] {
            let mut stream = TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("connect");
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .expect("write");
            let mut answer = String::new();
            stream.read_to_string(&mut answer).await.expect("read");
            assert!(
                answer.starts_with(&format!("HTTP/1.1 {status}")),
                "{target}: {answer}"
            );
        }
        let target = server.await.expect("join").expect("target");
        assert_eq!(target, "/callback?code=c0de&state=st4te");
    }

    #[test]
    fn recognises_spotify_answers() {
        assert!(is_spotify_answer("/callback?code=c&state=s", "s"));
        assert!(is_spotify_answer(
            "/callback?error=access_denied&state=s",
            "s"
        ));
        assert!(!is_spotify_answer("/callback?code=c&state=x", "s"));
        assert!(!is_spotify_answer("/callback?code=&state=s", "s"));
        assert!(!is_spotify_answer("/callback?xcode=c&state=s", "s"));
        assert!(!is_spotify_answer("/callback", "s"));
    }

    #[test]
    fn token_store_roundtrip_is_private() {
        let dir = std::env::temp_dir().join(format!("klangwerk-dj-test-{}", random_string(10)));
        let store = TokenStore::new(&dir);
        assert!(store.load().expect("load").is_none());
        let token = StoredToken {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now_secs() + 3600,
            scope: SCOPES.into(),
        };
        store.save(&token).expect("save");
        let loaded = store.load().expect("load").expect("token");
        assert_eq!(loaded.refresh_token, "r");
        assert!(!loaded.expires_soon());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(store.delete().expect("delete"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
