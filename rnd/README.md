# Berimor: передовые знания по агентным системам

Собранная база знаний по архитектурам, фреймворкам, протоколам, памяти, инструментам, мульти-агентным системам, воплощенным агентам, бенчмаркам и обучению LLM-агентов.

## Структура

### Архитектуры конкретных агентов
- `hermes.md` — архитектура Hermes Agent (Nous Research): CLI, gateway, профили, навыки, MCP, cron, kanban, делегирование, безопасность
- `claude_code.md` — архитектура Claude Code (Anthropic): core loop, контекст, расширения, permission modes, checkpoints, subagents, MCP
- `hermes_vs_claude_code.md` — сравнение архитектур, паттернов и практических выводов
- `architecture_analysis.md` — анализ базы, 4 варианта архитектуры, оценочная матрица, выбор лучшего (гибрид: процессный остов + CodeAct + агентный люк)

### Фреймворки и инфраструктура
- `agent_frameworks.md` — обзор 20+ агентных фреймворков: архитектура, мультиагентность, память, production readiness
- `frameworks.md` — фреймворки, протоколы и инфраструктура (MCP, DSPy, OpenAgents, LangChain/LangGraph/LlamaIndex, Anthropic patterns, production evaluation-driven framework)

### Фундаментальные темы
- `reasoning.md` — стратегии рассуждения и планирования (ReAct, Reflexion, ToT, RAP, SwiftSage, DeepSeek-R1)
- `multi_agent.md` — мульти-агентные системы (AutoGen, MetaGPT, CAMEL, AgentSims, ProAgent)
- `memory.md` — память и контекст (MemGPT, UniMem)
- `tools.md` — инструменты и действия (Toolformer, Gorilla, HuggingGPT, RestGPT, ReWOO, LLM+P, LLMCompiler)
- `embodied.md` — воплощенные, веб- и GUI-агенты (Voyager, OS-Copilot, AutoWebGLM, UI-TARS)
- `safety_and_compliance.md` — безопасность, compliance, нейросимволические агенты, compliance-by-construction, EU AI Act
- `guardrails.md` — механизмы защиты: guardrails vs capability-модель, реализации в Hermes/Claude Code/Jarvis, SSRF, scrubber, approvals
- `benchmarks.md` — бенчмарки и оценка (AgentBench, SWE-bench)
- `tuning.md` — обучение и адаптация (AgentTuning)
- `generative_agents.md` — генеративные агенты и симуляции (Generative Agents)
- `surveys.md` — обзорные статьи и курируемые списки
- `references.md` — библиография
- `*.json` — сырые результаты поиска (arxiv, openalex, core index, framework search/github/openalex)
- `framework_readmes/` — README файлов 18 агентных фреймворков

## Источники

- arXiv API (export.arxiv.org)
- OpenAlex API (api.openalex.org)
- Model Context Protocol (modelcontextprotocol.io)
- Anthropic Engineering Blog — "Building effective agents" (anthropic.com/research/building-effective-agents)
- Anthropic Docs — Claude Code overview, how Claude Code works, extend Claude Code (docs.anthropic.com)
- Hermes Agent docs (hermes-agent.nousresearch.com) и встроенный скилл `hermes-agent`

Rate limit соблюдался: минимум 5 секунд между запросами к одному источнику. Semantic Scholar API вернул 403 даже с User-Agent; материалы оттуда не собраны.

## Статистика

- Ядерных статей: 34
- Обзорных статей и курируемых списков: 7
- Агентных фреймворков: 25 (20+ детально описаны, 18 README сохранены)
- Дополнительных источников поиска: 263 уникальных записи в raw-индексе
- Документов по агентным архитектурам: 5 (Hermes, Claude Code, сравнение, safety/compliance, фреймворки)
- Всего уникальных источников в индексе: 41+ (ядро + обзоры + compliance/production)
