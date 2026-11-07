# klangwerk-dj

Kommandozeilenprogramm in Rust. Eine KI (Claude) plant aus deinem Wunsch eine Titelfolge mit
Übergängen. Das Programm sucht die Titel auf Spotify und reiht sie zur richtigen Zeit in deine
Wiedergabe ein.

```
$ klangwerk-dj vorschlag "Sonntagmorgen, ruhig, Jazz bis Soul" -n 4
Sonntagmorgen in Moll und Dur
Beginnt leise, wird gegen Ende wärmer und etwas tanzbarer.

 1. ✓ Norah Jones – Don't Know Why  [≈88 BPM · 4B / B♭-Dur · Energie 3/10]
    ↳ Sanfter Einstieg, passt zur ruhigen Morgenstimmung.
 2. ✓ …
```
(Beispielausgabe, gekürzt)

## Wie die Übergänge funktionieren

Spotify verbietet Apps, Titel zu mischen, zu überblenden oder zu überlagern
([Developer Policy](https://developer.spotify.com/policy/), Abschnitt III, Punkt 7). Die Übergänge
entstehen deshalb über **Auswahl und Reihenfolge**:

- Die KI ordnet die Titel nach Tempo, Tonart (Camelot-Rad) und Energie. Zu jedem Titel nennt sie
  in einem Satz, warum er auf den vorherigen folgt. BPM und Tonart schätzt die KI aus ihrem Wissen.
- Der Autopilot (`dj`) reiht jeden Titel erst kurz vor Ende des laufenden Titels ein (Standard:
  20 s vorher). So bleibt die geplante Folge erhalten, auch wenn du zwischendurch selbst etwas
  auswählst.
- Weiche Blenden kannst du in der Spotify-App selbst einschalten (Einstellungen → Wiedergabe →
  Crossfade). Das ist eine Funktion von Spotify, nicht von diesem Programm.

## Was das Programm bewusst nicht tut

| Regel | Umsetzung |
|---|---|
| Keine Spotify-Inhalte in KI-Modelle ([Developer Terms](https://developer.spotify.com/terms) IV.2.a; „Spotify Content“ schließt Metadaten ein) | Die KI bekommt nur deinen Wunsch, einen optional von dir eingetippten Anschlusstitel und ihre eigenen früheren Vorschläge. Laufende Titel, Suchtreffer oder andere Spotify-Daten gehen nie an die KI. |
| Kein Mischen oder Überlagern | Es wird nur eingereiht, nie gemischt. |
| Spotify-Inhalte nicht dauerhaft speichern | Gespeichert wird nur das Anmelde-Token (Dateirechte 600). |
| Minimale Rechte | Scopes: `user-read-playback-state`, `user-read-currently-playing`, `user-modify-playback-state`. |

Das ist eine Lesart der Regeln, keine Rechtsberatung. Gedacht ist das Programm für die private
Nutzung: Jede Person verwendet ihre eigene Spotify-App im Development Mode.

## Voraussetzungen

- **Spotify Premium.** Der Development Mode verlangt seit Februar 2026 Premium beim Besitzer der
  App, und Einreihen sowie Wiedergabe-Start funktionieren nur mit Premium.
- **Eine eigene Spotify-App** (kostenlos, siehe Einrichtung).
- **Ein Claude-API-Schlüssel** von https://platform.claude.com

## Installation

**Linux und macOS** (Terminal):
```sh
curl -fsSL https://raw.githubusercontent.com/g2ssxnbdq2-byte/klangwerk-dj/main/install.sh | sh
```

**Windows** (PowerShell):
```powershell
irm https://raw.githubusercontent.com/g2ssxnbdq2-byte/klangwerk-dj/main/install.ps1 | iex
```

Die Skripte laden das passende Paket aus dem neuesten
[Release](https://github.com/g2ssxnbdq2-byte/klangwerk-dj/releases/latest), prüfen die SHA-256-Prüfsumme
und installieren ohne Administratorrechte nach `~/.local/bin` bzw.
`%LOCALAPPDATA%\Programs\klangwerk-dj`. Wer ein Skript vor dem Ausführen lesen möchte:
[install.sh](install.sh), [install.ps1](install.ps1). Ein erneuter Aufruf aktualisiert auf die
neueste Version.

| Paket | Läuft auf |
|---|---|
| `linux-x86_64` | 64-Bit-PCs mit glibc 2.28 oder neuer (Ubuntu 20.04+, Debian 10+) |
| `linux-aarch64` | Raspberry Pi mit 64-Bit-System, ARM-Server |
| `linux-armv7` | Raspberry Pi mit 32-Bit-System |
| `windows-x86_64` | Windows 10 und 11, 64 Bit |
| `macos-universal` | Apple Silicon und Intel, ab macOS 11 |

Die Pakete sind nicht signiert. Windows-SmartScreen kann beim ersten Start warnen
(„Weitere Informationen“ → „Trotzdem ausführen“). Unter macOS entfernt das Installationsskript die
Quarantäne-Markierung selbst; bei einem Download über den Browser geht das mit
`xattr -d com.apple.quarantine klangwerk-dj`. Unter Windows läuft das Programm am besten im
Windows-Terminal, dort erscheinen Zeichen wie ✓ und ↳ richtig.

Mit Rust (1.80+) geht es auch aus dem Quellcode:
`cargo install --git https://github.com/g2ssxnbdq2-byte/klangwerk-dj --locked`

## Einrichtung

1. Unter https://developer.spotify.com/dashboard eine App anlegen:
   - Redirect URI: `http://127.0.0.1:8888/callback` (genau so; Spotify lehnt `localhost` ab)
   - API: *Web API*
   - Die **Client ID** kopieren. Ein Client-Secret wird nicht gebraucht (PKCE).
2. Zugangsdaten speichern:
   ```sh
   klangwerk-dj einrichten
   ```
   Das Programm fragt nach der Client ID und dem Claude-API-Schlüssel und legt sie in
   `einstellungen.env` im Datenordner ab (siehe unten). Unter Linux und macOS ist die Datei nur
   für dich lesbar, unter Windows liegt sie in deinem Benutzerprofil. Gesetzte
   Umgebungsvariablen haben Vorrang vor der Datei.
3. Anmelden:
   ```sh
   klangwerk-dj anmelden
   ```
   Öffne den angezeigten Link im Browser. Läuft der Browser auf einem anderen Rechner als das
   Programm (etwa bei einem Server ohne Bildschirm), zeigt die Weiterleitung eine Fehlerseite.
   Kopiere dann die komplette Adresse aus der Adresszeile und füge sie im Terminal ein.
   Alternativ geht ein SSH-Tunnel: `ssh -L 8888:127.0.0.1:8888 <server>`, danach läuft es von
   selbst.

**Datenordner** (Token und Einstellungen):
Linux `~/.config/klangwerk-dj`, macOS `~/Library/Application Support/de.Klangwerk.klangwerk-dj`,
Windows `%APPDATA%\Klangwerk\klangwerk-dj\config`. Mit `KLANGWERK_DATEN` änderbar.

### Mehrere Personen

Am einfachsten legt jede Person eine eigene Spotify-App und einen eigenen Claude-Schlüssel an.
Alternativ trägst du bis zu 5 Personen in deiner App unter *User Management* mit ihrer
Spotify-E-Mail ein; dann nutzen alle deine Client ID. Gibst du deinen Claude-Schlüssel weiter,
laufen die Kosten über dein Konto.

## Befehle

| Befehl | Zweck |
|---|---|
| `einrichten` | Client ID und Claude-API-Schlüssel speichern |
| `anmelden` / `abmelden` | Bei Spotify anmelden bzw. Token löschen |
| `status` | Was gerade läuft |
| `geraete` | Spotify-Geräte mit IDs |
| `vorschlag "<Wunsch>" [-n 12] [--ab "Interpret – Titel"]` | KI-Plan anzeigen und auf Spotify suchen |
| `vorschlag … --ohne-spotify` | Nur die KI fragen; braucht keine Spotify-Anmeldung |
| `vorschlag … --warteschlange` | Alle gefundenen Titel sofort einreihen |
| `dj "<Wunsch>" [--starten] [--endlos] [--vorlauf 20]` | Autopilot mit zeitgenauem Einreihen. `--endlos` plant immer weiter, bis Strg+C. |

Globale Optionen: `--json`, `--modell`, `--aufwand low|medium|high|xhigh|max`, `--ohne-fallback`.
`vorschlag` und `dj` nehmen zusätzlich `--geraet <ID>` (Standard: das aktive Gerät).

## Einstellungen

| Variable | Standard | Bedeutung |
|---|---|---|
| `SPOTIFY_CLIENT_ID` | – | Client ID der eigenen Spotify-App |
| `SPOTIFY_REDIRECT_URI` | `http://127.0.0.1:8888/callback` | Muss im Dashboard eingetragen sein |
| `ANTHROPIC_API_KEY` | – | Claude-API-Schlüssel (alternativ `ANTHROPIC_AUTH_TOKEN`) |
| `KLANGWERK_MODELL` | `claude-opus-5-5` | Claude-Modell |
| `KLANGWERK_AUFWAND` | `medium` | Nachdenk-Aufwand der KI |
| `KLANGWERK_OHNE_FALLBACK` | aus | Server-seitiges Ausweichmodell bei Ablehnung abschalten |
| `KLANGWERK_DATEN` | je nach System, siehe oben | Ordner für Token und `einstellungen.env` |
| `RUST_LOG` | `klangwerk_dj=info,warn` | Protokollstufe (Ausgabe auf stderr) |

## Technik

- **Claude Messages API** per HTTP (für Rust gibt es kein offizielles SDK), API-Version
  `2026-10-21`. Die Antwort ist per JSON-Schema festgelegt (`output_config.format`), mit
  einstellbarem Aufwand (`output_config.effort`). Bei Ablehnung springt serverseitig ein
  Ausweichmodell ein (`fallbacks: "default"`, Beta `server-side-fallback-2026-07-01`).
  `stop_reason` wird vor dem Inhalt geprüft (`refusal`, `max_tokens`).
- **Spotify Web API**: OAuth mit PKCE und automatischer Token-Erneuerung. Suche mit höchstens
  10 Treffern (Grenze des Development Mode). Ein KI-Vorschlag wird nur eingereiht, wenn Interpret
  und Titel zu einem Treffer passen.
- **Fehlerbehandlung**: Wiederholungen mit exponentiellem Backoff und `Retry-After` bei
  408/409/429/5xx/529. Kein erneuter Versuch bei `QUOTA_EXCEEDED`. Einreihen wird bei unklarem
  Ausgang nicht wiederholt, damit kein Titel doppelt landet.

## Als Dienst laufen lassen (Linux, optional)

`~/.config/systemd/user/klangwerk-dj.service`:

```ini
[Unit]
Description=klangwerk-dj Autopilot

[Service]
ExecStart=%h/.local/bin/klangwerk-dj dj "Abends entspannt, Soul und Jazz" --endlos
Restart=on-failure
RestartSec=30
```

Die Zugangsdaten kommen aus `einstellungen.env`. Danach:
`systemctl --user enable --now klangwerk-dj`.

## Veröffentlichen

Ein Tag `v<version>` (passend zu `Cargo.toml`) startet `.github/workflows/release.yml`. Der
Workflow baut auf echten Linux-, Windows- und macOS-Rechnern und legt das Release mit allen Paketen
und `SHA256SUMS` an. Unter Windows wird mit statischer C-Laufzeit und Control Flow Guard gebaut,
unter macOS als Universal-Binary mit Ad-hoc-Signatur. Jeder Push durchläuft vorher die CI auf
allen drei Systemen, mit Clippy, Unit-Tests und dem Ende-zu-Ende-Test in `tests/e2e/`.

```sh
git tag v0.1.0 && git push origin v0.1.0
```

Lokal ohne GitHub baut `scripts/build-release.sh` die Pakete für Linux, Windows und macOS auf
einem Linux-Rechner (mit [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) und
Zig 0.14).

## Entwicklung

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo build --release && python3 -I tests/e2e/run.py target/release/klangwerk-dj
```

Der Ende-zu-Ende-Test startet einen nachgebildeten Spotify- und Claude-Server (`tests/e2e/mock.py`)
und prüft unter anderem Anmeldung mit PKCE, Wiederholung bei Überlastung, die Reihenfolge im
Autopiloten und dass keine Spotify-Daten an die KI gehen.

## Lizenz

MIT, siehe [LICENSE](LICENSE). Nicht mit Spotify oder Anthropic verbunden.
