"""Code-server command handler for clud agent."""

import os
import socket
import subprocess
import sys
import threading
import time
import webbrowser


def handle_code_command(port: int | None = None) -> int:
    """Handle the --code command by launching code-server via npx."""

    def is_port_available(port: int) -> bool:
        """Check if a port is available for binding."""
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
                sock.bind(("localhost", port))
                return True
        except OSError:
            return False

    def find_available_port(start_port: int = 8080) -> int:
        """Find an available port starting from start_port."""
        for port_candidate in range(start_port, start_port + 100):
            if is_port_available(port_candidate):
                return port_candidate
        raise RuntimeError(f"No available ports found starting from {start_port}")

    # Find available port
    if port is None:
        port = find_available_port(8080)
    else:
        # User specified a port, check if it's available
        if not is_port_available(port):
            print(f"⚠️  Port {port} is not available, finding alternative...")
            port = find_available_port(port)

    # Get current working directory
    workspace = os.getcwd()

    print(f"🚀 Launching code-server on port {port}...")
    print(f"📁 Workspace: {workspace}")
    print()

    # Build npx command
    cmd = [
        "npx",
        "code-server",
        "--bind-addr",
        f"0.0.0.0:{port}",
        "--auth",
        "none",
        "--disable-telemetry",
        workspace,
    ]

    # Schedule browser opening
    def open_browser_delayed() -> None:
        time.sleep(3)
        url = f"http://localhost:{port}"
        print(f"\n🌐 Opening browser to {url}")
        try:
            webbrowser.open(url)
            print(f"✓ VS Code server is now accessible at {url}")
            print("\nPress Ctrl+C to stop the server")
        except Exception as e:
            print(f"Could not open browser automatically: {e}")
            print(f"Please open {url} in your browser")

    browser_thread = threading.Thread(target=open_browser_delayed, daemon=True)
    browser_thread.start()

    # Run code-server
    try:
        result = subprocess.run(cmd, check=False)
        return result.returncode
    except FileNotFoundError:
        print("Error: npx not found. Make sure Node.js is installed.", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\n\nStopping code-server...", file=sys.stderr)
        return 0
    except Exception as e:
        print(f"Error running code-server: {e}", file=sys.stderr)
        return 1
