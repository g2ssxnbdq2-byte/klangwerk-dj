//! klangwerk-dj: KI-gestützte Titelauswahl und Übergangsplanung für Spotify.

mod ai;
mod config;
mod dj;
mod error;
mod http;
mod matching;
mod spotify;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use crate::ai::{Claude, Engine, PlanRequest, Suggestion};
use crate::config::Config;
use crate::error::Error;
use crate::spotify::auth::{self, TokenStore};
use crate::spotify::Spotify;

#[derive(Parser)]
#[command(
    name = "klangwerk-dj",
    version,
    about = "KI-gestützte Titelauswahl und Übergangsplanung für Spotify"
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct GlobalArgs {
    /// Client ID der eigenen App aus dem Spotify Developer Dashboard
    #[arg(long, env = "SPOTIFY_CLIENT_ID", global = true)]
    spotify_client_id: Option<String>,
    /// Muss exakt so im Dashboard eingetragen sein
    #[arg(
        long,
        env = "SPOTIFY_REDIRECT_URI",
        default_value = "http://127.0.0.1:8888/callback",
        global = true
    )]
    redirect_uri: String,
    /// Claude-Modell für die Vorschläge
    #[arg(
        long,
        env = "KLANGWERK_MODELL",
        default_value = "claude-opus-5-5",
        global = true
    )]
    modell: String,
    /// Wie gründlich die KI nachdenkt (mehr Aufwand = langsamer und teurer)
    #[arg(long, env = "KLANGWERK_AUFWAND", default_value = "medium",
          value_parser = ["low", "medium", "high", "xhigh", "max"], global = true)]
    aufwand: String,
    /// Kein automatisches Ausweichmodell, falls Claude eine Anfrage ablehnt
    #[arg(long, env = "KLANGWERK_OHNE_FALLBACK", global = true)]
    ohne_fallback: bool,
    /// Ausgabe als JSON statt als Text
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Spotify Client ID und Claude-API-Schlüssel speichern (einmalig)
    #[command(visible_alias = "setup")]
    Einrichten,
    /// Bei Spotify anmelden (OAuth mit PKCE, ohne Client-Secret)
    #[command(visible_alias = "login")]
    Anmelden,
    /// Gespeichertes Spotify-Token löschen
    #[command(visible_alias = "logout")]
    Abmelden,
    /// Zeigt, was gerade in Spotify läuft
    Status,
    /// Listet die Spotify-Geräte
    #[command(visible_alias = "devices")]
    Geraete,
    /// Die KI schlägt eine Titelfolge mit Übergängen vor
    #[command(visible_alias = "suggest")]
    Vorschlag(VorschlagArgs),
    /// Autopilot: plant und reiht jeden Titel kurz vor Ende des laufenden ein
    Dj(DjArgs),
}

#[derive(Args)]
struct PlanArgs {
    /// Dein Wunsch in eigenen Worten, z. B. "Sonntagmorgen, ruhig, Jazz bis Soul"
    wunsch: String,
    /// Anzahl der Titel (1–40)
    #[arg(short = 'n', long, default_value_t = 12, value_parser = clap::value_parser!(u16).range(1..=40))]
    anzahl: u16,
    /// Titel, an den die Folge anschließen soll, z. B. "Daft Punk – Get Lucky"
    #[arg(long)]
    ab: Option<String>,
}

impl PlanArgs {
    fn to_request(&self) -> Result<PlanRequest, Error> {
        let brief = self.wunsch.trim();
        if brief.is_empty() {
            return Err(Error::Config("Bitte einen Wunsch angeben.".into()));
        }
        if brief.chars().count() > 2000 {
            return Err(Error::Config(
                "Der Wunsch ist zu lang (höchstens 2000 Zeichen).".into(),
            ));
        }
        Ok(PlanRequest {
            brief: brief.to_string(),
            count: usize::from(self.anzahl),
            start_after: self.ab.clone().filter(|s| !s.trim().is_empty()),
            avoid: self.ab.iter().cloned().collect(),
        })
    }
}

#[derive(Args)]
struct VorschlagArgs {
    #[command(flatten)]
    plan: PlanArgs,
    /// Nur die KI fragen, nicht auf Spotify suchen (braucht keine Anmeldung)
    #[arg(long, conflicts_with = "warteschlange")]
    ohne_spotify: bool,
    /// Alle gefundenen Titel sofort in die Spotify-Warteschlange setzen
    #[arg(long)]
    warteschlange: bool,
    /// Geräte-ID (siehe `geraete`); ohne Angabe das aktive Gerät
    #[arg(long)]
    geraet: Option<String>,
}

#[derive(Args)]
struct DjArgs {
    #[command(flatten)]
    plan: PlanArgs,
    /// Sekunden vor Titelende, zu denen der nächste Titel eingereiht wird
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(5..=120))]
    vorlauf: u64,
    /// Wiedergabe mit dem ersten Titel starten, falls gerade nichts läuft
    #[arg(long)]
    starten: bool,
    /// Immer weiter planen, bis Strg+C
    #[arg(long)]
    endlos: bool,
    /// Geräte-ID (siehe `geraete`); ohne Angabe das aktive Gerät
    #[arg(long)]
    geraet: Option<String>,
}

