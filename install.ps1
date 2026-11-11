# klangwerk-dj installieren (Windows 10/11, PowerShell):
#   irm https://raw.githubusercontent.com/g2ssxnbdq2-byte/klangwerk-dj/main/install.ps1 | iex
#
# Optional: $env:KLANGWERK_VERSION = '0.1.0' (sonst die neueste Version)
# Hinweis: Die Datei enthaelt bewusst nur ASCII-Zeichen, damit sie auch in Windows PowerShell 5.1 sicher laeuft.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'g2ssxnbdq2-byte/klangwerk-dj'
$bin = 'klangwerk-dj'

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'klangwerk-dj gibt es nur fuer 64-Bit-Windows.'
}

$version = $env:KLANGWERK_VERSION
if (-not $version) {
    $release = Invoke-RestMethod -UseBasicParsing -Headers @{ 'User-Agent' = 'klangwerk-dj-installer' } `
        -Uri "https://api.github.com/repos/$repo/releases/latest"
    $version = $release.tag_name.TrimStart('v')
}
if ($version -notmatch '\A[0-9A-Za-z.+-]+\z') {
    throw "Ungueltige Versionsangabe: $version"
}

$name = "$bin-$version-windows-x86_64"
$asset = "$name.zip"
$base = "https://github.com/$repo/releases/download/v$version"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("klangwerk-dj-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    Write-Host "Lade $asset ..."
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')

    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') |
        Where-Object { (($_ -split '\s+')[1] -replace '^\*|^\./', '') -eq $asset } |
        Select-Object -First 1
    if (-not $line) { throw "Keine Pruefsumme fuer $asset gefunden." }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -Path (Join-Path $tmp $asset)).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { throw 'Die Pruefsumme stimmt nicht. Installation abgebrochen.' }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    $dest = Join-Path $env:LOCALAPPDATA 'Programs\klangwerk-dj'
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item -Force (Join-Path $tmp "$name\$bin.exe") $dest
    Copy-Item -Force (Join-Path $tmp "$name\README.md") $dest

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ } }
    if ($entries -notcontains $dest) {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $dest) -join ';'), 'User')
        Write-Host "$dest wurde zum PATH hinzugefuegt. Bitte ein neues Terminal oeffnen."
    }

    $installed = & (Join-Path $dest "$bin.exe") --version
    Write-Host "Installiert: $installed in $dest"
    Write-Host ''
    Write-Host 'Weiter mit: klangwerk-dj einrichten'
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
