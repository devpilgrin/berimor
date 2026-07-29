# Агентные фреймворки и их архитектура

> Собрано из GitHub README, официальной документации и блогов. Интервал 5 секунд между запросами к одному источнику соблюдался.

## 1. Общие фреймворки и платформы

### LangChain

- **Repo:** https://github.com/langchain-ai/langchain
- **Stars:** 142k+
- **Описание:** The agent engineering platform.
- **Архитектура:**
  - `langchain-core` — базовые абстракции (LLMs, prompts, output parsers, tools, retrievers, vector stores).
  - `langchain` — цепочки, агенты, retrieval.
  - `langchain-community` — интеграции с 100+ сервисов.
  - `langgraph` — stateful multi-agent workflows.
- **Модель/провайдеры:** 100+ через integrations (OpenAI, Anthropic, Google, Azure, AWS, локальные модели).
- **Инструменты:** Function calling, MCP, custom tools, retrieval, vector stores.
- **Память:** ConversationBufferMemory, vector store memory, checkpointing через LangGraph.

### LangGraph

- **Repo:** https://github.com/langchain-ai/langgraph
- **Stars:** 38k+
- **Описание:** Low-level orchestration framework for building stateful agents.
- **Архитектура:**
  - **Graph-based state machine:** узлы — вызовы LLM или функции; ребра — условные переходы.
  - **Persistence:** checkpointing, resumability, human-in-the-loop.
  - **Channels:** message passing между узлами.
  - **Sub-graphs:** композиция сложных workflows.
- **Паттерны:** ReAct, reflection, planning, multi-agent collaboration.
- **Использование:** когда нужен контроль над состоянием и flow, а не просто вызов LLM.

### LlamaIndex

- **Repo:** https://github.com/run-llama/llama_index
- **Stars:** 51k+
- **Описание:** Leading document agent and OCR platform.
- **Архитектура:**
  - **Data connectors** — загрузка из файлов, API, БД.
  - **Indices** — VectorStoreIndex, SummaryIndex, TreeIndex, KnowledgeGraphIndex.
  - **Query engines** — RAG pipelines.
  - **Agents** — over structured/unstructured data.
  - **LlamaParse** — document agent platform (Parse, Extract, Index, Split, Agents).
- **Инструменты:** function calling, MCP, document parsing, OCR.

### DSPy

- **Repo:** https://github.com/stanfordnlp/dspy
- **Stars:** 36k+
- **Описание:** Framework for programming — not prompting — language models.
- **Архитектура:**
  - **Signatures** — декларативные спецификации вход/выход.
  - **Modules** — композиционные Python-классы (ChainOfThought, ReAct, ProgramOfThought).
  - **Optimizers** — автоматическая оптимизация prompts и weights (BootstrapFewShot, MIPRO, GEPA).
  - **Compilation** — pipeline компилируется в эффективную программу.
- **Принцип:** пишите код, а не промпты; DSPy учится оптимизировать.

### OpenAgents

- **Repo:** https://github.com/xlang-ai/OpenAgents
- **Stars:** 4.8k+
- **Описание:** Open platform for language agents in the wild.
- **Архитектура:**
  - **Data Agent** — анализ данных с Python/SQL.
  - **Plugins Agent** — 200+ API tools.
  - **Web Agent** — autonomous web browsing.
- **Особенность:** web UI для non-expert пользователей, локальный деплой.

## 2. Мультиагентные фреймворки

### AutoGen / AG2

- **Repo:** https://github.com/microsoft/autogen (maintenance mode) / https://github.com/ag2ai/ag2
- **Stars:** 60k+ / 4.8k+
- **Описание:** Programming framework for agentic AI / Open-Source AgentOS.
- **Архитектура AG2:**
  - **Agent** — базовый блок: model provider + tools + reply.
  - **Tools** — Python functions with `@tool` decorator.
  - **Network** — `Hub` + typed `channels` для мультиагентной коммуникации.
  - **Agent harness** — persistent knowledge, context assembly, history compaction.
  - **Human in the loop** — пауза для подтверждения.
- **AutoGen legacy:** `AssistantAgent`, `UserProxyAgent`, `GroupChat`, `AgentTool`.
- **Статус:** AutoGen в maintenance mode; AG2 — community-driven successor.

### CrewAI