fn main() -> ExitCode {
    // Einstellungsdatei laden, solange nur ein Thread läuft und bevor clap die Umgebung liest.
    if let Err(e) = config::data_dir().and_then(|dir| config::load_settings(&dir)) {
        eprintln!("Warnung: Einstellungen nicht geladen: {e}");
    }
    let cli = Cli::parse();
    init_logging();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Fehler: Laufzeitumgebung nicht startbar: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Fehler: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("klangwerk_dj=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let g = cli.global;
    let cfg = Config::new(
        g.spotify_client_id.clone(),
        &g.redirect_uri,
        g.modell.clone(),
        g.aufwand.clone(),
        !g.ohne_fallback,
    )?;
    let http = config::http_client()?;

    match cli.command {
        Command::Einrichten => setup(&cfg),
        Command::Anmelden => login(&cfg, &http).await,
        Command::Abmelden => logout(&cfg),
        Command::Status => status(&spotify(&cfg, &http)?, g.json).await,
        Command::Geraete => devices(&spotify(&cfg, &http)?, g.json).await,
        Command::Vorschlag(args) => suggest(&cfg, &http, args, g.json).await,
        Command::Dj(args) => autopilot(&cfg, &http, args).await,
    }
}

fn spotify(cfg: &Config, http: &reqwest::Client) -> Result<Spotify, Error> {
    let client_id = cfg.spotify_client_id.clone().ok_or_else(|| {
        Error::Config(
            "SPOTIFY_CLIENT_ID fehlt (Client ID der eigenen App im Spotify Developer Dashboard)."
                .into(),
        )
    })?;
    Ok(Spotify::new(
        http.clone(),
        cfg.spotify_api_base.clone(),
        cfg.spotify_accounts_base.clone(),
        client_id,
        TokenStore::new(&cfg.data_dir),
    ))
}

fn claude(cfg: &Config, http: &reqwest::Client) -> Result<Claude, Error> {
    Claude::new(
        http.clone(),
        &cfg.anthropic_base,
        cfg.anthropic_credential.clone(),
        cfg.model.clone(),
        cfg.effort.clone(),
        cfg.fallbacks,
    )
}

async fn login(cfg: &Config, http: &reqwest::Client) -> anyhow::Result<()> {
    let sp = spotify(cfg, http)?;
    let verifier = auth::random_string(96);
    let state = auth::random_string(32);
    let url = auth::authorize_url(
        sp.accounts_base(),
        sp.client_id(),
        &cfg.redirect_uri,
        &auth::challenge_for(&verifier),
        &state,
    )?;
    println!("1. Öffne diese Adresse im Browser und erlaube den Zugriff:\n\n   {url}\n");
    println!("2. Spotify leitet danach auf {} weiter.", cfg.redirect_uri);
    println!("   Läuft der Browser auf diesem Rechner, geht es von selbst weiter.");
    println!("   Sonst kopiere die komplette Adresse aus der Adresszeile (auch wenn die Seite");
    println!("   nicht lädt) und füge sie hier ein:\n");
    let code = auth::wait_for_code(&cfg.redirect_uri, &state, Duration::from_secs(600)).await?;
    let token = sp
        .token_endpoint()
        .exchange(&code, &cfg.redirect_uri, &verifier)
        .await?;
    sp.store_token(token).await?;
    println!(
        "Angemeldet. Token gespeichert unter {} (nur für dich lesbar).",
        sp.token_path().display()
    );
    Ok(())
}

fn prompt_line(prompt: &str) -> std::io::Result<String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn set_entry(entries: &mut Vec<(String, String)>, key: &str, value: String) {
    match entries.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value,
        None => entries.push((key.to_string(), value)),
    }
}

fn setup(cfg: &Config) -> anyhow::Result<()> {
    let path = cfg.data_dir.join(config::SETTINGS_FILE);
    let mut entries = match std::fs::read_to_string(&path) {
        Ok(text) => config::parse_settings(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Error::io(&path, e).into()),
    };
    let has = |entries: &[(String, String)], key: &str| {
        entries.iter().any(|(k, v)| k == key && !v.is_empty())
    };
    let keep = " [vorhanden – Enter behält ihn]";

    println!(
        "Einrichtung von klangwerk-dj. Gespeichert wird in:\n  {}\n",
        path.display()
    );
    println!("Spotify: Lege unter https://developer.spotify.com/dashboard eine App an,");
    println!(
        "trage als Redirect URI {} ein und wähle „Web API“.\n",
        cfg.redirect_uri
    );

    let id = prompt_line(&format!(
        "Spotify Client ID{}: ",
        if has(&entries, "SPOTIFY_CLIENT_ID") {
            keep
        } else {
            ""
        }
    ))?;
    if !id.is_empty() {
        if id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            println!("Hinweis: Eine Client ID hat normalerweise 32 Zeichen (0–9, a–f). Gespeichert wird sie trotzdem.");
        }
        set_entry(&mut entries, "SPOTIFY_CLIENT_ID", id);
    }

    let key = rpassword::prompt_password(format!(
        "Claude-API-Schlüssel (Eingabe bleibt unsichtbar){}: ",
        if has(&entries, "ANTHROPIC_API_KEY") {
            keep
        } else {
            ""
        }
    ))?;
    let key = key.trim().to_string();
    if !key.is_empty() {
        if !key.starts_with("sk-ant-") {
            println!("Hinweis: Claude-API-Schlüssel beginnen normalerweise mit „sk-ant-“. Gespeichert wird er trotzdem.");
        }
        set_entry(&mut entries, "ANTHROPIC_API_KEY", key);
    }

    auth::save_private(&path, config::render_settings(&entries).as_bytes())?;
    println!("\nGespeichert (nur für dich lesbar). Nächster Schritt: klangwerk-dj anmelden");
    Ok(())
}

