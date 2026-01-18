"""HTML template for multi-terminal Playwright daemon.

Generates an HTML page with xterm.js terminals in a grid layout, each connected
to a WebSocket for PTY communication.
"""

from __future__ import annotations


def get_html_template(port: int, num_terminals: int = 8) -> str:
    """Generate HTML template for multi-terminal grid layout.

    Args:
        port: WebSocket server port
        num_terminals: Number of terminals to create (default 8)

    Returns:
        HTML string with xterm.js terminals and WebSocket connections
    """
    # Calculate grid dimensions (2 columns, dynamic rows)
    cols = 2
    rows = (num_terminals + cols - 1) // cols

    # Generate terminal divs
    terminal_divs = "\n".join(f'        <div id="terminal-{i}" class="terminal"></div>' for i in range(num_terminals))

    # Generate terminal initialization JS
    terminal_init_js = "\n".join(
        f"""
            // Terminal {i}
            const term{i} = new Terminal({{
                cursorBlink: true,
                fontSize: 14,
                fontFamily: 'Consolas, "Courier New", monospace',
                theme: {{
                    background: '#1e1e1e',
                    foreground: '#d4d4d4',
                    cursor: '#d4d4d4',
                    cursorAccent: '#1e1e1e',
                    selection: 'rgba(255, 255, 255, 0.3)',
                    black: '#000000',
                    red: '#cd3131',
                    green: '#0dbc79',
                    yellow: '#e5e510',
                    blue: '#2472c8',
                    magenta: '#bc3fbc',
                    cyan: '#11a8cd',
                    white: '#e5e5e5',
                    brightBlack: '#666666',
                    brightRed: '#f14c4c',
                    brightGreen: '#23d18b',
                    brightYellow: '#f5f543',
                    brightBlue: '#3b8eea',
                    brightMagenta: '#d670d6',
                    brightCyan: '#29b8db',
                    brightWhite: '#e5e5e5'
                }}
            }});
            const fitAddon{i} = new FitAddon.FitAddon();
            const webLinksAddon{i} = new WebLinksAddon.WebLinksAddon();
            term{i}.loadAddon(fitAddon{i});
            term{i}.loadAddon(webLinksAddon{i});
            term{i}.open(document.getElementById('terminal-{i}'));
            fitAddon{i}.fit();
            terminals.push({{ term: term{i}, fitAddon: fitAddon{i}, ws: null }});"""
        for i in range(num_terminals)
    )

    # Generate WebSocket connection JS
    websocket_init_js = "\n".join(
        f"""
            // WebSocket for Terminal {i}
            const ws{i} = new WebSocket('ws://localhost:{port}/ws/{i}');
            ws{i}.binaryType = 'arraybuffer';
            ws{i}.onopen = () => {{
                console.log('Terminal {i} connected');
                terminals[{i}].ws = ws{i};
                // Send initial resize
                const dims{i} = {{ cols: terminals[{i}].term.cols, rows: terminals[{i}].term.rows }};
                ws{i}.send(JSON.stringify({{ type: 'resize', ...dims{i} }}));
            }};
            ws{i}.onmessage = (event) => {{
                if (event.data instanceof ArrayBuffer) {{
                    terminals[{i}].term.write(new Uint8Array(event.data));
                }} else {{
                    terminals[{i}].term.write(event.data);
                }}
            }};
            ws{i}.onclose = () => {{
                console.log('Terminal {i} disconnected');
                terminals[{i}].term.write('\\r\\n[Connection closed]\\r\\n');
            }};
            ws{i}.onerror = (err) => {{
                console.error('Terminal {i} error:', err);
            }};
            terminals[{i}].term.onData((data) => {{
                if (ws{i}.readyState === WebSocket.OPEN) {{
                    ws{i}.send(data);
                }}
            }});
            terminals[{i}].term.onResize(({{ cols, rows }}) => {{
                if (ws{i}.readyState === WebSocket.OPEN) {{
                    ws{i}.send(JSON.stringify({{ type: 'resize', cols, rows }}));
                }}
            }});"""
        for i in range(num_terminals)
    )

    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>CLUD Multi-Terminal</title>
    <!-- xterm.js v5.3.0 -->
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/xterm@5.3.0/css/xterm.css">
    <script src="https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.js"></script>
    <script src="https://cdn.jsdelivr.net/npm/xterm-addon-fit@0.8.0/lib/xterm-addon-fit.js"></script>
    <script src="https://cdn.jsdelivr.net/npm/xterm-addon-web-links@0.9.0/lib/xterm-addon-web-links.js"></script>
    <style>
        * {{
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }}
        html, body {{
            height: 100%;
            width: 100%;
            background: #1e1e1e;
            overflow: hidden;
        }}
        .container {{
            display: grid;
            grid-template-columns: repeat({cols}, 1fr);
            grid-template-rows: repeat({rows}, 1fr);
            gap: 4px;
            padding: 4px;
            height: 100vh;
            width: 100vw;
        }}
        .terminal {{
            background: #1e1e1e;
            border: 1px solid #333;
            border-radius: 4px;
            overflow: hidden;
        }}
        .xterm {{
            height: 100%;
            width: 100%;
            padding: 4px;
        }}
        .xterm-viewport {{
            overflow-y: auto !important;
        }}
    </style>
</head>
<body>
    <div class="container">
{terminal_divs}
    </div>
    <script>
        const terminals = [];

        document.addEventListener('DOMContentLoaded', () => {{
            // Initialize terminals
{terminal_init_js}

            // Connect WebSockets
{websocket_init_js}

            // Handle window resize
            let resizeTimeout;
            window.addEventListener('resize', () => {{
                clearTimeout(resizeTimeout);
                resizeTimeout = setTimeout(() => {{
                    terminals.forEach((t, idx) => {{
                        t.fitAddon.fit();
                        if (t.ws && t.ws.readyState === WebSocket.OPEN) {{
                            t.ws.send(JSON.stringify({{
                                type: 'resize',
                                cols: t.term.cols,
                                rows: t.term.rows
                            }}));
                        }}
                    }});
                }}, 100);
            }});
        }});
    </script>
</body>
</html>"""

    return html


def get_minimal_html_template(port: int, num_terminals: int = 8) -> str:
    """Generate minimal HTML template for testing purposes.

    A simplified version that doesn't require CDN access.

    Args:
        port: WebSocket server port
        num_terminals: Number of terminals to create (default 8)

    Returns:
        Minimal HTML string for testing
    """
    terminal_divs = "\n".join(f'        <div id="terminal-{i}" class="terminal">Terminal {i} (ws://localhost:{port}/ws/{i})</div>' for i in range(num_terminals))

    return f"""<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <title>CLUD Multi-Terminal (Minimal)</title>
    <style>
        body {{ background: #1e1e1e; color: #d4d4d4; font-family: monospace; margin: 0; }}
        .container {{ display: grid; grid-template-columns: repeat(2, 1fr); gap: 4px; padding: 4px; height: 100vh; }}
        .terminal {{ background: #2d2d2d; border: 1px solid #444; padding: 8px; }}
    </style>
</head>
<body>
    <div class="container">
{terminal_divs}
    </div>
</body>
</html>"""
