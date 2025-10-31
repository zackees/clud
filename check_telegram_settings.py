#!/usr/bin/env python3
"""Check where Telegram credentials are stored and what they contain."""

import sys
from pathlib import Path

# Add src to path to import modules
sys.path.insert(0, str(Path(__file__).parent / "src"))

from clud.secrets import get_credential_store

TELEGRAM_SERVICE = "clud-telegram"
BOT_TOKEN_KEY = "bot-token"
CHAT_ID_KEY = "chat-id"


def check_telegram_credentials() -> None:
    """Check Telegram credentials and where they're stored."""
    credential_store = get_credential_store()

    if credential_store is None:
        print("ERROR: No credential store available")
        sys.exit(1)

    print(f"Credential store type: {type(credential_store).__name__}")
    print()

    # Check storage location based on type
    if hasattr(credential_store, 'config_dir'):
        print(f"Storage location: {credential_store.config_dir}")
        print(f"  Key file: {credential_store.key_file}")
        print(f"  Creds file: {credential_store.creds_file}")
        print(f"  Creds file exists: {credential_store.creds_file.exists()}")
        print()

    # Get credentials
    try:
        bot_token = credential_store.get_password(TELEGRAM_SERVICE, BOT_TOKEN_KEY)
        chat_id = credential_store.get_password(TELEGRAM_SERVICE, CHAT_ID_KEY)

        print("Stored credentials:")
        if bot_token:
            # Mask the token for security
            if len(bot_token) > 20:
                masked_token = bot_token[:10] + "..." + bot_token[-10:]
            else:
                masked_token = bot_token[:5] + "..." if len(bot_token) > 5 else "***"
            print(f"  Bot token: {masked_token}")
            print(f"  Token length: {len(bot_token)}")
            print(f"  Token is empty string: {bot_token == ''}")

            # Validate token format
            if ":" in bot_token:
                parts = bot_token.split(":")
                print(f"  Token format looks valid: {parts[0].isdigit()} (bot_id is numeric)")
            else:
                print("  Token format looks INVALID (missing ':')")
        else:
            print("  Bot token: None")

        if chat_id:
            print(f"  Chat ID: {chat_id}")
        else:
            print("  Chat ID: None")

    except Exception as e:
        print(f"ERROR: Failed to retrieve credentials: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    check_telegram_credentials()
