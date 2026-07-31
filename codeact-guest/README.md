# codeact-guest

Гостевой WASM-модуль CodeAct (ROADMAP E8) — исполняет JS-программу
через QuickJS (`rquickjs`) внутри песочницы Wasmtime
(`berimor-executors::codeact::wasm_host`). См. doc-комментарий
`src/main.rs` для протокола ввода/вывода.

**Не член основного workspace** (`../Cargo.toml`) — намеренно: сборка
`rquickjs`/`rquickjs-sys` компилирует C-исходники QuickJS под
`wasm32-wasip1`, автоматически скачивая `wasi-sdk` с GitHub Releases в
своём `build.rs`. Если бы этот crate был частью workspace, каждый
`cargo build --workspace` (в том числе не имеющий отношения к CodeAct)
тянул бы это за собой — замедляя основной CI на всех трёх ОС ради
задачи, которую трогают редко.

Вместо этого результат сборки коммитится как
`../crates/berimor-executors/assets/codeact-guest.wasm` и используется
`WasmHost` как обычный статический артефакт (`include_bytes!`).

## Пересборка

Нужно, когда меняется `src/main.rs` или версия `rquickjs`. Проще всего
— через Docker (не трогает системный Rust):

```sh
docker run --rm -v "$(pwd)":/work -w /work rust:1-bookworm bash -c '
  rustup target add wasm32-wasip1 &&
  cargo build --release --target wasm32-wasip1
'
cp target/wasm32-wasip1/release/codeact-guest.wasm \
   ../crates/berimor-executors/assets/codeact-guest.wasm
```

Либо напрямую, если локально уже есть `rustup` с установленным target
`wasm32-wasip1`:

```sh
cargo build --release --target wasm32-wasip1
cp target/wasm32-wasip1/release/codeact-guest.wasm \
   ../crates/berimor-executors/assets/codeact-guest.wasm
```

После пересборки — прогнать тесты `berimor-executors` (`cargo test -p
berimor-executors codeact`), они реально исполняют этот `.wasm` через
`WasmHost`.
