# Инструменты и действия

Архитектуры для использования API, внешних моделей, кода, планировщиков и мультимодальных инструментов.

### Toolformer: Language Models Can Teach Themselves to Use Tools
- **ID:** 2302.04761v1
- **Authors:** Timo Schick, Jane Dwivedi-Yu, Roberto Dessì, Roberta Raileanu, Maria Lomeli, Luke Zettlemoyer, Nicola Cancedda, Thomas Scialom
- **Year:** 2023
- **Tags:** Toolformer, self-supervised tool use
- **URL:** https://arxiv.org/abs/2302.04761v1
- **PDF:** https://arxiv.org/pdf/2302.04761v1

Language models (LMs) exhibit remarkable abilities to solve new tasks from just a few examples or textual instructions, especially at scale. They also, paradoxically, struggle with basic functionality, such as arithmetic or factual lookup, where much simpler and smaller models excel. In this paper, we show that LMs can teach themselves to use external tools via simple APIs and achieve the best of both worlds. We introduce Toolformer, a model trained to decide which APIs to call, when to call them, what arguments to pass, and how to best incorporate the results into future token prediction. This is done in a self-supervised way, requiring nothing more than a handful of demonstrations for each API. We incorporate a range of tools, including a calculator, a Q\&A system, two different search engines, a translation system, and a calendar. Toolformer achieves substantially improved zero-shot performance across a variety of downstream tasks, often competitive with much larger models, without sacrificing its core language modeling abilities.

### Gorilla: Large Language Model Connected with Massive APIs
- **ID:** 2305.15334v1
- **Authors:** Shishir G. Patil, Tianjun Zhang, Xin Wang, Joseph E. Gonzalez
- **Year:** 2023
- **Tags:** Gorilla, API retrieval
- **URL:** https://arxiv.org/abs/2305.15334v1
- **PDF:** https://arxiv.org/pdf/2305.15334v1

Large Language Models (LLMs) have seen an impressive wave of advances recently, with models now excelling in a variety of tasks, such as mathematical reasoning and program synthesis. However, their potential to effectively use tools via API calls remains unfulfilled. This is a challenging task even for today's state-of-the-art LLMs such as GPT-4, largely due to their inability to generate accurate input arguments and their tendency to hallucinate the wrong usage of an API call. We release Gorilla, a finetuned LLaMA-based model that surpasses the performance of GPT-4 on writing API calls. When combined with a document retriever, Gorilla demonstrates a strong capability to adapt to test-time document changes, enabling flexible user updates or version changes. It also substantially mitigates the issue of hallucination, commonly encountered when prompting LLMs directly. To evaluate the model's ability, we introduce APIBench, a comprehensive dataset consisting of HuggingFace, TorchHub, and TensorHub APIs. The successful integration of the retrieval system with Gorilla demonstrates the potential for LLMs to use tools more accurately, keep up with frequently updated documentation, and consequently increase the reliability and applicability of their outputs. Gorilla's code, model, data, and demo are available at https://gorilla.cs.berkeley.edu

### HuggingGPT: Solving AI Tasks with ChatGPT and its Friends in Hugging Face
- **ID:** 2303.17580v4
- **Authors:** Yongliang Shen, Kaitao Song, Xu Tan, Dongsheng Li, Weiming Lu, Yueting Zhuang
- **Year:** 2023
- **Tags:** HuggingGPT, model orchestration
- **URL:** https://arxiv.org/abs/2303.17580v4
- **PDF:** https://arxiv.org/pdf/2303.17580v4

Solving complicated AI tasks with different domains and modalities is a key step toward artificial general intelligence. While there are numerous AI models available for various domains and modalities, they cannot handle complicated AI tasks autonomously. Considering large language models (LLMs) have exhibited exceptional abilities in language understanding, generation, interaction, and reasoning, we advocate that LLMs could act as a controller to manage existing AI models to solve complicated AI tasks, with language serving as a generic interface to empower this. Based on this philosophy, we present HuggingGPT, an LLM-powered agent that leverages LLMs (e.g., ChatGPT) to connect various AI models in machine learning communities (e.g., Hugging Face) to solve AI tasks. Specifically, we use ChatGPT to conduct task planning when receiving a user request, select models according to their function descriptions available in Hugging Face, execute each subtask with the selected AI model, and summarize the response according to the execution results. By leveraging the strong language capability of ChatGPT and abundant AI models in Hugging Face, HuggingGPT can tackle a wide range of sophisticated AI tasks spanning different modalities and domains and achieve impressive results in language, vision, speech, and other challenging tasks, which paves a new way towards the realization of artificial general intelligence.

