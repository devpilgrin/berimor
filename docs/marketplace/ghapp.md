# berimor как GitHub App: регистрация и публикация в Marketplace (agent-apps)

Инструкция для владельца (часть шагов — только в браузере, юридически ваши).

## 1. Регистрация GitHub App (manifest flow)

Откройте в браузере (залогиненным под devpilgrin):

```
https://github.com/settings/apps/new
```

и подставьте манифест (кнопка «Create GitHub App from manifest» либо
через curl с формой `manifest=<json>`):

```json
{
  "name": "berimor-agent",
  "url": "https://github.com/devpilgrin/berimor",
  "hook_attributes": { "url": "https://ВАШ_ПУБЛИЧНЫЙ_ХОСТ/webhooks/github", "active": true },
  "redirect_url": "https://github.com/devpilgrin/berimor",
  "public": true,
  "default_permissions": {
    "issues": "write",
    "pull_requests": "write",
    "contents": "read"
  },
  "default_events": ["issue_comment"],
  "description": "Детерминированный агентный исполнительный контур: метка /berimor в комментарии запускает процесс с контрактами и гейтами, итог — комментарием."
}
```

После создания GitHub покажет: **App ID**, сгенерирует **webhook secret**
и предложит скачать **private key (PEM)** — сохраните его в
`~/.config/berimor/ghapp-private-key.pem` (права 600).

## 2. Конфигурация сервиса

В `~/.config/berimor/config.toml`:

```toml
[github_app]
app_id = 000000            # из шага 1
private_key_path = "~/.config/berimor/ghapp-private-key.pem"
process = "ci/agent-handler.yaml"   # ваш процесс-обработчик
trigger = "/berimor"                # метка в комментарии (опционально)
```

В `~/.config/berimor/secrets.env`:

```bash
BERIMOR_GHAPP_SECRET=<webhook secret из шага 1>
```

Запуск: `berimor serve` (порт из `[serve]`), публичный доступ — ваш
хостинг/туннель; URL вебхука из манифеста должен указывать на
`https://ВАШ_ХОСТ/webhooks/github`. Вход процесса: `{repo, issue, comment}`.

## 3. Установка приложения на репозиторий

`https://github.com/apps/berimor-agent` → Install → выбрать репозиторий.
Комментарий `/berimor <команда>` в issue/PR запускает процесс.

## 4. Листинг в Marketplace (категория: AI agents / agent-apps)

1. Settings → Developer settings → GitHub Apps → berimor-agent →
   **Marketplace** (в левом меню) → «List in Marketplace».
2. Заполнить: категория **AI agents** (agent-apps), описание (текст ниже),
   логотип (квадрат ≥ 200px), homepage `https://github.com/devpilgrin/berimor`,
   privacy policy URL, support email.
3. Ценовой план: **Free** (платные требуют верифицированную организацию).
4. Принять Marketplace Developer Agreement и отправить на проверку.

### Описание для листинга (EN)

> berimor is a deterministic agent runtime: the model thinks, the code
> decides. Drop `/berimor` in any issue or PR comment and the app runs a
> declarative YAML process — contracts validate every model output,
> capability gates control every action, and a full audit journal is kept
> on the service side. Results come back as a comment. Free and open
> source (Apache-2.0).

### Описание для листинга (RU, если потребуется)

> berimor — детерминированный агентный контур: модель думает, код решает.
> Метка `/berimor` в комментарии к issue или PR запускает декларативный
> YAML-процесс: контракты валидируют вывод модели, гейты контролируют
> действия, полный аудит-журнал остаётся на сервисе. Итог — комментарием.
> Бесплатно, открытый код (Apache-2.0).

## Что остаётся за владельцем (не автоматизируется)

- Публичный хостинг/туннель для вебхуков (URL в манифесте).
- Логотип приложения (квадратное изображение).
- Принятие Marketplace Developer Agreement и сабмит листинга.
- Privacy policy: страница (можно файлом в репозитории или вики).