- **Repo:** https://github.com/crewAIInc/crewAI
- **Stars:** 56k+
- **Описание:** Fast and flexible multi-agent automation framework.
- **Архитектура:**
  - **Crews** — ролевые агенты (role, goal, backstory) с автономным сотрудничеством.
  - **Flows** — event-driven workflows с состоянием, branching, routing.
  - **YAML конфигурация** — agents.yaml и tasks.yaml.
  - **AMP (Agent Management Platform)** — control plane с tracing, observability, security.
- **Модель/провайдеры:** OpenAI по умолчанию, поддержка Ollama, LiteLLM, других.

### MetaGPT

- **Repo:** https://github.com/geekan/MetaGPT
- **Stars:** 69k+
- **Описание:** The Multi-Agent Framework: First AI Software Company.
- **Архитектура:**
  - **Software Company as Multi-Agent System** — product managers, architects, project managers, engineers.
  - **SOP-driven** — Standard Operating Procedures как промпты.
  - **Code = SOP(Team)** — философия материализации SOP в код.
  - **Roles** — каждый агент имеет роль, goal, constraints, tools.
- **Процесс:** one line requirement → user stories → competitive analysis → requirements → data structures → APIs → documents.

### Camel-AI

- **Repo:** https://github.com/camel-ai/camel
- **Stars:** 17.5k+
- **Описание:** First and best multi-agent framework. Finding the Scaling Law of Agents.
- **Архитектура:**
  - **ChatAgent** — базовый агент с memory, tools, model.
  - **RolePlaying** — два агента взаимодействуют для генерации данных.
  - **Workforce** — иерархическая команда агентов.
  - **Design principles:** Evolvability, Scalability, Statefulness, Code-as-Prompt.
- **Модули:** Models, Agents, Toolkits, Memory, RAG, Graph RAG, Data Generation.

### AgentScope

- **Repo:** https://github.com/modelscope/agentscope
- **Stars:** 28k+
- **Описание:** Production-ready, easy-to-use agent framework.
- **Архитектура:**
  - **Event System** — unified event bus для frontend и human-in-the-loop.
  - **Permission System** — fine-grained контроль над tools и resources.
  - **Multi-tenancy & Multi-session Service** — изоляция tenant/session.
  - **Workspace / Sandbox** — local, Docker, E2B, OpenSandbox, Daytona.
  - **Middleware System** — composable hooks для reasoning-acting loop.
- **Модель:** DashScope, OpenAI, Anthropic, локальные.

### BeeAI

- **Repo:** https://github.com/i-am-bee/beeai-framework
- **Stars:** 3.3k+
- **Описание:** Build production-ready AI agents in Python and TypeScript.
- **Архитектура:**
  - **RequirementAgent** — правила и условия для предсказуемого поведения.
  - **Backend** — unified interface к любому LLM provider.
  - **Middleware** — trajectory, retry, logging.
  - **Tools** — Wikipedia, OpenMeteo, Think, Handoff.
- **Особенность:** dual-language (Python + TypeScript), production focus.

### agency-swarm

- **Repo:** https://github.com/VRSEN/agency-swarm
- **Stars:** 4.5k+
- **Описание:** Reliable Multi-Agent Orchestration Framework.
- **Архитектура:**
  - **Agency** — коллекция агентов с communication flows.
  - **Agent roles** — CEO, Virtual Assistant, Developer с tailored instructions.
  - **Type-safe tools** — Pydantic models + OpenAI Agents SDK.
  - **State persistence** — load/save threads callbacks.
- **Особенность:** построен поверх OpenAI Agents SDK, ориентирован на реальные организационные структуры.

## 3. Production и enterprise фреймворки

### Google ADK (Agent Development Kit)

- **Repo:** не найден публичный GitHub; документация https://google.github.io/adk-docs/
- **Описание:** Open-source framework от Google для full-stack разработки агентов и мультиагентных систем.
- **Архитектура:**
  - **Multi-Agent by Design** — иерархия специализированных агентов.
  - **Rich Model Ecosystem** — Gemini, Vertex AI Model Garden, LiteLLM (Anthropic, Meta, Mistral).
  - **Rich Tool Ecosystem** — pre-built tools, MCP, LangChain/LlamaIndex, agents as tools.
  - **Flexible Orchestration** — `Sequential`, `Parallel`, `Loop` workflow agents; `LlmAgent` transfer для dynamic routing.
  - **Built-in streaming** — bidirectional audio/video.
  - **Built-in Evaluation** — trajectory и response quality assessment.
- **Деплой:** containerized, CLI, Web UI, API Server, Python API.

### OpenAI Agents SDK

