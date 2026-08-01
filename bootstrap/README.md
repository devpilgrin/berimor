# bootstrap

npm-пакет установщика Berimor — см. `docs/arch/deployment.md` §2–3, ADR-0025.

## Первая установка: скачивание, верификация, распаковка (D3)

При первом запуске (`dist/index.js`) bootstrap определяет платформу
(`platform.ts`), проверяет локальный кэш (`~/.cache/berimor/bin` на Linux,
`~/Library/Caches/berimor/bin` на macOS, `%LOCALAPPDATA%\berimor\bin` на
Windows — `cache-dir.ts`), при отсутствии — скачивает платформенный архив с
GitHub Releases (`download.ts`), сверяет его SHA-256 с записью в
`checksums.json` (`checksum.ts` + `checksums-manifest.ts`) и только затем
атомарно распаковывает (`extract.ts`: временная директория рядом с целевой +
`rename` одной операцией). Несовпадение хэша или отсутствие записи в
`checksums.json` — явный отказ, ничего не распаковывается.

`checksums.json` пишется не разработчиком, а CI (`release.yml`, job
`publish`) на этапе публикации — сравнение идёт с независимым от GitHub
Release каналом (npm), см. `docs/ROADMAP.md` §14 (D3) для полного разбора,
почему верификация через уже существующий `verify.ts` (`berimor verify`,
делегирование нативному бинарнику) циркулярна именно на первой установке и
не используется здесь; `verify.ts` остаётся заделом для D4 (само-обновление).

```sh
npm run build
npm test    # node:test, встроенный в Node ≥20 — новых dev-зависимостей нет
```

## Локальная разработка

`postinstall` запускает `dist/postinstall.js`, поэтому при первом клоне репозитория нужна сборка до `npm install` от зависимостей:

```sh
npm install --ignore-scripts
npm run build
npm install   # теперь postinstall находит dist/ и проходит
```

В опубликованном пакете `dist/` уже собран и входит в тарбол — этот порядок нужен только локально, до первого `npm run build`.
