# Фреймворки и инфраструктура

Платформы, протоколы и компиляторы, которые систематизируют вызовы LLM, интеграцию инструментов и сборку конвейеров.

## Model Context Protocol (MCP)

- **Source:** https://modelcontextprotocol.io/introduction
- **Spec:** https://modelcontextprotocol.io/specification

MCP — открытый стандарт для подключения AI-приложений к внешним системам (файлам, базам данных, API, workflows). Антропоморфная аналогия: USB-C порт для AI-приложений.

### Архитектура

MCP разделяет участников и слои:

- **MCP Host** — AI-приложение (Claude Desktop, VS Code, Cursor), которое координирует один или несколько MCP-клиентов.
- **MCP Client** — компонент, поддерживающий соединение с MCP-сервером и получающий от него контекст.
- **MCP Server** — программа, предоставляющая контекст: локальная (stdio) или удалённая (Streamable HTTP).

### Слои протокола

- **Data layer** — JSON-RPC 2.0 протокол: discovery (`server/discover`), возможности (capabilities), примитивы (tools/resources/prompts), уведомления.
- **Transport layer** — stdio для локальных процессов или Streamable HTTP для удалённых серверов с bearer/API-key аутентификацией.

### Примитивы сервера

- **Tools** — исполняемые функции (file ops, API calls, DB queries).
- **Resources** — источники данных для контекста (содержимое файлов, записи БД, ответы API).
- **Prompts** — переиспользуемые шаблоны (system prompts, few-shot examples).

### Примитивы клиента

- **Elicitation** — сервер может запросить у пользователя дополнительную информацию или подтверждение действия.
- **Sampling** (deprecated в 2026-07-28) — сервер запрашивает completion у клиентского LLM.
- **Logging** (deprecated в 2026-07-28) — рекомендуется stderr или OpenTelemetry.

### Уведомления

Подписки `subscriptions/listen` позволяют серверу пушить изменения (например, список tools обновился) в клиент через JSON-RPC notifications.

## Anthropic: Building Effective Agents

- **Source:** https://www.anthropic.com/engineering/building-effective-agents
- **Published:** Dec 19, 2024

Практические наблюдения Anthropic с десятками команд, строящих LLM-агентов. Главный вывод: успешные production-системы используют простые компонуемые паттерны, а не сложные фреймворки.

### Workflows vs Agents

Anthropic различает два типа агентических систем:

- **Workflows** — заранее заданные, программно управляемые цепочки (predictable, deterministic).
- **Agents** — системы, где LLM сам принимает решения, использует инструменты и работает в цикле на основе feedback из среды (flexible, autonomous).

### Основные паттерны

1. **Augmented LLM** — базовый блок: LLM + retrieval + tools + memory. MCP — один из способов интеграции инструментов.
2. **Prompt chaining** — разбиение задачи на фиксированную последовательность шагов, с programmatic gates между ними. Лучше, когда задача легко декомпозируется, а латентность приемлема ради точности.
3. **Routing** — классификация входа и направление в специализированный follow-up. Хорошо, когда есть чёткие категории и классификатор работает точно.
4. **Parallelization** — разделение на независимые подзадачи, выполняемые параллельно, с агрегацией результатов (sectioning и voting).
5. **Orchestrator-workers** — центральный LLM динамически декомпозирует задачу, делегирует worker-LLM и синтезирует результаты. Подходит, когда подзадачи нельзя предопределить.
6. **Evaluator-optimizer** — один LLM генерирует ответ, другой оценивает и даёт feedback в цикле. Эффективно, когда есть чёткие критерии оценки.
7. **Agents** — LLM в цикле с инструментами и ground truth из среды. Нужны clear tool docs, checkpoints, guardrails, sandbox-тестирование.

### Принципы

- Начинать с простых решений и добавлять сложность только когда это улучшает результат.
- Использовать LLM API напрямую, фреймворки — с пониманием того, что под капотом.
- Тщательно проектировать tool interfaces (ACI — agent-computer interface) не меньше, чем HCI.

## Популярные фреймворки

### Production evaluation-driven framework (Nubank)

- **Title:** Building Customer Support AI Agents at 100M-User Scale: An Evaluation-Driven Framework
- **arXiv:** 2606.08867v2 (June 2026)
- **Authors:** Aman Gupta, Kevin Rossell, Edesio Alcobaça, Jose Chrystian Lima Pacheco, Carolina Baptista de Lima, Shao Tang, Luiz Paulo Rabachini, Luis Moneda, Herbert Fei, Daniel Silva, Rohan Ramanath
- **Category:** cs.CL
- **DOI:** 10.1145/3770855.3818332
- **Link:** https://arxiv.org/abs/2606.08867v2

Опыт построения production-агентов поддержки клиентов Nubank (100M+ пользователей). Основная теза: качество evaluation pipeline напрямую определяет скорость итераций и конечный бизнес-результат.

#### Архитектурные компоненты

- **Модульная context engineering** — инструкции, рутины, макросы, описания инструментов и рабочая память версионируются независимо, что позволяет проводить целевые эксперименты.
- **ReACT-ядро** — LLM чередует reasoning (chain-of-thought) с использованием внешних инструментов для самостоятельного выполнения задач.
- **Безопасность и аудит** — слой идентификации/аутентификации, транзакционные точки входа для бизнес-логики, журналы аудита с маскированием чувствительных полей.
- **Двухцикловая модель разработки:**
  - **Быстрый цикл** — offline-оценка → быстрая обратная связь → итерация промптов.
  - **Медленный цикл** — online-метрики производства → архитектурные изменения.