- **Repo:** https://github.com/openai/openai-agents-python (403 при доступе; docs https://openai.github.io/openai-agents-python/)
- **Описание:** Production-ready upgrade of Swarm. Lightweight package with few abstractions.
- **Архитектура:**
  - **Primitives:** Agents (LLM + instructions + tools), Handoffs (delegation), Guardrails (validation).
  - **Agent loop** — built-in: tool invocation, result feedback, continuation.
  - **Tracing** — visualization, debugging, monitoring, evaluation, fine-tuning.
  - **Sandbox agents** — isolated workspaces with manifest-defined files.
  - **MCP integration** — same as function tools.
  - **Sessions** — persistent memory layer.
  - **Voice** — Realtime agents with interruption detection.
- **Выбор:** Responses API — low-level; Agents SDK — managed workflows.

### Semantic Kernel

- **Repo:** https://github.com/microsoft/semantic-kernel
- **Stars:** 28k+
- **Описание:** Integrate cutting-edge LLM technology quickly and easily into your apps.
- **Архитектура:**
  - **Kernel** — центральный объект: plugins, planners, memory.
  - **Plugins** — native functions + semantic functions (prompts).
  - **Planners** — автоматическое планирование последовательности действий.
  - **Memory** — embeddings + vector stores.
  - **Connectors** — OpenAI, Azure OpenAI, HuggingFace, custom.
- **Языки:** C#, Python, Java.

### PydanticAI

- **Repo:** https://github.com/pydantic/pydantic-ai
- **Stars:** 18.8k+
- **Описание:** GenAI Agent Framework, the Pydantic way.
- **Архитектура:**
  - **Agent[DepsType, OutputType]** — generic agent с type-safe dependencies и structured output.
  - **Dependency injection** — `RunContext[DepsType]` для передачи данных/логики.
  - **Capabilities** — Thinking, WebSearch, CodeExecution, etc.
  - **Structured output** — Pydantic models для валидации.
  - **Model-agnostic** — OpenAI, Anthropic, Gemini, Grok, Mistral, local.
- **Принцип:** FastAPI feeling для GenAI: type hints, validation, ergonomics.

## 4. Специализированные фреймворки

### smolagents

- **Repo:** https://github.com/huggingface/smolagents
- **Stars:** 28.5k+
- **Описание:** Barebones library for agents that think in code.
- **Архитектура:**
  - **CodeAgent** — ReAct loop, где actions — Python code snippets.
  - **ToolCallingAgent** — классический function calling.
  - **Managed agents** — мультиагентные сценарии.
  - **Model-agnostic** — InferenceClientModel, LiteLLMModel, OpenAIModel, local.
- **Память:** `agent.memory` — хранит историю как chat messages.
- **Бенчмарк:** open models (DeepSeek-R1) конкурируют с closed-source.

### TaskWeaver

- **Repo:** https://github.com/microsoft/TaskWeaver
- **Stars:** 6.1k+
- **Описание:** Code-first agent framework for data analytics tasks.
- **Архитектура:**
  - **Code-first** — user requests → code snippets → plugin execution.
  - **Plugins** — functions для data analytics.
  - **Stateful** — сохраняет состояние между шагами.
  - **Planner** — разбивает задачу на подзадачи.
  - **Code Generator** — генерирует код для каждого шага.
  - **Code Executor** — выполняет код в изолированном окружении.

### OpenHands

- **Repo:** https://github.com/All-Hands-AI/OpenHands
- **Stars:** 82k+
- **Описание:** AI-Driven Development / Agent Canvas.
- **Архитектура:**
  - **Agent Server** — REST API для запуска агентов на single machine.
  - **Agent Canvas** — UI для подключения к multiple Agent Servers.
  - **Backends** — local, remote, cloud (Docker sandbox).
  - **ACP-compatible** — работает с Claude Code, Codex, Gemini.
  - **Automation Server** — scheduled/event-driven agents.
- **Особенность:** self-hosted developer control center.

### browser-use

- **Repo:** https://github.com/browser-use/browser-use
- **Stars:** 107k+
- **Описание:** Make websites accessible for AI agents.
- **Архитектура:**
  - **Browser Agent** — LLM + browser control.
  - **DOM parsing** — извлечение интерактивных элементов.
  - **Actions** — click, type, scroll, navigate.
  - **Vision** — скриншоты + accessibility tree.
  - **Cloud** — hosted version с custom tools, MCP, integrations.
- **Применение:** form filling, data extraction, QA automation.

### Letta (formerly MemGPT)

