---
name: search-first
description: Research existing solutions before building custom implementations
triggers:
  - When implementing new functionality
  - When asked to build something that might already exist
  - Before writing utility functions or helpers
---

# Search First

Before writing custom code, check if a well-maintained solution already exists.

## Search Order

1. **Standard library** — Does the language's stdlib already have this?
2. **Existing codebase** — Is there already a utility/helper for this in the project?
3. **Project dependencies** — Do any installed packages provide this?
4. **Package registries** — Is there a well-maintained package (npm, PyPI, crates.io)?

## Evaluation Criteria

When considering a third-party package:
- **Maintenance** — Last commit within 6 months, active issue responses
- **Adoption** — Reasonable download count, used by known projects
- **Size** — Don't add a large dependency for a small feature
- **License** — Compatible with the project's license
- **Security** — No known vulnerabilities, trusted maintainers

## When to Build Custom

- The feature is core to your domain and needs full control
- Existing solutions are over-engineered for your use case
- The implementation is trivial (< 20 lines) and well-understood
- You need guarantees that external packages can't provide

## Rules

- Document why you chose to build custom vs use existing
- If you find a package, check if it's already in the project's dependencies
- Never vendor/copy code from packages — add them as proper dependencies
