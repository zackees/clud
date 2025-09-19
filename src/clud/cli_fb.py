"""CLI trampoline for clud-fb executable."""

import sys

from clud.agent_foreground import main

if __name__ == "__main__":
    sys.exit(main())
