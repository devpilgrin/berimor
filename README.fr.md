<div align="center">

<img src="docs/assets/logo.png" alt="Berimor" width="640">

**Le modèle pense. Le code décide.**

[Русский](README.md) · [English](README.en.md) · [Deutsch](README.de.md) · **[Français](README.fr.md)** · [Español](README.es.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md)

</div>

Agent universel pour LLM à noyau déterministe : le routage des tâches, le branchement des processus, la sélection du contexte et l'admission à l'exécution sont décidés par du code — le modèle exécute des étapes étroites et vérifiables. Fonctionne avec des modèles locaux et cloud, faibles et puissants.

[![GitHub release](https://img.shields.io/github/v/release/devpilgrin/berimor?logo=github&label=release)](https://github.com/devpilgrin/berimor/releases/latest)
[![npm](https://img.shields.io/npm/v/berimor?logo=npm&label=npm)](https://www.npmjs.com/package/berimor)
[![CI](https://img.shields.io/github/actions/workflow/status/devpilgrin/berimor/ci.yml?branch=main&label=CI)](https://github.com/devpilgrin/berimor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-844%20green-brightgreen)](#infrastructure-du-projet)

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

## Pourquoi c'est nécessaire

La plupart des « agents IA » sont construits de la même façon : on donne au modèle un ensemble d'outils et on lui demande de décider lui-même quoi faire. Pour une démo — pratique. En production — peu fiable : le modèle oublie des étapes, invente des faits, part dans la mauvaise direction, et une commande dangereuse part dans le terminal sur un « y » tapé machinalement.

Berimor repose sur le postulat inverse : **on ne peut pas confier l'orchestration au modèle — on peut lui confier l'exécution.** La tâche est décomposée en étapes à l'avance ou pilotée par une boucle déterministe ; tout ce que produit le modèle subit une vérification stricte avant que l'on puisse s'y fier ; tout ce qui peut nuire passe par une barrière qui ne s'annule pas d'une pression sur Entrée.

| | CLI agentique typique | Berimor |
|---|---|---|
| Qui décide de la suite | Le modèle (en espérant qu'il soit raisonnable) | Le code (graphe de processus, boucle déterministe) |
| Panne au milieu d'une tâche | « Relancez et priez » | Journal d'événements : reprise exactement au point d'interruption |
| Action dangereuse | Confirmation que la fatigue transforme en YOLO | Deny-statique : l'interdit n'est tout simplement pas demandé |
| Modèle faible/local | « Achetez un modèle plus cher » | Médiation : retry avec explication de l'erreur → escalade vers l'humain |
| Extensions | Le plugin reçoit tout | Le sous-agent/plugin reçoit un sous-ensemble des droits du parent — par le code |
| Reproductibilité | Aucune | Complète : journal → replay → état à n'importe quel instant |

## Ce qui le distingue

**1. Les décisions — du code déterministe, pas du texte dans un prompt.**
Branchements, boucles, timeouts, branches parallèles avec barrière join, migration de versions d'un processus en cours — tout cela relève du Process Engine, et non de l'espoir que le modèle se souvienne des instructions. On ne peut pas confier aux modèles faibles la sélection du contexte et le routage — c'est donc le code qui s'en charge.

**2. La sécurité — une structure, pas une discipline de l'utilisateur.**
La table deny des opérations destructives ne se contourne pas par une confirmation. Le jail de fichiers ne sort pas du dossier de travail. La barrière réseau ne laisse pas passer vers les plages fermées (y compris les camouflages NAT64/6to4/Teredo et les contournements par redirections et userinfo dans l'URL). Les secrets sont masqués à tous les points de fuite — mais la barrière d'admission voit les vraies valeurs : le masquage n'aveugle pas la vérification.

**3. Boucle libre — sous surveillance.**
Mode « raisonnement → action → observation » pour les tâches impossibles à décomposer en étapes à l'avance. Chaque action interne passe par la même barrière de capabilities qu'une étape de processus — la liberté de raisonnement ne signifie pas la liberté vis-à-vis des règles. En option : autocritique et stratégie « proposer — exécuter — vérifier ».

**4. Le code du modèle s'exécute dans une vraie sandbox.**
Pour « fusionne 12 tables et trouve les anomalies », le modèle écrit un programme JavaScript. Celui-ci passe une analyse statique par un vrai parseur (liste blanche d'identifiants — `eval`/`Function`/`Math.random` sont rejetés avant exécution), puis est exécuté par QuickJS au sein de WebAssembly (Wasmtime) avec du fuel, une limite mémoire et un plafond d'appels d'outils. WASI — avec un jeu de droits vide : ni fichiers ni réseau, même potentiellement. L'unique fonction hôte passe par la même barrière.

**5. La mémoire — comme un système d'ingénierie, pas comme un buffer.**
La mémoire de travail se compacte en cas de dépassement du budget. L'épisodique — recherche plein texte (FTS5). La sémantique — déduplication des faits, les conflits ne sont pas écrasés silencieusement, une panne du stockage est indiscernable de « pas de faits » et ne génère pas de faux doublons. Graphe d'entités — relations entre faits, persistant. Skills — recettes réutilisables pour résoudre des tâches similaires, sous forme de fichiers lisibles.

**6. Écosystème d'extensions avec plafond de droits.**
- **Skills** (SKILL.md) — rôles experts pour le chat : déclencheur par le code (pas par le modèle), plafond d'outils par le filtre du dispatcher.
- **Sous-agents** (agent.yaml) — boucle agentique imbriquée avec son propre budget et journal ; droits de l'enfant = intersection avec ceux du parent, extension impossible. Imbrication de spawn — uniquement avec `allow_spawn: true` explicite, profondeur limitée par le code.
- **Plugins** — processus isolés avec manifeste ACL et signature keyless sigstore : installation depuis une liste de confiance avec confirmation TOFU, comme SSH.
- **MCP** — serveurs d'outils externes via le protocole ouvert Model Context Protocol (SDK Rust officiel rmcp, ADR-0023) : ils se connectent par la section `[[mcp_servers]]` de la config, rejoignent le dispatcher commun après les outils intégrés et les plugins, et passent la même barrière de capabilities que n'importe quelle étape de processus. Fonctionne aussi dans l'autre sens : Berimor peut exposer ses propres outils via MCP.

Tout cela s'installe en une seule commande — depuis le catalogue ou **n'importe quel dépôt git** : `berimor skill install code-review-ru --from https://github.com/...`.

## Infrastructure du projet

**Workspace Rust à raison d'un crate par composant** — Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage. Le module WASM invité (`codeact-guest/`) vit comme un crate séparé et est commité en tant qu'artefact prêt à l'emploi — le build normal n'est pas ralenti.

**Discipline de vérification.** Chaque release : `cargo fmt` + `clippy -D warnings` + `cargo test --workspace` (844 tests : unitaires, d'intégration, e2e via le vrai binaire, fixtures golden de processus et d'entrées malveillantes). Les composants critiques passent une revue indépendante obligatoire. Audit complet autonome (`docs/audit-2026-07-31.md`) — **tous les constats sont corrigés ou documentés en connaissance de cause**.

**Supply chain comme les grands.** Releases multiplateformes (Linux x64/arm64, macOS arm64, Windows x64) avec signature keyless cosign/sigstore — aucune clé privée n'existe nulle part. Vérification : `berimor verify <archive>`. Publication npm avec provenance, SBOM (CycloneDX) dans le pipeline, l'auto-mise à jour (`berimor self-update`) est implémentée sur les primitives du Process Engine — même journal et même reprise après panne que pour les processus ordinaires, et non un script ad hoc.

**Architecture documentée avant le code.** `docs/arch/` — spécification autosuffisante, implémentable sur n'importe quel stack ; `docs/ADR/` — journal des décisions avec les alternatives rejetées ; `docs/ROADMAP.md` — file de tâches avec la classe de modèle exécutant pour chacune.

## Installation

### Méthode 1 : npm (la plus simple)

```sh
npm install -g berimor
berimor --version
```

L'installateur détecte lui-même la plateforme, télécharge le binaire signé depuis la dernière release GitHub et vérifie le SHA-256 avant décompression. Le paquet est publié avec provenance (liaison du build au workflow CI).

### Méthode 2 : binaire prêt à l'emploi depuis GitHub

Les versions à jour se trouvent sur la page des [releases](https://github.com/devpilgrin/berimor/releases/latest). Ci-dessous — les commandes pour télécharger une version précise (remplacez `v0.19.0` par celle voulue si une version plus récente est sortie).

**Linux** (x64 ou arm64) :

```sh
VERSION=v0.19.0
ARCH=x64   # ou arm64
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-linux-${ARCH}.tar.gz"
tar -xzf "berimor-${VERSION}-linux-${ARCH}.tar.gz"
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**macOS** (Apple Silicon uniquement — M1/M2/M3 et plus récent ; les builds Intel ne sont pas encore publiés, pour un Mac Intel — méthode 3 ci-dessous) :

```sh
VERSION=v0.19.0
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-darwin-arm64.tar.gz"
tar -xzf "berimor-${VERSION}-darwin-arm64.tar.gz"
xattr -d com.apple.quarantine berimor   # le binaire n'est pas encore signé Apple — sinon Gatekeeper refusera de le lancer
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**Windows** (x64), PowerShell :

```powershell
$Version = "v0.19.0"
Invoke-WebRequest -Uri "https://github.com/devpilgrin/berimor/releases/download/$Version/berimor-$Version-win32-x64.zip" -OutFile berimor.zip
Expand-Archive -Path berimor.zip -DestinationPath .\
.\berimor.exe --version
```

Le binaire n'est pas encore signé — Windows SmartScreen peut afficher l'avertissement « Windows a protégé votre ordinateur » : « Informations complémentaires » → « Exécuter quand même ». Pour appeler `berimor` depuis n'importe quel dossier, déplacez `berimor.exe` dans un répertoire déjà présent dans le `PATH`, ou ajoutez vous-même le dossier courant au `PATH`.

Chaque archive est accompagnée d'un fichier `<archive>.sigstore.json` — signature keyless cosign/sigstore liée à l'identité du workflow CI qui a construit la release (ADR-0026). Vérifier : `berimor verify <archive>` — la commande est déjà dans le binaire téléchargé (installe la racine de confiance sigstore à jour via le réseau au premier appel). Il s'agit d'une signature indépendante d'Apple/Microsoft — elle ne lève pas les avertissements Gatekeeper/SmartScreen ci-dessus, qui concernent une étape distincte, pas encore réalisée.

### Méthode 3 : compiler depuis les sources (tout OS)

Seul [Rust](https://rustup.rs/) est nécessaire (version stable) :

```sh
git clone https://github.com/devpilgrin/berimor.git
cd berimor
cargo build --release -p berimor-cli
./target/release/berimor --version
```

Sous Windows, la dernière commande est `.\target\release\berimor.exe --version`.

## Démarrage rapide

```sh
berimor          # = berimor chat : dialogue interactif avec l'agent
```

Au premier lancement, l'assistant proposera de connecter des modèles depuis des presets (Kimi, DeepSeek, OpenAI, Claude via OpenRouter, locaux via Ollama/llama.cpp/LM Studio) — choisissez les numéros ou les noms, collez la clé API (elle ira dans `~/.config/berimor/secrets.env` avec des droits « propriétaire seul », pas dans la config). Au lieu d'une clé API, on peut se connecter par abonnement — `berimor login` (OAuth avec PKCE : Claude Pro/Max, ChatGPT Plus/Pro ; les jetons vont dans le même `secrets.env`, le rafraîchissement est transparent). Plus tard, la même chose — `berimor setup` ou directement dans le chat avec la commande `/models add`.

Commandes utiles du chat : `/help`, `/models`, `/skills`, `/config`, `/exit`.

Processus déterministes (plan YAML déclaratif à contrats stricts — le principal mode « combat ») : `berimor run <process.yaml>`. Exemples de processus et de configurations — dans [`fixtures/golden/processes/`](fixtures/golden/processes/) et [`CONTRIBUTING.md`](CONTRIBUTING.md).

Automatisation par-dessus les processus : `berimor schedule add` + `berimor daemon` — exécution des processus selon un calendrier ; `berimor serve` — service HTTP par-dessus run/schedule/sessions (avec jeton, sans accès anonyme) ; `berimor sessions` — registre des sessions actives de l'hôte ; `berimor trace <instance>` — traçage lisible du journal de n'importe quelle exécution.

Extensions en une commande :

```sh
berimor skill install code-review-ru                                    # depuis le catalogue
berimor skill install my-skill --from https://github.com/user/repo      # depuis n'importe quel git
berimor agent install researcher
berimor plugin install devpilgrin/berimor-plugin-hello                  # plugin signé
berimor plugin install-local ./my-plugin --allow-unsigned               # local, en connaissance de cause
```

## Structure du projet

| Couche | Répertoire | Contenu |
|---|---|---|
| Noyau de l'agent | `crates/` | Workspace Rust — un crate par composant : Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage |
| Sandbox CodeAct | `codeact-guest/` | Invité QuickJS sous wasm32-wasip1 — crate séparé, commité comme artefact prêt à l'emploi |
| Bootstrap | `bootstrap/` | Paquet npm d'installation/mise à jour (TypeScript), voir « Installation » ci-dessus |
| Architecture | `docs/arch/` | Spécification autosuffisante — principes, composants, diagrammes (`docs/arch/views/`). Voir `docs/arch/README.md` |
| Décisions | `docs/ADR/` | Journal des décisions d'architecture : contexte, alternatives, conséquences. Voir `docs/ADR/README.md` |
| Plan de développement | `docs/ROADMAP.md` | File de tâches par phases, décomposition en sous-tâches, complexité, classe de modèle exécutant |
| Audit | `docs/audit-2026-07-31.md` | Audit de sécurité indépendant — tous les constats corrigés ou documentés en connaissance de cause |
| Données de test | `fixtures/golden/` | Jeux golden : exemples de processus, contrats, entrées malveillantes |
| Recherches | `docs/rnd/` | Couche auxiliaire : sources et analyse des frameworks agentiques existants. Voir `docs/rnd/README.md` |

`crates/` et `bootstrap/` — l'agent lui-même, du code écrit selon la file de `docs/ROADMAP.md`. `docs/arch/` — la couche de décisions pures derrière lui : ne mentionne pas de projets ni produits concrets (sauf `docs/arch/deployment.md` et `docs/arch/stack.md`, où c'est une exception assumée), expose l'architecture de façon à ce qu'elle puisse être implémentée sur n'importe quel stack. `docs/ADR/` consigne pourquoi chaque décision a été prise, y compris les alternatives rejetées. `docs/rnd/` — couche auxiliaire de sources sur laquelle s'est appuyée la conception, ne fait pas partie de l'agent.

## Licence

Apache License 2.0 — voir [`LICENSE`](LICENSE).

## Contribuer

Voir [`CONTRIBUTING.md`](CONTRIBUTING.md) et [`docs/ROADMAP.md`](docs/ROADMAP.md) pour choisir une tâche.
