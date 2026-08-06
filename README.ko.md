<div align="center">

<img src="docs/assets/logo.png" alt="Berimor" width="640">

**모델은 생각한다. 코드가 결정한다.**

[Русский](README.md) · [English](README.en.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · **[한국어](README.ko.md)**

</div>

결정론적 코어를 갖춘 LLM용 범용 에이전트: 작업 라우팅, 프로세스 분기, 컨텍스트 선별, 실행 허가는 코드가 결정하고, 모델은 좁고 검증 가능한 단계만 수행합니다. 로컬 모델과 클라우드 모델, 약한 모델과 강한 모델 모두와 함께 작동합니다.

[![GitHub release](https://img.shields.io/github/v/release/devpilgrin/berimor?logo=github&label=release)](https://github.com/devpilgrin/berimor/releases/latest)
[![npm](https://img.shields.io/npm/v/berimor?logo=npm&label=npm)](https://www.npmjs.com/package/berimor)
[![CI](https://img.shields.io/github/actions/workflow/status/devpilgrin/berimor/ci.yml?branch=main&label=CI)](https://github.com/devpilgrin/berimor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-719%20green-brightgreen)](#프로젝트-인프라)

![Rust](https://img.shields.io/badge/Rust-stable-DEA584?logo=rust&logoColor=white)
![WebAssembly](https://img.shields.io/badge/sandbox-Wasmtime-654FF0?logo=webassembly&logoColor=white)
![QuickJS](https://img.shields.io/badge/guest-QuickJS-F7DF1E?logo=javascript&logoColor=black)
![SQLite](https://img.shields.io/badge/storage-SQLite%20%2B%20FTS5%20%2B%20vec-003B57?logo=sqlite&logoColor=white)
![tokio](https://img.shields.io/badge/async-tokio-0B7B8A)
![MCP](https://img.shields.io/badge/protocol-MCP-5B5BD6)
![ratatui](https://img.shields.io/badge/TUI-ratatui-E95420)
![sigstore](https://img.shields.io/badge/supply--chain-sigstore%20keyless-2E8B57)
![oxc](https://img.shields.io/badge/static%20analysis-oxc__parser-black)

---

## 왜 필요한가

대부분의 "AI 에이전트"는 똑같이 생겼습니다: 모델에게 도구 세트를 주고 무엇을 할지 스스로 결정하게 합니다. 데모에는 편리하지만, 실무에는 믿을 수 없습니다: 모델은 단계를 잊어버리고, 사실을 지어내고, 엉뚱한 곳으로 빠지며, 위험한 명령은 습관적으로 누른 "y" 한 번에 터미널로 날아갑니다.

Berimor는 정반대의 전제 위에 세워졌습니다: **모델에게 오케스트레이션을 맡길 수는 없다 — 실행만 맡길 수 있다.** 작업은 미리 단계로 분필되거나 결정론적 루프에 의해 구동됩니다. 모델이 출력하는 모든 것은 신뢰할 수 있기 전에 엄격한 검증을 거칩니다. 해를 끼칠 수 있는 모든 것은 Enter 키로 취소되지 않는 게이트를 통과합니다.

| | 일반적인 에이전트 CLI | Berimor |
|---|---|---|
| 다음에 무엇을 할지 결정하는 주체 | 모델(상식에 대한 기대) | 코드(프로세스 그래프, 결정론적 루프) |
| 작업 도중 실패 | "재시작하고 기도하세요" | 이벤트 저널: 중단된 지점에서 정확히 재개 |
| 위험한 작업 | 피로가 YOLO로 바꿔버리는 확인 프롬프트 | Deny 정적 규칙: 금지된 것은 아예 묻지 않음 |
| 약한/로컬 모델 | "더 비싼 모델을 사세요" | 미디에이션: 오류 설명과 함께 재시도 → 사람에게 에스컬레이션 |
| 확장 | 플러그인이 모든 권한을 얻음 | 서브에이전트/플러그인은 부모 권한의 부분집합을 얻음 — 코드로 보장 |
| 재현성 | 없음 | 완전함: 저널 → 리플레이 → 임의 시점의 상태 |

## 무엇이 다른가

**1. 결정은 프롬프트 속 텍스트가 아니라 결정론적 코드다.**
분기, 루프, 타임아웃, join 배리어가 있는 병렬 브랜치, 실행 중인 프로세스의 버전 마이그레이션 — 이 모든 것은 모델이 지시를 기억할 것이라는 기대가 아니라 Process Engine입니다. 약한 모델에게 컨텍스트 선별과 라우팅을 맡길 수 없으므로, 코드가 이를 담당합니다.

**2. 보안은 사용자의 절제가 아니라 구조다.**
파괴적 작업의 Deny 테이블은 확인으로 뒤집을 수 없습니다. 파일 시스템 jail은 작업 디렉터리를 벗어나지 않습니다. 네트워크 게이트는 폐쇄된 대역을 허용하지 않습니다(NAT64/6to4/Teredo 위장과 리다이렉트 및 URL의 userinfo를 통한 우회 포함). 시크릿은 모든 유출 지점에서 마스킹됩니다 — 그러나 허가 게이트는 실제 값을 봅니다: 마스킹은 검사를 눈멀게 하지 않습니다.

**3. 자유 루프 — 감독 하에.**
미리 단계로 분해할 수 없는 작업을 위한 "추론 → 행동 → 관찰" 모드. 루프 내의 모든 행동은 프로세스 단계와 동일한 capability 게이트를 통과합니다 — 추론의 자유가 규칙으로부터의 자유를 의미하지는 않습니다. 선택 사항: 자기 비판과 "제안 — 실행 — 검증" 전략.

**4. 모델이 작성한 코드는 진짜 샌드박스에서 실행된다.**
"테이블 12개를 병합하고 이상 징후를 찾아라" 같은 작업에서 모델은 JavaScript 프로그램을 작성합니다. 프로그램은 실제 파서로 정적 분석을 거치고(식별자 화이트리스트 — `eval`/`Function`/`Math.random`은 실행 전에 거부됨), WebAssembly(Wasmtime) 안의 QuickJS에서 퓨얼, 메모리 상한, 도구 호출 횟수 상한과 함께 실행됩니다. WASI는 빈 권한 집합: 파일도 네트워크도 잠재적으로조차 없습니다. 유일한 호스트 함수도 동일한 게이트를 통과합니다.

**5. 메모리는 버퍼가 아니라 엔지니어링 시스템으로서.**
워킹 메모리는 예산 초과 시 압축됩니다. 에피소드 메모리 — 전문 검색(FTS5). 시맨틱 메모리 — 사실 중복 제거, 충돌은 조용히 덮어쓰이지 않으며, 스토리지 장애는 "사실이 없음"과 구분할 수 없고 거짓 중복을 만들지 않습니다. 엔티티 그래프 — 사실 간의 연결, 영속적. 스킬 — 유사한 작업을 해결하기 위한 재사용 가능한 레시피로, 읽을 수 있는 파일입니다.

**6. 권한 상한이 있는 확장 생태계.**
- **스킬**(SKILL.md) — 채팅용 전문가 역할: 트리거는 코드로(모델이 아니라), 도구 상한은 디스패처 필터로 보장.
- **서브에이전트**(agent.yaml) — 자체 예산과 저널을 가진 중첩 에이전트 루프; 자식의 권한 = 부모 권한과의 교집합이며 확장 불가. 중첩 스폰은 명시적 `allow_spawn: true`일 때만 가능하며 깊이는 코드로 제한.
- **플러그인** — ACL 매니페스트와 sigstore 키리스 서명을 갖춘 격리 프로세스: SSH처럼 신뢰 목록에서 설치하고 TOFU 확인.

이 모든 것을 한 명령으로 설치할 수 있습니다 — 카탈로그에서, 또는 **임의의 git 저장소**에서: `berimor skill install code-review-ru --from https://github.com/...`.

## 프로젝트 인프라

**컴포넌트당 하나의 크레이트로 구성된 Rust workspace** — Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage. 게스트 WASM 모듈(`codeact-guest/`)은 별도의 crate로 존재하며 빌드된 아티팩트로 커밋되어 있습니다 — 일반 빌드가 느려지지 않습니다.

**검증 규율.** 모든 릴리스: `cargo fmt` + `clippy -D warnings` + `cargo test --workspace`(719개 테스트: 유닛, 통합, 실제 바이너리를 통한 e2e, 프로세스 및 악의적 입력의 골든 픽스처). 중요 컴포넌트는 필수 독립 리뷰를 거칩니다. 전체 독립 감사(`docs/audit-2026-07-31.md`) — **모든 지적 사항이 해결되었거나 의식적으로 문서화됨**.

**어른의 서플라이 체인.** 크로스 플랫폼 릴리스(Linux x64/arm64, macOS arm64, Windows x64)에 cosign/sigstore 키리스 서명 — 개인 키는 어디에도 존재하지 않습니다. 검증: `berimor verify <아카이브>`. npm 퍼블리시는 provenance와 함께, 파이프라인에 SBOM(CycloneDX), 셀프 업데이트(`berimor self-update`)는 Process Engine 프리미티브 위에 구현 — 임시 스크립트가 아니라 일반 프로세스와 동일한 저널과 장애 복구.

**아키텍처는 코드보다 먼저 문서화.** `docs/arch/` — 어떤 스택에서도 구현 가능한 자족적 명세; `docs/ADR/` — 기각된 대안을 포함한 결정 저널; `docs/ROADMAP.md` — 각 작업에 실행 모델 클래스가 붙은 작업 큐.

## 설치

### 방법 1: npm(가장 간단)

```sh
npm install -g berimor
berimor --version
```

설치 프로그램이 플랫폼을 자동으로 감지하고, GitHub 최신 릴리스에서 서명된 바이너리를 다운로드하며, 압축 해제 전에 SHA-256을 대조합니다. 패키지는 provenance와 함께 퍼블리시됩니다(빌드가 CI 워크플로에 연결됨).

### 방법 2: GitHub의 미리 빌드된 바이너리

최신 버전은 [릴리스 페이지](https://github.com/devpilgrin/berimor/releases/latest)에 있습니다. 아래는 특정 버전을 다운로드하는 명령입니다(더 새로운 버전이 나왔다면 `v0.19.0`을 원하는 버전으로 바꾸세요).

**Linux**(x64 또는 arm64):

```sh
VERSION=v0.19.0
ARCH=x64   # 또는 arm64
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-linux-${ARCH}.tar.gz"
tar -xzf "berimor-${VERSION}-linux-${ARCH}.tar.gz"
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**macOS**(Apple Silicon 전용 — M1/M2/M3 이상; Intel 빌드는 아직 퍼블리시되지 않음, Intel Mac은 아래 방법 3 사용):

```sh
VERSION=v0.19.0
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-darwin-arm64.tar.gz"
tar -xzf "berimor-${VERSION}-darwin-arm64.tar.gz"
xattr -d com.apple.quarantine berimor   # 바이너리가 아직 Apple 서명되지 않음 — 그렇지 않으면 Gatekeeper가 실행을 거부함
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**Windows**(x64), PowerShell:

```powershell
$Version = "v0.19.0"
Invoke-WebRequest -Uri "https://github.com/devpilgrin/berimor/releases/download/$Version/berimor-$Version-win32-x64.zip" -OutFile berimor.zip
Expand-Archive -Path berimor.zip -DestinationPath .\
.\berimor.exe --version
```

바이너리는 아직 서명되지 않았습니다 — Windows SmartScreen이 "Windows에서 PC를 보호했습니다" 경고를 표시할 수 있습니다: "추가 정보" → "실행". 어느 폴더에서나 `berimor`를 호출하려면 `berimor.exe`를 이미 `PATH`에 있는 디렉터리로 옮기거나 현재 폴더를 직접 `PATH`에 추가하세요.

각 아카이브에는 `<아카이브>.sigstore.json` 파일이 동반됩니다 — 릴리스를 빌드한 CI 워크플로의 아이덴티티에 연결된 cosign/sigstore 키리스 서명(ADR-0026). 검증: `berimor verify <아카이브>` — 명령 자체는 다운로드한 바이너리에 이미 포함되어 있습니다(첫 호출 시 네트워크를 통해 최신 sigstore 신뢰 루트를 설치). 이것은 Apple/Microsoft와 무관한 서명입니다 — 위의 Gatekeeper/SmartScreen 경고를 없애주지 않으며, 그것들은 아직 완료되지 않은 별도의 단계에 관한 것입니다.

### 방법 3: 소스에서 빌드(모든 OS)

[Rust](https://rustup.rs/)(안정 버전)만 필요합니다:

```sh
git clone https://github.com/devpilgrin/berimor.git
cd berimor
cargo build --release -p berimor-cli
./target/release/berimor --version
```

Windows에서는 마지막 명령이 `.\target\release\berimor.exe --version`입니다.

## 빠른 시작

```sh
berimor          # = berimor chat: 에이전트와의 대화형 채팅
```

첫 실행 시 마법사가 프리셋에서 모델 연결을 제안합니다(Kimi, DeepSeek, OpenAI, OpenRouter를 통한 Claude, Ollama/llama.cpp/LM Studio를 통한 로컬 모델) — 번호나 이름을 선택하고 API 키를 붙여넣으세요(설정 파일이 아니라 `~/.config/berimor/secrets.env`에 "소유자 전용" 권한으로 저장됨). 나중에 같은 작업은 `berimor setup` 또는 채팅에서 바로 `/models add` 명령으로 할 수 있습니다.

유용한 채팅 명령: `/help`, `/models`, `/skills`, `/config`, `/exit`.

결정론적 프로세스(엄격한 계약을 가진 선언적 YAML 플랜 — 주요 "실전" 모드): `berimor run <process.yaml>`. 프로세스 및 설정 예제는 [`fixtures/golden/processes/`](fixtures/golden/processes/)와 [`CONTRIBUTING.md`](CONTRIBUTING.md)에 있습니다.

한 명령으로 확장 설치:

```sh
berimor skill install code-review-ru                                    # 카탈로그에서
berimor skill install my-skill --from https://github.com/user/repo      # 임의의 git에서
berimor agent install researcher
berimor plugin install devpilgrin/berimor-plugin-hello                  # 서명된 플러그인
berimor plugin install-local ./my-plugin --allow-unsigned               # 로컬, 알고서 하는 설치
```

## 프로젝트 구조

| 레이어 | 디렉터리 | 내용 |
|---|---|---|
| 에이전트 코어 | `crates/` | Rust workspace — 컴포넌트당 하나의 크레이트: Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage |
| CodeAct 샌드박스 | `codeact-guest/` | wasm32-wasip1용 QuickJS 게스트 — 별도 crate, 빌드된 아티팩트로 커밋됨 |
| Bootstrap | `bootstrap/` | 설치/업데이트용 npm 패키지(TypeScript), 위의 "설치" 참조 |
| 아키텍처 | `docs/arch/` | 자족적 명세 — 원칙, 컴포넌트, 다이어그램(`docs/arch/views/`). `docs/arch/README.md` 참조 |
| 결정 | `docs/ADR/` | 아키텍처 결정 저널: 컨텍스트, 대안, 결과. `docs/ADR/README.md` 참조 |
| 개발 계획 | `docs/ROADMAP.md` | 페이즈별 작업 큐, 하위 작업으로의 분해, 복잡도, 실행 모델 클래스 |
| 감사 | `docs/audit-2026-07-31.md` | 독립 보안 감사 — 모든 지적 사항이 해결되었거나 의식적으로 문서화됨 |
| 테스트 데이터 | `fixtures/golden/` | 골든 세트: 프로세스, 계약, 악의적 입력 예제 |
| 리서치 | `docs/rnd/` | 보조 레이어: 기존 에이전트 프레임워크의 소스와 분석. `docs/rnd/README.md` 참조 |

`crates/`와 `bootstrap/`이 에이전트 자체이며, `docs/ROADMAP.md`의 큐에 따라 작성된 코드입니다. `docs/arch/`는 그 뒤에 있는 순수한 결정의 레이어입니다: 구체적인 프로젝트와 제품을 언급하지 않으며(`docs/arch/deployment.md`와 `docs/arch/stack.md`는 의식적인 예외), 어떤 스택에서도 구현할 수 있도록 아키텍처를 서술합니다. `docs/ADR/`는 기각된 대안을 포함해 각 결정이 납득된 이유를 기록합니다. `docs/rnd/`는 설계의 근거가 된 보조 소스 레이어로, 에이전트의 일부가 아닙니다.

## 라이선스

Apache License 2.0 — [`LICENSE`](LICENSE) 참조.

## 기여

작업 선택은 [`CONTRIBUTING.md`](CONTRIBUTING.md)와 [`docs/ROADMAP.md`](docs/ROADMAP.md)를 참조하세요.
