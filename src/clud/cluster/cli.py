"""
CLI entry point for CLUD-CLUSTER.

Provides commands:
- serve: Run the FastAPI server
- migrate: Run database migrations
- bot: Run the Telegram bot (requires bot extra)
"""

import argparse
import logging
import sys


def serve(args: argparse.Namespace) -> None:
    """Start the CLUD-CLUSTER server."""
    import uvicorn

    from .config import settings

    # Override settings from CLI args
    if args.host:
        settings.host = args.host
    if args.port:
        settings.port = args.port
    if args.reload:
        settings.reload = args.reload

    logging.info(f"Starting CLUD-CLUSTER server on {settings.host}:{settings.port}")

    uvicorn.run(
        "clud.cluster.app:app",
        host=settings.host,
        port=settings.port,
        reload=settings.reload,
        log_level=settings.log_level.lower(),
    )


def migrate(args: argparse.Namespace) -> int:
    """Run database migrations."""
    print("Database migrations not yet implemented")
    print("Currently using automatic table creation on startup")
    return 0


def bot(args: argparse.Namespace) -> int:
    """Run the Telegram bot."""
    print("Telegram bot not yet implemented")
    return 1


def main() -> int:
    """Main entry point for clud-cluster CLI."""
    parser = argparse.ArgumentParser(description="CLUD-CLUSTER - Cluster control plane for clud agents")
    subparsers = parser.add_subparsers(dest="command", help="Command to run")

    # Serve command
    serve_parser = subparsers.add_parser("serve", help="Start the CLUD-CLUSTER server")
    serve_parser.add_argument("--host", type=str, help="Host to bind to")
    serve_parser.add_argument("--port", type=int, help="Port to bind to")
    serve_parser.add_argument("--reload", action="store_true", help="Enable auto-reload")

    # Migrate command
    _migrate_parser = subparsers.add_parser("migrate", help="Run database migrations")

    # Bot command
    _bot_parser = subparsers.add_parser("bot", help="Run Telegram bot")

    args = parser.parse_args()

    if args.command == "serve":
        serve(args)
        return 0
    elif args.command == "migrate":
        return migrate(args)
    elif args.command == "bot":
        return bot(args)
    else:
        parser.print_help()
        return 1


if __name__ == "__main__":
    sys.exit(main())
