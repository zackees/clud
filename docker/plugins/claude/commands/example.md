# Example Plugin Command

This is an example slash command that demonstrates how to create Claude CLI plugins.

## Usage

Type `/example` in Claude Code to execute this command.

## Description

This command serves as a template for creating custom Claude CLI commands. It shows the basic structure and format required for plugin commands.

## Implementation

When this command is executed, Claude will:

1. Read this markdown file
2. Use the content as context for the conversation
3. Help you understand how to create more plugins

## Creating Your Own Commands

1. Create a new `.md` file in `docker/plugins/claude/commands/`
2. Use the filename (without extension) as your slash command name
3. Write helpful documentation and instructions
4. The content becomes available to Claude when the command is used

This plugin system allows you to extend Claude Code with project-specific commands and workflows.