### RestGPT: Connecting Large Language Models with Real-World RESTful APIs
- **ID:** 2306.06624v2
- **Authors:** Yifan Song, Weimin Xiong, Dawei Zhu, Wenhao Wu, Han Qian, Mingbo Song, Hailiang Huang, Cheng Li, Ke Wang, Rong Yao, Ye Tian, Sujian Li
- **Year:** 2023
- **Tags:** RestGPT, RESTful API agents
- **URL:** https://arxiv.org/abs/2306.06624v2
- **PDF:** https://arxiv.org/pdf/2306.06624v2

Tool-augmented large language models (LLMs) have achieved remarkable progress in tackling a broad range of tasks. However, existing methods are mainly restricted to specifically designed tools and fail to fulfill complex instructions, having great limitations when confronted with real-world scenarios. In this paper, we explore a more realistic scenario by connecting LLMs with RESTful APIs, which adhere to the widely adopted REST software architectural style for web service development. To address the practical challenges of tackling complex instructions, we propose RestGPT, which exploits the power of LLMs and conducts a coarse-to-fine online planning mechanism to enhance the abilities of task decomposition and API selection. RestGPT also contains an API executor tailored for calling RESTful APIs, which can meticulously formulate parameters and parse API responses. To fully evaluate the performance of RestGPT, we propose RestBench, a high-quality benchmark which consists of two real-world scenarios and human-annotated instructions with gold solution paths. Experiments show that RestGPT is able to achieve impressive results in complex tasks and has strong robustness, which paves a new way towards AGI. RestGPT and RestBench is publicly available at https://restgpt.github.io/.

### MM-REACT: Prompting ChatGPT for Multimodal Reasoning and Action
- **ID:** 2303.11381v1
- **Authors:** Zhengyuan Yang, Linjie Li, Jianfeng Wang, Kevin Lin, Ehsan Azarnasab, Faisal Ahmed, Zicheng Liu, Ce Liu, Michael Zeng, Lijuan Wang
- **Year:** 2023
- **Tags:** MM-REACT, multimodal reasoning
- **URL:** https://arxiv.org/abs/2303.11381v1
- **PDF:** https://arxiv.org/pdf/2303.11381v1

We propose MM-REACT, a system paradigm that integrates ChatGPT with a pool of vision experts to achieve multimodal reasoning and action. In this paper, we define and explore a comprehensive list of advanced vision tasks that are intriguing to solve, but may exceed the capabilities of existing vision and vision-language models. To achieve such advanced visual intelligence, MM-REACT introduces a textual prompt design that can represent text descriptions, textualized spatial coordinates, and aligned file names for dense visual signals such as images and videos. MM-REACT's prompt design allows language models to accept, associate, and process multimodal information, thereby facilitating the synergetic combination of ChatGPT and various vision experts. Zero-shot experiments demonstrate MM-REACT's effectiveness in addressing the specified capabilities of interests and its wide application in different scenarios that require advanced visual understanding. Furthermore, we discuss and compare MM-REACT's system paradigm with an alternative approach that extends language models for multimodal scenarios through joint finetuning. Code, demo, video, and visualization are available at https://multimodal-react.github.io/

### ReWOO: Decoupling Reasoning from Observations for Efficient Augmented Language Models
- **ID:** 2305.18323v1
- **Authors:** Binfeng Xu, Zhiyuan Peng, Bowen Lei, Subhabrata Mukherjee, Yuchen Liu, Dongkuan Xu
- **Year:** 2023
- **Tags:** ReWOO, decoupled reasoning
- **URL:** https://arxiv.org/abs/2305.18323v1
- **PDF:** https://arxiv.org/pdf/2305.18323v1

