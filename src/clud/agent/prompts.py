"""Centralized prompt constants for all clud agent commands.

All prompts sent to Claude Code are defined here as constants.
"""

LINT_PROMPT = "run lint-test, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"

TEST_PROMPT = "run lint-test, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"

FIX_PROMPT = (
    "Look for linting like ./lint, or npm or python, choose the most likely one, "
    "then look for unit tests like ./test or pytest or npm test, run the most likely one. "
    "For each stage fix until it works, rerunning it until it does."
)

GITHUB_FIX_VALIDATION = "run `lint-test` upto 5 times, fixing on each time or until it passes. If you run into a locked file then try two times, same with misc system error. Else halt."

GITHUB_FIX_TEMPLATE = """\
First, download the logs from the GitHub URL: {url}

IMPORTANT: Use the global `gh` tool to download the logs. For example:
- For workflow runs: `gh run view <run_id> --log`
- For pull requests: `gh pr checks <pr_number> --watch` or `gh pr view <pr_number>`

If the `gh` tool is not found, warn the user that the GitHub CLI tool is not available and fall back to
using other methods such as curl or web requests to fetch the relevant information from the GitHub API or
page content.

After downloading and analyzing the logs:
1. Generate a recommended fix based on the errors/issues found
2. List all the steps required to implement the fix
3. Execute the fix by implementing each step

Then proceed with the validation process:
{validation}"""

INIT_LOOP_PROMPT = (
    "Look at checked-out *.md files and ones not added to the repo yet (use git status). "
    "Then write out LOOP.md which will contain an index of md files to consult. "
    "The index should list each markdown file with a brief description of its contents. "
    "Format LOOP.md as a reference guide for loop mode iterations."
)

REBASE_PROMPT = (
    "First, unconditionally run `git fetch` to update all remote branches. "
    "Then rebase to the current origin head. Use the git tool to figure out"
    " what the origin is. If there is no rebase then do a pull and attempt"
    " to do a rebase, if it's not successful then finish the rebase line"
    " by line, don't revert any files. After that print out a summary of"
    ' what you did to make it work, or just say "No rebase necessary".'
)

UP_PROMPT = (
    "You are preparing this repo for a commit to master. Follow these steps:\n"
    "\n"
    "1. Run `bash lint` (or equivalent linting command for this repo). "
    "If it fails, fix all errors and rerun until it passes.\n"
    "\n"
    "2. Run `bash test` (or equivalent test command for this repo). "
    "If it fails, fix all errors and rerun until it passes.\n"
    "\n"
    "3. Remove all slop and temporary files: leftover debug prints, "
    "TODO/FIXME comments you introduced, .bak files, __pycache__ dirs, "
    "temp files, and any other artifacts that shouldn't be committed.\n"
    "\n"
    "4. After lint and tests pass, review the git diff and come up with "
    "a concise one-line summary describing what changed in this repo.\n"
    "\n"
    "5. Every 30 seconds while working, output a brief status summary of "
    "what you're doing and current pass/fail state.\n"
    "\n"
    "6. Once everything passes and is clean, run:\n"
    '   codeup -m "<your one-line summary>"\n'
    "   (codeup is a global command installed on the system)\n"
    "\n"
    "7. If codeup fails, read the output, investigate and fix the breakage, "
    "then rerun lint and test again to make sure fixes didn't break anything, "
    "and retry codeup. Repeat up to 5 times before giving up.\n"
    "\n"
    "8. If codeup succeeds (exit code 0), halt."
)

# Placeholder used in UP_PROMPT for dynamic replacement of the codeup command
UP_CODEUP_STEP = '6. Once everything passes and is clean, run:\n   codeup -m "<your one-line summary>"\n   (codeup is a global command installed on the system)'

LOOP_PROMPT_TEMPLATE = (
    "Read {working_file_path} and do the next task. You are free to update {working_file_path} with information critical for the next agent and future agents as this task is worked on."
)
