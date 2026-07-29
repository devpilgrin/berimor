# Воплощенные, веб- и GUI-агенты

Агенты, действующие в Minecraft, операционной системе, браузере и через графический интерфейс.

### Voyager: An Open-Ended Embodied Agent with Large Language Models
- **ID:** 2305.16291v2
- **Authors:** Guanzhi Wang, Yuqi Xie, Yunfan Jiang, Ajay Mandlekar, Chaowei Xiao, Yuke Zhu, Linxi Fan, Anima Anandkumar
- **Year:** 2023
- **Tags:** Voyager, Minecraft agent, lifelong learning
- **URL:** https://arxiv.org/abs/2305.16291v2
- **PDF:** https://arxiv.org/pdf/2305.16291v2

We introduce Voyager, the first LLM-powered embodied lifelong learning agent in Minecraft that continuously explores the world, acquires diverse skills, and makes novel discoveries without human intervention. Voyager consists of three key components: 1) an automatic curriculum that maximizes exploration, 2) an ever-growing skill library of executable code for storing and retrieving complex behaviors, and 3) a new iterative prompting mechanism that incorporates environment feedback, execution errors, and self-verification for program improvement. Voyager interacts with GPT-4 via blackbox queries, which bypasses the need for model parameter fine-tuning. The skills developed by Voyager are temporally extended, interpretable, and compositional, which compounds the agent's abilities rapidly and alleviates catastrophic forgetting. Empirically, Voyager shows strong in-context lifelong learning capability and exhibits exceptional proficiency in playing Minecraft. It obtains 3.3x more unique items, travels 2.3x longer distances, and unlocks key tech tree milestones up to 15.3x faster than prior SOTA. Voyager is able to utilize the learned skill library in a new Minecraft world to solve novel tasks from scratch, while other techniques struggle to generalize. We open-source our full codebase and prompts at https://voyager.minedojo.org/.

### OS-Copilot: Towards Generalist Computer Agents with Self-Improvement
- **ID:** 2402.07456v2
- **Authors:** Zhiyong Wu, Chengcheng Han, Zichen Ding, Zhenmin Weng, Zhoumianze Liu, Shunyu Yao, Tao Yu, Lingpeng Kong
- **Year:** 2024
- **Tags:** OS-Copilot, generalist computer agent
- **URL:** https://arxiv.org/abs/2402.07456v2
- **PDF:** https://arxiv.org/pdf/2402.07456v2

Autonomous interaction with the computer has been a longstanding challenge with great potential, and the recent proliferation of large language models (LLMs) has markedly accelerated progress in building digital agents. However, most of these agents are designed to interact with a narrow domain, such as a specific software or website. This narrow focus constrains their applicability for general computer tasks. To this end, we introduce OS-Copilot, a framework to build generalist agents capable of interfacing with comprehensive elements in an operating system (OS), including the web, code terminals, files, multimedia, and various third-party applications. We use OS-Copilot to create FRIDAY, a self-improving embodied agent for automating general computer tasks. On GAIA, a general AI assistants benchmark, FRIDAY outperforms previous methods by 35%, showcasing strong generalization to unseen applications via accumulated skills from previous tasks. We also present numerical and quantitative evidence that FRIDAY learns to control and self-improve on Excel and Powerpoint with minimal supervision. Our OS-Copilot framework and empirical findings provide infrastructure and insights for future research toward more capable and general-purpose computer agents.

### AutoWebGLM: A Large Language Model-based Web Navigating Agent
- **ID:** 2404.03648v2
- **Authors:** Hanyu Lai, Xiao Liu, Iat Long Iong, Shuntian Yao, Yuxuan Chen, Pengbo Shen, Hao Yu, Hanchen Zhang, Xiaohan Zhang, Yuxiao Dong, Jie Tang
- **Year:** 2024
- **Tags:** AutoWebGLM, web navigation
- **URL:** https://arxiv.org/abs/2404.03648v2
- **PDF:** https://arxiv.org/pdf/2404.03648v2