#### Ключевые результаты

| Метрика | Прирост |
|---------|---------|
| tNPS (transactional Net Promoter Score) | +37 п.п. |
| Self-Service Rate (SSR) | +29 п.п. |
| Разрыв с лучшими людьми | в пределах нескольких п.п. |

#### GEPA (Generative Evaluation Prompt Alignment)

Автоматическая оптимизация промптов через генеративную оценку. В одном из примеров восстановила точность с 51,11% до 77,78%, тогда как ручная оптимизация дала только 50,00%. GEPA также повысила согласованность между разными judge-моделями.

#### Выводы для практики

- Ориентироваться на бизнес-метрики (tNPS, SSR), а не только на точность/F1.
- Offline-метрики должны коррелировать с online-результатами.
- Контекстные компоненты должны быть независимо версионируемы.
- Автоматическая оптимизация промптов может превосходить ручную.

### LangChain, LangGraph, LlamaIndex

- **LangChain** — библиотека для сборки цепочек LLM-вызовов, инструментов, памяти и RAG. Часто критикуется за избыточную абстракцию, но остаётся точкой входа.
- **LangGraph** — слой над LangChain для построения stateful multi-agent приложений с циклами, условными переходами и persistence.
- **LlamaIndex** — фреймворк для RAG и индексации данных: data connectors, indices, query engines, agents over structured/unstructured data.

### DSPy: Compiling Declarative Language Model Calls into Self-Improving Pipelines
- **ID:** 2310.03714v1
- **Authors:** Omar Khattab, Arnav Singhvi, Paridhi Maheshwari, Zhiyuan Zhang, Keshav Santhanam, Sri Vardhamanan, Saiful Haq, Ashutosh Sharma, Thomas T. Joshi, Hanna Moazam, Heather Miller, Matei Zaharia, Christopher Potts
- **Year:** 2023
- **Tags:** DSPy, declarative LM programming
- **URL:** https://arxiv.org/abs/2310.03714v1
- **PDF:** https://arxiv.org/pdf/2310.03714v1

The ML community is rapidly exploring techniques for prompting language models (LMs) and for stacking them into pipelines that solve complex tasks. Unfortunately, existing LM pipelines are typically implemented using hard-coded "prompt templates", i.e. lengthy strings discovered via trial and error. Toward a more systematic approach for developing and optimizing LM pipelines, we introduce DSPy, a programming model that abstracts LM pipelines as text transformation graphs, i.e. imperative computational graphs where LMs are invoked through declarative modules. DSPy modules are parameterized, meaning they can learn (by creating and collecting demonstrations) how to apply compositions of prompting, finetuning, augmentation, and reasoning techniques. We design a compiler that will optimize any DSPy pipeline to maximize a given metric. We conduct two case studies, showing that succinct DSPy programs can express and optimize sophisticated LM pipelines that reason about math word problems, tackle multi-hop retrieval, answer complex questions, and control agent loops. Within minutes of compiling, a few lines of DSPy allow GPT-3.5 and llama2-13b-chat to self-bootstrap pipelines that outperform standard few-shot prompting (generally by over 25% and 65%, respectively) and pipelines with expert-created demonstrations (by up to 5-46% and 16-40%, respectively). On top of that, DSPy programs compiled to open and relatively small LMs like 770M-parameter T5 and llama2-13b-chat are competitive with approaches that rely on expert-written prompt chains for proprietary GPT-3.5. DSPy is available at https://github.com/stanfordnlp/dspy

### OpenAgents: An Open Platform for Language Agents in the Wild
- **ID:** 2310.10634v1
- **Authors:** Tianbao Xie, Fan Zhou, Zhoujun Cheng, Peng Shi, Luoxuan Weng, Yitao Liu, Toh Jing Hua, Junning Zhao, Qian Liu, Che Liu, Leo Z. Liu, Yiheng Xu, Hongjin Su, Dongchan Shin, Caiming Xiong, Tao Yu
- **Year:** 2023
- **Tags:** OpenAgents, open platform
- **URL:** https://arxiv.org/abs/2310.10634v1
- **PDF:** https://arxiv.org/pdf/2310.10634v1

Language agents show potential in being capable of utilizing natural language for varied and intricate tasks in diverse environments, particularly when built upon large language models (LLMs). Current language agent frameworks aim to facilitate the construction of proof-of-concept language agents while neglecting the non-expert user access to agents and paying little attention to application-level designs. We present OpenAgents, an open platform for using and hosting language agents in the wild of everyday life. OpenAgents includes three agents: (1) Data Agent for data analysis with Python/SQL and data tools; (2) Plugins Agent with 200+ daily API tools; (3) Web Agent for autonomous web browsing. OpenAgents enables general users to interact with agent functionalities through a web user interface optimized for swift responses and common failures while offering developers and researchers a seamless deployment experience on local setups, providing a foundation for crafting innovative language agents and facilitating real-world evaluations. We elucidate the challenges and opportunities, aspiring to set a foundation for future research and development of real-world language agents.
