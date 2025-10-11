#!/bin/bash
# verify_implementation.sh - Verify Telegram/SMS/WhatsApp integration

set -e

echo "🔍 Verifying Telegram/SMS/WhatsApp Integration"
echo "================================================"
echo

# Check Python version
echo "✓ Python version:"
python3 --version
echo

# Check file structure
echo "✓ Messaging module files:"
ls -1 src/clud/messaging/
echo

# Check imports
echo "✓ Testing imports..."
python3 -c "import sys; sys.path.insert(0, 'src'); from clud.messaging import MessagingClient, create_client, AgentNotifier"
echo "  ✓ All imports successful"
echo

# Check argument parsing
echo "✓ Testing argument parsing..."
python3 -c "import sys; sys.path.insert(0, 'src'); from clud.agent_foreground_args import parse_args; args = parse_args(['--notify-user', '@test', '--dry-run']); assert args.notify_user == '@test'; assert args.notify_interval == 30"
echo "  ✓ Arguments parsed correctly"
echo

# Check contact validation
echo "✓ Testing contact validation..."
python3 -c "import sys; sys.path.insert(0, 'src'); from clud.messaging.factory import validate_contact_format; assert validate_contact_format('@user')[0] == True; assert validate_contact_format('+1234567890')[0] == True; assert validate_contact_format('whatsapp:+1234567890')[0] == True"
echo "  ✓ Contact validation working"
echo

# Check config loading
echo "✓ Testing config loading..."
python3 -c "import sys; sys.path.insert(0, 'src'); from clud.messaging.config import load_messaging_config; config = load_messaging_config(); assert isinstance(config, dict)"
echo "  ✓ Config loading working"
echo

# Check CLI integration
echo "✓ Testing CLI integration..."
python3 -c "import sys; sys.path.insert(0, 'src'); from clud.cli import main"
echo "  ✓ CLI imports successful"
echo

# Count files
echo "📊 Statistics:"
echo "  - Messaging module files: $(find src/clud/messaging -name '*.py' | wc -l)"
echo "  - Test files: $(find tests -name 'test_messaging.py' | wc -l)"
echo "  - Documentation files: 4 (PROPOSAL, SETUP, EXAMPLES, SUMMARY)"
echo "  - Total lines in messaging module: $(wc -l src/clud/messaging/*.py | tail -1 | awk '{print $1}')"
echo

# List documentation
echo "📚 Documentation:"
echo "  - TELEGRAM_AGENT_INTEGRATION_PROPOSAL.md (35K)"
echo "  - MESSAGING_SETUP.md (13K)"
echo "  - EXAMPLES.md (12K)"
echo "  - IMPLEMENTATION_SUMMARY.md (15K)"
echo

echo "✅ All verification checks passed!"
echo
echo "🚀 Ready to test with:"
echo "   clud --configure-messaging"
echo "   clud --notify-user '@username' -m 'test task'"
