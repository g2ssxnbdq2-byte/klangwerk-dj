#!/bin/sh
# klangwerk-dj installieren (Linux und macOS):
#   curl -fsSL https://raw.githubusercontent.com/g2ssxnbdq2-byte/klangwerk-dj/main/install.sh | sh
#
# Optional: KLANGWERK_VERSION=0.1.0 (sonst die neueste Version),
#           KLANGWERK_INSTALL_DIR=/pfad (Standard: ~/.local/bin)
set -eu

REPO="g2ssxnbdq2-byte/klangwerk-dj"
BIN="klangwerk-dj"
INSTALL_DIR="${KLANGWERK_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${KLANGWERK_VERSION:-}"

say() { printf '%s\n' "$*"; }
fail() { printf 'Fehler: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "Das Programm „$1“ wird benötigt."; }

need curl
need tar
need uname

os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Linux)
        case "$arch" in
            x86_64 | amd64) target=linux-x86_64 ;;
            aarch64 | arm64) target=linux-aarch64 ;;
            armv7l | armv7*) target=linux-armv7 ;;
            *) fail "Für Linux auf $arch gibt es kein fertiges Paket. Alternative: cargo install --git https://github.com/$REPO" ;;
        esac
        ;;
    Darwin) target=macos-universal ;;
    *) fail "Unbekanntes System „$os“. Für Windows gibt es einen PowerShell-Befehl, siehe https://github.com/$REPO" ;;
esac

if [ -z "$VERSION" ]; then
    latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
        || fail "Die neueste Version ließ sich nicht ermitteln."
    VERSION=${latest##*/tag/v}
    [ "$VERSION" != "$latest" ] || fail "Es gibt noch keine veröffentlichte Version."
fi

asset="$BIN-$VERSION-$target.tar.gz"
base="https://github.com/$REPO/releases/download/v$VERSION"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Lade $asset …"
curl -fsSL -o "$tmp/$asset" "$base/$asset" || fail "Download fehlgeschlagen: $base/$asset"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || fail "Prüfsummen ließen sich nicht laden."

expected=$(awk -v f="$asset" '{ n = $2; sub(/^\*/, "", n); sub(/^\.\//, "", n); if (n == f) print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || fail "Keine Prüfsumme für $asset gefunden."
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$asset" | awk '{ print $1 }')
else
    actual=$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')
fi
[ "$expected" = "$actual" ] || fail "Die Prüfsumme stimmt nicht. Installation abgebrochen."

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
cp "$tmp/$BIN-$VERSION-$target/$BIN" "$INSTALL_DIR/$BIN.neu"
chmod 755 "$INSTALL_DIR/$BIN.neu"
mv -f "$INSTALL_DIR/$BIN.neu" "$INSTALL_DIR/$BIN"
if [ "$os" = Darwin ]; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/$BIN" 2>/dev/null || true
fi

say "Installiert: $("$INSTALL_DIR/$BIN" --version) in $INSTALL_DIR"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        say ""
        say "Hinweis: $INSTALL_DIR ist noch nicht im PATH. Füge diese Zeile in ~/.profile"
        say "(bzw. ~/.zshrc auf dem Mac) ein und öffne ein neues Terminal:"
        say "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac
say ""
say "Weiter mit: klangwerk-dj einrichten"
