# Backlog Tab

The Backlog feature provides task visualization and parsing from a `Backlog.md` file in your project directory.

## Features

- **Task Visualization**: View tasks organized by status (To Do, In Progress, Done)
- **Task Parsing**: Parse `Backlog.md` files in GitHub-style task list format
- **Status Detection**: Automatic status detection based on section headings
- **Priority Support**: Optional priority metadata in tasks
- **Timestamp Tracking**: Automatic created_at/updated_at timestamps

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

## Usage

The Backlog parser can be used programmatically:

```python
from clud.backlog.parser import BacklogParser

# Parse a Backlog.md file
parser = BacklogParser()
tasks = parser.parse("/path/to/Backlog.md")

for task in tasks:
    print(f"{task.id}: {task.title} ({task.status})")
```

## Architecture

- **Parser**: `src/clud/backlog/parser.py` - Markdown parser for Backlog.md
- **Models**: `BacklogTask`, `StatusType`, `PriorityType` dataclasses

## Components

- `src/clud/backlog/parser.py` - Markdown parser for Backlog.md (includes BacklogTask, StatusType, PriorityType models)
- `tests/test_backlog_parser.py` - Parser unit tests

## Testing

```bash
# Unit tests
uv run pytest tests/test_backlog_parser.py -vv

# Full test suite
bash test
```

## Related Documentation

- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
