//! Vorschläge auf Spotify auflösen, anzeigen und als Autopilot zeitgenau einreihen.
//!
//! Übergänge: Spotify verbietet Apps, Titel zu mischen, zu überblenden oder zu
//! überlagern. Der Autopilot steuert deshalb nur, *welcher* Titel als Nächstes
//! kommt, und reiht ihn kurz vor Ende des laufenden Titels ein. So bleibt die
//! geplante Reihenfolge erhalten, auch wenn man zwischendurch selbst etwas wählt.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::ai::{Engine, Plan, PlanRequest, Suggestion};
use crate::error::{Error, Result};
use crate::matching::best_match;
use crate::spotify::api::CurrentlyPlaying;
use crate::spotify::{Spotify, Track};

#[derive(Debug, Clone, Serialize)]
pub struct Resolved {
    pub suggestion: Suggestion,
    pub track: Option<Track>,
}

pub async fn resolve_one(spotify: &Spotify, s: &Suggestion) -> Result<Option<Track>> {
    let clean = |x: &str| x.replace('"', "");
    let queries = [
        format!(
            "track:\"{}\" artist:\"{}\"",
            clean(&s.title),
            clean(&s.artist)
        ),
        format!("{} {}", clean(&s.artist), clean(&s.title)),
    ];
    for query in &queries {
        let found = spotify.search_tracks(query, 5).await?;
        if let Some(track) = best_match(&s.artist, &s.title, &found) {
            return Ok(Some(track.clone()));
        }
    }
    Ok(None)
}

pub async fn resolve_all(spotify: &Spotify, plan: &Plan) -> Result<Vec<Resolved>> {
    let mut out = Vec::with_capacity(plan.tracks.len());
    for suggestion in &plan.tracks {
        let track = match resolve_one(spotify, suggestion).await {
            Ok(t) => t,
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                warn!("Suche nach „{}“ fehlgeschlagen: {e}", suggestion.label());
                None
            }
        };
        out.push(Resolved {
            suggestion: suggestion.clone(),
            track,
        });
    }
    Ok(out)
}

pub fn print_plan(plan: &Plan, resolved: Option<&[Resolved]>) {
    println!("{}", plan.session_name);
    println!("{}\n", plan.energy_arc);
    for (i, s) in plan.tracks.iter().enumerate() {
        let entry = resolved.and_then(|r| r.get(i));
        let mark = match entry {
            None => "",
            Some(r) if r.track.is_some() => "✓ ",
            Some(_) => "✗ ",
        };
        println!("{:>2}. {mark}{}  [{}]", i + 1, s.label(), s.facts());
        println!("    ↳ {}", s.transition);
        if let Some(track) = entry.and_then(|r| r.track.as_ref()) {
            let spotify_label = track.label();
            if spotify_label.to_lowercase() != s.label().to_lowercase() {
                println!("    Spotify: {spotify_label}");
            }
        }
    }
    if let Some(r) = resolved {
        let missing = r.iter().filter(|x| x.track.is_none()).count();
        if missing > 0 {
            println!("\nOhne eindeutigen Spotify-Treffer, daher übersprungen (✗): {missing}");
        }
    }
    println!("\nBPM und Tonart sind Schätzungen der KI.");
}

fn announce(prefix: &str, r: &Resolved) {
    let s = &r.suggestion;
    println!("{prefix}: {}  [{}]", s.label(), s.facts());
    println!("   ↳ {}", s.transition);
}

pub struct Autopilot<E: Engine + 'static> {
    pub spotify: Arc<Spotify>,
    pub engine: Arc<E>,
    pub brief: String,
    pub batch: usize,
    pub lead: Duration,
    pub endless: bool,
    pub device: Option<String>,
}

