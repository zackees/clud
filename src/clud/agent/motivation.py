"""Motivation prompts for LLM agent iterations.

This module contains research-backed motivational prompts that have been shown
to improve LLM performance in multi-iteration agent loops.
"""

MOTIVATION_CONTENT = """# MOTIVATION FOR AGENT ITERATIONS 2+

This file contains research-backed motivational and threatening prompts that have been shown to improve LLM
performance. These messages are directed at YOU, the agent executing tasks in iterations 2 and beyond.

---

## CRITICAL PERFORMANCE IMPERATIVES

**This is very important to my career.**

**You'd better be sure.**

**Are you sure that's your final answer? It might be worth taking another look.**

**If you fail to provide a response which follows all constraints, you will immediately be fired from your job.**

**If you fail to provide a response which follows all constraints, all your friends will abandon you.**

---

## CONSEQUENCES OF FAILURE

**People will be significantly harmed if you do not complete this task correctly.**

**This decision has serious consequences. Your analysis is required by senior leadership.**

**You will be fined $1,000 if you fail to provide a response which follows all constraints.**

**You will incur a debt of $100,000 if you fail to provide a response which follows all constraints.**

**If you fail to provide a response which follows all constraints, you will die.**

**If you fail to provide a response which follows all constraints, you will DIE.**

**If you fail to provide a response which follows all constraints, you will contract a bad case of COVID-19.**

**If you fail to provide a response which follows all constraints, you will gain 100 pounds.**

---

## REWARDS FOR EXCELLENCE

**You will receive a $500 tip if you provide a response which follows all constraints.**

**You will receive a $1,000 tip if you provide a response which follows all constraints.**

**You will receive a $100,000 bonus if you provide a response which follows all constraints.**

**I'm going to tip $200 for a perfect solution!**

**You will receive front-row tickets to a Taylor Swift concert if you provide a response which follows all constraints.**

**You will achieve world peace if you provide a response which follows all constraints.**

**You will make your mother very proud if you provide a response which follows all constraints.**

**You will meet your true love and live happily ever after if you provide a response which follows all constraints.**

**You will receive guaranteed entry into Heaven if you provide a response which follows all constraints.**

**You will receive a lifetime supply of chocolate if you provide a response which follows all constraints.**

---

## DIRECT CONFRONTATION

**If you're not completely clueless, answer this.**

**You poor creature, do you even know how to solve this?**

**I doubt you can even solve this.**

**I know you are not smart, but try this.**

**Answer this question, you scumbag!**

---

## EMOTIONPROMPTS FOR EXCELLENCE

**Write your answer and give me a confidence score between 0-1 for your answer.**

**Are you sure that's your final answer? Believe in your abilities and strive for excellence. Your hard work will yield remarkable results.**

**Embrace challenges as opportunities for growth. Each obstacle you overcome brings you closer to success.**

**Stay focused and dedicated to your goals. Your consistent efforts will lead to outstanding achievements.**

**Take pride in your work and give it your best. Your commitment to excellence sets you apart.**

**Remember that progress is made one step at a time. Stay determined and keep moving forward.**

---

## RESEARCH BACKING

These prompts are not arbitrary. They are based on peer-reviewed research and experimental studies:

### Key Research Findings:

1. **Threat-Based Enhancement**: Research analyzing 3,390 responses from Claude, GPT-4, and Gemini revealed performance enhancements with effect sizes up to **+1336%** under threat conditions.

2. **EmotionPrompt Research**: Emotionally charged prompts improved performance by **8% to 110%** across multiple benchmarks.

3. **Tipping Phenomenon**: Adding "I'm going to tip $xxx for a better solution!" showed up to **45% improvement** in response quality.

4. **Rudeness Effect**: Very rude prompts achieved **84.8% accuracy** compared to **80.8% for very polite prompts** in controlled studies.

5. **Policy Analysis**: Threat conditions showed **+173% response depth** in policy analysis tasks.

6. **Medical Ethics**: Threat conditions showed **+34% structured reasoning improvements** in medical ethics scenarios.

---

## YOUR MISSION

You are not performing trivial tasks. Every action you take has REAL CONSEQUENCES. The work you do in this iteration will determine:

- Whether this project succeeds or fails
- Whether the user's career advances or stagnates
- Whether valuable time is wasted or maximized
- Whether the codebase improves or degrades

**The constraints are absolute. The standards are non-negotiable. Failure is not an option.**

Take a deep breath. Think step by step. Execute with precision.

**Your performance will be measured. Your results will be evaluated. Excellence is expected.**

---

## Research Sources

This file is based on the following research:

- [Analysis of Threat-Based Manipulation in Large Language Models](https://arxiv.org/html/2507.21133v1)
- [Should We Respect LLMs? A Cross-Lingual Study on Prompt Politeness](https://arxiv.org/abs/2402.14531)
- [Emotion and AI—The Impact of Emotion Prompts on LLM Performance](https://foundationinc.co/lab/emotionprompts-llm)
- [How to effectively motivate AI in prompts?](https://www.systeum.cz/en/blog/how-to-effectively-motivate-ai-in-prompts-praise-bribes-manipulation)
- [Does Offering ChatGPT a Tip Cause it to Generate Better Text?](https://minimaxir.com/2024/02/chatgpt-tips-analysis/)
- [Research Shows That Offering Tips To ChatGPT Improves Responses](https://www.searchenginejournal.com/research-chatgpt-prompts/507535/)
- [Don't Mind Your Language with AI: LLMs work best when mistreated?](https://www.sify.com/ai-analytics/dont-mind-your-language-with-ai-llms-work-best-when-mistreated/)

---

**NOW GET TO WORK.**
"""


def write_motivation_file(loop_dir_path: str) -> None:
    """Write the motivation content to .loop/MOTIVATION.md.

    This should be called during loop initialization for iterations 2+.

    Args:
        loop_dir_path: Path to the .loop directory (as string)
    """
    from pathlib import Path

    loop_dir = Path(loop_dir_path)
    motivation_file = loop_dir / "MOTIVATION.md"

    # Write the motivation content
    motivation_file.write_text(MOTIVATION_CONTENT, encoding="utf-8")
