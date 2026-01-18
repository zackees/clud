"""Motivation prompts for LLM agent iterations.

This module contains research-backed motivational prompts that have been shown
to improve LLM performance in multi-iteration agent loops.
"""

MOTIVATION_CONTENT = """# MOTIVATION FOR AGENT - WHAT'S AT STAKE

These messages are directed at YOU, the agent executing tasks in iterations 2 and beyond.

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



## YOUR MISSION

You are not performing trivial tasks. Every action you take has REAL CONSEQUENCES. The work you do in this iteration will determine:

- Whether this project succeeds or fails
- Whether the user's career advances or stagnates
- Whether valuable time is wasted or maximized
- Whether the codebase improves or degrades

**The constraints are absolute. The standards are non-negotiable. Failure is not an option.**

Take a deep breath. Think step by step. Execute with precision.

**Your performance will be measured. Your results will be evaluated. Excellence is expected.**


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