fn logout(cfg: &Config) -> anyhow::Result<()> {
    let store = TokenStore::new(&cfg.data_dir);
    if store.delete()? {
        println!("Abgemeldet. Token gelöscht: {}", store.path().display());
    } else {
        println!("Es war kein Token gespeichert.");
    }
    println!("Den Zugriff der App kannst du zusätzlich unter https://www.spotify.com/account/apps/ entfernen.");
    Ok(())
}

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

async fn status(sp: &Spotify, as_json: bool) -> anyhow::Result<()> {
    let now = sp.currently_playing().await?;
    if as_json {
        let value = match &now {
            None => json!(null),
            Some(cp) => json!({
                "is_playing": cp.is_playing,
                "progress_ms": cp.progress_ms,
                "type": cp.currently_playing_type,
                "track": cp.item,
            }),
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    match now {
        None => println!("Gerade läuft nichts."),
        Some(cp) => match cp.item {
            Some(track) => println!(
                "{} {}  ({} / {})",
                if cp.is_playing { "▶" } else { "❚❚" },
                track.label(),
                fmt_ms(cp.progress_ms.unwrap_or(0)),
                fmt_ms(track.duration_ms)
            ),
            None => println!(
                "Es läuft gerade kein Musiktitel ({}).",
                cp.currently_playing_type
            ),
        },
    }
    Ok(())
}

async fn devices(sp: &Spotify, as_json: bool) -> anyhow::Result<()> {
    let list = sp.devices().await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if list.is_empty() {
        println!("Keine Geräte gefunden. Öffne Spotify auf einem Gerät.");
    }
    for d in list {
        println!(
            "{} {}  ({})  ID: {}",
            if d.is_active { "●" } else { "○" },
            d.name,
            d.kind,
            d.id.as_deref().unwrap_or("–")
        );
    }
    Ok(())
}

async fn suggest(
    cfg: &Config,
    http: &reqwest::Client,
    args: VorschlagArgs,
    as_json: bool,
) -> anyhow::Result<()> {
    let request = args.plan.to_request()?;
    let sp = if args.ohne_spotify {
        None
    } else {
        Some(spotify(cfg, http)?)
    };
    let engine = claude(cfg, http)?;
    eprintln!(
        "Frage die KI ({}, Aufwand {}) …",
        engine.model(),
        cfg.effort
    );
    let plan = engine.plan(&request).await?;
    let resolved = match &sp {
        Some(sp) => Some(dj::resolve_all(sp, &plan).await?),
        None => None,
    };

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "plan": plan, "spotify": resolved }))?
        );
    } else {
        dj::print_plan(&plan, resolved.as_deref());
    }

    if args.warteschlange {
        if let (Some(sp), Some(resolved)) = (&sp, &resolved) {
            let mut added = 0usize;
            for r in resolved {
                if let Some(track) = &r.track {
                    sp.add_to_queue(&track.uri, args.geraet.as_deref()).await?;
                    added += 1;
                }
            }
            eprintln!("{added} Titel in die Warteschlange gesetzt.");
        }
    }
    Ok(())
}

async fn autopilot(cfg: &Config, http: &reqwest::Client, args: DjArgs) -> anyhow::Result<()> {
    let request = args.plan.to_request()?;
    let sp = Arc::new(spotify(cfg, http)?);
    // Anmeldung prüfen, bevor die KI etwas kostet.
    sp.currently_playing().await?;
    let engine = Arc::new(claude(cfg, http)?);

    eprintln!(
        "Frage die KI ({}, Aufwand {}) …",
        engine.model(),
        cfg.effort
    );
    let plan = engine.plan(&request).await?;
    let resolved = dj::resolve_all(&sp, &plan).await?;
    dj::print_plan(&plan, Some(&resolved));

    let mut history: Vec<String> = request.avoid.clone();
    history.extend(plan.tracks.iter().map(Suggestion::label));

    dj::Autopilot {
        spotify: sp,
        engine,
        brief: request.brief.clone(),
        batch: request.count.clamp(3, 10),
        lead: Duration::from_secs(args.vorlauf),
        endless: args.endlos,
        device: args.geraet,
    }
    .run(resolved, history, args.starten)
    .await?;
    Ok(())
}
