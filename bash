#!/bin/bash
# CLUD development script runner
# This script provides a unified interface for running development tasks

set -e

# Function to show help
show_help() {
    echo "CLUD Development Script Runner"
    echo "Usage: bash <command> [options]"
    echo ""
    echo "Available commands:"
    echo "  install    - Set up development environment with Python 3.13"
    echo "  test       - Run tests using pytest"
    echo "  lint       - Run Python linting with ruff and pyright"
    echo "  clean      - Remove build artifacts, caches, and virtual environment"
    echo "  help       - Show this help message"
    echo ""
    echo "Examples:"
    echo "  bash install         # Set up development environment"
    echo "  bash test            # Run all tests"
    echo "  bash test -v         # Run tests with verbose output"
    echo "  bash lint            # Run linting and formatting"
    echo "  bash clean           # Clean up build artifacts"
}

# Check if no arguments provided
if [ $# -eq 0 ]; then
    echo "Error: No command specified"
    echo ""
    show_help
    exit 1
fi

# Get the command
COMMAND=$1
shift  # Remove the command from arguments so we can pass the rest to the script

# Get the directory where this script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Execute the appropriate command
case "$COMMAND" in
    install)
        echo "🚀 Setting up development environment..."
        exec "$SCRIPT_DIR/install" "$@"
        ;;
    test)
        echo "🧪 Running tests..."
        exec "$SCRIPT_DIR/test" "$@"
        ;;
    lint)
        echo "🔍 Running linting..."
        exec "$SCRIPT_DIR/lint" "$@"
        ;;
    clean)
        echo "🧹 Cleaning up..."
        exec "$SCRIPT_DIR/clean" "$@"
        ;;
    help|--help|-h)
        show_help
        exit 0
        ;;
    *)
        echo "Error: Unknown command '$COMMAND'"
        echo ""
        show_help
        exit 1
        ;;
esac