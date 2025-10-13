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
        elif self.path == "/" or self.path == "/index.html":
            # Serve index.html with cache-busting headers
            try:
                index_path = Path("index.html")
                if index_path.exists():
                    with open(index_path, "rb") as f:
                        content = f.read()

                    self.send_response(200)
                    self.send_header("Content-Type", "text/html; charset=utf-8")
                    self.send_header("Cache-Control", "no-cache, no-store, must-revalidate, max-age=0")
                    self.send_header("Pragma", "no-cache")
                    self.send_header("Expires", "0")
                    self.end_headers()
                    self.wfile.write(content)
                else:
                    self.send_error(404, "File not found")
            except Exception as e:
                self.send_error(500, f"Server error: {e}")
        else:
            # Serve other static files normally
            super().do_GET()

    def do_POST(self) -> None:
        """Handle POST requests - save chat ID."""
        if self.path == "/api/save-chat-id":
            try:
                # Read request body
                content_length = int(self.headers.get("Content-Length", 0))
                body = self.rfile.read(content_length).decode("utf-8")
                data = json.loads(body)

                chat_id = data.get("chat_id", "").strip()
                if not chat_id:
                    self.send_response(400)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Access-Control-Allow-Origin", "*")
                    self.end_headers()
                    response = {"status": "error", "message": "chat_id is required"}
                    self.wfile.write(json.dumps(response).encode("utf-8"))
                    return

                # Import here to avoid circular dependencies
                from ..agent.foreground import load_telegram_credentials, save_telegram_credentials

                # Load existing bot token
                bot_token, _ = load_telegram_credentials()
                if not bot_token:
                    self.send_response(400)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Access-Control-Allow-Origin", "*")
                    self.end_headers()
                    response = {"status": "error", "message": "Bot token not found. Please save bot token first."}
                    self.wfile.write(json.dumps(response).encode("utf-8"))
                    return

                # Save with the detected chat_id
                save_telegram_credentials(bot_token, chat_id)

                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                response = {"status": "ok", "message": "Chat ID saved successfully"}
                self.wfile.write(json.dumps(response).encode("utf-8"))

            except Exception as e:
                self.send_response(500)
                self.send_header("Content-Type", "application/json")
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                response = {"status": "error", "message": str(e)}
                self.wfile.write(json.dumps(response).encode("utf-8"))
        else:
            # Unknown POST endpoint
            self.send_response(404)
            self.end_headers()

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
