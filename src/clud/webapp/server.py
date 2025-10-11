"""Minimal HTTP server for serving Telegram Web App static files."""

import json
import os
import sys
import threading
import time
import webbrowser
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path


class TelegramWebAppHandler(SimpleHTTPRequestHandler):
    """Custom handler that serves static files and provides API endpoints."""

    # Store the original CWD before changing to static dir
    original_cwd: str = ""

    def do_GET(self) -> None:
        """Handle GET requests - serve API or static files."""
        if self.path == "/api/info":
            # Serve info API endpoint
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()

            info = {"cwd": self.original_cwd, "status": "ready"}

            self.wfile.write(json.dumps(info).encode("utf-8"))
        else:
            # Serve static files
            super().do_GET()

    def log_message(self, format: str, *args: object) -> None:
        """Suppress log messages."""
        pass


def run_server() -> int:
    """Start simple HTTP server to serve webapp files.

    Automatically picks an available port.

    Returns:
        Exit code (0 for success)
    """
    # Store original CWD before changing directories
    original_cwd = os.getcwd()
    TelegramWebAppHandler.original_cwd = original_cwd

    # Change to webapp static directory
    webapp_dir = Path(__file__).parent / "static"
    if not webapp_dir.exists():
        print(f"Error: Static directory not found: {webapp_dir}", file=sys.stderr)
        return 1

    os.chdir(webapp_dir)

    # Create server with port 0 (auto-assign available port)
    server = HTTPServer(("localhost", 0), TelegramWebAppHandler)

    # Get the actual port that was assigned
    actual_port = server.server_address[1]
    url = f"http://localhost:{actual_port}"

    print(f"\nTelegram Web App server running at {url}")
    print("\nSetup Instructions:")
    print("1. Message @BotFather in Telegram")
    print("2. Choose your bot → 'Bot Settings' → 'Menu Button' → 'Configure menu button'")
    print(f"3. Enter URL: {url}")
    print("4. Open your bot in Telegram and click the menu button to launch the web app")
    print("\nNote: For mobile testing, you'll need to use a tunnel service like ngrok")
    print("      since localhost is not accessible from your phone.")
    print("\nPress Ctrl+C to stop\n")

    # Run server in separate daemon thread
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()

    # Auto-open browser after short delay
    def open_browser_delayed() -> None:
        time.sleep(2)
        print(f"\n🌐 Opening browser to {url}")
        try:
            webbrowser.open(url)
            print(f"✓ Telegram Web App is now accessible at {url}")
        except Exception as e:
            print(f"Could not open browser automatically: {e}")
            print(f"Please open {url} in your browser")

    browser_thread = threading.Thread(target=open_browser_delayed, daemon=True)
    browser_thread.start()

    try:
        # Main thread waits for keyboard interrupt
        while True:
            time.sleep(0.1)
    except KeyboardInterrupt:
        print("\n\nShutting down server...")
        server.shutdown()  # Safe - called from different thread
        server_thread.join(timeout=5)
    finally:
        server.server_close()  # Release the port

    print("Server stopped")
    return 0
