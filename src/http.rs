//! Gemeinsames HTTP-Verhalten: Wiederholungen bei vorübergehenden Fehlern,
//! `Retry-After` beachten, Körper vollständig einlesen.

use std::time::Duration;

use rand::Rng;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{RequestBuilder, StatusCode};
use tracing::warn;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_millis(800),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// Vollständig gelesene Antwort.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn text_lossy(&self, max_chars: usize) -> String {
        String::from_utf8_lossy(&self.body)
            .chars()
            .take(max_chars)
            .collect()
    }
}

pub fn retryable_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 409 | 429 | 500 | 502 | 503 | 504 | 529
    )
}

pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let raw = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    let secs: f64 = raw.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

pub fn backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(6);
    let jitter: f64 = rand::thread_rng().gen_range(0.0..0.25);
    policy
        .base_delay
        .saturating_mul(factor)
        .mul_f64(1.0 + jitter)
        .min(policy.max_delay)
}

/// Spotify meldet erschöpfte Kontingente als 429 mit `reason: QUOTA_EXCEEDED`.
/// Das löst sich nicht durch kurzes Warten, also nicht wiederholen.
fn quota_exhausted(reply: &Reply) -> bool {
    reply.status == StatusCode::TOO_MANY_REQUESTS
        && reply.body.windows(14).any(|w| w == b"QUOTA_EXCEEDED")
}

/// Sendet eine Anfrage und wiederholt sie bei vorübergehenden Fehlern.
///
/// `make` baut die Anfrage für jeden Versuch neu. Bei `idempotent == false`
/// wird nur wiederholt, wenn der Server die Anfrage sicher nicht verarbeitet
/// hat (Verbindungsaufbau gescheitert, 429, 529).
pub async fn send_with_retry<F>(
    service: &'static str,
    policy: &RetryPolicy,
    idempotent: bool,
    mut make: F,
) -> Result<Reply>
where
    F: FnMut() -> RequestBuilder,
{
    let mut attempt: u32 = 1;
    loop {
        let delay = match make().send().await {
            Ok(resp) => {
                let status = resp.status();
                let headers = resp.headers().clone();
                match resp.bytes().await {
                    Ok(bytes) => {
                        let reply = Reply {
                            status,
                            headers,
                            body: bytes.to_vec(),
                        };
                        let may_retry = retryable_status(status)
                            && !quota_exhausted(&reply)
                            && (idempotent || matches!(status.as_u16(), 429 | 529));
                        if !may_retry || attempt >= policy.max_attempts {
                            return Ok(reply);
                        }
                        let delay = match retry_after(&reply.headers) {
                            Some(d) if d > policy.max_delay => return Ok(reply),
                            Some(d) => d,
                            None => backoff(policy, attempt),
                        };
                        warn!(
                            service,
                            status = status.as_u16(),
                            attempt,
                            "vorübergehender Fehler, neuer Versuch in {:.1} s",
                            delay.as_secs_f64()
                        );
                        delay
                    }
                    Err(e) => {
                        if !idempotent || attempt >= policy.max_attempts {
                            return Err(Error::Network { service, source: e });
                        }
                        let delay = backoff(policy, attempt);
                        warn!(service, attempt, "Antwort abgebrochen ({e}), neuer Versuch");
                        delay
                    }
                }
            }
            Err(e) => {
                let transient =
                    e.is_connect() || (idempotent && (e.is_timeout() || e.is_request()));
                if !transient || attempt >= policy.max_attempts {
                    return Err(Error::Network { service, source: e });
                }
                let delay = backoff(policy, attempt);
                warn!(service, attempt, "Verbindungsproblem ({e}), neuer Versuch");
                delay
            }
        };
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn response(status_line: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status_line}\r\n{extra}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn mock(responses: Vec<String>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            for r in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = sock.write_all(r.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/"), hits)
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(5),
            max_delay: Duration::from_secs(2),
        }
    }

    #[tokio::test]
    async fn retries_overloaded_then_succeeds() {
        let (url, hits) = mock(vec![
            response("529 Overloaded", "retry-after: 0\r\n", "{}"),
            response("200 OK", "", "{\"ok\":true}"),
        ])
        .await;
        let client = reqwest::Client::new();
        let reply = send_with_retry("Test", &fast(), true, || client.get(&url))
            .await
            .expect("reply");
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.json().expect("json")["ok"], true);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn quota_exceeded_is_not_retried() {
        let body =
            r#"{"error":{"status":429,"message":"Too many requests","reason":"QUOTA_EXCEEDED"}}"#;
        let (url, hits) = mock(vec![
            response("429 Too Many Requests", "retry-after: 0\r\n", body),
            response("200 OK", "", "{}"),
        ])
        .await;
        let client = reqwest::Client::new();
        let reply = send_with_retry("Test", &fast(), true, || client.get(&url))
            .await
            .expect("reply");
        assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_idempotent_request_is_not_retried_on_server_error() {
        let (url, hits) = mock(vec![
            response("503 Service Unavailable", "", "{}"),
            response("200 OK", "", "{}"),
        ])
        .await;
        let client = reqwest::Client::new();
        let reply = send_with_retry("Test", &fast(), false, || client.post(&url))
            .await
            .expect("reply");
        assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parses_retry_after_seconds() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "2".parse().expect("header"));
        assert_eq!(retry_after(&h), Some(Duration::from_secs(2)));
        h.insert(RETRY_AFTER, "-1".parse().expect("header"));
        assert_eq!(retry_after(&h), None);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        assert!(backoff(&p, 1) >= Duration::from_millis(100));
        assert!(backoff(&p, 2) >= Duration::from_millis(200));
        assert_eq!(backoff(&p, 9), Duration::from_millis(500));
    }
}
