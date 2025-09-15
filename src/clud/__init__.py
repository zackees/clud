"""Claude in a Docker Box package."""

import os
import sys

# Set UTF-8 encoding for stdin, stdout, and stderr
if sys.platform == "win32":
    # On Windows, ensure UTF-8 encoding for all streams
    try:
        sys.stdin.reconfigure(encoding='utf-8')
        sys.stdout.reconfigure(encoding='utf-8')
        sys.stderr.reconfigure(encoding='utf-8')
    except AttributeError:
        # Python < 3.7 fallback
        import codecs
        sys.stdin = codecs.getreader('utf-8')(sys.stdin.detach())
        sys.stdout = codecs.getwriter('utf-8')(sys.stdout.detach())
        sys.stderr = codecs.getwriter('utf-8')(sys.stderr.detach())

# Set environment variable for subprocess UTF-8 handling
os.environ.setdefault('PYTHONIOENCODING', 'utf-8')