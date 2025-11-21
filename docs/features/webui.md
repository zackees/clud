# Web UI

Launch browser-based interface for Claude Code with real-time streaming.

## Quick Start

```bash
# Default port (8888)
clud --webui

# Custom port
clud --webui 3000
# or
clud --ui 3000
```

## Features

- **Real-time streaming chat** interface with Claude Code
- **Integrated terminal console** with xterm.js (split-pane layout) - See [Terminal Console](terminal.md)
- **Backlog tab** for visualizing tasks from `Backlog.md` - See [Backlog Tab](backlog.md)
- **Project directory selection**
- **Conversation history** (stored in browser localStorage)
- **Dark/light theme toggle**
- **Mobile-responsive design**
- **WebSocket-based communication**
- **Markdown rendering** with code syntax highlighting
- **YOLO mode** (no permission prompts)

## Architecture

### Backend

- **FastAPI** application with WebSocket support
- **Handler classes** for chat, projects, and history
- **Cross-platform PTY** session management for terminals
- **Terminal I/O** streaming via WebSocket

### Frontend

**Svelte 5 + SvelteKit + TypeScript** (migrated from vanilla JS):

```
frontend/
├── src/lib/components/   # UI components
│   ├── Chat.svelte
│   ├── Terminal.svelte
│   ├── Backlog.svelte
│   ├── DiffViewer.svelte
│   ├── Settings.svelte
│   └── History.svelte
├── src/lib/stores/       # Svelte stores for state management
│   ├── app.ts
│   ├── chat.ts
│   └── settings.ts
├── src/lib/services/     # WebSocket and API services
└── build/                # Production build output (served by FastAPI)
```

### Static Files

- Production: `src/clud/webui/frontend/build/`
- Fallback: `static/` (if build missing)

## Configuration

- **Default port**: 8888 (auto-detects if unavailable)
- **Browser auto-opens** after 2-second delay
- **Server logs** to console with INFO level
- **Stop server**: Press Ctrl+C

## Development

### Install Frontend Dependencies

```bash
cd src/clud/webui/frontend
npm install
```

### Run Development Server

```bash
cd src/clud/webui/frontend
npm run dev
```

- Hot reload enabled
- Default port: 5173

### Build for Production

```bash
cd src/clud/webui/frontend
npm run build
```

- Outputs to `build/` directory
- Uses `@sveltejs/adapter-static` for SPA mode

### Type-Check Svelte Components

```bash
cd src/clud/webui/frontend
npm run check
```

## Inspiration

Inspired by: [sugyan/claude-code-webui](https://github.com/sugyan/claude-code-webui)

## Related Documentation

- [Terminal Console](terminal.md) - Integrated terminal documentation
- [Backlog Tab](backlog.md) - Task visualization documentation
- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
