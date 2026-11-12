//! Ordnet KI-Vorschläge (Interpret, Titel) den Treffern der Spotify-Suche zu.
//! Lieber einen Titel auslassen als einen falschen einreihen.

use std::collections::HashSet;

use crate::spotify::Track;

fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

/// Kleinschreibung, ohne Klammerzusätze, ohne „ - Remastered …“, ohne „feat. …“.
/// Besteht ein Name nur aus einem solchen Zusatz – etwa „(Nice Dream)“ –,
/// bleibt der ganze Text erhalten.
pub fn normalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut kept = String::with_capacity(lower.len());
    let mut depth = 0u32;
    for c in lower.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    let base = [" - ", " – ", " — "]
        .iter()
        .filter_map(|sep| kept.find(sep))
        .filter(|&i| i > 0)
        .min()
        .map_or(kept.as_str(), |i| &kept[..i]);
    let core = words(base)
        .take_while(|t| !matches!(*t, "feat" | "ft" | "featuring"))
        .collect::<Vec<_>>()
        .join(" ");
    if core.is_empty() {
        words(&lower).collect::<Vec<_>>().join(" ")
    } else {
        core
    }
}

/// Jaccard-Ähnlichkeit der Wortmengen zweier normalisierter Texte.
pub fn similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let ta: HashSet<&str> = a.split(' ').collect();
    let tb: HashSet<&str> = b.split(' ').collect();
    let common = ta.intersection(&tb).count() as f64;
    let all = ta.union(&tb).count() as f64;
    common / all
}

/// `needle` steht als zusammenhängende Wortfolge in `haystack`: „beatles“ in
/// „the beatles“, aber nicht „daf“ in „daft punk“.
fn contains_words(haystack: &str, needle: &str) -> bool {
    let hay: Vec<&str> = haystack.split(' ').collect();
    let need: Vec<&str> = needle.split(' ').collect();
    hay.windows(need.len()).any(|w| w == need.as_slice())
}

fn artist_matches(wanted: &str, candidate: &str) -> bool {
    let n = normalize(candidate);
    !n.is_empty()
        && (n == wanted
            || similarity(&n, wanted) >= 0.5
            || contains_words(wanted, &n)
            || contains_words(&n, wanted))
}

pub fn best_match<'a>(artist: &str, title: &str, candidates: &'a [Track]) -> Option<&'a Track> {
    let want_artist = normalize(artist);
    let want_title = normalize(title);
    if want_artist.is_empty() || want_title.is_empty() {
        return None;
    }
    let mut best: Option<(&Track, f64)> = None;
    for track in candidates {
        if !track
            .artists
            .iter()
            .any(|a| artist_matches(&want_artist, &a.name))
        {
            continue;
        }
        let score = similarity(&normalize(&track.name), &want_title);
        if score >= 0.5 && best.map_or(true, |(_, b)| score > b) {
            best = Some((track, score));
        }
    }
    best.map(|(t, _)| t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::api::Artist;

    fn track(artist: &str, name: &str) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            duration_ms: 1,
            artists: vec![Artist {
                name: artist.into(),
            }],
        }
    }

    #[test]
    fn normalizes_versions_and_features() {
        assert_eq!(
            normalize("Get Lucky (feat. Pharrell Williams) - Radio Edit"),
            "get lucky"
        );
        assert_eq!(normalize("Hey Jude - Remastered 2015"), "hey jude");
        assert_eq!(normalize("Song ft. Somebody"), "song");
        assert_eq!(normalize("Leftovers"), "leftovers");
        assert_eq!(normalize("Für Elise"), "für elise");
    }

    #[test]
    fn keeps_names_that_consist_only_of_an_addition() {
        assert_eq!(normalize("(Nice Dream)"), "nice dream");
        assert_eq!(normalize("[Untitled]"), "untitled");
        let found = vec![track("Radiohead", "(Nice Dream)")];
        assert!(best_match("Radiohead", "(Nice Dream)", &found).is_some());
    }

    #[test]
    fn artist_must_match_whole_words() {
        let found = vec![track("Daft Punk", "One More Time")];
        assert!(best_match("Daf", "One More Time", &found).is_none());
        assert!(best_match("Daft Punk", "One More Time", &found).is_some());
        // Ein Interpret namens „A“ steckt als Buchstabe in fast jedem Namen.
        let found = vec![track("A", "Hey Jude")];
        assert!(best_match("The Beatles", "Hey Jude", &found).is_none());
        // Nennt die KI zwei Interpreten, genügt einer davon.
        let found = vec![track("Daft Punk", "Get Lucky")];
        assert!(best_match("Daft Punk, Pharrell Williams", "Get Lucky", &found).is_some());
    }

    #[test]
    fn picks_matching_artist_and_title() {
        let found = vec![
            track("Tribute Band", "Get Lucky"),
            track("Daft Punk", "Get Lucky - Radio Edit"),
            track("Daft Punk", "Lose Yourself to Dance"),
        ];
        let m = best_match("Daft Punk", "Get Lucky", &found).expect("match");
        assert_eq!(m.uri, "spotify:track:Get Lucky - Radio Edit");
    }

    #[test]
    fn rejects_wrong_artist() {
        let found = vec![track("Cover Kids", "Hey Jude")];
        assert!(best_match("The Beatles", "Hey Jude", &found).is_none());
    }

    #[test]
    fn tolerates_articles_in_artist_names() {
        let found = vec![track("Beatles", "Hey Jude")];
        assert!(best_match("The Beatles", "Hey Jude", &found).is_some());
    }
}