- **Repo:** https://github.com/letta-ai/letta
- **Stars:** 24k+
- **Описание:** Platform for stateful agents with advanced memory.
- **Архитектура:**
  - **Stateful agents** — memory that learns and self-improves.
  - **Memory blocks** — core memory, archival memory, recall memory.
  - **Letta Agent** — CLI, desktop app, Slack channels.
  - **Letta Agent SDK** — TypeScript SDK для приложений.
  - **App Server** — self-hosted API server.
  - **Constellation** — agent cloud.
- **Наследие:** successor of MemGPT.

## 5. Устаревшие и исторические фреймворки

### AutoGPT

- **Repo:** https://github.com/Significant-Gravitas/AutoGPT
- **Stars:** 185k+
- **Описание:** Vision of accessible AI for everyone.
- **Архитектура:** autonomous loop с long-term memory, web browsing, file I/O, GPT-4.
- **Статус:** активно развивается, но концепция "autonomous agent" вытеснена managed frameworks.

### BabyAGI

- **Repo:** https://github.com/yoheinakajima/babyagi
- **Stars:** 22k+
- **Описание:** Task-driven autonomous agent.
- **Архитектура:** task creation → task prioritization → task execution → result storage.
- **Статус:** proof-of-concept, повлиял на последующие фреймворки.

## Сравнительная таблица

| Фреймворк | Основная парадигма | Мультиагент | Память | Провайдеры | Production |
|-----------|-------------------|-------------|--------|------------|------------|
| LangChain | Chains/agents | Через LangGraph | Базовая | 100+ | Средний |
| LangGraph | State machine | Да | Checkpointing | 100+ | Высокий |
| LlamaIndex | RAG/agents | Ограничено | Vector store | 100+ | Средний |
| DSPy | Programming | Нет | Оптимизация | Любые | Высокий |
| AutoGen/AG2 | Conversation | Да | Compaction | OpenAI, etc. | Высокий |
| CrewAI | Roles/flows | Да | Базовая | OpenAI, Ollama | Высокий |
| MetaGPT | SOP/software co | Да | Базовая | OpenAI, etc. | Средний |
| Camel-AI | RolePlaying | Да | Stateful | OpenAI, etc. | Исслед. |
| AgentScope | Event-driven | Да | Middleware | DashScope, etc. | Высокий |
| BeeAI | Requirements | Да | Middleware | Любые | Высокий |
| agency-swarm | OpenAI Agents | Да | Callbacks | OpenAI | Высокий |
| Google ADK | Workflow agents | Да | Sessions | Gemini, LiteLLM | Высокий |
| OpenAI Agents | Agent loop | Handoffs | Sessions | OpenAI | Высокий |
| Semantic Kernel | Plugins/planners | Да | Memory | OpenAI, Azure | Высокий |
| PydanticAI | Type-safe | Через DI | RunContext | Любые | Высокий |
| smolagents | Code actions | Managed | Memory | Любые | Средний |
| TaskWeaver | Code-first | Нет | Stateful | OpenAI, etc. | Высокий |
| OpenHands | Agent server | ACP | Sandbox | Любые | Высокий |
| browser-use | Web automation | Нет | Базовая | OpenAI, etc. | Высокий |
| Letta | Stateful memory | Нет | Advanced | Любые | Высокий |

## Архитектурные тенденции

1. **От промптов к программированию.** DSPy и PydanticAI заменяют промпты декларативным кодом и type-safe интерфейсами.
2. **От function calling к code actions.** smolagents и TaskWeaver используют Python-код как действия агента.
3. **Мультиагентность как стандарт.** Почти все современные фреймворки поддерживают orchestration, handoffs, subagents.
4. **Production readiness.** Трассировка, observability, evaluation, sandboxing, human-in-the-loop — обязательные компоненты.
5. **MCP как универсальный интерфейс.** LangChain, OpenAI Agents SDK, Google ADK, CrewAI поддерживают MCP для инструментов.
6. **Stateful execution.** LangGraph, Letta, OpenAI Agents SDK делают упор на persistence и resumability.
7. **Event-driven и middleware.** AgentScope, BeeAI, CrewAI Flows используют событийную модель для контроля.

## Ссылки

- GitHub repos: см. `framework_github.json`
- README files: `framework_readmes/`
- OpenAlex search: `framework_openalex.json`
- Google ADK: https://google.github.io/adk-docs/
- OpenAI Agents SDK: https://openai.github.io/openai-agents-python/
- Microsoft Agent Framework: https://github.com/microsoft/agent-framework
