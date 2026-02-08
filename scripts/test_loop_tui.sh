#!/bin/bash
# Manual test script for loop TUI integration
#
# This script helps test the loop TUI functionality manually.
# Run this in an interactive terminal to verify all features work.

set -e

echo "==================================="
echo "Loop TUI Manual Test Script"
echo "==================================="
echo ""

# Check if we're in the clud project directory
if [ ! -f "pyproject.toml" ]; then
    echo "Error: Must run from clud project root directory"
    exit 1
fi

# Clean up any previous test artifacts
echo "Cleaning up previous test artifacts..."
rm -f test_output.py DONE.md
rm -rf .loop

echo ""
echo "Test 1: Basic TUI Launch"
echo "------------------------"
echo "This will launch the TUI with a simple 1-iteration loop."
echo "Expected behavior:"
echo "  - TUI should launch with split-pane layout"
echo "  - Menu should show at bottom with 'Options' and 'Exit'"
echo "  - Output should stream to the top pane"
echo "  - After completion, DONE.md should be created"
echo ""
echo "Press Enter to start Test 1, or Ctrl-C to skip..."
read

uv run clud --loop .loop/TEST_LOOP.md --loop-ui --loop-count 1

echo ""
echo "Test 1 complete!"
echo ""

# Check if DONE.md was created
if [ -f "DONE.md" ]; then
    echo "✅ DONE.md was created successfully"
    cat DONE.md
    echo ""
else
    echo "⚠️  DONE.md was not created"
fi

# Clean up
echo "Cleaning up test artifacts..."
rm -f test_output.py DONE.md
rm -rf .loop

echo ""
echo "Test 2: Menu Navigation"
echo "-----------------------"
echo "This test verifies menu interactions during loop execution."
echo "Expected behavior:"
echo "  - Press Tab/Arrow keys to navigate menu"
echo "  - Press Enter on 'Options' to open submenu"
echo "  - Submenu should show '← Back', 'Edit UPDATE.md', 'Halt'"
echo "  - Escape should return to main menu"
echo "  - 'Halt' should stop the loop gracefully"
echo ""
echo "Press Enter to start Test 2, or Ctrl-C to skip..."
read

# Create a longer test that runs multiple iterations
cat > .loop/MENU_TEST.md <<EOF
# Menu Test Loop

## Task
For each iteration, just print "Iteration complete" and wait.

Do NOT create DONE.md until iteration 5.
EOF

uv run clud --loop .loop/MENU_TEST.md --loop-ui --loop-count 5

echo ""
echo "Test 2 complete!"
echo ""

# Clean up
rm -f DONE.md
rm -rf .loop

echo ""
echo "==================================="
echo "Manual Testing Complete!"
echo "==================================="
echo ""
echo "Next steps:"
echo "  1. Verify TUI layout looks correct"
echo "  2. Test keyboard navigation thoroughly"
echo "  3. Test 'Edit UPDATE.md' functionality"
echo "  4. Test 'Halt' functionality"
echo "  5. Verify streaming output appears in real-time"
echo ""
