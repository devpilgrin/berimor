<div align="center">

<img src="docs/assets/logo.png" alt="Berimor" width="640">

**El modelo piensa. El código decide.**

[Русский](README.md) · [English](README.en.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · **[Español](README.es.md)** · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md)

</div>

Agente universal para LLM con núcleo determinista: el enrutamiento de tareas, la ramificación de procesos, la selección de contexto y la admisión a la ejecución los decide el código — el modelo ejecuta pasos estrechos y verificables. Funciona con modelos locales y en la nube, débiles y potentes.

[![GitHub release](https://img.shields.io/github/v/release/devpilgrin/berimor?logo=github&label=release)](https://github.com/devpilgrin/berimor/releases/latest)
[![npm](https://img.shields.io/npm/v/berimor?logo=npm&label=npm)](https://www.npmjs.com/package/berimor)
[![CI](https://img.shields.io/github/actions/workflow/status/devpilgrin/berimor/ci.yml?branch=main&label=CI)](https://github.com/devpilgrin/berimor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-719%20green-brightgreen)](#infraestructura-del-proyecto)

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

## Para qué sirve

La mayoría de los «agentes de IA» están construidos igual: se le da al modelo un conjunto de herramientas y se le pide que decida por sí mismo qué hacer. Para una demo — cómodo. En el trabajo — poco fiable: el modelo olvida pasos, inventa hechos, se desvía por el camino equivocado, y un comando peligroso sale al terminal con un «y» pulsado por inercia.

Berimor se construye sobre el supuesto contrario: **no se puede confiar la orquestación al modelo — se le puede confiar la ejecución.** La tarea se descompone en pasos por adelantado o la dirige un bucle determinista; todo lo que produce el modelo pasa una verificación estricta antes de poder confiar en ello; todo lo que puede dañar pasa por una barrera que no se anula pulsando Enter.

| | CLI agéntico típico | Berimor |
|---|---|---|
| Quién decide qué hacer después | El modelo (con la esperanza de que sea sensato) | El código (grafo de proceso, bucle determinista) |
| Fallo a mitad de la tarea | «Reinicia y reza» | Registro de eventos: continuación exactamente desde el punto de corte |
| Acción peligrosa | Confirmación que el cansancio convierte en YOLO | Deny-estática: lo prohibido ni siquiera se pregunta |
| Modelo débil/local | «Compra un modelo más caro» | Mediación: reintento con explicación del error → escalado al humano |
| Extensiones | El plugin lo recibe todo | El subagente/plugin recibe un subconjunto de los derechos del padre — por código |
| Reproducibilidad | Ninguna | Completa: registro → replay → estado en cualquier momento |

## En qué se diferencia

**1. Las decisiones — código determinista, no texto en el prompt.**
Ramificaciones, bucles, timeouts, ramas paralelas con barrera join, migración de versiones de un proceso en ejecución — todo esto es Process Engine, no la esperanza de que el modelo recuerde las instrucciones. A los modelos débiles no se les puede confiar la selección de contexto ni el enrutamiento — por tanto, de eso se encarga el código.

**2. La seguridad — estructura, no disciplina del usuario.**
La tabla deny de operaciones destructivas no se revoca con una confirmación. El jail de archivos no sale de la carpeta de trabajo. La barrera de red no deja pasar hacia rangos cerrados (incluidos los camuflajes NAT64/6to4/Teredo y los bypasses mediante redirecciones y userinfo en la URL). Los secretos se enmascaran en todos los puntos de fuga — pero la barrera de admisión ve los valores reales: el enmascaramiento no ciega la verificación.

**3. Bucle libre — bajo supervisión.**
Modo «razonamiento → acción → observación» para tareas que no se pueden descomponer en pasos por adelantado. Cada acción interna pasa por la misma barrera de capabilities que un paso de proceso — la libertad de razonamiento no significa libertad de las reglas. Opcional: autocrítica y estrategia «proponer — ejecutar — verificar».

**4. El código del modelo se ejecuta en una sandbox de verdad.**
Para «fusiona 12 tablas y encuentra anomalías», el modelo escribe un programa JavaScript. Este pasa un análisis estático con un parser real (lista blanca de identificadores — `eval`/`Function`/`Math.random` se rechazan antes de la ejecución), y se ejecuta con QuickJS dentro de WebAssembly (Wasmtime) con fuel, límite de memoria y techo de llamadas a herramientas. WASI — con un conjunto de derechos vacío: ni archivos ni red, ni siquiera potencialmente. La única función host pasa por la misma barrera.

**5. La memoria — como un sistema de ingeniería, no como un búfer.**
La memoria de trabajo se compacta al desbordar el presupuesto. La episódica — búsqueda de texto completo (FTS5). La semántica — deduplicación de hechos, los conflictos no se sobrescriben en silencio, un fallo del almacenamiento es indistinguible de «no hay hechos» y no genera duplicados falsos. Grafo de entidades — relaciones entre hechos, persistente. Skills — recetas reutilizables para resolver tareas similares, archivos legibles.

**6. Ecosistema de extensiones con techo de derechos.**
- **Skills** (SKILL.md) — roles expertos para el chat: disparador por código (no por el modelo), techo de herramientas por el filtro del dispatcher.
- **Subagentes** (agent.yaml) — bucle agéntico anidado con su propio presupuesto y registro; derechos del hijo = intersección con los del padre, no se pueden ampliar. El spawn anidado — solo con `allow_spawn: true` explícito, profundidad limitada por código.
- **Plugins** — procesos aislados con manifiesto ACL y firma keyless sigstore: instalación desde una lista de confianza con confirmación TOFU, como SSH.

Todo esto se instala con un solo comando — desde el catálogo o **cualquier repositorio git**: `berimor skill install code-review-ru --from https://github.com/...`.

## Infraestructura del proyecto

**Workspace Rust con un crate por componente** — Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage. El módulo WASM invitado (`codeact-guest/`) vive como un crate separado y está commiteado como artefacto listo — el build normal no se ralentiza.

**Disciplina de verificación.** Cada release: `cargo fmt` + `clippy -D warnings` + `cargo test --workspace` (719 tests: unitarios, de integración, e2e a través del binario real, fixtures golden de procesos y de entradas maliciosas). Los componentes críticos pasan una revisión independiente obligatoria. Auditoría completa e independiente (`docs/audit-2026-07-31.md`) — **todos los hallazgos están cerrados o documentados conscientemente**.

**Supply chain como la gente seria.** Releases multiplataforma (Linux x64/arm64, macOS arm64, Windows x64) con firma keyless cosign/sigstore — la clave privada no existe en ningún sitio. Verificación: `berimor verify <archivo>`. Publicación npm con provenance, SBOM (CycloneDX) en el pipeline, la autoactualización (`berimor self-update`) está implementada sobre las primitivas del Process Engine — el mismo registro y la misma recuperación tras fallo que los procesos ordinarios, no un script ad hoc.

**Arquitectura documentada antes que el código.** `docs/arch/` — especificación autosuficiente, implementable en cualquier stack; `docs/ADR/` — registro de decisiones con las alternativas rechazadas; `docs/ROADMAP.md` — cola de tareas con la clase de modelo ejecutor para cada una.

## Instalación

### Método 1: npm (el más sencillo)

```sh
npm install -g berimor
berimor --version
```

El instalador detecta la plataforma por sí mismo, descarga el binario firmado desde la última release de GitHub y comprueba el SHA-256 antes de descomprimir. El paquete se publica con provenance (vinculación del build al workflow de CI).

### Método 2: binario listo desde GitHub

Las versiones actuales están en la página de [releases](https://github.com/devpilgrin/berimor/releases/latest). Abajo — los comandos para descargar una versión concreta (sustituye `v0.19.0` por la que necesites si ha salido una más reciente).

**Linux** (x64 o arm64):

```sh
VERSION=v0.19.0
ARCH=x64   # o arm64
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-linux-${ARCH}.tar.gz"
tar -xzf "berimor-${VERSION}-linux-${ARCH}.tar.gz"
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**macOS** (solo Apple Silicon — M1/M2/M3 y más nuevos; los builds para Intel aún no se publican, para Mac Intel — método 3 más abajo):

```sh
VERSION=v0.19.0
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-darwin-arm64.tar.gz"
tar -xzf "berimor-${VERSION}-darwin-arm64.tar.gz"
xattr -d com.apple.quarantine berimor   # el binario aún no está firmado por Apple — si no, Gatekeeper se negará a ejecutarlo
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

El binario aún no está firmado — Windows SmartScreen puede mostrar el aviso «Windows protegió su equipo»: «Más información» → «Ejecutar de todos modos». Para invocar `berimor` desde cualquier carpeta, mueve `berimor.exe` a un directorio que ya esté en el `PATH`, o añade tú mismo la carpeta actual al `PATH`.

Cada archivo va acompañado de un fichero `<archivo>.sigstore.json` — firma keyless cosign/sigstore vinculada a la identidad del workflow de CI con el que se construyó la release (ADR-0026). Verificar: `berimor verify <archivo>` — el propio comando ya está en el binario descargado (instala la raíz de confianza sigstore actualizada por red en la primera llamada). Es una firma independiente de Apple/Microsoft — no elimina los avisos de Gatekeeper/SmartScreen de arriba, que corresponden a un paso distinto, aún no realizado.

### Método 3: compilar desde las fuentes (cualquier SO)

Solo se necesita [Rust](https://rustup.rs/) (versión estable):

```sh
git clone https://github.com/devpilgrin/berimor.git
cd berimor
cargo build --release -p berimor-cli
./target/release/berimor --version
```

En Windows, el último comando es `.\target\release\berimor.exe --version`.

## Inicio rápido

```sh
berimor          # = berimor chat: diálogo interactivo con el agente
```

En el primer arranque, el asistente propondrá conectar modelos desde presets (Kimi, DeepSeek, OpenAI, Claude a través de OpenRouter, locales a través de Ollama/llama.cpp/LM Studio) — elige números o nombres, pega la clave de API (irá a `~/.config/berimor/secrets.env` con permisos «solo propietario», no a la config). Más tarde, lo mismo — `berimor setup` o directamente en el chat con el comando `/models add`.

Comandos útiles del chat: `/help`, `/models`, `/skills`, `/config`, `/exit`.

Procesos deterministas (plan YAML declarativo con contratos estrictos — el principal modo «de combate»): `berimor run <process.yaml>`. Ejemplos de procesos y configuraciones — en [`fixtures/golden/processes/`](fixtures/golden/processes/) y [`CONTRIBUTING.md`](CONTRIBUTING.md).

Extensiones con un comando:

```sh
berimor skill install code-review-ru                                    # desde el catálogo
berimor skill install my-skill --from https://github.com/user/repo      # desde cualquier git
berimor agent install researcher
berimor plugin install devpilgrin/berimor-plugin-hello                  # plugin firmado
berimor plugin install-local ./my-plugin --allow-unsigned               # local, conscientemente
```

## Estructura del proyecto

| Capa | Directorio | Contenido |
|---|---|---|
| Núcleo del agente | `crates/` | Workspace Rust — un crate por componente: Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage |
| Sandbox CodeAct | `codeact-guest/` | Invitado QuickJS bajo wasm32-wasip1 — crate separado, commiteado como artefacto listo |
| Bootstrap | `bootstrap/` | Paquete npm del instalador/actualizador (TypeScript), ver «Instalación» arriba |
| Arquitectura | `docs/arch/` | Especificación autosuficiente — principios, componentes, diagramas (`docs/arch/views/`). Ver `docs/arch/README.md` |
| Decisiones | `docs/ADR/` | Registro de decisiones de arquitectura: contexto, alternativas, consecuencias. Ver `docs/ADR/README.md` |
| Plan de desarrollo | `docs/ROADMAP.md` | Cola de tareas por fases, descomposición en subtareas, complejidad, clase de modelo ejecutor |
| Auditoría | `docs/audit-2026-07-31.md` | Auditoría de seguridad independiente — todos los hallazgos cerrados o documentados conscientemente |
| Datos de prueba | `fixtures/golden/` | Conjuntos golden: ejemplos de procesos, contratos, entradas maliciosas |
| Investigación | `docs/rnd/` | Capa auxiliar: fuentes y análisis de frameworks agénticos existentes. Ver `docs/rnd/README.md` |

`crates/` y `bootstrap/` — el agente en sí, código escrito según la cola de `docs/ROADMAP.md`. `docs/arch/` — la capa de decisiones puras detrás de él: no menciona proyectos ni productos concretos (salvo `docs/arch/deployment.md` y `docs/arch/stack.md`, donde es una excepción consciente), expone la arquitectura de modo que pueda implementarse en cualquier stack. `docs/ADR/` registra por qué se tomó cada decisión, incluidas las alternativas rechazadas. `docs/rnd/` — capa auxiliar de fuentes en la que se apoyó el diseño, no forma parte del agente.

## Licencia

Apache License 2.0 — ver [`LICENSE`](LICENSE).

## Contribuir

Ver [`CONTRIBUTING.md`](CONTRIBUTING.md) y [`docs/ROADMAP.md`](docs/ROADMAP.md) para elegir una tarea.
