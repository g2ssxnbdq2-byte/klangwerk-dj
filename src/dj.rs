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

/// Pause nach erfolglosem Nachplanen: 2, 4, 8, 16, dann höchstens 30 Minuten.
/// Jeder Versuch kann eine KI-Anfrage kosten, ein Dauerfehler soll das nicht
/// alle zwei Minuten tun.
fn refill_pause(failures: u32) -> Duration {
    let factor = 1u64 << failures.saturating_sub(1).min(4);
    Duration::from_secs((120 * factor).min(1800))
}

/// Pause nach abgewiesenem Einreihen: 3, 6, 12, dann 24 Sekunden. Der zweite
/// Versuch kommt schnell genug für den laufenden Titel, eine anhaltende
/// Abweisung löst trotzdem keine Anfrage im Drei-Sekunden-Takt aus.
fn enqueue_pause(rejections: u32) -> Duration {
    Duration::from_secs(3u64 << rejections.saturating_sub(1).min(3))
}

fn front_uri(queue: &VecDeque<Resolved>) -> Option<&str> {
    queue.front()?.track.as_ref().map(|t| t.uri.as_str())
}

pub struct Autopilot<E: Engine + 'static> {
    pub spotify: Arc<Spotify>,
    pub engine: Arc<E>,
    pub brief: String,
    /// Titel, an den der erste Plan anschließen sollte (`--ab`).
    pub start_after: Option<String>,
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
        // Woran die nächste Planung anschließt: der letzte Vorschlag, der wirklich
        // gespielt wird. Vorschläge ohne Spotify-Treffer zählen nicht, sonst plante
        // die KI den Übergang von einem Titel aus, der nie läuft. Es bleibt bei der
        // Schreibweise der KI; Spotify-Daten gehen nicht an sie zurück.
        let mut anchor: Option<String> = queue
            .back()
            .map(|r| r.suggestion.label())
            .or_else(|| self.start_after.clone());

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
        // Ein Einreihen mit offenem Ausgang (Zeitüberschreitung, 5xx): Der Titel
        // bleibt vorn im Plan, bis sich zeigt, ob Spotify ihn angenommen hat.
        let mut unconfirmed = false;
        let mut enqueue_rejections: u32 = 0;
        let mut refill: Option<JoinHandle<Result<Vec<Resolved>>>> = None;
        let mut refill_pause_until: Option<Instant> = None;
        let mut refill_failures: u32 = 0;
        let mut idle = Duration::from_secs(5);

        loop {
            if self.endless
                && refill.is_none()
                && queue.len() <= 2
                && refill_pause_until.map_or(true, |t| Instant::now() >= t)
            {
                refill = Some(self.spawn_refill(&history, anchor.clone()));
            }
            if refill.as_ref().is_some_and(|h| h.is_finished()) {
                if let Some(handle) = refill.take() {
                    let planned = match handle.await {
                        Ok(Ok(batch)) => {
                            let mut found = 0usize;
                            for r in batch {
                                let label = r.suggestion.label();
                                history.push(label.clone());
                                if r.track.is_some() {
                                    anchor = Some(label);
                                    found += 1;
                                    queue.push_back(r);
                                }
                            }
                            info!("{found} weitere Titel geplant");
                            found > 0
                        }
                        Ok(Err(e)) if e.is_fatal() => return Err(e),
                        Ok(Err(e)) => {
                            warn!("Nachplanen fehlgeschlagen: {e}");
                            false
                        }
                        Err(e) => {
                            warn!("Nachplanen abgebrochen: {e}");
                            false
                        }
                    };
                    if planned {
                        refill_failures = 0;
                    } else {
                        refill_failures = refill_failures.saturating_add(1);
                        let pause = refill_pause(refill_failures);
                        warn!(
                            "Kein neuer Titel geplant, nächster Versuch in {} Minuten",
                            pause.as_secs() / 60
                        );
                        refill_pause_until = Some(Instant::now() + pause);
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
                    if unconfirmed && front_uri(&queue) == Some(track.uri.as_str()) {
                        // Der Titel läuft: Das Einreihen mit offenem Ausgang ist doch
                        // angekommen. Ein zweiter Versuch würde ihn doppelt spielen.
                        if let Some(arrived) = queue.pop_front() {
                            announce("Läuft jetzt", &arrived);
                        }
                        unconfirmed = false;
                    }
                    let remaining = track.duration_ms.saturating_sub(progress_ms.unwrap_or(0));
                    let lead = u64::try_from(self.lead.as_millis()).unwrap_or(u64::MAX);
                    let mut retry_soon = false;
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
                                        unconfirmed = false;
                                        enqueue_rejections = 0;
                                    }
                                    Err(e) if e.is_fatal() => return Err(e),
                                    Err(e) => {
                                        warn!("Einreihen fehlgeschlagen: {e}");
                                        if e.not_processed() {
                                            // Sicher nicht angekommen: gleich noch einmal,
                                            // solange der laufende Titel nicht zu Ende ist.
                                            enqueue_rejections =
                                                enqueue_rejections.saturating_add(1);
                                            retry_soon = true;
                                        } else {
                                            // Ausgang offen: während dieses Titels kein
                                            // zweiter Versuch. Läuft danach etwas anderes,
                                            // wird der Titel dort erneut eingereiht.
                                            unconfirmed = true;
                                            queued_during = Some(track.uri.clone());
                                            enqueue_rejections = 0;
                                        }
                                        queue.push_front(next);
                                    }
                                }
                            }
                        }
                        if retry_soon {
                            enqueue_pause(enqueue_rejections)
                        } else {
                            Duration::from_millis(remaining + 1500)
                        }
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

    fn spawn_refill(
        &self,
        history: &[String],
        start_after: Option<String>,
    ) -> JoinHandle<Result<Vec<Resolved>>> {
        let spotify = Arc::clone(&self.spotify);
        let engine = Arc::clone(&self.engine);
        let request = PlanRequest {
            brief: self.brief.clone(),
            count: self.batch,
            start_after,
            avoid: history.to_vec(),
        };
        info!("Plane weitere Titel …");
        tokio::spawn(async move {
            let plan = engine.plan(&request).await?;
            resolve_all(&spotify, &plan).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refill_pause_doubles_up_to_half_an_hour() {
        let minutes: Vec<u64> = (1..=7).map(|n| refill_pause(n).as_secs() / 60).collect();
        assert_eq!(minutes, [2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(refill_pause(0), Duration::from_secs(120));
        assert_eq!(refill_pause(u32::MAX), Duration::from_secs(1800));
    }

    #[test]
    fn enqueue_pause_starts_short_and_stays_below_the_poll_limit() {
        let seconds: Vec<u64> = (1..=6).map(|n| enqueue_pause(n).as_secs()).collect();
        assert_eq!(seconds, [3, 6, 12, 24, 24, 24]);
        assert_eq!(enqueue_pause(0), Duration::from_secs(3));
        assert_eq!(enqueue_pause(u32::MAX), Duration::from_secs(24));
    }
}
