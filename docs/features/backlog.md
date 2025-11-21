# Backlog Tab

The Web UI includes an integrated Backlog tab for visualizing and managing project tasks from a `Backlog.md` file in your project directory.

## Features

- **Task Visualization**: View tasks organized by status (To Do, In Progress, Done)
- **Status Filtering**: Filter tasks by status using button controls
- **Search**: Search tasks by title or description
- **Real-time Updates**: Refresh button to reload tasks from `Backlog.md`
- **Task Statistics**: Header displays task counts by status
- **Markdown-based**: Tasks are stored in a simple `Backlog.md` file in your project root

## Usage

1. **Create Backlog.md**: Create a `Backlog.md` file in your project root directory
2. **Open Web UI**: Launch the Web UI with `clud --webui`
3. **Navigate to Backlog Tab**: Click the "Backlog" tab in the navigation bar
4. **View Tasks**: Tasks are automatically loaded and displayed by status
5. **Filter Tasks**: Click status buttons (All, To Do, In Progress, Done) to filter
6. **Search Tasks**: Use the search input to find tasks by title or description
7. **Refresh**: Click the refresh button to reload tasks from the file

## Backlog.md Format

The parser supports GitHub-style task lists with status sections and optional metadata:

```markdown
# Backlog

## To Do
- [ ] #1 Add user authentication (priority: high)
  - Implement OAuth2 flow
  - Add JWT token handling
- [ ] #2 Create dashboard UI (priority: medium)
  - Design wireframes
  - Implement frontend components

## In Progress
- [ ] #3 Fix login bug
  - Debug session handling
  - Add error logging

## Done
- [x] #4 Setup project structure
  - Initialize repository
  - Configure build system
- [x] #5 Write documentation (priority: low)
  - README.md completed
  - API docs added
```

## Task Format Details

- **Task ID**: `#N` format (e.g., `#1`, `#2`) - auto-extracted from task text
- **Status**: Determined by section heading (To Do, In Progress, Done)
- **Checkbox**: `- [ ]` for incomplete, `- [x]` for complete
- **Priority**: Optional inline metadata `(priority: high|medium|low)`
- **Description**: Indented sub-items under the main task
- **Timestamps**: Automatically tracked (created_at, updated_at)

## API Endpoint

- **Endpoint**: `GET /api/backlog`
- **Response Format**:
  ```json
  {
    "tasks": [
      {
        "id": "1",
        "title": "Add user authentication",
        "status": "todo",
        "description": "Implement OAuth2 flow and JWT tokens",
        "priority": "high",
        "created_at": 1736899200,
        "updated_at": 1736899200
      }
    ]
  }
  ```
- **Error Handling**: Returns empty tasks array if `Backlog.md` is missing or unreadable

## Architecture

- **Backend**: Backlog parser (`backlog/parser.py`) reads and parses `Backlog.md`
- **API Handler**: `BacklogHandler` in `webui/api.py` provides REST endpoint
- **Frontend**: Svelte component (`frontend/src/lib/components/Backlog.svelte`)
- **Data Flow**: File → Parser → API → WebSocket → UI

## Components

- `src/clud/backlog/parser.py` - Markdown parser for Backlog.md (includes BacklogTask, StatusType, PriorityType models)
- `src/clud/webui/api.py` - BacklogHandler for API endpoint
- `src/clud/webui/frontend/src/lib/components/Backlog.svelte` - Backlog UI component
- `tests/test_backlog_parser.py` - Parser unit tests (15 tests)
- `tests/test_backlog_tab_e2e.py` - E2E tests (9 tests)

## Testing

```bash
# Unit tests
uv run pytest tests/test_backlog_parser.py -vv  # 15 tests

# E2E tests
uv run pytest tests/test_backlog_tab_e2e.py -vv  # 9 tests

# Full test suite
bash test --full  # Includes all Backlog tests
```

## Limitations

- **Read-Only**: Current implementation only reads tasks (no editing via UI)
- **Single File**: Only supports `Backlog.md` in project root (no custom paths)
- **No Persistence**: Task updates must be made by editing `Backlog.md` directly
- **No Sorting**: Tasks displayed in file order (no custom sorting)

## Future Enhancements

- AI agent integration for automated task management
- Task editing and creation via UI
- Task sorting and grouping options
- Multiple backlog file support
- Task dependencies and relationships

## Related Documentation

- [Web UI](webui.md)
- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
