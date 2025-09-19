"""CLI trampoline for clud-bg executable."""

import sys

from clud.agent_background import main

if __name__ == "__main__":
    sys.exit(main())
