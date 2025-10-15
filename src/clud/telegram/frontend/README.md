# Claude Code - Telegram Frontend

SvelteKit-based web interface for monitoring and interacting with Claude Code Telegram bot sessions in real-time.

## Features

- **Real-time Session Monitoring**: View all active Telegram bot sessions
- **Live Message Updates**: WebSocket-based real-time message streaming
- **Session Management**: Select, view, and delete sessions
- **Responsive Design**: Works on desktop and mobile devices
- **Dark/Light Theme**: Toggle between themes with persistence
- **Connection Status**: Visual indicators for WebSocket connection state
- **Typing Indicators**: See when the bot is responding
- **Message History**: Full conversation history with timestamps

## Development

```bash
# Install dependencies
npm install

# Start dev server (with proxy to backend on localhost:8889)
npm run dev

# Type check
npm run check

# Run tests
npm run test

# Build for production
npm run build

# Preview production build
npm run preview
```

## Architecture

### Components

- **SessionList**: Sidebar showing all active sessions
- **ChatView**: Main chat interface with message display
- **ConnectionStatus**: WebSocket connection indicator

### Services

- **WebSocket Service**: Real-time communication with backend
- **API Service**: REST API calls for session management

### Stores (Svelte 5 Runes)

- **app.svelte.ts**: Application state (sessions, theme, connection)
- **messages.svelte.ts**: Message history per session

### Tech Stack

- **Svelte 5**: Modern reactive framework with runes
- **SvelteKit**: Full-stack framework with SPA adapter
- **TypeScript**: Type-safe development
- **Vite**: Fast build tool
- **Vitest**: Unit testing framework

## Project Structure

```
src/
├── lib/
│   ├── components/      # Svelte components
│   ├── services/        # API and WebSocket services
│   ├── stores/          # State management (Svelte runes)
│   ├── types.ts         # TypeScript type definitions
│   └── utils.ts         # Utility functions
├── routes/
│   ├── +layout.svelte   # Root layout
│   ├── +layout.ts       # SSR config (disabled)
│   └── +page.svelte     # Main page
├── tests/               # Unit tests
├── app.css              # Global styles
├── app.d.ts             # TypeScript declarations
└── app.html             # HTML template
```

## WebSocket Protocol

### Client → Server

```json
{
  "type": "subscribe",
  "session_id": "uuid",
  "auth_token": "token"
}

{
  "type": "send_message",
  "content": "message text"
}

{
  "type": "ping"
}
```

### Server → Client

```json
{
  "type": "history",
  "messages": [...]
}

{
  "type": "message",
  "message": {...}
}

{
  "type": "typing",
  "is_typing": true
}

{
  "type": "error",
  "error": "error message"
}
```

## REST API Endpoints

- `GET /api/telegram/sessions` - List all sessions
- `GET /api/telegram/sessions/{id}` - Get session details
- `DELETE /api/telegram/sessions/{id}` - Delete session
- `GET /api/telegram/auth` - Get auth token
- `GET /api/telegram/health` - Health check

## Configuration

### Development Proxy

The dev server proxies API and WebSocket requests to `localhost:8889` (configurable in `vite.config.ts`).

### Production Build

Build output goes to `build/` directory, which is served by the FastAPI backend at `/` endpoint.

## Testing

Unit tests use Vitest with jsdom environment. Run with:

```bash
npm run test              # Run once
npm run test:watch        # Watch mode
npm run test:ui           # UI mode
npm run test:coverage     # With coverage
```

## Theme

The app supports dark and light themes with CSS custom properties. Theme preference is saved to localStorage.

## Browser Compatibility

- Modern browsers with WebSocket support
- ES2020+ JavaScript features
- CSS Grid and Flexbox
