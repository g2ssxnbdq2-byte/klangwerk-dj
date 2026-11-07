//! KI-Engine: plant eine Titelfolge mit Übergängen.
//!
//! Wichtig für die Spotify-Regeln: Die KI bekommt nur, was der Nutzer selbst
//! eingibt, und ihre eigenen früheren Vorschläge. Daten aus der Spotify-API
//! (laufender Titel, Suchtreffer, Metadaten) werden ihr nie übergeben.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info};
use url::Url;

use crate::config::Credential;
use crate::error::{Error, Result};
use crate::http::{send_with_retry, Reply, RetryPolicy};
use crate::matching::normalize;

pub const API_VERSION: &str = "2026-10-21";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Modelle, die `fallbacks: "default"` annehmen.
const FALLBACK_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-fable-5-1",
    "claude-sonnet-5-5",
];
const MAX_AVOID: usize = 200;

#[derive(Debug, Clone)]
pub struct PlanRequest {
    /// Wunsch des Nutzers in eigenen Worten.
    pub brief: String,
    pub count: usize,
    /// Titel, an den die Folge anschließen soll (vom Nutzer genannt oder ein früherer KI-Vorschlag).
    pub start_after: Option<String>,
    /// Frühere KI-Vorschläge, die nicht wiederholt werden sollen.
    pub avoid: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub artist: String,
    pub title: String,
    pub bpm_estimate: Option<u32>,
    pub key_estimate: Option<String>,
    pub energy: u8,
    pub transition: String,
}

impl Suggestion {
    pub fn label(&self) -> String {
        format!("{} – {}", self.artist, self.title)
    }

    pub fn facts(&self) -> String {
        let mut facts = Vec::new();
        if let Some(b) = self.bpm_estimate {
            facts.push(format!("≈{b} BPM"));
        }
        if let Some(k) = &self.key_estimate {
            facts.push(k.clone());
        }
        facts.push(format!("Energie {}/10", self.energy));
        facts.join(" · ")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub session_name: String,
    pub energy_arc: String,
    pub tracks: Vec<Suggestion>,
}

/// Austauschbare KI-Engine.
pub trait Engine: Send + Sync {
    fn plan(&self, request: &PlanRequest) -> impl Future<Output = Result<Plan>> + Send;
}

const SYSTEM_PROMPT: &str = "\
Du bist ein erfahrener Musikkurator und DJ. Du planst eine Titelfolge, die über Spotify \
nacheinander abgespielt wird. Die Titel laufen ohne Überblendung direkt hintereinander; \
der Übergang entsteht allein durch Auswahl und Reihenfolge.

So arbeitest du:
- Schlage nur real veröffentlichte Aufnahmen vor, die sehr wahrscheinlich auf Spotify verfügbar sind. Erfinde keine Titel.
- Schreibe Interpret und Titel so, wie sie offiziell veröffentlicht wurden, ohne Zusätze wie „Remastered“ oder „Radio Edit“. Bei mehreren Interpreten nenne den Hauptinterpreten.
- Ordne die Titel so, dass jeder gut auf den vorherigen folgt: ähnliches Tempo (oder halbes/doppeltes), harmonisch passende Tonarten (benachbart auf dem Camelot-Rad), Energie in kleinen Schritten. Einen bewussten Bruch nur, wenn der Wunsch ihn nahelegt.
- Nicht derselbe Interpret zweimal direkt hintereinander, kein Titel doppelt, nichts aus der Liste der bereits vorgeschlagenen Titel.
- bpm_estimate und key_estimate sind Schätzungen aus deinem Wissen (Tonart als Camelot-Code mit Tonartname, z. B. „8A / a-Moll“). Wenn du unsicher bist, gib null an.
- energy: ganze Zahl von 1 (sehr ruhig) bis 10 (Höhepunkt).
- transition: ein kurzer deutscher Satz, warum der Titel auf den vorherigen folgt; beim ersten Titel, warum er zum Wunsch oder zum genannten Anschlusstitel passt.
- session_name: kurzer deutscher Name für die Folge. energy_arc: ein deutscher Satz zum Energieverlauf.";

pub fn plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session_name": { "type": "string" },
            "energy_arc": { "type": "string" },
            "tracks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "artist": { "type": "string" },
                        "title": { "type": "string" },
                        "bpm_estimate": { "anyOf": [{ "type": "integer" }, { "type": "null" }] },
                        "key_estimate": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                        "energy": { "type": "integer" },
                        "transition": { "type": "string" }
                    },
                    "required": ["artist", "title", "bpm_estimate", "key_estimate", "energy", "transition"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["session_name", "energy_arc", "tracks"],
        "additionalProperties": false
    })
}

