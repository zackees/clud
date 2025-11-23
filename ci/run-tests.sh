#!/bin/bash

# FastLED Docker + Playwright Test Runner
# This script demonstrates how to run Playwright tests against Docker-compiled FastLED

set -e

echo "🚀 Starting FastLED Docker + Playwright Testing"
echo "================================================"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if Docker is available
if ! command -v docker &> /dev/null; then
    print_error "Docker is not installed or not in PATH"
    exit 1
fi

# Check if Docker Compose is available
if ! command -v docker-compose &> /dev/null; then
    print_error "Docker Compose is not installed or not in PATH"
    exit 1
fi

# Clean up any existing containers
print_status "Cleaning up existing containers..."
docker-compose down -v --remove-orphans 2>/dev/null || true

# Build the FastLED Docker image
print_status "Building FastLED Docker image..."
docker-compose build fastled-web

# Start the FastLED web service
print_status "Starting FastLED web service..."
docker-compose up -d fastled-web

# Wait for the web service to be ready
print_status "Waiting for FastLED web service to be ready..."
timeout=30
counter=0
while ! curl -s http://localhost:8080 > /dev/null; do
    if [ $counter -ge $timeout ]; then
        print_error "FastLED web service failed to start within ${timeout} seconds"
        docker-compose logs fastled-web
        exit 1
    fi
    sleep 1
    counter=$((counter + 1))
done

print_status "FastLED web service is ready!"

# Install Playwright dependencies if not already installed
print_status "Installing Playwright dependencies..."
npm install @playwright/test

# Install browsers
print_warning "Installing Playwright browsers (this may show dependency warnings)..."
npx playwright install --with-deps 2>&1 | tee playwright-install.log

# Run Playwright tests
print_status "Running Playwright tests against FastLED Docker container..."
echo "This will demonstrate console errors when FastLED compilation/loading fails"
echo ""

# Run tests and capture output
if npx playwright test --config=ci/playwright.config.js 2>&1 | tee test-output.log; then
    print_status "Tests completed successfully"
else
    print_warning "Tests completed with expected failures (demonstrating console errors)"
fi

# Show console errors from test output
print_status "Console errors found during testing:"
echo "======================================"
grep -i "error\|fail\|undefined" test-output.log || echo "No explicit error patterns found in output"

# Show FastLED compilation errors specifically
print_status "FastLED-specific errors:"
echo "========================"
grep -i "fastled\|compilation\|undefined reference" test-output.log || echo "No FastLED-specific errors found"

# Generate HTML report
print_status "Generating HTML test report..."
npx playwright show-report --host 0.0.0.0 --port 9323 &
REPORT_PID=$!

echo ""
print_status "Test execution complete!"
print_status "HTML report available at: http://localhost:9323"
print_warning "The console errors demonstrated are expected when FastLED is not properly compiled/loaded"

# Clean up
print_status "Cleaning up containers..."
docker-compose down

echo ""
print_status "Summary:"
echo "- FastLED Docker container was built and started"
echo "- Playwright tests were executed against the web interface"
echo "- Console errors were captured and logged (as expected)"
echo "- Test results are available in HTML format"

# Kill report server after a delay
sleep 2
kill $REPORT_PID 2>/dev/null || true