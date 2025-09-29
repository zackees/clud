# clud

Claude, but on god mode, by default.

[![Build and Push Multi Docker Image](https://github.com/zackees/clud/actions/workflows/build_multi_docker_image.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/build_multi_docker_image.yml)[![Windows Tests](https://github.com/zackees/clud/actions/workflows/windows-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-test.yml)[![macOS Tests](https://github.com/zackees/clud/actions/workflows/macos-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-test.yml)[![Linux Tests](https://github.com/zackees/clud/actions/workflows/linux-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-test.yml)
[![Integration Tests](https://github.com/zackees/clud/actions/workflows/integration-tests.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/integration-tests.yml)

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity**. In other words, it unlockes god mode for claude cli.

The name `clud` is simply a shorter, easier-to-type version of `claude`. Use `-bg` to launch the sandboxed background agent with full Docker server capabilities.

## Why is clud batter?

Claude Code's safety prompts, while well-intentioned, slow down experienced developers. `clud` removes these friction points by running Claude Code with `--dangerously-skip-permissions` in both foreground and background modes, delivering the uninterrupted coding experience Claude Code was meant to provide.

## Installation

```bash
pip install clud
```

## Quick Start

```bash
# Unleash Claude Code instantly (YOLO mode enabled by default)
clud

# Launch background agent with full Docker server capabilities
clud -bg

# Launch background agent with web UI for browser-based development
clud -bg --ui
```

## Operation Modes

### Foreground Agent (Default)

Launches Claude Code directly with YOLO mode enabled - no permission prompts, maximum velocity:

```bash
clud [directory]                    # Unleash Claude Code in directory
clud -p "refactor this entire app"  # Execute with specific prompt
clud -m "add error handling"        # Send direct message
clud --continue                     # Continue previous conversation
```

**Unleashed Features:**
- Claude Code with dangerous permissions enabled by default
- Zero interruption workflow - no safety prompts
- Containerized development environment
- Automatic project directory mounting
- Direct prompt execution for rapid iteration

### Background Agent (`-bg`)

Full Docker server mode with the same YOLO approach - perfect for complex development workflows:

```bash
clud -bg [directory]               # Launch container shell (YOLO enabled)
clud -bg --ui                      # Launch code-server web UI
clud -bg --build                   # Build custom Docker image
clud -bg --update                  # Pull latest runtime updates
clud -bg --ssh-keys                # Mount SSH keys for git operations
```

**Unleashed Features:**
- Interactive container shell with YOLO Claude Code
- Code-server web UI on port 8743 with dangerous permissions
- SSH key mounting for seamless git operations
- Custom Docker image building and management
- Advanced networking and security configuration

## Configuration

### API Key Setup

```bash
# Interactive setup

# Use environment variable
export ANTHROPIC_API_KEY="sk-ant-..."

# Use command line
clud --api-key "sk-ant-..."
```

## Docker Hub

The Docker image is available on Docker Hub as: `niteris/clud`

## Development

To develop this package:

```bash
# Setup development environment
bash install

# Run tests
bash test

# Lint code
bash lint

# Build Docker image
docker build -t niteris/clud .
```

### Windows Development

This environment requires you to use `git-bash` for proper Unix-like shell support.


# Links
  * Aggregate of claude enhancements: https://github.com/hesreallyhim/awesome-claude-code?tab=readme-ov-file
  * https://github.com/SuperClaude-Org/SuperClaude_Framework
  * https://github.com/ayoubben18/ab-method