Large language models (LLMs) have fueled many intelligent web agents, but most existing ones perform far from satisfying in real-world web navigation tasks due to three factors: (1) the complexity of HTML text data (2) versatility of actions on webpages, and (3) task difficulty due to the open-domain nature of the web. In light of these challenges, we develop the open AutoWebGLM based on ChatGLM3-6B. AutoWebGLM can serve as a powerful automated web navigation agent that outperform GPT-4. Inspired by human browsing patterns, we first design an HTML simplification algorithm to represent webpages with vital information preserved succinctly. We then employ a hybrid human-AI method to build web browsing data for curriculum training. Finally, we bootstrap the model by reinforcement learning and rejection sampling to further facilitate webpage comprehension, browser operations, and efficient task decomposition by itself. For comprehensive evaluation, we establish a bilingual benchmark -- AutoWebBench -- for real-world web navigation tasks. We evaluate AutoWebGLM across diverse web navigation benchmarks, demonstrating its potential to tackle challenging tasks in real environments. Related code, model, and data are released at \url{https://github.com/THUDM/AutoWebGLM}.

### UI-TARS: Pioneering Automated GUI Interaction with Native Agents
- **ID:** 2501.12326v1
- **Authors:** Yujia Qin, Yining Ye, Junjie Fang, Haoming Wang, Shihao Liang, Shizuo Tian, Junda Zhang, Jiahao Li, Yunxin Li, Shijue Huang, Wanjun Zhong, Kuanye Li, Jiale Yang, Yu Miao, Woyu Lin, Longxiang Liu, Xu Jiang, Qianli Ma, Jingyu Li, Xiaojun Xiao, Kai Cai, Chuang Li, Yaowei Zheng, Chaolin Jin, Chen Li, Xiao Zhou, Minchao Wang, Haoli Chen, Zhaojian Li, Haihua Yang, Haifeng Liu, Feng Lin, Tao Peng, Xin Liu, Guang Shi
- **Year:** 2025
- **Tags:** UI-TARS, GUI interaction
- **URL:** https://arxiv.org/abs/2501.12326v1
- **PDF:** https://arxiv.org/pdf/2501.12326v1

This paper introduces UI-TARS, a native GUI agent model that solely perceives the screenshots as input and performs human-like interactions (e.g., keyboard and mouse operations). Unlike prevailing agent frameworks that depend on heavily wrapped commercial models (e.g., GPT-4o) with expert-crafted prompts and workflows, UI-TARS is an end-to-end model that outperforms these sophisticated frameworks. Experiments demonstrate its superior performance: UI-TARS achieves SOTA performance in 10+ GUI agent benchmarks evaluating perception, grounding, and GUI task execution. Notably, in the OSWorld benchmark, UI-TARS achieves scores of 24.6 with 50 steps and 22.7 with 15 steps, outperforming Claude (22.0 and 14.9 respectively). In AndroidWorld, UI-TARS achieves 46.6, surpassing GPT-4o (34.5). UI-TARS incorporates several key innovations: (1) Enhanced Perception: leveraging a large-scale dataset of GUI screenshots for context-aware understanding of UI elements and precise captioning; (2) Unified Action Modeling, which standardizes actions into a unified space across platforms and achieves precise grounding and interaction through large-scale action traces; (3) System-2 Reasoning, which incorporates deliberate reasoning into multi-step decision making, involving multiple reasoning patterns such as task decomposition, reflection thinking, milestone recognition, etc. (4) Iterative Training with Reflective Online Traces, which addresses the data bottleneck by automatically collecting, filtering, and reflectively refining new interaction traces on hundreds of virtual machines. Through iterative training and reflection tuning, UI-TARS continuously learns from its mistakes and adapts to unforeseen situations with minimal human intervention. We also analyze the evolution path of GUI agents to guide the further development of this domain.

### Building Browser Agents: Architecture, Security, and Practical Solutions
- **ID:** 2511.19477v1
- **Authors:** Aram Vardanyan
- **Year:** 2025
- **Tags:** browser agents, architecture, security
- **URL:** https://arxiv.org/abs/2511.19477v1
- **PDF:** https://arxiv.org/pdf/2511.19477v1

Browser agents enable autonomous web interaction but face critical reliability and security challenges in production. This paper presents findings from building and operating a production browser agent. The analysis examines where current approaches fail and what prevents safe autonomous operation. The fundamental insight: model capability does not limit agent performance; architectural decisions determine success or failure. Security analysis of real-world incidents reveals prompt injection attacks make general-purpose autonomous operation fundamentally unsafe. The paper argues against developing general browsing intelligence in favor of specialized tools with programmatic constraints, where safety boundaries are enforced through code instead of large language model (LLM) reasoning. Through hybrid context management combining accessibility tree snapshots with selective vision, comprehensive browser tooling matching human interaction capabilities, and intelligent prompt engineering, the agent achieved approximately 85% success rate on the WebGames benchmark across 53 diverse challenges (compared to approximately 50% reported for prior browser agents and 95.7% human baseline).

### UI-Evol: Automatic Knowledge Evolving for Computer Use Agents
- **ID:** 2505.21964v2
- **Authors:** Ziyun Zhang, Xinyi Liu, Xiaoyi Zhang, Jun Wang, Gang Chen, Yan Lu
- **Year:** 2025
- **Tags:** UI-Evol, knowledge evolution
- **URL:** https://arxiv.org/abs/2505.21964v2
- **PDF:** https://arxiv.org/pdf/2505.21964v2

External knowledge has played a crucial role in the recent development of computer use agents. We identify a critical knowledge-execution gap: retrieved knowledge often fails to translate into effective real-world task execution. Our analysis shows even 90% correct knowledge yields only 41% execution success rate. To bridge this gap, we propose UI-Evol, a plug-and-play module for autonomous GUI knowledge evolution. UI-Evol consists of two stages: a Retrace Stage that extracts faithful objective action sequences from actual agent-environment interactions, and a Critique Stage that refines existing knowledge by comparing these sequences against external references. We conduct comprehensive experiments on the OSWorld benchmark with the state-of-the-art Agent S2. Our results demonstrate that UI-Evol not only significantly boosts task performance but also addresses a previously overlooked issue of high behavioral standard deviation in computer use agents, leading to superior performance on computer use tasks and substantially improved agent reliability.
