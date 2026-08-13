# Рекомендованные MCP-серверы

Berimor подключает внешние инструменты по открытому протоколу MCP
(официальный Rust SDK rmcp, ADR-0023). Сервер объявляется секцией
`[[mcp_servers]]` в конфиге (`~/.config/berimor/config.toml` — глобально,
`.berimor/config.toml` — на проект), запускается как stdio-процесс и
встаёт в общий диспетчер **после** встроенных инструментов и плагинов.
Каждый вызов MCP-инструмента проходит тот же capability-гейт, что и любой
шаг процесса: мутирующие действия — через подтверждение, deny-статика —
безусловна.

Формат секции (stdio; `url`-транспорта нет — сервер всегда локальный
процесс, запущенный berimor):

```toml
[[mcp_servers]]
name = "имя"        # для сообщений об ошибках и разрешения конфликтов
command = "npx"     # исполняемый файл
args = ["-y", "пакет", "..."]
```

Секреты (токены API серверов) — через переменные окружения: процесс
наследует окружение berimor, значения из `~/.config/berimor/secrets.env`
доступны как `$ИМЯ` (имена можно зарегистрировать в `secret_envs` для
маскировки в выводе). Ключи в `args` не вписывайте — конфиг не хранилище
секретов.

## Эталонные (modelcontextprotocol/servers)

Имена npm-пакетов проверены в реестре 2026-08-13.

```toml
# Файловая система за пределами рабочей области (свои каталоги-аргументы)
[[mcp_servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]

# Память-граф между сессиями (дополнение к встроенной памяти)
[[mcp_servers]]
name = "memory"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-memory"]

# Пошаговое рассуждение как инструмент
[[mcp_servers]]
name = "sequential-thinking"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-sequential-thinking"]

# Загрузка страниц (fetch с преобразованием в markdown)
[[mcp_servers]]
name = "fetch"
command = "uvx"
args = ["mcp-server-fetch"]

# Расширенный git (история, ветки, blame)
[[mcp_servers]]
name = "git"
command = "uvx"
args = ["mcp-server-git", "--repository", "."]

# Часовые пояса и время
[[mcp_servers]]
name = "time"
command = "uvx"
args = ["mcp-server-time"]
```

Сервер `everything` из того же репозитория — тестовый эталон протокола,
в рабочую конфигурацию не добавляйте.

## Проверенные сообществом

```toml
# Браузерная автоматизация (Playwright): навигация, клики, скриншоты
[[mcp_servers]]
name = "playwright"
command = "npx"
args = ["-y", "@playwright/mcp@latest"]

# GitHub API: репозитории, PR, issues (нужен GITHUB_PERSONAL_ACCESS_TOKEN
# в окружении — официальный сервер github/github-mcp-server, бинарь)
[[mcp_servers]]
name = "github"
command = "github-mcp-server"
args = ["stdio"]

# PostgreSQL: запросы read-only и интроспекция схемы
[[mcp_servers]]
name = "postgres"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://user@host/db"]

# SQLite: локальные базы
[[mcp_servers]]
name = "sqlite"
command = "uvx"
args = ["mcp-server-sqlite", "--db-path", "/path/to/base.db"]

# Поиск Brave (нужен BRAVE_API_KEY в окружении)
[[mcp_servers]]
name = "brave-search"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-brave-search"]
```

## Правила эксплуатации

- **Порядок в диспетчере**: встроенные → плагины → MCP → заглушки.
  Конфликт имён инструментов между серверами виден по имени сервера в
  сообщении об ошибке.
- **Гейт не отключается**: MCP-инструмент без известной политики
  получает mutates-декларацию сервера; сомнительное — спрашивает.
  Разрешения «для сессии/проекта» работают и на MCP-инструменты.
- **Сетевая активность сервера** — вне сетевого гейта berimor (он
  контролирует вызовы инструментов, не трафик дочерних процессов);
  доверие к серверу = доверие к его поставщику. Предпочитайте серверы
  из списка выше или аудируемые исходники.
- **Отладка**: подключённые источники инструментов видны в стартовой
  строке чата («инструменты: … + конфигурация оператора»); сбойный
  сервер не роняет сессию — его инструменты отвечают ошибкой диспетча.
- Свой сервер — навык `mcp-builder` из каталога berimor-skills.