impl<E: Engine + 'static> Autopilot<E> {
    pub async fn run(
        self,
        initial: Vec<Resolved>,
        mut history: Vec<String>,
        start: bool,
    ) -> Result<()> {
        let mut queue: VecDeque<Resolved> =
            initial.into_iter().filter(|r| r.track.is_some()).collect();
        if queue.is_empty() && !self.endless {
            return Err(Error::Unexpected {
                service: "Autopilot",
                message: "Keiner der vorgeschlagenen Titel wurde auf Spotify gefunden.".into(),
            });
        }
        let device = self.device.as_deref();

        if start {
            let playing =
                matches!(self.spotify.currently_playing().await?, Some(cp) if cp.is_playing);
            if !playing {
                if let Some(first) = queue.pop_front() {
                    let uri = first
                        .track
                        .as_ref()
                        .map(|t| t.uri.clone())
                        .unwrap_or_default();
                    self.spotify.play(&[uri], device).await?;
                    announce("Start", &first);
                }
            }
        }

        println!("\nAutopilot läuft. Strg+C beendet ihn; die Wiedergabe in Spotify läuft weiter.");
        let mut queued_during: Option<String> = None;
        let mut refill: Option<JoinHandle<Result<Vec<Resolved>>>> = None;
        let mut refill_pause_until: Option<Instant> = None;
        let mut idle = Duration::from_secs(5);

        loop {
            if self.endless
                && refill.is_none()
                && queue.len() <= 2
                && refill_pause_until.map_or(true, |t| Instant::now() >= t)
            {
                refill = Some(self.spawn_refill(&history));
            }
            if refill.as_ref().is_some_and(|h| h.is_finished()) {
                if let Some(handle) = refill.take() {
                    match handle.await {
                        Ok(Ok(batch)) => {
                            let found = batch.iter().filter(|r| r.track.is_some()).count();
                            for r in batch {
                                history.push(r.suggestion.label());
                                if r.track.is_some() {
                                    queue.push_back(r);
                                }
                            }
                            info!("{found} weitere Titel geplant");
                            if found == 0 {
                                refill_pause_until =
                                    Some(Instant::now() + Duration::from_secs(120));
                            }
                        }
                        Ok(Err(e)) if e.is_fatal() => return Err(e),
                        Ok(Err(e)) => {
                            warn!("Nachplanen fehlgeschlagen: {e}");
                            refill_pause_until = Some(Instant::now() + Duration::from_secs(120));
                        }
                        Err(e) => {
                            warn!("Nachplanen abgebrochen: {e}");
                            refill_pause_until = Some(Instant::now() + Duration::from_secs(120));
                        }
                    }
                }
            }
            if queue.is_empty() && !self.endless {
                println!("Alle geplanten Titel sind eingereiht. Autopilot beendet.");
                return Ok(());
            }

            let wait = match self.spotify.currently_playing().await {
                Ok(Some(CurrentlyPlaying {
                    is_playing: true,
                    item: Some(track),
                    progress_ms,
                    ..
                })) => {
                    idle = Duration::from_secs(5);
                    let remaining = track.duration_ms.saturating_sub(progress_ms.unwrap_or(0));
                    let lead = u64::try_from(self.lead.as_millis()).unwrap_or(u64::MAX);
                    let wait = if remaining <= lead {
                        if queued_during.as_deref() != Some(track.uri.as_str()) {
                            if let Some(next) = queue.pop_front() {
                                let uri = next
                                    .track
                                    .as_ref()
                                    .map(|t| t.uri.clone())
                                    .unwrap_or_default();
                                match self.spotify.add_to_queue(&uri, device).await {
                                    Ok(()) => {
                                        announce("Als Nächstes", &next);
                                        queued_during = Some(track.uri.clone());
                                    }
                                    Err(e) if e.is_fatal() => return Err(e),
                                    Err(e) => {
                                        warn!("Einreihen fehlgeschlagen: {e}");
                                        queue.push_front(next);
                                    }
                                }
                            }
                        }
                        Duration::from_millis(remaining + 1500)
                    } else {
                        Duration::from_millis(remaining - lead)
                    };
                    wait.clamp(Duration::from_secs(1), Duration::from_secs(30))
                }
                Ok(_) => {
                    let w = idle;
                    idle = (idle * 2).min(Duration::from_secs(60));
                    w
                }
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => {
                    warn!("Wiedergabestatus nicht abrufbar: {e}");
                    Duration::from_secs(15)
                }
            };

            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = tokio::signal::ctrl_c() => {
                    println!("\nAutopilot beendet.");
                    return Ok(());
                }
            }
        }
    }

    fn spawn_refill(&self, history: &[String]) -> JoinHandle<Result<Vec<Resolved>>> {
        let spotify = Arc::clone(&self.spotify);
        let engine = Arc::clone(&self.engine);
        let request = PlanRequest {
            brief: self.brief.clone(),
            count: self.batch,
            start_after: history.last().cloned(),
            avoid: history.to_vec(),
        };
        info!("Plane weitere Titel …");
        tokio::spawn(async move {
            let plan = engine.plan(&request).await?;
            resolve_all(&spotify, &plan).await
        })
    }
}
