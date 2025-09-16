# Docker Plugin Integration for Claude CLI

This document describes the implementation of automatic Claude CLI plugin installation for the CLUD development environment.

## Overview

The CLUD Docker container now supports automatic installation of Claude CLI slash commands from plugins stored in the project directory. This allows developers to package project-specific Claude commands that are automatically available when working in the containerized environment.

## Implementation

### Directory Structure

```
docker/
└── plugins/
    └── claude/
        └── commands/
            ├── example.md
            ├── deploy.md
            └── test.md
```

### How It Works

1. **Build Time**: During Docker image build, plugins from `docker/plugins/claude/commands/` are copied to `/root/.claude/commands/` in the container
2. **Runtime**: The background agent (`bg.py`) automatically installs plugins from the workspace to the system after the initial sync
3. **Plugin Format**: Each `.md` file becomes a slash command (e.g., `example.md` → `/example`)

### Components Modified

#### Dockerfile Changes
- Added plugin copying during build process
- Creates `/root/.claude/commands/` directory with proper permissions

#### Background Agent (bg.py) Enhancement
- Added `install_claude_plugins()` method
- Automatically installs plugins after initial workspace sync
- Handles plugin installation errors gracefully

### Usage

1. **Create Plugin**: Add `.md` files to `docker/plugins/claude/commands/`
2. **Build Container**: Plugins are automatically included in the Docker image
3. **Run Container**: Background agent installs plugins on startup
4. **Use Commands**: Type `/commandname` in Claude Code to execute

### Example Plugin

See `docker/plugins/claude/commands/example.md` for a template showing the required format and structure.

### Benefits

- **Project-Specific**: Each project can have its own set of Claude commands
- **Version Controlled**: Plugins are stored in the repository
- **Automatic**: No manual installation required
- **Isolated**: Plugins are contained within the project environment

## Plugin Development Guidelines

1. **Naming**: Use descriptive filenames without spaces (use hyphens or underscores)
2. **Content**: Include clear documentation and usage instructions
3. **Purpose**: Focus on project-specific workflows and commands
4. **Format**: Standard Markdown format with clear sections

This implementation ensures that Claude CLI plugins are seamlessly integrated into the development workflow while maintaining isolation and version control.