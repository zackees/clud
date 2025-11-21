# Troubleshooting

## Claude Code Installation Issues

The `clud` tool automatically installs Claude Code when it's not detected on your system. However, the official npm package `@anthropic-ai/claude-code` may occasionally have issues.

### Automatic Fallback Strategies

When installation fails, `clud` automatically tries multiple methods:

#### 1. Local --prefix Install (Default, Recommended)

- Installs `@latest` to `~/.clud/npm` directory using `npm install --prefix`
- Isolated from global npm installations
- Controlled by `clud`

#### 2. Global Install with Isolated Prefix (Automatic Fallback)

- Falls back if local install fails with module errors
- Uses `npm install -g` with `NPM_CONFIG_PREFIX=~/.clud/npm` environment variable
- Installs to `~/.clud/npm` (same as default method, not system-wide)
- Works with both bundled nodejs-wheel npm and system npm

#### 3. Specific Version Install (Automatic Fallback)

- Falls back if global install also fails
- Tries known-working version (e.g., `v0.6.0`)
- Installs to `~/.clud/npm` using `npm install --prefix`
- May use older but more stable version

**Technical Detail**: `clud` bundles its own npm via `nodejs-wheel`. To prevent npm global installs from going to the virtual environment (where they'd be inaccessible), we set the `NPM_CONFIG_PREFIX` environment variable to `~/.clud/npm` for all npm operations. This ensures all installation methods (--prefix, -g, or specific version) install to the same controlled location.

### Common Installation Errors

#### "Cannot find module '../lib/cli.js'"

- **Cause**: Broken npm package structure (missing internal files)
- **Solution**: `clud` automatically tries global install and specific version fallbacks
- **Manual workaround**: Install globally with `npm install -g @anthropic-ai/claude-code@latest`

#### "EACCES" or "permission denied"

- **Cause**: Insufficient permissions for npm installation
- **Solution**: Fix npm permissions following [npm docs](https://docs.npmjs.com/resolving-eacces-permissions-errors)
- **Alternative**: Use `sudo` for global install (not recommended on shared systems)

#### "ENOTFOUND" or network errors

- **Cause**: Network connectivity issues or npm registry unavailable
- **Solution**: Check internet connection, try again later
- **Alternative**: Install behind proxy with appropriate npm configuration

#### Installation succeeded but executable not found

- **Cause**: npm installed to unexpected location
- **Solution**: Check `~/.clud/npm/node_modules/.bin/` for `claude` or `claude.cmd`
- **Manual workaround**: Set `PATH` to include the npm bin directory

### Manual Installation Methods

If automatic installation fails completely:

#### 1. Global npm install

```bash
npm install -g @anthropic-ai/claude-code@latest
```

#### 2. Direct download from Anthropic

- Visit: https://claude.ai/download
- Download installer for your platform
- Follow installation instructions

#### 3. Clear npm cache and retry

```bash
npm cache clean --force
clud --install-claude
```

#### 4. Use clud installation command

```bash
clud --install-claude
```

### Verifying Installation

Once installed (automatically or manually), verify with:

```bash
claude --version
```

The `clud` tool will automatically detect Claude Code in:
- `~/.clud/npm/` (local installation)
- System PATH (global npm installation)
- Common Windows npm locations (`%APPDATA%\npm\`)

### Getting Help

If installation issues persist:
- Check clud logs for detailed error messages
- Review the troubleshooting guidance printed by failed installations
- Report issues at: https://github.com/anthropics/claude-code/issues
- Note: Installation errors from the official npm package are Anthropic's responsibility

## Related Documentation

- [Development Setup](setup.md)
- [Architecture](architecture.md)
- [Code Quality Standards](code-quality.md)