pub fn user_message(req: &PlanRequest) -> String {
    let mut msg = format!(
        "Wunsch: {}\nAnzahl: genau {} Titel",
        req.brief.trim(),
        req.count
    );
    if let Some(start) = req
        .start_after
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        msg.push_str(&format!("\nDie Folge schließt an diesen Titel an: {start}"));
    }
    if !req.avoid.is_empty() {
        msg.push_str("\nBereits vorgeschlagen, bitte nicht wiederholen:");
        let skip = req.avoid.len().saturating_sub(MAX_AVOID);
        for entry in req.avoid.iter().skip(skip) {
            msg.push_str("\n- ");
            msg.push_str(entry);
        }
    }
    msg
}

/// Claude über die Messages-API (rohes HTTP; für Rust gibt es kein offizielles SDK).
pub struct Claude {
    http: reqwest::Client,
    endpoint: Url,
    credential: Credential,
    model: String,
    effort: String,
    fallbacks: bool,
    retry: RetryPolicy,
}

impl Claude {
    pub fn new(
        http: reqwest::Client,
        base: &Url,
        credential: Option<Credential>,
        model: String,
        effort: String,
        fallbacks: bool,
    ) -> Result<Self> {
        let credential = credential.ok_or_else(|| {
            Error::Config("ANTHROPIC_API_KEY ist nicht gesetzt (oder ANTHROPIC_AUTH_TOKEN).".into())
        })?;
        let endpoint = base
            .join("v1/messages")
            .map_err(|e| Error::Config(format!("ANTHROPIC_BASE_URL: {e}")))?;
        Ok(Self {
            http,
            endpoint,
            credential,
            model,
            effort,
            fallbacks,
            retry: RetryPolicy::default(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn uses_fallbacks(&self) -> bool {
        self.fallbacks && FALLBACK_MODELS.contains(&self.model.as_str())
    }

    pub fn request_body(&self, req: &PlanRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": 16000,
            "system": SYSTEM_PROMPT,
            "output_config": {
                "effort": self.effort,
                "format": { "type": "json_schema", "schema": plan_schema() }
            },
            "messages": [{ "role": "user", "content": user_message(req) }]
        });
        if self.uses_fallbacks() {
            body["fallbacks"] = json!("default");
        }
        body
    }

    fn beta_header(&self) -> Option<String> {
        let mut betas = Vec::new();
        if self.uses_fallbacks() {
            betas.push(FALLBACK_BETA);
        }
        if matches!(self.credential, Credential::OAuth(_)) {
            betas.push(OAUTH_BETA);
        }
        (!betas.is_empty()).then(|| betas.join(","))
    }
}

impl Engine for Claude {
    async fn plan(&self, req: &PlanRequest) -> Result<Plan> {
        let body = self.request_body(req);
        let beta = self.beta_header();
        let reply = send_with_retry("Claude", &self.retry, true, || {
            let mut builder = self
                .http
                .post(self.endpoint.clone())
                .header("anthropic-version", API_VERSION)
                .timeout(Duration::from_secs(300))
                .json(&body);
            builder = match &self.credential {
                Credential::ApiKey(key) => builder.header("x-api-key", key),
                Credential::OAuth(token) => builder.bearer_auth(token),
            };
            if let Some(b) = &beta {
                builder = builder.header("anthropic-beta", b);
            }
            builder
        })
        .await?;

        if !reply.status.is_success() {
            return Err(api_error(&reply));
        }
        let response: Value =
            serde_json::from_slice(&reply.body).map_err(|e| unexpected(e.to_string()))?;
        let served_by = response["model"].as_str().unwrap_or("?");
        info!(
            modell = served_by,
            eingabe_tokens = response["usage"]["input_tokens"].as_u64().unwrap_or(0),
            ausgabe_tokens = response["usage"]["output_tokens"].as_u64().unwrap_or(0),
            "KI-Antwort erhalten"
        );
        if served_by != self.model {
            debug!("Antwort kam vom Ausweichmodell {served_by}");
        }
        parse_plan(&response, req.count)
    }
}

fn unexpected(message: impl Into<String>) -> Error {
    Error::Unexpected {
        service: "Claude",
        message: message.into(),
    }
}

fn api_error(reply: &Reply) -> Error {
    let body = reply.json().unwrap_or_default();
    let kind = body["error"]["type"].as_str().unwrap_or("unbekannt");
    let message = body["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| reply.text_lossy(200));
    let hint = match reply.status.as_u16() {
        401 => " (API-Schlüssel prüfen)",
        402 => " (Guthaben/Abrechnung prüfen)",
        _ => "",
    };
    Error::Api {
        service: "Claude",
        status: reply.status.as_u16(),
        message: format!("{kind}: {message}{hint}"),
    }
}

#[derive(Deserialize)]
struct RawPlan {
    session_name: String,
    energy_arc: String,
    tracks: Vec<RawSuggestion>,
}

#[derive(Deserialize)]
struct RawSuggestion {
    artist: String,
    title: String,
    bpm_estimate: Option<i64>,
    key_estimate: Option<String>,
    energy: i64,
    transition: String,
}

/// Wertet eine Messages-API-Antwort aus. Prüft zuerst `stop_reason`, dann
/// werden nur Textblöcke gelesen (Thinking- und Fallback-Blöcke ignoriert).
pub fn parse_plan(response: &Value, count: usize) -> Result<Plan> {
    match response["stop_reason"].as_str() {
        Some("refusal") => {
            return Err(Error::Refused {
                category: response["stop_details"]["category"]
                    .as_str()
                    .unwrap_or("ohne Angabe")
                    .to_string(),
            })
        }
        Some("max_tokens") => return Err(Error::Truncated),
        _ => {}
    }
    let text: String = response["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect()
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err(unexpected("Die Antwort enthält keinen Text."));
    }
    let raw: RawPlan = serde_json::from_str(text.trim())
        .map_err(|e| unexpected(format!("JSON passt nicht zum Schema: {e}")))?;

    let mut seen = HashSet::new();
    let tracks: Vec<Suggestion> = raw
        .tracks
        .into_iter()
        .filter_map(|s| {
            let artist = s.artist.trim().to_string();
            let title = s.title.trim().to_string();
            if artist.is_empty()
                || title.is_empty()
                || !seen.insert((normalize(&artist), normalize(&title)))
            {
                return None;
            }
            Some(Suggestion {
                artist,
                title,
                bpm_estimate: s
                    .bpm_estimate
                    .filter(|b| (40..=250).contains(b))
                    .and_then(|b| u32::try_from(b).ok()),
                key_estimate: s
                    .key_estimate
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty()),
                energy: u8::try_from(s.energy.clamp(1, 10)).unwrap_or(5),
                transition: s.transition.trim().to_string(),
            })
        })
        .take(count)
        .collect();
    if tracks.is_empty() {
        return Err(unexpected("Die KI hat keine verwertbaren Titel geliefert."));
    }
    Ok(Plan {
        session_name: raw.session_name.trim().to_string(),
        energy_arc: raw.energy_arc.trim().to_string(),
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude(model: &str, credential: Credential) -> Claude {
        Claude::new(
            reqwest::Client::new(),
            &Url::parse("https://api.anthropic.com/").expect("url"),
            Some(credential),
            model.into(),
            "medium".into(),
            true,
        )
        .expect("client")
    }

    fn request() -> PlanRequest {
        PlanRequest {
            brief: "Sonntagmorgen, ruhig".into(),
            count: 3,
            start_after: Some("Norah Jones – Sunrise".into()),
            avoid: vec!["A – B".into()],
        }
    }

    #[test]
    fn request_body_uses_structured_output_and_fallbacks() {
        let c = claude("claude-opus-5-5", Credential::ApiKey("k".into()));
        let body = c.request_body(&request());
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("thinking").is_none());
        assert!(body.get("temperature").is_none());
        let content = body["messages"][0]["content"].as_str().expect("text");
        assert!(content.contains("genau 3 Titel"));
        assert!(content.contains("Norah Jones – Sunrise"));
        assert!(content.contains("- A – B"));
        assert_eq!(c.beta_header().as_deref(), Some(FALLBACK_BETA));
    }

    #[test]
    fn no_fallbacks_for_models_without_support() {
        let c = claude("claude-haiku-5-5", Credential::OAuth("t".into()));
        assert!(c.request_body(&request()).get("fallbacks").is_none());
        assert_eq!(c.beta_header().as_deref(), Some(OAUTH_BETA));
    }

    #[test]
    fn schema_closes_every_object() {
        let schema = plan_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["tracks"]["items"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn reads_text_after_thinking_and_fallback_blocks() {
        let plan_json = r#"{"session_name":"Morgenlicht","energy_arc":"Ruhig, dann wärmer.","tracks":[
            {"artist":"Norah Jones","title":"Don't Know Why","bpm_estimate":88,"key_estimate":"4B / B♭-Dur","energy":3,"transition":"Sanfter Einstieg."},
            {"artist":"Norah Jones","title":"Don't Know Why","bpm_estimate":88,"key_estimate":null,"energy":3,"transition":"Doppelt."},
            {"artist":"Bill Withers","title":"Lovely Day","bpm_estimate":900,"key_estimate":"  ","energy":15,"transition":"Mehr Wärme."}]}"#;
        let response = json!({
            "model": "claude-opus-5-5",
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "", "signature": "x" },
                { "type": "fallback", "from": { "model": "a" }, "to": { "model": "b" } },
                { "type": "text", "text": plan_json }
            ]
        });
        let plan = parse_plan(&response, 10).expect("plan");
        assert_eq!(plan.session_name, "Morgenlicht");
        assert_eq!(plan.tracks.len(), 2, "Doppelter Titel wird entfernt");
        assert_eq!(
            plan.tracks[1].bpm_estimate, None,
            "unplausible BPM verworfen"
        );
        assert_eq!(plan.tracks[1].key_estimate, None);
        assert_eq!(plan.tracks[1].energy, 10, "Energie auf 1–10 begrenzt");
        assert_eq!(parse_plan(&response, 1).expect("plan").tracks.len(), 1);
    }

    #[test]
    fn reports_refusal_and_truncation() {
        let refusal = json!({ "stop_reason": "refusal", "stop_details": { "category": "cyber" }, "content": [] });
        assert!(
            matches!(parse_plan(&refusal, 3), Err(Error::Refused { category }) if category == "cyber")
        );
        let truncated =
            json!({ "stop_reason": "max_tokens", "content": [{ "type": "text", "text": "{" }] });
        assert!(matches!(parse_plan(&truncated, 3), Err(Error::Truncated)));
        let empty = json!({ "stop_reason": "end_turn", "content": [] });
        assert!(parse_plan(&empty, 3).is_err());
    }

    #[test]
    fn avoid_list_is_capped() {
        let mut req = request();
        req.avoid = (0..500).map(|i| format!("X – {i}")).collect();
        let msg = user_message(&req);
        assert!(!msg.contains("X – 299\n"));
        assert!(msg.contains("X – 499"));
        assert_eq!(msg.matches("\n- ").count(), MAX_AVOID);
    }
}
