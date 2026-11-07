"""Ende-zu-Ende-Test: startet mock.py (nachgebildetes Spotify und Claude) und treibt das echte Binary.

Aufruf: python -I tests/e2e/run.py target/release/klangwerk-dj[.exe]
Läuft unter Linux, macOS und Windows; braucht keine echten Zugangsdaten.
"""
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

BIN = os.path.abspath(sys.argv[1])
HERE = os.path.dirname(os.path.abspath(__file__))
results = []


def check(name, cond, detail=""):
    results.append((name, bool(cond)))
    print(("OK   " if cond else "FAIL ") + name + (f"  ({detail})" if detail and not cond else ""), flush=True)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


port, rport = free_port(), free_port()
work = tempfile.mkdtemp(prefix="kwdj-e2e-")
log_path = os.path.join(work, "log.jsonl")
mock = subprocess.Popen([sys.executable, "-I", os.path.join(HERE, "mock.py"), str(port), log_path])

env = dict(os.environ,
           SPOTIFY_CLIENT_ID="testclient",
           SPOTIFY_REDIRECT_URI=f"http://127.0.0.1:{rport}/callback",
           SPOTIFY_API_BASE=f"http://127.0.0.1:{port}/v1/",
           SPOTIFY_ACCOUNTS_BASE=f"http://127.0.0.1:{port}/accounts/",
           ANTHROPIC_BASE_URL=f"http://127.0.0.1:{port}/claude/",
           ANTHROPIC_API_KEY="sk-test-123",
           KLANGWERK_DATEN=os.path.join(work, "daten"),
           RUST_LOG="warn")
env.pop("ANTHROPIC_AUTH_TOKEN", None)


def run(args, timeout=60, environ=None):
    return subprocess.run([BIN] + args, capture_output=True, text=True, encoding="utf-8",
                          env=environ or env, timeout=timeout)


def entries():
    with open(log_path, encoding="utf-8") as f:
        return [json.loads(line) for line in f]


for _ in range(50):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
        break
    except OSError:
        time.sleep(0.1)

