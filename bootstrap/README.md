# bootstrap

npm-пакет установщика Berimor — см. `arch/deployment.md` §2–3, ADR-0025.

## Локальная разработка

`postinstall` запускает `dist/postinstall.js`, поэтому при первом клоне репозитория нужна сборка до `npm install` от зависимостей:

```sh
npm install --ignore-scripts
npm run build
npm install   # теперь postinstall находит dist/ и проходит
```

В опубликованном пакете `dist/` уже собран и входит в тарбол — этот порядок нужен только локально, до первого `npm run build`.
