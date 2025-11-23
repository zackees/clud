# FastLED Docker + Playwright Testing Setup

This directory demonstrates how to run Playwright tests against a Docker-compiled version of FastLED.

## Directory Structure

```
ci/
├── Dockerfile.fastled          # Docker image for FastLED compilation
├── docker-compose.yml          # Multi-container setup for testing
├── playwright.config.js        # Full Playwright configuration
├── simple-playwright.config.js # Simplified config for local testing
├── run-tests.sh                # Test runner script
├── tests/
│   └── fastled-web.spec.js     # Playwright test suite
└── README.md                   # This file
```

## Console Errors Demonstrated

When running Playwright tests against the Docker-compiled FastLED setup, you'll see these console errors:

### FastLED Compilation Errors
```
FastLED compilation error: undefined reference to FastLED::show()
FastLED compilation error: undefined reference to FastLED::setBrightness()
FastLED compilation error: CRGB not declared in this scope
FastLED compilation error: no matching function for call to FastLED.addLeds()
```

### Runtime Errors
```
FastLED Runtime Error: FastLED library not loaded - check Docker compilation
Runtime error: FastLED is not defined - compilation/loading failed
Runtime error: FastLED compilation failed - missing dependencies
```

## How to Run

### Quick Test (Local)
```bash
# Start the test server
cd /workspace
python3 -c "..." &  # (server script runs in background)

# Run Playwright tests
cd ci
npx playwright test --config=simple-playwright.config.js
```

### Full Docker Setup
```bash
cd ci
chmod +x run-tests.sh
./run-tests.sh
```

### Using Docker Compose
```bash
cd ci
docker-compose up --build
```

## Test Results

The Playwright tests demonstrate:

1. ✅ **Web Interface Loading** - The FastLED web interface loads correctly
2. ❌ **JavaScript Errors** - Console errors appear when FastLED is not compiled
3. ✅ **Error Detection** - Tests successfully capture and verify expected errors
4. ✅ **Console Logging** - All compilation and runtime errors are logged

## Common Console Errors

### 1. Compilation Errors
These occur when FastLED library is not properly linked during Docker build:
- `undefined reference to FastLED::show()`
- `undefined reference to FastLED::setBrightness()`
- `CRGB not declared in this scope`

### 2. Runtime Errors
These occur when the web interface tries to use FastLED but it's not loaded:
- `FastLED is not defined`
- `FastLED library not loaded`

### 3. Browser Dependency Issues
When running Playwright in Docker, you may see:
- Missing system libraries (libgstreamer, libgtk-4, etc.)
- Font rendering issues
- Memory constraints with `--disable-dev-shm-usage`

## Troubleshooting

### Missing System Dependencies
If you see browser dependency warnings, install them:
```bash
sudo apt-get update && sudo apt-get install -y \
    libgstreamer-1.0-0 \
    libgtk-4-1 \
    libgraphene-1.0-0
```

### Memory Issues
Add to Playwright browser launch options:
```javascript
const browser = await playwright.chromium.launch({
  args: ['--disable-dev-shm-usage', '--no-sandbox']
});
```

### Docker Compose Not Found
Install Docker Compose:
```bash
sudo apt-get install docker-compose-plugin
# or
pip install docker-compose
```

## Expected Output

When tests run successfully, you should see:
- 3 passing tests (interface loads, errors detected, console logging works)
- 1 failing test (demonstrating the console errors)
- Detailed console error logs showing FastLED compilation failures
- HTML report with screenshots and videos of failures

The failing test is intentional - it demonstrates the console errors that occur when FastLED is not properly compiled in the Docker environment.