try:
    # 1) Anmeldung im Einfüge-Modus, PKCE wird vom Mock geprüft
    p = subprocess.Popen([BIN, "anmelden"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, text=True, encoding="utf-8", env=env)
    auth_url = None
    for line in p.stdout:
        if "/accounts/authorize?" in line:
            auth_url = line.strip()
            break
    q = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(auth_url).query))
    urllib.request.urlopen(f"http://127.0.0.1:{port}/mock/challenge?c={q['code_challenge']}").read()
    p.stdin.write(f"http://127.0.0.1:{rport}/callback?code=testcode&state={q['state']}\n")
    p.stdin.flush()
    out, err = p.communicate(timeout=30)
    token_file = os.path.join(work, "daten", "spotify-token.json")
    check("anmelden: Exit 0", p.returncode == 0, err)
    check("anmelden: PKCE vom Server bestätigt", any(e.get("pkce_check") for e in entries()))
    check("anmelden: Token-Datei angelegt", os.path.exists(token_file))
    if os.name == "posix":
        check("anmelden: Token-Datei mit Rechten 600", (os.stat(token_file).st_mode & 0o777) == 0o600)
    check("anmelden: Scopes minimal", q.get("scope") ==
          "user-read-playback-state user-read-currently-playing user-modify-playback-state")

    # 2) Vorschlag: erster Claude-Aufruf 529 -> Wiederholung, dann Plan
    r = run(["vorschlag", "Testwunsch für den Abend", "-n", "4"])
    check("vorschlag: Exit 0", r.returncode == 0, r.stderr)
    check("vorschlag: 3 gefunden, 1 übersprungen",
          r.stdout.count(". ✓ ") == 3 and r.stdout.count(". ✗ ") == 1 and "übersprungen (✗): 1" in r.stdout,
          r.stdout)
    check("vorschlag: Spotify-Fassung angezeigt", "Radio Edit" in r.stdout)
    claude_calls = [e for e in entries() if e.get("path") == "/claude/v1/messages"]
    check("vorschlag: 529 wurde wiederholt", len(claude_calls) == 2, len(claude_calls))
    req = claude_calls[-1]
    h, body = req["headers"], req["json"]
    check("claude: Header", h.get("x-api-key") == "sk-test-123" and h.get("anthropic-version") == "2026-10-21"
          and h.get("anthropic-beta") == "server-side-fallback-2026-07-01", h)
    check("claude: Modell, Schema, Aufwand, Fallback",
          body["model"] == "claude-opus-5-5" and body["output_config"]["format"]["type"] == "json_schema"
          and body["output_config"]["effort"] == "medium" and body["fallbacks"] == "default"
          and "temperature" not in body and "thinking" not in body, list(body.keys()))

    # 3) JSON-Ausgabe
    r = run(["--json", "vorschlag", "Testwunsch", "-n", "4"])
    data = json.loads(r.stdout) if r.returncode == 0 else {}
    check("vorschlag --json: gültig", r.returncode == 0 and len(data["plan"]["tracks"]) == 4
          and data["spotify"][3]["track"]["uri"] == "spotify:track:daft", r.stderr)

    # 4) Autopilot: reiht jeden Titel kurz vor Ende des laufenden ein, in Planreihenfolge
    before = len(entries())
    r = run(["dj", "Testwunsch", "-n", "4", "--vorlauf", "20"], timeout=120)
    queued = [e["query"].get("uri") for e in entries()[before:]
              if e.get("path") == "/v1/me/player/queue" and e.get("method") == "POST"]
    check("dj: Exit 0", r.returncode == 0, r.stderr)
    check("dj: Reihenfolge eingehalten",
          queued == ["spotify:track:norah", "spotify:track:bill", "spotify:track:daft"], queued)
    check("dj: beendet sich nach dem letzten Titel", "Autopilot beendet" in r.stdout, r.stdout)

    # 5) Kein Spotify-Inhalt an die KI (Spotify-URIs, laufender Titel, Spotify-Schreibweise, Tokens)
    markers = ["spotify:track", "Laufender Titel", "Irgendwer", "(feat. Pharrell", "AT1", "AT2"]
    leaked = [m for e in entries() if e.get("path", "").startswith("/claude/")
              for m in markers if m in json.dumps(e, ensure_ascii=False)]
    check("Datenschutz: keine Spotify-Daten in KI-Anfragen", not leaked, leaked)

    # 6) Abgelaufenes Token wird erneuert, Refresh-Token bleibt erhalten
    with open(token_file, encoding="utf-8") as f:
        tok = json.load(f)
    tok["expires_at"] = 0
    with open(token_file, "w", encoding="utf-8") as f:
        json.dump(tok, f)
    r = run(["status"])
    with open(token_file, encoding="utf-8") as f:
        tok2 = json.load(f)
    check("status: Token erneuert", r.returncode == 0 and tok2["access_token"] == "AT2"
          and tok2["refresh_token"] == "RT1", r.stderr)
    check("status: Ausgabe", "Laufender Titel" in r.stdout, r.stdout)

    # 7) Klare Meldung ohne Anmeldung
    r = run(["abmelden"])
    r2 = run(["status"])
    check("abmelden + status: klare Meldung", r.returncode == 0 and r2.returncode == 1
          and "klangwerk-dj anmelden" in r2.stderr, r2.stderr)

    # 8) Nur KI, ohne Spotify-Client-ID
    env_ki = dict(env)
    env_ki.pop("SPOTIFY_CLIENT_ID")
    r = run(["vorschlag", "Nur KI", "-n", "2", "--ohne-spotify"], environ=env_ki)
    check("vorschlag --ohne-spotify: läuft ohne Spotify", r.returncode == 0 and ". ✓ " not in r.stdout
          and r.stdout.count("↳") == 2, r.stdout + r.stderr)

    # 9) Einstellungsdatei wird gelesen, echte Umgebung hat Vorrang
    os.makedirs(env["KLANGWERK_DATEN"], exist_ok=True)
    with open(os.path.join(env["KLANGWERK_DATEN"], "einstellungen.env"), "w", encoding="utf-8") as f:
        f.write("ANTHROPIC_API_KEY=sk-aus-datei\nKLANGWERK_AUFWAND=low\n")
    env_file = dict(env_ki)
    env_file.pop("ANTHROPIC_API_KEY")
    r = run(["vorschlag", "Datei", "-n", "1", "--ohne-spotify"], environ=env_file)
    last = [e for e in entries() if e.get("path") == "/claude/v1/messages"][-1]
    check("einstellungen.env: Schlüssel und Aufwand übernommen", r.returncode == 0
          and last["headers"].get("x-api-key") == "sk-aus-datei"
          and last["json"]["output_config"]["effort"] == "low", r.stderr)
finally:
    mock.terminate()

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} Prüfungen bestanden")
sys.exit(1 if failed else 0)
