# Pipe Mode (Unix & Windows)

The `clud` command supports I/O piping for seamless integration with Unix-style command chains. Pipe mode automatically activates when stdin is not a TTY (pseudo-terminal).

## Input Piping

```bash
# Pipe prompt from echo
echo "make me a poem about roses" | clud

# Pipe from file
cat prompt.txt | clud

# Pipe from command output
git log --oneline -5 | clud
```

## Output Piping

```bash
# Pipe output to cat
clud -p "list unix commands" | cat

# Pipe to less for paging
clud -p "explain python asyncio" | less

# Pipe to grep for filtering
clud -p "generate json data" | grep -E "^\{.*\}$"
```

## Chained Pipes

```bash
# Input and output piping together
echo "summarize this" | clud | cat

# Complex pipeline
cat article.txt | clud | tee summary.txt | wc -w
```

## How It Works

- When stdin is piped (non-TTY), `clud` automatically reads the entire input and uses it as the prompt
- Works seamlessly with `-p` flag for explicit prompts: `clud -p "prompt" | cat`
- Compatible with both Unix (Linux/macOS) and Windows (git-bash/MSYS2)
- Uses standard `sys.stdin.isatty()` detection for cross-platform compatibility

## Use Cases

### Code Review

```bash
git diff | clud -p "review these changes for bugs"
```

### Data Processing

```bash
cat data.json | clud -p "extract all email addresses"
```

### Documentation

```bash
cat README.md | clud -p "create a quick start guide" | tee QUICKSTART.md
```

### Batch Processing

```bash
find . -name "*.py" | clud -p "list all functions defined in these files"
```

## Platform Compatibility

### Unix/Linux/macOS

Works natively with all standard Unix tools:
- `cat`, `grep`, `sed`, `awk`
- `curl`, `wget`
- `jq`, `yq`
- Any command that outputs to stdout

### Windows

Works with git-bash/MSYS2 environment:
- Git bash provides Unix-like pipe support
- Standard Windows cmd.exe pipes also work
- PowerShell pipelines are supported

## Technical Details

### Detection

```python
import sys

if not sys.stdin.isatty():
    # Pipe mode activated
    prompt = sys.stdin.read()
```

### Buffer Handling

- Stdin is read completely before processing
- No streaming input support (reads entire input at once)
- Output is streamed in real-time

### Exit Codes

- Exit code is propagated from Claude Code subprocess
- Allows pipe chains to fail properly with `set -e`

## Related Documentation

- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
