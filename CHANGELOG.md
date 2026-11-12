# Änderungen

## Unveröffentlicht

### Behoben

- **Absturz bei übergroßem `Retry-After`.** Eine Antwort mit einem Wert wie `Retry-After: 1e20`
  beendete das Programm mit einem Panic. Solche Werte gelten jetzt als „länger als jede Grenze“,
  die Antwort wird ohne Wiederholung zurückgegeben.
- **Anmeldung brach bei fremden Anfragen ab.** Der lokale Rückruf-Server nahm die erste Anfrage
  auf `/callback` als Antwort von Spotify, auch wenn sie von einem anderen Programm kam. Er wartet
  jetzt weiter, bis eine Antwort mit dem `state` der laufenden Anmeldung eintrifft.
- **Autopilot: abgewiesenes Einreihen.** Wies Spotify das Einreihen ab (etwa weil kurz kein Gerät
  aktiv war), kam der Titel erst am Ende des übernächsten Titels an die Reihe. Jetzt folgt der
  nächste Versuch nach 3 Sekunden, bei anhaltender Abweisung nach 6, 12 und 24 Sekunden.
- **Autopilot: doppelte Titel nach unklarem Ausgang.** Meldete Spotify einen Fehler, hatte den
  Titel aber schon eingereiht, landete er ein zweites Mal in der Warteschlange. Beginnt der Titel
  zu laufen, gilt er jetzt als angekommen.
- **`--endlos`: Dauerfehler beim Nachplanen.** Schlug das Nachplanen fehl, kam alle zwei Minuten
  eine neue KI-Anfrage. Die Pause wächst jetzt von 2 über 4, 8 und 16 auf höchstens 30 Minuten.
- **`--endlos`: Anschluss an den richtigen Titel.** Das Nachplanen schließt an den letzten
  Vorschlag an, der auf Spotify gefunden wurde, und sonst an den mit `--ab` genannten Titel.
- **Titelabgleich.** Der Interpret muss in ganzen Wörtern übereinstimmen („Daf“ passt nicht mehr
  auf „Daft Punk“). Titel, die nur aus einem Klammerzusatz bestehen, etwa „(Nice Dream)“, werden
  wieder gefunden.
- **Zwei Programme mit demselben Token.** Läuft der Autopilot als Dienst und daneben ein
  einzelner Befehl, konnte der eine dem anderen die Anmeldung entziehen. Vor dem Erneuern wird die
  Token-Datei jetzt neu gelesen und ein dort schon erneuertes Token übernommen.

### Geändert

- **Dienst-Adressen nur noch über https.** `SPOTIFY_API_BASE`, `SPOTIFY_ACCOUNTS_BASE` und
  `ANTHROPIC_BASE_URL` müssen https verwenden. http ist nur für den eigenen Rechner erlaubt
  (`127.0.0.1`, `[::1]`, `localhost`). Wer bisher eine http-Adresse im Heimnetz eingetragen hatte,
  bekommt beim Start eine Fehlermeldung.
- **KI-Anfragen sparsamer.** Eine Planung wird bei Überlastung höchstens zweimal wiederholt und
  nach einer Zeitüberschreitung gar nicht, weil jeder Versuch Minuten dauern und Geld kosten kann.
- **Installationsskripte.** `install.sh` lädt nur über https (auch nach Weiterleitungen,
  mindestens TLS 1.2). `install.sh` und `install.ps1` lehnen Versionsangaben mit unerwarteten
  Zeichen ab.
- **Release-Ablauf.** Ein Release wird nur veröffentlicht, wenn die komplette CI für denselben
  Stand grün ist. Schreibrechte hat allein der veröffentlichende Job. Fremde Actions sind auf
  Commits festgelegt, Dependabot schlägt wöchentlich Aktualisierungen vor.
- `scripts/build-release.sh` legt die Lizenz in jedes Paket.

### Neu

- `SECURITY.md` mit dem Meldeweg für Schwachstellen.
- Ende-zu-Ende-Test für gestörtes Einreihen (Abweisung und Fehler mit unklarem Ausgang).

## 0.1.0 – 2026-11-07

Erste Veröffentlichung: KI-Titelfolgen mit Übergängen für Spotify, Autopilot mit zeitgenauem
Einreihen, Pakete für Linux, Windows und macOS.
