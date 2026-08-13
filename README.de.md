<div align="center">

<img src="docs/assets/logo.png" alt="Berimor" width="640">

**Das Modell denkt. Der Code entscheidet.**

[Русский](README.md) · [English](README.en.md) · **[Deutsch](README.de.md)** · [Français](README.fr.md) · [Español](README.es.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md)

</div>

Ein universeller LLM-Agent mit deterministischem Kern: Aufgabenrouting, Prozessverzweigung, Kontextauswahl und Ausführungsfreigabe entscheidet der Code — das Modell führt enge, prüfbare Schritte aus. Arbeitet mit lokalen und Cloud-Modellen, schwachen wie starken.

[![GitHub release](https://img.shields.io/github/v/release/devpilgrin/berimor?logo=github&label=release)](https://github.com/devpilgrin/berimor/releases/latest)
[![npm](https://img.shields.io/npm/v/berimor?logo=npm&label=npm)](https://www.npmjs.com/package/berimor)
[![CI](https://img.shields.io/github/actions/workflow/status/devpilgrin/berimor/ci.yml?branch=main&label=CI)](https://github.com/devpilgrin/berimor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-927%20green-brightgreen)](#projektinfrastruktur)

![Rust](https://img.shields.io/badge/Rust-stable-DEA584?logo=rust&logoColor=white)
![WebAssembly](https://img.shields.io/badge/sandbox-Wasmtime-654FF0?logo=webassembly&logoColor=white)
![QuickJS](https://img.shields.io/badge/guest-QuickJS-F7DF1E?logo=javascript&logoColor=black)
![SQLite](https://img.shields.io/badge/storage-SQLite%20%2B%20FTS5%20%2B%20vec-003B57?logo=sqlite&logoColor=white)
![tokio](https://img.shields.io/badge/async-tokio-0B7B8A)
![MCP](https://img.shields.io/badge/protocol-MCP-5B5BD6)
![ratatui](https://img.shields.io/badge/TUI-ratatui-E95420)
![sigstore](https://img.shields.io/badge/supply--chain-sigstore%20keyless-2E8B57)
[![Socket](https://badge.socket.dev/npm/package/berimor)](https://socket.dev/npm/package/berimor)
![oxc](https://img.shields.io/badge/static%20analysis-oxc__parser-black)

---

## Wozu das Ganze

Die meisten „KI-Agenten“ sind gleich aufgebaut: Das Modell bekommt einen Satz Werkzeuge und soll selbst entscheiden, was zu tun ist. Für eine Demo praktisch. In der Praxis unzuverlässig: Das Modell vergisst Schritte, erfindet Fakten, schwenkt in die falsche Richtung — und ein gefährlicher Befehl rutscht mit einem automatisch getippten „y“ ins Terminal.

Berimor baut auf der entgegengesetzten Annahme auf: **Dem Modell kann man keine Orchestrierung anvertrauen — anvertrauen kann man ihm nur die Ausführung.** Die Aufgabe wird vorab in Schritte zerlegt oder von einer deterministischen Schleife gesteuert; alles, was das Modell ausgibt, durchläuft eine strenge Prüfung, bevor man sich darauf verlassen kann; alles, was Schaden anrichten kann, läuft über ein Gate, das sich nicht mit Enter wegdrücken lässt.

| | Typischer Agenten-CLI | Berimor |
|---|---|---|
| Wer entscheidet, was als Nächstes passiert | Das Modell (Hoffnung auf Vernunft) | Der Code (Prozessgraph, deterministische Schleife) |
| Abbruch mitten in der Aufgabe | „Neu starten und beten“ | Ereignisjournal: Fortsetzung exakt am Abbruchpunkt |
| Gefährliche Aktion | Eine Bestätigung, die Müdigkeit in YOLO verwandelt | Deny-Statik: Verbotenes wird gar nicht erst erfragt |
| Schwaches/lokales Modell | „Kaufen Sie ein teureres Modell“ | Mediation: Retry mit Fehlererklärung → Eskalation an den Menschen |
| Erweiterungen | Ein Plugin bekommt alles | Subagent/Plugin bekommt eine Teilmenge der Elternrechte — per Code durchgesetzt |
| Reproduzierbarkeit | Keine | Vollständig: Journal → Replay → Zustand zu jedem beliebigen Zeitpunkt |

## Was es unterscheidet

**1. Entscheidungen sind deterministischer Code, kein Text im Prompt.**
Verzweigungen, Schleifen, Timeouts, parallele Zweige mit Join-Barriere, Versionsmigration eines laufenden Prozesses — all das ist die Process Engine, nicht die Hoffnung, dass das Modell sich an seine Anweisungen erinnert. Schwachen Modellen darf man Kontextauswahl und Routing nicht anvertrauen — also übernimmt das der Code.

**2. Sicherheit ist Struktur, nicht Disziplin des Nutzers.**
Die Deny-Tabelle destruktiver Operationen lässt sich nicht per Bestätigung überstimmen. Das Dateisystem-Jail verlässt das Arbeitsverzeichnis nie. Das Netzwerk-Gate lässt nichts in private Bereiche (inklusive NAT64-/6to4-/Teredo-Tarnungen und Umgehungen über Redirects und Userinfo in URLs). Secrets werden an jeder Leckstelle maskiert — doch das Zulassungs-Gate sieht die echten Werte: Die Maskierung blendet die Prüfung nicht.

**3. Freie Schleife — unter Aufsicht.**
Der Modus „Reasoning → Aktion → Beobachtung“ für Aufgaben, die sich nicht vorab in Schritte zerlegen lassen. Jede Aktion darin durchläuft dasselbe Capability-Gate wie ein Prozessschritt — Freiheit des Denkens bedeutet nicht Freiheit von Regeln. Optional: Selbstkritik und die Strategie „vorschlagen — ausführen — verifizieren“.

**4. Modellgenerierter Code läuft in einer echten Sandbox.**
Für „merge diese 12 Tabellen und finde Anomalien“ schreibt das Modell ein JavaScript-Programm. Es durchläuft eine statische Analyse mit einem echten Parser (Identifier-Allowlist — `eval`/`Function`/`Math.random` werden vor der Ausführung abgelehnt) und läuft dann unter QuickJS innerhalb von WebAssembly (Wasmtime) mit Fuel, Speicherlimit und Obergrenze für Tool-Aufrufe. WASI kommt mit einem leeren Capability-Set: weder Dateien noch Netzwerk, nicht einmal potenziell. Die einzige Host-Funktion läuft über dasselbe Gate.

**5. Speicher als Ingenieurssystem, nicht als Puffer.**
Der Arbeitsspeicher wird bei Budgetüberschreitung kompaktiert. Episodisch — Volltextsuche (FTS5). Semantisch — Deduplizierung von Fakten; Konflikte werden nie stillschweigend überschrieben; ein Speicherausfall ist von „keine Fakten vorhanden“ nicht zu unterscheiden und erzeugt keine falschen Duplikate. Ein Entitätengraph — Beziehungen zwischen Fakten, persistent. Skills — wiederverwendbare Rezepte zur Lösung ähnlicher Aufgaben, als lesbare Dateien.

**6. Ein Erweiterungsökosystem mit Rechte-Obergrenze.**
- **Skills** (SKILL.md) — Expertenrollen für den Chat: Das Triggern erfolgt per Code (nicht durch das Modell); die Tool-Obergrenze wird durch den Filter des Dispatchers durchgesetzt.
- **Subagenten** (agent.yaml) — eine verschachtelte Agentenschleife mit eigenem Budget und Journal; die Rechte des Kindes = Schnittmenge mit den Rechten des Elternteils; ausweiten lassen sie sich nicht. Verschachteltes Spawnen nur mit explizitem `allow_spawn: true`; die Tiefe ist per Code begrenzt.
- **Plugins** — isolierte Prozesse mit ACL-Manifest und Keyless-Signatur per sigstore: Installation aus einer Trusted List mit TOFU-Bestätigung, wie bei SSH.
- **MCP** — externe Tool-Server über das offene Model Context Protocol (offizielles Rust-SDK rmcp, ADR-0023): Sie werden über einen Abschnitt `[[mcp_servers]]` in der Konfiguration angebunden, reihen sich im gemeinsamen Dispatcher nach den eingebauten Tools und Plugins ein und durchlaufen dasselbe Capability-Gate wie jeder Prozessschritt. Es funktioniert auch umgekehrt: Berimor kann eigene Tools über MCP anbieten.

All das installiert sich mit einem einzigen Befehl — aus dem Katalog oder aus **beliebigen git-Repositories**: `berimor skill install code-review-ru --from https://github.com/...`.

## Projektinfrastruktur

**Rust-Workspace mit einem Crate pro Komponente** — Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage. Das WASM-Gastmodul (`codeact-guest/`) lebt als separates Crate und ist als fertiges Artefakt eingecheckt — der normale Build wird nicht verlangsamt.

**Prüfdisziplin.** Jedes Release: `cargo fmt` + `clippy -D warnings` + `cargo test --workspace` (927 Tests: Unit-, Integrations-, E2E-Tests über das echte Binary, goldene Fixtures von Prozessen und bösartigen Eingaben). Kritische Komponenten durchlaufen ein obligatorisches unabhängiges Review. Ein vollständiges eigenständiges Audit (`docs/audit-2026-07-31.md`) — **alle Befunde sind geschlossen oder bewusst dokumentiert**.

**Supply Chain auf erwachsenem Niveau.** Plattformübergreifende Releases (Linux x64/arm64, macOS arm64, Windows x64) mit Keyless-Signatur per cosign/sigstore — der private Schlüssel existiert nirgendwo. Verifikation: `berimor verify <Archiv>`. npm-Veröffentlichung mit Provenance, SBOM (CycloneDX) in der Pipeline, Selbstaktualisierung (`berimor self-update`) auf Primitiven der Process Engine implementiert — dasselbe Journal und dieselbe Fehlerwiederherstellung wie bei gewöhnlichen Prozessen, kein Ad-hoc-Skript.

**Architektur ist vor dem Code dokumentiert.** `docs/arch/` — eine eigenständige Spezifikation, auf jedem Stack implementierbar; `docs/ADR/` — ein Entscheidungsjournal mit den verworfenen Alternativen; `docs/ROADMAP.md` — die Aufgabenwarteschlange mit der jeweils zugewiesenen Modellklasse des Ausführers.

## Installation

### Weg 1: npm (am einfachsten)

```sh
npm install -g berimor
berimor --version
```

Der Installer erkennt Ihre Plattform automatisch, lädt das signierte Binary aus dem neuesten GitHub-Release herunter und prüft den SHA-256 vor dem Entpacken. Das Paket wird mit Provenance veröffentlicht (Build-Nachweis, an den CI-Workflow gebunden).

### Weg 2: Fertiges Binary von GitHub

Aktuelle Versionen finden Sie auf der [Releases](https://github.com/devpilgrin/berimor/releases/latest)-Seite. Nachfolgend die Befehle zum Herunterladen einer bestimmten Version (ersetzen Sie `v0.19.0` durch die gewünschte, falls ein neueres Release erschienen ist).

**Linux** (x64 oder arm64):

```sh
VERSION=v0.19.0
ARCH=x64   # oder arm64
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-linux-${ARCH}.tar.gz"
tar -xzf "berimor-${VERSION}-linux-${ARCH}.tar.gz"
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**macOS** (nur Apple Silicon — M1/M2/M3 und neuer; Intel-Builds werden derzeit nicht veröffentlicht, für Intel-Macs siehe Weg 3 unten):

```sh
VERSION=v0.19.0
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-darwin-arm64.tar.gz"
tar -xzf "berimor-${VERSION}-darwin-arm64.tar.gz"
xattr -d com.apple.quarantine berimor   # das Binary ist noch nicht von Apple signiert — sonst verweigert Gatekeeper den Start
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**Windows** (x64), PowerShell:

```powershell
$Version = "v0.19.0"
Invoke-WebRequest -Uri "https://github.com/devpilgrin/berimor/releases/download/$Version/berimor-$Version-win32-x64.zip" -OutFile berimor.zip
Expand-Archive -Path berimor.zip -DestinationPath .\
.\berimor.exe --version
```

Das Binary ist noch nicht signiert — Windows SmartScreen zeigt möglicherweise die Warnung „Der Computer wurde durch Windows geschützt“ an: „Weitere Informationen“ → „Trotzdem ausführen“. Um `berimor` aus jedem Ordner aufrufen zu können, verschieben Sie `berimor.exe` in ein Verzeichnis, das bereits im `PATH` liegt, oder fügen Sie den aktuellen Ordner selbst zum `PATH` hinzu.

Jedes Archiv wird von einer Datei `<Archiv>.sigstore.json` begleitet — einer Keyless-Signatur per cosign/sigstore, gebunden an die Identität des CI-Workflows, mit dem das Release gebaut wurde (ADR-0026). Verifizieren mit: `berimor verify <Archiv>` — der Befehl steckt bereits im heruntergeladenen Binary (beim ersten Aufruf installiert er über das Netzwerk eine frische sigstore-Vertrauenswurzel). Das ist eine von Apple/Microsoft unabhängige Signatur — die oben genannten Gatekeeper-/SmartScreen-Warnungen hebt sie nicht auf; diese betreffen einen separaten, noch nicht umgesetzten Schritt.

### Weg 3: Aus dem Quellcode bauen (jedes OS)

Sie brauchen nur [Rust](https://rustup.rs/) (stabile Version):

```sh
git clone https://github.com/devpilgrin/berimor.git
cd berimor
cargo build --release -p berimor-cli
./target/release/berimor --version
```

Unter Windows lautet der letzte Befehl `.\target\release\berimor.exe --version`.

## Schnellstart

```sh
berimor          # = berimor chat: interaktiver Dialog mit dem Agenten
```

Beim ersten Start bietet der Assistent an, Modelle aus Presets anzubinden (Kimi, DeepSeek, OpenAI, Claude via OpenRouter, lokale Modelle via Ollama/llama.cpp/LM Studio) — wählen Sie Nummern oder Namen, fügen Sie den API-Schlüssel ein (er landet in `~/.config/berimor/secrets.env` mit den Rechten „nur Eigentümer“, nicht in der Konfiguration). Statt eines API-Schlüssels können Sie sich auch mit einem Abonnement anmelden — `berimor login` (OAuth mit PKCE: Claude Pro/Max, ChatGPT Plus/Pro; Tokens landen in derselben `secrets.env`, die Erneuerung erfolgt transparent). Später geht dasselbe mit `berimor setup` oder direkt im Chat mit dem Befehl `/models add`.

Nützliche Chat-Befehle: `/help`, `/models`, `/skills`, `/config`, `/exit`. Die Sprache der TUI-Oberfläche — `/config locale` (8 Sprachen: ru, en, de, fr, es, zh-CN, ja, ko; die Auswahl wird in der lokalen Konfiguration gespeichert, Abschnitt `[ui]`).

Deterministische Prozesse (deklarativer YAML-Plan mit strengen Verträgen — der primäre „Kampfmodus“): `berimor run <process.yaml>`. Beispiele für Prozesse und Konfigurationen finden sich in [`fixtures/golden/processes/`](fixtures/golden/processes/) und [`CONTRIBUTING.md`](CONTRIBUTING.md).

Automatisierung auf Basis von Prozessen: `berimor schedule add` + `berimor daemon` — zeitgesteuerte Ausführung von Prozessen; `berimor serve` — ein HTTP-Dienst auf Basis von run/schedule/sessions (mit Token, ohne anonymen Zugriff); `berimor sessions` — ein Register der laufenden Sitzungen des Hosts; `berimor trace <Instanz>` — menschenlesbare Journal-Nachverfolgung eines beliebigen Laufs.

Erweiterungen mit einem Befehl:

```sh
berimor skill install code-review-ru                                    # aus dem Katalog
berimor skill install my-skill --from https://github.com/user/repo      # aus beliebigem git
berimor agent install researcher
berimor plugin install devpilgrin/berimor-plugin-hello                  # signiertes Plugin
berimor plugin install-local ./my-plugin --allow-unsigned               # lokal, bewusst
```

## Wie das Projekt aufgebaut ist

| Schicht | Verzeichnis | Inhalt |
|---|---|---|
| Agentenkern | `crates/` | Rust-Workspace — ein Crate pro Komponente: Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage |
| CodeAct-Sandbox | `codeact-guest/` | QuickJS-Gast für wasm32-wasip1 — separates Crate, als fertiges Artefakt eingecheckt |
| Bootstrap | `bootstrap/` | npm-Paket für Installation/Updates (TypeScript), siehe „Installation“ oben |
| Architektur | `docs/arch/` | eigenständige Spezifikation — Prinzipien, Komponenten, Diagramme (`docs/arch/views/`). Siehe `docs/arch/README.md` |
| Entscheidungen | `docs/ADR/` | Journal der Architekturentscheidungen: Kontext, Alternativen, Konsequenzen. Siehe `docs/ADR/README.md` |
| Entwicklungsplan | `docs/ROADMAP.md` | Aufgabenwarteschlange nach Phasen, Zerlegung in Teilaufgaben, Komplexität, Modellklasse des Ausführers |
| Audit | `docs/audit-2026-07-31.md` | unabhängiges Sicherheitsaudit — alle Befunde geschlossen oder bewusst dokumentiert |
| Testdaten | `fixtures/golden/` | goldene Sets: Beispiele für Prozesse, Verträge, bösartige Eingaben |
| Forschung | `docs/rnd/` | unterstützende Schicht: Quellen und Analyse bestehender Agenten-Frameworks. Siehe `docs/rnd/README.md` |

`crates/` und `bootstrap/` sind der Agent selbst — Code, der aus der Warteschlange in `docs/ROADMAP.md` geschrieben wurde. `docs/arch/` ist die Schicht reiner Entscheidungen dahinter: Sie erwähnt keine konkreten Projekte und Produkte (mit Ausnahme von `docs/arch/deployment.md` und `docs/arch/stack.md`, wo dies eine bewusste Ausnahme ist) und beschreibt die Architektur so, dass sie auf jedem Stack implementiert werden kann. `docs/ADR/` hält fest, warum jede Entscheidung getroffen wurde, einschließlich der verworfenen Alternativen. `docs/rnd/` ist die unterstützende Quellenschicht, auf der das Design aufbaute; sie ist kein Teil des Agenten.

## Lizenz

Apache License 2.0 — siehe [`LICENSE`](LICENSE).

## Mitmachen

Siehe [`CONTRIBUTING.md`](CONTRIBUTING.md) und [`docs/ROADMAP.md`](docs/ROADMAP.md) zur Auswahl einer Aufgabe.
