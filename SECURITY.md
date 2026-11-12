# Sicherheit

## Eine Schwachstelle melden

Bitte melde Sicherheitslücken vertraulich und nicht als öffentliches Issue:
Im Repo unter **Security → Report a vulnerability**
([direkter Link](https://github.com/g2ssxnbdq2-byte/klangwerk-dj/security/advisories/new)).

Fehlt die Schaltfläche, eröffne ein Issue ohne technische Einzelheiten und bitte darin um einen
vertraulichen Kanal.

Hilfreich sind: die betroffene Version (`klangwerk-dj --version`), das Betriebssystem, die Schritte
zum Nachstellen und eine Einschätzung, was ein Angreifer damit erreichen könnte. Schicke keine
echten Token oder API-Schlüssel mit.

Das Projekt wird in der Freizeit betreut. Eine Antwort kommt in der Regel innerhalb einer Woche.

## Unterstützte Versionen

Korrekturen gibt es für die jeweils neueste veröffentlichte Version.

## Was das Programm schützt

- Das Spotify-Token und die Einstellungen mit dem Claude-API-Schlüssel liegen im Datenordner und
  sind unter Linux und macOS nur für den eigenen Benutzer lesbar (Dateirechte 600).
- Token und Schlüssel gehen nur über https an Spotify und Anthropic. Umgebogene Dienst-Adressen
  (`SPOTIFY_API_BASE`, `SPOTIFY_ACCOUNTS_BASE`, `ANTHROPIC_BASE_URL`) müssen https verwenden;
  http ist nur für den eigenen Rechner erlaubt.
- Die Anmeldung nutzt OAuth mit PKCE und prüft den `state`-Wert. Der lokale Rückruf-Server lauscht
  nur auf dem eigenen Rechner und nimmt nur die Antwort an, die zur laufenden Anmeldung gehört.
- Die Installationsskripte laden nur über https und prüfen die SHA-256-Prüfsumme des Pakets.

## Bekannte Grenzen

- Die Pakete sind nicht signiert. Die Prüfsummen in `SHA256SUMS` liegen im selben Release wie die
  Pakete; sie schützen vor Übertragungsfehlern, nicht vor einem übernommenen GitHub-Konto.
- Unter Windows gibt es keine eigene Rechtevergabe für die Token-Datei; sie liegt im
  Benutzerprofil und ist so geschützt wie dieses.
