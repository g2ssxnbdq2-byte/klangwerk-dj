//! Einstellungen aus Kommandozeile und Umgebungsvariablen.

use std::path::PathBuf;
use std::time::Duration;

use tracing::warn;
use url::Url;

use crate::error::{Error, Result};

/// Zugangsdaten für die Claude-API.
#[derive(Clone)]
pub enum Credential {
    /// `ANTHROPIC_API_KEY`, gesendet als `x-api-key`.
    ApiKey(String),
    /// `ANTHROPIC_AUTH_TOKEN` (OAuth), gesendet als Bearer-Token.
    OAuth(String),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::ApiKey(_) => f.write_str("ApiKey(***)"),
            Credential::OAuth(_) => f.write_str("OAuth(***)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub spotify_client_id: Option<String>,
    pub redirect_uri: Url,
    pub spotify_api_base: Url,
    pub spotify_accounts_base: Url,
    pub anthropic_base: Url,
    pub anthropic_credential: Option<Credential>,
    pub model: String,
    pub effort: String,
    pub fallbacks: bool,
    pub data_dir: PathBuf,
}

impl Config {
    pub fn new(
        spotify_client_id: Option<String>,
        redirect_uri: &str,
        model: String,
        effort: String,
        fallbacks: bool,
    ) -> Result<Self> {
        let redirect_uri = parse_redirect(redirect_uri)?;
        let data_dir = data_dir()?;
        let anthropic_credential = non_empty_env("ANTHROPIC_API_KEY")
            .map(Credential::ApiKey)
            .or_else(|| non_empty_env("ANTHROPIC_AUTH_TOKEN").map(Credential::OAuth));

        Ok(Self {
            spotify_client_id: spotify_client_id.filter(|s| !s.trim().is_empty()),
            redirect_uri,
            spotify_api_base: env_base("SPOTIFY_API_BASE", "https://api.spotify.com/v1/")?,
            spotify_accounts_base: env_base(
                "SPOTIFY_ACCOUNTS_BASE",
                "https://accounts.spotify.com/",
            )?,
            anthropic_base: env_base("ANTHROPIC_BASE_URL", "https://api.anthropic.com/")?,
            anthropic_credential,
            model: model.trim().to_string(),
            effort,
            fallbacks,
            data_dir,
        })
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Ordner für Token und Einstellungen:
/// Linux `~/.config/klangwerk-dj`, macOS `~/Library/Application Support/de.Klangwerk.klangwerk-dj`,
/// Windows `%APPDATA%\Klangwerk\klangwerk-dj\config`. Überschreibbar mit `KLANGWERK_DATEN`.
pub fn data_dir() -> Result<PathBuf> {
    match non_empty_env("KLANGWERK_DATEN") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => directories::ProjectDirs::from("de", "Klangwerk", "klangwerk-dj")
            .map(|d| d.config_dir().to_path_buf())
            .ok_or_else(|| {
                Error::Config(
                    "Kein Konfigurationsordner gefunden; bitte KLANGWERK_DATEN setzen.".into(),
                )
            }),
    }
}

// ---------------------------------------------------------------------------
// Einstellungsdatei (`einstellungen.env`): KEY=VALUE, `#` leitet Kommentare ein.
// Echte Umgebungsvariablen haben Vorrang vor der Datei.

pub const SETTINGS_FILE: &str = "einstellungen.env";

/// Nur diese Schlüssel werden aus der Datei übernommen.
pub const SETTINGS_KEYS: &[&str] = &[
    "SPOTIFY_CLIENT_ID",
    "SPOTIFY_REDIRECT_URI",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "KLANGWERK_MODELL",
    "KLANGWERK_AUFWAND",
    "KLANGWERK_OHNE_FALLBACK",
    "RUST_LOG",
];

pub fn parse_settings(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            Some((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

pub fn render_settings(entries: &[(String, String)]) -> String {
    let mut out = String::from("# klangwerk-dj – Einstellungen. Nur für dich lesbar halten.\n");
    for (key, value) in entries {
        out.push_str(&format!("{key}={value}\n"));
    }
    out
}

/// Welche Einträge übernommen werden: nur bekannte Schlüssel, nicht leer,
/// und nur, wenn die Umgebung den Wert nicht schon setzt.
pub fn settings_to_apply(
    entries: &[(String, String)],
    already_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    entries
        .iter()
        .filter(|(k, v)| SETTINGS_KEYS.contains(&k.as_str()) && !v.is_empty() && !already_set(k))
        .cloned()
        .collect()
}

/// Lädt die Einstellungsdatei in die Prozessumgebung. Muss vor dem Start
/// weiterer Threads aufgerufen werden.
pub fn load_settings(dir: &std::path::Path) -> Result<()> {
    let path = dir.join(SETTINGS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::io(&path, e)),
    };
    for (key, value) in settings_to_apply(&parse_settings(&text), |k| non_empty_env(k).is_some()) {
        std::env::set_var(key, value);
    }
    Ok(())
}

/// Basisadresse aus der Umgebung; endet immer auf `/`, damit `Url::join` Pfade anhängt.
fn env_base(name: &str, default: &str) -> Result<Url> {
    let raw = non_empty_env(name).unwrap_or_else(|| default.to_string());
    parse_base(name, &raw)
}

/// An diese Adressen gehen Anmelde-Token und API-Schlüssel. Deshalb nur
/// `https`; `http` ist allein für den eigenen Rechner erlaubt (Tests, lokaler Proxy).
fn parse_base(name: &str, raw: &str) -> Result<Url> {
    let raw = raw.trim();
    let with_slash = if raw.ends_with('/') {
        raw.to_string()
    } else {
        format!("{raw}/")
    };
    let url = Url::parse(&with_slash).map_err(|e| Error::Config(format!("{name}: {e}")))?;
    let loopback = match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err(Error::Config(format!(
            "{name} muss https verwenden. http ist nur für den eigenen Rechner erlaubt \
             (127.0.0.1, [::1], localhost)."
        ))),
    }
}

/// Spotify akzeptiert `http` nur für Loopback-Adressen (127.0.0.1, [::1]),
/// nicht für `localhost`. Alles andere muss `https` sein.
fn parse_redirect(raw: &str) -> Result<Url> {
    let url = Url::parse(raw.trim()).map_err(|e| Error::Config(format!("Redirect-URI: {e}")))?;
    let host = url.host_str().unwrap_or_default();
    match url.scheme() {
        "https" => Ok(url),
        "http" if host == "127.0.0.1" || host == "[::1]" => Ok(url),
        "http" if host == "localhost" => {
            warn!("Spotify lehnt „localhost“ als Redirect-URI ab; verwende http://127.0.0.1:<port>/callback");
            Ok(url)
        }
        _ => Err(Error::Config(
            "Die Redirect-URI muss https verwenden oder http://127.0.0.1:<port>/… sein.".into(),
        )),
    }
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("klangwerk-dj/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| Error::Config(format!("HTTP-Client: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_loopback_and_https_redirects() {
        assert!(parse_redirect("http://127.0.0.1:8888/callback").is_ok());
        assert!(parse_redirect("http://[::1]:8888/callback").is_ok());
        assert!(parse_redirect("https://example.org/cb").is_ok());
        assert!(parse_redirect("http://example.org/cb").is_err());
    }

    #[test]
    fn base_urls_get_trailing_slash() {
        let u = env_base("KLANGWERK_TEST_UNSET_VAR", "https://api.spotify.com/v1").expect("url");
        assert_eq!(
            u.join("me/player").expect("join").as_str(),
            "https://api.spotify.com/v1/me/player"
        );
    }

    #[test]
    fn base_urls_require_https_except_on_loopback() {
        for ok in [
            "https://api.example.org/v1",
            "http://127.0.0.1:9000/v1/",
            "http://[::1]:9000",
            "http://localhost:9000/claude",
        ] {
            assert!(parse_base("TEST", ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.example.org/",
            "http://192.168.1.10:8080/",
            "http://127.0.0.1.example.org/",
            "ftp://127.0.0.1/",
            "file:///etc/",
        ] {
            assert!(
                matches!(parse_base("TEST", bad), Err(Error::Config(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn parses_settings_file() {
        let text = "# Kommentar\nSPOTIFY_CLIENT_ID=abc\nexport ANTHROPIC_API_KEY=\"sk-ant-x=y\"\n\nKLANGWERK_AUFWAND='high'\nkaputt\n";
        let entries = parse_settings(text);
        assert_eq!(
            entries,
            vec![
                ("SPOTIFY_CLIENT_ID".to_string(), "abc".to_string()),
                ("ANTHROPIC_API_KEY".to_string(), "sk-ant-x=y".to_string()),
                ("KLANGWERK_AUFWAND".to_string(), "high".to_string()),
            ]
        );
        assert_eq!(parse_settings(&render_settings(&entries)), entries);
    }

    #[test]
    fn environment_wins_over_settings_and_unknown_keys_are_ignored() {
        let entries = vec![
            ("SPOTIFY_CLIENT_ID".to_string(), "aus-datei".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-ant-datei".to_string()),
            ("PATH".to_string(), "/boese".to_string()),
            ("KLANGWERK_MODELL".to_string(), String::new()),
        ];
        let apply = settings_to_apply(&entries, |k| k == "ANTHROPIC_API_KEY");
        assert_eq!(
            apply,
            vec![("SPOTIFY_CLIENT_ID".to_string(), "aus-datei".to_string())]
        );
    }

    #[test]
    fn credential_debug_hides_secret() {
        let c = Credential::ApiKey("sk-geheim".into());
        assert!(!format!("{c:?}").contains("geheim"));
    }
}