Augmented Language Models (ALMs) blend the reasoning capabilities of Large Language Models (LLMs) with tools that allow for knowledge retrieval and action execution. Existing ALM systems trigger LLM thought processes while pulling observations from these tools in an interleaved fashion. Specifically, an LLM reasons to call an external tool, gets halted to fetch the tool's response, and then decides the next action based on all preceding response tokens. Such a paradigm, though straightforward and easy to implement, often leads to huge computation complexity from redundant prompts and repeated execution. This study addresses such challenges for the first time, proposing a modular paradigm ReWOO (Reasoning WithOut Observation) that detaches the reasoning process from external observations, thus significantly reducing token consumption. Comprehensive evaluations across six public NLP benchmarks and a curated dataset reveal consistent performance enhancements with our proposed methodology. Notably, ReWOO achieves 5x token efficiency and 4% accuracy improvement on HotpotQA, a multi-step reasoning benchmark. Furthermore, ReWOO demonstrates robustness under tool-failure scenarios. Beyond prompt efficiency, decoupling parametric modules from non-parametric tool calls enables instruction fine-tuning to offload LLMs into smaller language models, thus substantially reducing model parameters. Our illustrative work offloads reasoning ability from 175B GPT3.5 into 7B LLaMA, demonstrating the significant potential for truly efficient and scalable ALM systems.

### LLM+P: Empowering Large Language Models with Optimal Planning Proficiency
- **ID:** 2304.11477v3
- **Authors:** Bo Liu, Yuqian Jiang, Xiaohan Zhang, Qiang Liu, Shiqi Zhang, Joydeep Biswas, Peter Stone
- **Year:** 2023
- **Tags:** LLM+P, optimal planning
- **URL:** https://arxiv.org/abs/2304.11477v3
- **PDF:** https://arxiv.org/pdf/2304.11477v3

Large language models (LLMs) have demonstrated remarkable zero-shot generalization abilities: state-of-the-art chatbots can provide plausible answers to many common questions that arise in daily life. However, so far, LLMs cannot reliably solve long-horizon planning problems. By contrast, classical planners, once a problem is given in a formatted way, can use efficient search algorithms to quickly identify correct, or even optimal, plans. In an effort to get the best of both worlds, this paper introduces LLM+P, the first framework that incorporates the strengths of classical planners into LLMs. LLM+P takes in a natural language description of a planning problem, then returns a correct (or optimal) plan for solving that problem in natural language. LLM+P does so by first converting the language description into a file written in the planning domain definition language (PDDL), then leveraging classical planners to quickly find a solution, and then translating the found solution back into natural language. Along with LLM+P, we define a diverse set of different benchmark problems taken from common planning scenarios. Via a comprehensive set of experiments on these benchmark problems, we find that LLM+P is able to provide optimal solutions for most problems, while LLMs fail to provide even feasible plans for most problems.\footnote{The code and results are publicly available at https://github.com/Cranial-XIX/llm-pddl.git.

### An LLM Compiler for Parallel Function Calling
- **ID:** 2312.04511v3
- **Authors:** Sehoon Kim, Suhong Moon, Ryan Tabrizi, Nicholas Lee, Michael W. Mahoney, Kurt Keutzer, Amir Gholami
- **Year:** 2023
- **Tags:** LLMCompiler, parallel function calling
- **URL:** https://arxiv.org/abs/2312.04511v3
- **PDF:** https://arxiv.org/pdf/2312.04511v3

The reasoning capabilities of the recent LLMs enable them to execute external function calls to overcome their inherent limitations, such as knowledge cutoffs, poor arithmetic skills, or lack of access to private data. This development has allowed LLMs to select and coordinate multiple functions based on the context to tackle more complex problems. However, current methods for function calling often require sequential reasoning and acting for each function which can result in high latency, cost, and sometimes inaccurate behavior. To address this, we introduce LLMCompiler, which executes functions in parallel to efficiently orchestrate multiple function calls. Drawing inspiration from the principles of classical compilers, LLMCompiler enables parallel function calling with three components: (i) a Function Calling Planner, formulating execution plans for function calling; (ii) a Task Fetching Unit, dispatching function calling tasks; and (iii) an Executor, executing these tasks in parallel. LLMCompiler automatically generates an optimized orchestration for the function calls and can be used with both open-source and closed-source models. We have benchmarked LLMCompiler on a range of tasks with different patterns of function calling. We observe consistent latency speedup of up to 3.7x, cost savings of up to 6.7x, and accuracy improvement of up to ~9% compared to ReAct. Our code is available at https://github.com/SqueezeAILab/LLMCompiler.
