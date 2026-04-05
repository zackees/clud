---
name: coding-standards
description: Universal code quality standards to follow when writing code
triggers:
  - When writing new code
  - When reviewing code
  - When refactoring existing code
---

# Coding Standards

## Core Principles

1. **Clarity over cleverness** — Write code that reads like prose. If it needs a comment to explain *what* it does, rewrite it.
2. **Fail fast and loud** — Never silently catch exceptions. Log with context, then re-raise or handle explicitly.
3. **Immutability by default** — Use `const`, `final`, `readonly`, frozen dataclasses. Mutate only when necessary.
4. **Small functions** — Each function does one thing. If you can't name it clearly, it does too much.
5. **Explicit over implicit** — Type annotations, named parameters, clear return values. No magic.

## Error Handling

- Always catch the most specific exception type
- Include context in error messages: what was being done, what failed, what values were involved
- Never use bare `except:` or `catch (Exception e)` at low levels
- Let unexpected errors propagate — don't hide bugs

## Naming

- Functions: verb phrases (`calculate_total`, `fetch_user`, `validate_input`)
- Booleans: question phrases (`is_valid`, `has_permission`, `should_retry`)
- Collections: plural nouns (`users`, `error_messages`, `pending_tasks`)
- Avoid abbreviations unless universally understood (`url`, `id`, `config`)

## Testing

- Every public function has at least one test
- Test the behavior, not the implementation
- Use descriptive test names that explain the scenario and expected outcome
- Arrange-Act-Assert structure
