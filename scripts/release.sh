#!/usr/bin/env bash
# Короткая команда релиза: scripts/release.sh 0.8.1
#
# Делает локальную часть (версии → коммит → тег → пуш), дальше CI:
# push тега v* запускает release.yml — матрица сборки, атомарный GitHub
# Release с артефактами/SBOM/подписями и npm publish bootstrap-пакета
# (последний требует секрета NPM_TOKEN — без него упадёт только этот
# шаг, сам релиз создастся, см. ROADMAP §14, D3).
#
# Дисциплина: версия без префикса v; дерево обязано быть чистым; тег и
# коммит создаются только если вся проверка прошла.
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="${1:?использование: scripts/release.sh X.Y.Z (без префикса v)}"
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: версия '$VERSION' не похожа на X.Y.Z" >&2
    exit 1
fi

if [ -n "$(git status --porcelain)" ]; then
    echo "error: рабочее дерево не чистое — сначала закоммитьте или спрячьте изменения" >&2
    exit 1
fi

if git rev-parse "v$VERSION" >/dev/null 2>&1; then
    echo "error: тег v$VERSION уже существует" >&2
    exit 1
fi

echo "==> версия workspace -> $VERSION"
sed -i "0,/^version = \".*\"$/s//version = \"$VERSION\"/" Cargo.toml
grep -m1 "^version = " Cargo.toml

echo "==> версия npm-пакета -> $VERSION"
(cd bootstrap && npm version "$VERSION" --no-git-tag-version >/dev/null)

echo "==> cargo check (обновление Cargo.lock)"
cargo check --workspace --quiet

git add Cargo.toml Cargo.lock bootstrap/package.json bootstrap/package-lock.json
git commit -q -m "release: v$VERSION"
git tag -a "v$VERSION" -m "v$VERSION"
git push origin main
git push origin "v$VERSION"

echo "==> запушено; release.yml соберёт и опубликует v$VERSION"

# Фолбэк (2026-08-06): с ~19:00 пуши тегов перестали порождать push-
# события workflow (GitHub классифицирует их как «dynamic» после
# ruleset-bypass — v0.20.0/v0.21.0 остались без запуска; причина на
# стороне GitHub, не пайплайна). Даём триггеру 30 с, после — dispatch.
sleep 30
RUN_ID=$(gh run list --workflow release.yml --limit 3 --json databaseId,headBranch --jq ".[] | select(.headBranch==\"v$VERSION\") | .databaseId" | head -1)
if [ -z "$RUN_ID" ]; then
    echo "==> push-триггер не сработал — workflow_dispatch под v$VERSION"
    gh workflow run release.yml -f tag="v$VERSION"
    sleep 15
    RUN_ID=$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
fi
echo "    наблюдение: gh run watch $RUN_ID"
