#!/bin/sh
# Baut Release-Pakete für Linux, Windows und macOS von einem Linux-Rechner aus.
# Voraussetzungen: rustup, zig 0.14 und cargo-zigbuild im PATH sowie die Rust-Ziele
#   rustup target add x86_64-pc-windows-gnu x86_64-apple-darwin aarch64-apple-darwin
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
DIST="dist/$VERSION"
rm -rf "$DIST"
mkdir -p "$DIST"

build() {
    echo ">> $1"
    nice -n 19 cargo zigbuild --release --locked --target "$1"
}

# Linux: glibc 2.28 als Untergrenze (Debian 10, Ubuntu 18.10 und neuer)
build x86_64-unknown-linux-gnu.2.28
# Windows 10/11 (64 Bit)
build x86_64-pc-windows-gnu
# macOS: Universal-Binary für Apple Silicon und Intel
build universal2-apple-darwin

pack_tar() {  # Zielname, Binärdatei
    tmp="$DIST/klangwerk-dj-$VERSION-$1"
    mkdir -p "$tmp"
    cp "$2" README.md "$tmp/"
    tar -C "$DIST" -czf "$tmp.tar.gz" "klangwerk-dj-$VERSION-$1"
    rm -rf "$tmp"
}

pack_tar linux-x86_64 target/x86_64-unknown-linux-gnu/release/klangwerk-dj
pack_tar macos-universal target/universal2-apple-darwin/release/klangwerk-dj

win="$DIST/klangwerk-dj-$VERSION-windows-x86_64"
mkdir -p "$win"
cp target/x86_64-pc-windows-gnu/release/klangwerk-dj.exe README.md "$win/"
(cd "$DIST" && zip -qr "klangwerk-dj-$VERSION-windows-x86_64.zip" "klangwerk-dj-$VERSION-windows-x86_64")
rm -rf "$win"

(cd "$DIST" && sha256sum ./*.tar.gz ./*.zip > SHA256SUMS)
echo
ls -l "$DIST"
cat "$DIST/SHA256SUMS"
