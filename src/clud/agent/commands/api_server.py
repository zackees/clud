"""API server command handler for clud agent."""

import sys


def handle_api_server_command(port: int | None = None) -> int:
    """Handle the --api-server command by launching the Message Handler API server."""
    try:
        # Set default port if not provided
        if port is None:
            port = 8765

        print(f"Starting Message Handler API server on port {port}...")
        print(f"API will be available at http://localhost:{port}")
        print()
        print("Endpoints:")
        print(f"  - POST   http://localhost:{port}/api/message")
        print(f"  - GET    http://localhost:{port}/api/instances")
        print(f"  - GET    http://localhost:{port}/api/instances/{{id}}")
        print(f"  - DELETE http://localhost:{port}/api/instances/{{id}}")
        print(f"  - POST   http://localhost:{port}/api/cleanup")
        print(f"  - GET    http://localhost:{port}/health")
        print()
        print("Press Ctrl+C to stop the server")
        print()

        # Import uvicorn and run the server
        import uvicorn

        from clud.api.server import create_app

        app = create_app()
        uvicorn.run(app, host="127.0.0.1", port=port, log_level="info")
        return 0

    except ImportError as e:
        print(f"Error: Missing required dependency: {e}", file=sys.stderr)
        print("Install with: pip install fastapi uvicorn", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\n\nStopping API server...", file=sys.stderr)
        return 0
    except Exception as e:
        print(f"Error running API server: {e}", file=sys.stderr)
        return 1
