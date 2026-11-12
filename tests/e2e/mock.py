"""Nachbildung der Spotify- und Claude-Endpunkte für einen Ende-zu-Ende-Test von klangwerk-dj.

Protokolliert jede Anfrage als JSON-Zeile in log.jsonl.
"""
import base64
import hashlib
import json
import sys
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]
STATE = {"authorize": None, "now_calls": 0, "queued": [], "claude_calls": 0,
         "queue_faults": [], "hold": 0, "play_next": None}

PLAN = {
    "session_name": "Testabend",
    "energy_arc": "Ruhig beginnen, langsam steigern.",
    "tracks": [
        {"artist": "Norah Jones", "title": "Don't Know Why", "bpm_estimate": 88, "key_estimate": "4B / B-Dur",
         "energy": 3, "transition": "Sanfter Einstieg."},
        {"artist": "Bill Withers", "title": "Lovely Day", "bpm_estimate": 98, "key_estimate": None,
         "energy": 5, "transition": "Etwas mehr Wärme."},
        {"artist": "Unbekannte Band", "title": "Gibt Es Nicht", "bpm_estimate": None, "key_estimate": None,
         "energy": 6, "transition": "Dieser Titel wird nicht gefunden."},
        {"artist": "Daft Punk", "title": "Get Lucky", "bpm_estimate": 116, "key_estimate": "11A / fis-Moll",
         "energy": 7, "transition": "Tanzbarer Abschluss."},
    ],
}

CATALOG = {
    "norah jones": {"uri": "spotify:track:norah", "name": "Don't Know Why", "duration_ms": 186000,
                    "artists": [{"name": "Norah Jones"}]},
    "bill withers": {"uri": "spotify:track:bill", "name": "Lovely Day", "duration_ms": 255000,
                     "artists": [{"name": "Bill Withers"}]},
    "daft punk": {"uri": "spotify:track:daft", "name": "Get Lucky (feat. Pharrell Williams) - Radio Edit",
                  "duration_ms": 248000, "artists": [{"name": "Daft Punk"}, {"name": "Pharrell Williams"}]},
}


def log(entry):
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(dict(entry, t=time.time()), ensure_ascii=False) + "\n")


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, code, obj=None, raw=None, headers=None):
        body = raw if raw is not None else (b"" if obj is None else json.dumps(obj).encode())
        self.send_response(code)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        if body:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def handle_any(self, method):
        url = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(url.query))
        raw = self.body()
        entry = {"method": method, "path": url.path, "query": q,
                 "headers": {k.lower(): v for k, v in self.headers.items()}}
        if raw:
            try:
                entry["json"] = json.loads(raw)
            except ValueError:
                entry["form"] = dict(urllib.parse.parse_qsl(raw.decode()))
        log(entry)

        # ---- Spotify Accounts
        if url.path == "/accounts/api/token":
            form = entry.get("form", {})
            if form.get("grant_type") == "authorization_code":
                challenge = base64.urlsafe_b64encode(
                    hashlib.sha256(form["code_verifier"].encode()).digest()).rstrip(b"=").decode()
                ok = form.get("code") == "testcode" and challenge == STATE.get("challenge")
                log({"pkce_check": ok})
                if not ok:
                    return self.send(400, {"error": "invalid_grant", "error_description": "PKCE"})
                return self.send(200, {"access_token": "AT1", "token_type": "Bearer", "expires_in": 3600,
                                       "refresh_token": "RT1", "scope": "user-read-playback-state"})
            return self.send(200, {"access_token": "AT2", "token_type": "Bearer", "expires_in": 3600})
        if url.path == "/mock/challenge":
            STATE["challenge"] = q.get("c")
            return self.send(204)
        if url.path == "/mock/queue-faults":
            # Störungen für die nächsten Einreih-Aufrufe, der Reihe nach:
            # "404" = abgewiesen, "502x" = Fehler gemeldet, aber trotzdem eingereiht.
            STATE["queue_faults"] = [f for f in q.get("f", "").split(",") if f]
            return self.send(204)
        if url.path == "/mock/state":
            return self.send(200, STATE)

        # ---- Claude
        if url.path == "/claude/v1/messages":
            STATE["claude_calls"] += 1
            if STATE["claude_calls"] == 1:
                return self.send(529, {"type": "error", "error": {"type": "overloaded_error", "message": "busy"}},
                                 headers={"retry-after": "0"})
            return self.send(200, {
                "id": "msg_test", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
                "stop_reason": "end_turn", "usage": {"input_tokens": 420, "output_tokens": 900},
                "content": [{"type": "thinking", "thinking": "", "signature": "sig"},
                            {"type": "text", "text": json.dumps(PLAN, ensure_ascii=False)}]})

        # ---- Spotify Web API (Präfix /v1/)
        auth = self.headers.get("Authorization", "")
        if url.path.startswith("/v1/") and not auth.startswith("Bearer AT"):
            return self.send(401, {"error": {"status": 401, "message": "Invalid access token"}})
        if url.path == "/v1/search":
            text = q.get("q", "").lower()
            hits = [t for k, t in CATALOG.items() if k in text]
            return self.send(200, {"tracks": {"items": hits}})
        if url.path == "/v1/me/player/currently-playing":
            if STATE["play_next"]:
                # Der trotz Fehlermeldung eingereihte Titel läuft jetzt.
                item = next(t for t in CATALOG.values() if t["uri"] == STATE["play_next"])
                STATE["play_next"] = None
                return self.send(200, {"is_playing": True, "progress_ms": item["duration_ms"] - 4000,
                                       "currently_playing_type": "track", "item": item})
            if STATE["hold"]:
                STATE["hold"] -= 1  # derselbe Titel läuft noch
            else:
                STATE["now_calls"] += 1
            n = STATE["now_calls"]
            # Steht eine Abweisung an, bleibt genug Restzeit für einen zweiten Versuch im selben Titel.
            progress = 165000 if STATE["queue_faults"][:1] == ["404"] else 176000
            return self.send(200, {"is_playing": True, "progress_ms": progress, "currently_playing_type": "track",
                                   "item": {"uri": f"spotify:track:laeuft{n}", "name": f"Laufender Titel {n}",
                                            "duration_ms": 180000, "artists": [{"name": "Irgendwer"}]}})
        if url.path == "/v1/me/player/queue" and method == "POST":
            fault = STATE["queue_faults"].pop(0) if STATE["queue_faults"] else None
            if fault == "404":
                STATE["hold"] = 1
                return self.send(404, {"error": {"status": 404, "reason": "NO_ACTIVE_DEVICE",
                                                 "message": "Player command failed: No active device found"}})
            STATE["queued"].append(q.get("uri"))
            if fault == "502x":
                STATE["play_next"] = q.get("uri")
                return self.send(502, {"error": {"status": 502, "message": "Bad gateway"}})
            return self.send(200)
        if url.path == "/v1/me/player/devices":
            return self.send(200, {"devices": [{"id": "dev1", "is_active": True, "name": "Wohnzimmer",
                                                "type": "Speaker", "volume_percent": 40}]})
        return self.send(404, {"error": {"status": 404, "message": "unbekannt"}})

    def do_GET(self):
        self.handle_any("GET")

    def do_POST(self):
        self.handle_any("POST")

    def do_PUT(self):
        self.handle_any("PUT")


ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
