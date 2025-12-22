"""Entry point for running the cron daemon as a module (python -m clud.cron)."""

import sys


def main() -> None:
    """Main entry point for the cron daemon module."""
    from clud.cron.daemon import CronDaemon

    if len(sys.argv) > 1 and sys.argv[1] == "run":
        daemon = CronDaemon()
        daemon.run_loop()
    else:
        print("Usage: python -m clud.cron run")
        sys.exit(1)


if __name__ == "__main__":
    main()
