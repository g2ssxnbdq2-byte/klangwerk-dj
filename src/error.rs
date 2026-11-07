//! Fehlertypen des Programms. Meldungen sind für Menschen geschrieben.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Konfiguration: {0}")]
    Config(String),

    #[error("Nicht bei Spotify angemeldet. Bitte zuerst `klangwerk-dj anmelden` ausführen.")]
    NotLoggedIn,

    #[error("Die Spotify-Anmeldung ist abgelaufen oder wurde widerrufen. Bitte `klangwerk-dj anmelden` erneut ausführen.")]
    LoginExpired,

    #[error("Anmeldung fehlgeschlagen: {0}")]
    Auth(String),

    #[error(
        "Kein aktives Spotify-Gerät. Öffne Spotify auf einem Gerät und starte kurz die Wiedergabe."
    )]
    NoActiveDevice,

    #[error("Diese Funktion verlangt Spotify Premium.")]
    PremiumRequired,

    #[error(
        "Das Spotify-Kontingent ist aufgebraucht (QUOTA_EXCEEDED). Bitte später erneut versuchen."
    )]
    QuotaExceeded,

    #[error("{service} meldet HTTP {status}: {message}")]
    Api {
        service: &'static str,
        status: u16,
        message: String,
    },

    #[error("Netzwerkfehler bei {service}: {source}")]
    Network {
        service: &'static str,
        #[source]
        source: reqwest::Error,
    },

    #[error("Die KI hat die Anfrage abgelehnt (Kategorie: {category}).")]
    Refused { category: String },

    #[error("Die KI-Antwort wurde abgeschnitten, weil das Token-Limit erreicht war.")]
    Truncated,

    #[error("Unerwartete Antwort von {service}: {message}")]
    Unexpected {
        service: &'static str,
        message: String,
    },

    #[error("Datei {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// Fehler, bei denen ein Weiterlaufen (z. B. im Autopilot) keinen Sinn ergibt.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Error::NotLoggedIn
                | Error::LoginExpired
                | Error::QuotaExceeded
                | Error::PremiumRequired
                | Error::Config(_)
                | Error::Auth(_)
                | Error::Io { .. }
                | Error::Api {
                    status: 400 | 401 | 403,
                    ..
                }
        )
    }

    pub fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
