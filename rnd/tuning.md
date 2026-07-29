# Обучение и адаптация агентов

Методы дообучения и обучения с подкреплением для придания моделям обобщенных агентных способностей.

### AgentTuning: Enabling Generalized Agent Abilities for LLMs
- **ID:** 2310.12823v2
- **Authors:** Aohan Zeng, Mingdao Liu, Rui Lu, Bowen Wang, Xiao Liu, Yuxiao Dong, Jie Tang
- **Year:** 2023
- **Tags:** AgentTuning, generalized agent abilities
- **URL:** https://arxiv.org/abs/2310.12823v2
- **PDF:** https://arxiv.org/pdf/2310.12823v2

Open large language models (LLMs) with great performance in various tasks have significantly advanced the development of LLMs. However, they are far inferior to commercial models such as ChatGPT and GPT-4 when acting as agents to tackle complex tasks in the real world. These agent tasks employ LLMs as the central controller responsible for planning, memorization, and tool utilization, necessitating both fine-grained prompting methods and robust LLMs to achieve satisfactory performance. Though many prompting methods have been proposed to complete particular agent tasks, there is lack of research focusing on improving the agent capabilities of LLMs themselves without compromising their general abilities. In this work, we present AgentTuning, a simple and general method to enhance the agent abilities of LLMs while maintaining their general LLM capabilities. We construct AgentInstruct, a lightweight instruction-tuning dataset containing high-quality interaction trajectories. We employ a hybrid instruction-tuning strategy by combining AgentInstruct with open-source instructions from general domains. AgentTuning is used to instruction-tune the Llama 2 series, resulting in AgentLM. Our evaluations show that AgentTuning enables LLMs' agent capabilities without compromising general abilities. The AgentLM-70B is comparable to GPT-3.5-turbo on unseen agent tasks, demonstrating generalized agent capabilities. We open source the AgentInstruct and AgentLM-7B, 13B, and 70B models at https://github.com/THUDM/AgentTuning, serving open and powerful alternatives to commercial LLMs for agent tasks.
