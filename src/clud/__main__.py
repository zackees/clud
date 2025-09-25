"""Allow running clud as a module with python -m clud."""

from .cli import main

if __name__ == "__main__":
    import sys

    sys.exit(main())
