<div align="center">

<img src="docs/assets/logo.png" alt="Berimor" width="640">

**モデルは考える。コードが決める。**

[Русский](README.md) · [English](README.en.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · [简体中文](README.zh-CN.md) · **[日本語](README.ja.md)** · [한국어](README.ko.md)

</div>

決定論的コアを持つ LLM 向けユニバーサルエージェント：タスクのルーティング、プロセスの分岐、コンテキストの選別、実行許可はコードが決定し、モデルは狭く検証可能なステップを実行するだけです。ローカルモデルにもクラウドモデルにも、弱いモデルにも強いモデルにも対応します。

[![GitHub release](https://img.shields.io/github/v/release/devpilgrin/berimor?logo=github&label=release)](https://github.com/devpilgrin/berimor/releases/latest)
[![npm](https://img.shields.io/npm/v/berimor?logo=npm&label=npm)](https://www.npmjs.com/package/berimor)
[![CI](https://img.shields.io/github/actions/workflow/status/devpilgrin/berimor/ci.yml?branch=main&label=CI)](https://github.com/devpilgrin/berimor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-843%20green-brightgreen)](#プロジェクトのインフラ)

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

## なぜ必要なのか

ほとんどの「AI エージェント」は同じ構造をしています：モデルにツール群を与え、何をすべきかをモデル自身に決めさせる。デモには便利ですが、実務には頼りません：モデルはステップを忘れ、事実をでっち上げ、方向を誤り、危険なコマンドは「y」を反射的に押した拍子にターミナルへ流れていきます。

Berimor は逆の前提に基づいて構築されています：**モデルにオーケストレーションは任せられない——任せられるのは実行だけ。** タスクは事前にステップへ分解されるか、決定論的ループによって駆動されます。モデルが出力するものはすべて、信頼に足る前に厳格な検証を通ります。害を及ぼし得るものはすべて、Enter キーでは取り消せないゲートを通ります。

| | 典型的なエージェント CLI | Berimor |
|---|---|---|
| 次に何をするか決めるのは | モデル（分別があることへの期待） | コード（プロセスグラフ、決定論的ループ） |
| タスク途中での失敗 | 「再起動して祈れ」 | イベントジャーナル：中断した場所から正確に再開 |
| 危険な操作 | 疲労が YOLO に変えてしまう確認プロンプト | Deny 静的ルール：禁止事項はそもそも問い合わせない |
| 弱い／ローカルモデル | 「もっと高いモデルを買え」 | メディエーション：エラー説明付きリトライ → 人間へエスカレーション |
| 拡張 | プラグインがすべての権限を得る | サブエージェント／プラグインは親の権限の部分集合を得る——コードによって保証 |
| 再現性 | なし | 完全：ジャーナル → リプレイ → 任意時点の状態 |

## 何が違うのか

**1. 決定は決定論的コードであり、プロンプト内のテキストではない。**
分岐、ループ、タイムアウト、join バリア付きの並列ブランチ、実行中プロセスのバージョンマイグレーション——これらはすべて Process Engine の仕事であり、モデルが指示を覚えていることへの期待ではありません。弱いモデルにコンテキスト選別やルーティングは任せられない——だからコードが担当します。

**2. セキュリティは構造であり、ユーザーの規律ではない。**
破壊的操作の Deny テーブルは確認によって覆せません。ファイルシステムの jail は作業ディレクトリの外に出られません。ネットワークゲートは閉じたレンジを通しません（NAT64/6to4/Teredo による偽装や、リダイレクトや URL の userinfo 経由のバイパスを含む）。シークレットはすべての漏洩ポイントでマスクされます——しかし許可ゲートは実際の値を見ます：マスキングはチェックを盲目にしません。

**3. 自由ループ——監督の下で。**
事前にステップへ分解できないタスクのための「推論 → 行動 → 観察」モード。ループ内の各行動は、プロセスのステップと同じ capability ゲートを通ります——推論の自由はルールからの自由を意味しません。オプション：自己批評と「提案 — 実行 — 検証」戦略。

**4. モデルが書いたコードは本物のサンドボックスで実行される。**
「12 のテーブルをマージして異常を見つけて」のようなタスクでは、モデルが JavaScript プログラムを書きます。それは本物のパーサーによる静的解析を通り（識別子ホワイトリスト——`eval`/`Function`/`Math.random` は実行前に拒否）、WebAssembly（Wasmtime）内の QuickJS で、フューエル、メモリ上限、ツール呼び出し回数の上限付きで実行されます。WASI は空の権限セット：ファイルもネットワークも潜在的にさえ存在しません。唯一のホスト関数も同じゲートを通ります。

**5. メモリはバッファではなく、工学システムとして。**
ワーキングメモリは予算超過時に圧縮されます。エピソード記憶——全文検索（FTS5）。セマンティック記憶——ファクトの重複排除、競合は黙って上書きされず、ストレージ障害は「ファクトがない」状態と区別がつかず、偽の重複を生みません。エンティティグラフ——ファクト間の関連、永続化。スキル——類似タスクを解くための再利用可能なレシピで、可読なファイルです。

**6. 権限の天井がある拡張エコシステム。**
- **スキル**（SKILL.md）——チャット用のエキスパートロール：トリガーはコードで（モデルではなく）、ツールの天井はディスパッチャのフィルタで保証。
- **サブエージェント**（agent.yaml）——独自の予算とジャーナルを持つ入れ子のエージェントループ；子の権限 = 親の権限との積集合で、拡大は不可。入れ子のスポーンは明示的な `allow_spawn: true` のみ、深さはコードで制限。
- **プラグイン**——ACL マニフェストと sigstore キーレス署名を持つ分離プロセス：SSH のように、信頼リストからのインストールと TOFU 確認。
- **MCP**——オープンプロトコル Model Context Protocol による外部ツールサーバー（公式 Rust SDK rmcp、ADR-0023）：設定の `[[mcp_servers]]` セクションで接続し、ビルトインツールとプラグインの後ろで共通ディスパッチャに登録され、プロセスのどのステップとも同じ capability ゲートを通ります。逆方向にも動作します：Berimor は自身のツールを MCP 経由で提供できます。

これらすべてを 1 コマンドでインストールできます——カタログから、または**任意の git リポジトリ**から：`berimor skill install code-review-ru --from https://github.com/...`。

## プロジェクトのインフラ

**コンポーネントごとに 1 クレートの Rust workspace**——Process Engine、Mediation、Executors、Memory、Capability、Model Pool、Actors、Tool Runtime、Context Engine、Eval、Storage。ゲスト WASM モジュール（`codeact-guest/`）は独立した crate として存在し、ビルド済みアーティファクトとしてコミットされています——通常のビルドは遅くなりません。

**チェックの規律。** すべてのリリースで：`cargo fmt` + `clippy -D warnings` + `cargo test --workspace`（843 テスト：ユニット、統合、実バイナリ経由の e2e、プロセスと悪意ある入力のゴールデンフィクスチャ）。重要コンポーネントは必須の独立レビューを通ります。完全な独立監査（`docs/audit-2026-07-31.md`）——**すべての指摘は解決済みか、意図的に文書化済み**。

**大人のサプライチェーン。** クロスプラットフォームリリース（Linux x64/arm64、macOS arm64、Windows x64）に cosign/sigstore キーレス署名——秘密鍵はどこにも存在しません。検証：`berimor verify <アーカイブ>`。npm 公開は provenance 付き、パイプラインに SBOM（CycloneDX）、セルフアップデート（`berimor self-update`）は Process Engine のプリミティブ上に実装——通常のプロセスと同じジャーナルと障害復旧で、アドホックなスクリプトではありません。

**アーキテクチャはコードより先に文書化。** `docs/arch/`——任意のスタックで実装可能な自己完結的な仕様；`docs/ADR/`——却下された代替案を含む決定ジャーナル；`docs/ROADMAP.md`——各タスクに実行モデルのクラスを付記したタスクキュー。

## インストール

### 方法 1：npm（最も簡単）

```sh
npm install -g berimor
berimor --version
```

インストーラーはプラットフォームを自動判別し、GitHub の最新リリースから署名済みバイナリをダウンロードし、展開前に SHA-256 を照合します。パッケージは provenance 付きで公開されます（ビルドが CI ワークフローに紐付け）。

### 方法 2：GitHub のビルド済みバイナリ

最新バージョンは[リリースページ](https://github.com/devpilgrin/berimor/releases/latest)にあります。以下は特定バージョンをダウンロードするコマンドです（より新しいバージョンが出ている場合は `v0.19.0` を置き換えてください）。

**Linux**（x64 または arm64）：

```sh
VERSION=v0.19.0
ARCH=x64   # または arm64
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-linux-${ARCH}.tar.gz"
tar -xzf "berimor-${VERSION}-linux-${ARCH}.tar.gz"
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**macOS**（Apple Silicon のみ——M1/M2/M3 以降；Intel 向けビルドはまだ公開されていません。Intel Mac は下の方法 3 を）：

```sh
VERSION=v0.19.0
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-darwin-arm64.tar.gz"
tar -xzf "berimor-${VERSION}-darwin-arm64.tar.gz"
xattr -d com.apple.quarantine berimor   # バイナリはまだ Apple 署名されていない——このままでは Gatekeeper が起動を拒否する
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**Windows**（x64）、PowerShell：

```powershell
$Version = "v0.19.0"
Invoke-WebRequest -Uri "https://github.com/devpilgrin/berimor/releases/download/$Version/berimor-$Version-win32-x64.zip" -OutFile berimor.zip
Expand-Archive -Path berimor.zip -DestinationPath .\
.\berimor.exe --version
```

バイナリはまだ署名されていません——Windows SmartScreen が「Windows によって PC が保護されました」という警告を表示することがあります：「詳細情報」→「実行」。任意のフォルダから `berimor` を呼べるようにするには、`berimor.exe` をすでに `PATH` にあるディレクトリに移すか、現在のフォルダを自分で `PATH` に追加してください。

各アーカイブには `<アーカイブ>.sigstore.json` が付随します——リリースをビルドした CI ワークフローのアイデンティティに紐付いた cosign/sigstore キーレス署名（ADR-0026）。検証：`berimor verify <アーカイブ>`——コマンド自体はダウンロードしたバイナリに含まれています（初回呼び出し時にネットワーク経由で最新の sigstore 信頼ルートをインストール）。これは Apple/Microsoft から独立した署名です——上記の Gatekeeper/SmartScreen の警告は消えません。それらは別の、まだ完了していないステップに関するものです。

### 方法 3：ソースからビルド（任意の OS）

必要なのは [Rust](https://rustup.rs/)（安定版）だけです：

```sh
git clone https://github.com/devpilgrin/berimor.git
cd berimor
cargo build --release -p berimor-cli
./target/release/berimor --version
```

Windows では最後のコマンドは `.\target\release\berimor.exe --version` です。

## クイックスタート

```sh
berimor          # = berimor chat：エージェントとの対話型チャット
```

初回起動時、ウィザードがプリセットからモデルを接続するよう提案します（Kimi、DeepSeek、OpenAI、OpenRouter 経由の Claude、Ollama/llama.cpp/LM Studio 経由のローカルモデル）——番号または名前を選び、API キーを貼り付けてください（`~/.config/berimor/secrets.env` に「所有者のみ」権限で保存され、設定ファイルには入りません）。API キーの代わりにサブスクリプションでログインすることもできます——`berimor login`（PKCE 付き OAuth：Claude Pro/Max、ChatGPT Plus/Pro；トークンは同じ `secrets.env` に保存され、更新は透過的）。後から同じことをするには `berimor setup`、またはチャット内で直接 `/models add` コマンドを使います。

便利なチャットコマンド：`/help`、`/models`、`/skills`、`/config`、`/exit`。

決定論的プロセス（厳格なコントラクトを持つ宣言的 YAML プラン——主要な「実戦」モード）：`berimor run <process.yaml>`。プロセスと設定の例は [`fixtures/golden/processes/`](fixtures/golden/processes/) と [`CONTRIBUTING.md`](CONTRIBUTING.md) にあります。

プロセスの上の自動化：`berimor schedule add` + `berimor daemon`——スケジュールに従ったプロセスの実行；`berimor serve`——run/schedule/sessions の上に立つ HTTP サービス（トークン付き、匿名アクセスなし）；`berimor sessions`——ホストのライブセッションのレジストリ；`berimor trace <インスタンス>`——任意のランのジャーナルを人間が読める形でトレース。

1 コマンドで拡張をインストール：

```sh
berimor skill install code-review-ru                                    # カタログから
berimor skill install my-skill --from https://github.com/user/repo      # 任意の git から
berimor agent install researcher
berimor plugin install devpilgrin/berimor-plugin-hello                  # 署名済みプラグイン
berimor plugin install-local ./my-plugin --allow-unsigned               # ローカル、承知の上で
```

## プロジェクトの構成

| レイヤー | ディレクトリ | 内容 |
|---|---|---|
| エージェントコア | `crates/` | Rust workspace——コンポーネントごとに 1 クレート：Process Engine、Mediation、Executors、Memory、Capability、Model Pool、Actors、Tool Runtime、Context Engine、Eval、Storage |
| CodeAct サンドボックス | `codeact-guest/` | wasm32-wasip1 向け QuickJS ゲスト——独立した crate、ビルド済みアーティファクトとしてコミット |
| Bootstrap | `bootstrap/` | インストーラー／アップデーターの npm パッケージ（TypeScript）、上の「インストール」参照 |
| アーキテクチャ | `docs/arch/` | 自己完結的な仕様——原則、コンポーネント、図（`docs/arch/views/`）。`docs/arch/README.md` 参照 |
| 決定 | `docs/ADR/` | アーキテクチャ決定ジャーナル：コンテキスト、代替案、帰結。`docs/ADR/README.md` 参照 |
| 開発計画 | `docs/ROADMAP.md` | フェーズ別タスクキュー、サブタスクへの分解、複雑度、実行モデルのクラス |
| 監査 | `docs/audit-2026-07-31.md` | 独立セキュリティ監査——すべての指摘は解決済みか、意図的に文書化済み |
| テストデータ | `fixtures/golden/` | ゴールデンセット：プロセス、コントラクト、悪意ある入力の例 |
| リサーチ | `docs/rnd/` | 補助レイヤー：既存エージェントフレームワークのソースと分析。`docs/rnd/README.md` 参照 |

`crates/` と `bootstrap/` がエージェント本体で、`docs/ROADMAP.md` のキューに従って書かれたコードです。`docs/arch/` はその背後にある純粋な決定のレイヤー：具体的なプロジェクトや製品には言及せず（`docs/arch/deployment.md` と `docs/arch/stack.md` は意図的な例外）、任意のスタックで実装できる形でアーキテクチャを記述しています。`docs/ADR/` は却下された代替案を含め、各決定の理由を記録しています。`docs/rnd/` は設計の拠り所となった補助的なソースのレイヤーであり、エージェントの一部ではありません。

## ライセンス

Apache License 2.0——[`LICENSE`](LICENSE) を参照。

## コントリビューション

タスクの選択は [`CONTRIBUTING.md`](CONTRIBUTING.md) と [`docs/ROADMAP.md`](docs/ROADMAP.md) を参照してください。
