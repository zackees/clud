// Claude Code Web UI - Main Application
(function() {
    'use strict';

    // Configuration
    const RECONNECT_DELAY = 3000;
    const THEME_KEY = 'claude-code-theme';

    // State
    let ws = null;
    let reconnectTimer = null;
    let currentProject = null;
    let isProcessing = false;
    let currentAssistantMessage = null;

    // DOM Elements
    const chatContainer = document.getElementById('chat-container');
    const messageInput = document.getElementById('message-input');
    const sendBtn = document.getElementById('send-btn');
    const statusDiv = document.getElementById('status');
    const projectSelector = document.getElementById('project-selector');
    const themeToggle = document.getElementById('theme-toggle');
    const themeIcon = themeToggle.querySelector('.theme-icon');

    // Initialize function removed - now using async startApp() at the bottom

    // Theme Management
    function initTheme() {
        const savedTheme = localStorage.getItem(THEME_KEY) || 'light';
        setTheme(savedTheme);
    }

    function setTheme(theme) {
        document.documentElement.setAttribute('data-theme', theme);
        themeIcon.textContent = theme === 'dark' ? '☀️' : '🌙';
        localStorage.setItem(THEME_KEY, theme);
    }

    function toggleTheme() {
        const currentTheme = document.documentElement.getAttribute('data-theme');
        const newTheme = currentTheme === 'dark' ? 'light' : 'dark';
        setTheme(newTheme);
    }

    // Project Management
    async function initProjectSelector() {
        try {
            // Fetch current working directory from server
            const response = await fetch('/api/cwd');
            const data = await response.json();
            currentProject = data.cwd;

            // Display the directory name (last part of path)
            const dirName = currentProject.split(/[\\/]/).filter(Boolean).pop() || currentProject;
            projectSelector.innerHTML = `<option value="${currentProject}">${dirName}</option>`;
        } catch (error) {
            console.error('Error fetching current directory:', error);
            // Fallback: let server decide (will use its cwd)
            currentProject = null;
            projectSelector.innerHTML = `<option value="">Current Directory</option>`;
        }
    }

    function handleProjectChange() {
        currentProject = projectSelector.value;
        addSystemMessage(`Switched to project: ${currentProject}`);
        saveToHistory('system', `Switched to project: ${currentProject}`);
    }

    // WebSocket Management
    function initWebSocket() {
        // FastAPI serves WebSocket on same port as HTTP, at /ws endpoint
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const port = window.location.port ? `:${window.location.port}` : '';
        const wsUrl = `${protocol}//${window.location.hostname}${port}/ws`;

        updateStatus('Connecting...', 'connecting');

        try {
            ws = new WebSocket(wsUrl);

            ws.onopen = handleWSOpen;
            ws.onmessage = handleWSMessage;
            ws.onclose = handleWSClose;
            ws.onerror = handleWSError;
        } catch (error) {
            console.error('WebSocket connection error:', error);
            updateStatus('Connection failed', 'error');
            scheduleReconnect();
        }
    }

    function handleWSOpen() {
        console.log('WebSocket connected');
        updateStatus('Connected', 'connected');
        messageInput.disabled = false;
        sendBtn.disabled = false;
        messageInput.focus();

        // Clear reconnect timer
        if (reconnectTimer) {
            clearTimeout(reconnectTimer);
            reconnectTimer = null;
        }
    }

    function handleWSMessage(event) {
        try {
            const data = JSON.parse(event.data);

            switch (data.type) {
                case 'ack':
                    updateStatus('Claude is thinking...', 'processing');
                    isProcessing = true;
                    break;

                case 'chunk':
                    handleChunk(data.content);
                    break;

                case 'done':
                    handleDone();
                    break;

                case 'error':
                    handleError(data.error);
                    break;

                case 'pong':
                    // Heartbeat response
                    break;

                default:
                    console.warn('Unknown message type:', data.type);
            }
        } catch (error) {
            console.error('Error parsing WebSocket message:', error);
        }
    }

    function handleWSClose() {
        console.log('WebSocket disconnected');
        updateStatus('Disconnected', 'error');
        messageInput.disabled = true;
        sendBtn.disabled = true;
        scheduleReconnect();
    }

    function handleWSError(error) {
        console.error('WebSocket error:', error);
        updateStatus('Connection error', 'error');
    }

    function scheduleReconnect() {
        if (reconnectTimer) return;

        reconnectTimer = setTimeout(() => {
            console.log('Attempting to reconnect...');
            initWebSocket();
        }, RECONNECT_DELAY);
    }

    function sendMessage(message) {
        if (!ws || ws.readyState !== WebSocket.OPEN) {
            addSystemMessage('Error: Not connected to server');
            return;
        }

        const payload = {
            type: 'chat',
            message: message,
            project_path: currentProject || null
        };

        ws.send(JSON.stringify(payload));
    }

    // Message Handling
    function handleChunk(content) {
        if (!currentAssistantMessage) {
            currentAssistantMessage = createMessage('assistant', '');
        }

        const contentDiv = currentAssistantMessage.querySelector('.message-content');
        const currentText = contentDiv.textContent;
        contentDiv.textContent = currentText + content;

        // Auto-scroll
        scrollToBottom();
    }

    function handleDone() {
        if (currentAssistantMessage) {
            const contentDiv = currentAssistantMessage.querySelector('.message-content');
            const text = contentDiv.textContent;

            // Process markdown-style code blocks
            contentDiv.innerHTML = formatMessage(text);

            saveToHistory('assistant', text);
            currentAssistantMessage = null;
        }

        isProcessing = false;
        updateStatus('Connected', 'connected');
        messageInput.disabled = false;
        sendBtn.disabled = false;
        messageInput.focus();
    }

    function handleError(error) {
        addSystemMessage(`Error: ${error}`, 'error');
        isProcessing = false;
        updateStatus('Connected', 'connected');
        messageInput.disabled = false;
        sendBtn.disabled = false;
    }

    // UI Functions
    function createMessage(role, content) {
        const messageDiv = document.createElement('div');
        messageDiv.className = `message ${role}`;

        const header = document.createElement('div');
        header.className = 'message-header';
        header.textContent = role === 'user' ? 'You' : role === 'assistant' ? 'Claude' : 'System';

        const contentDiv = document.createElement('div');
        contentDiv.className = 'message-content';
        contentDiv.innerHTML = formatMessage(content);

        messageDiv.appendChild(header);
        messageDiv.appendChild(contentDiv);

        // Remove welcome message if present
        const welcomeMsg = chatContainer.querySelector('.welcome-message');
        if (welcomeMsg) {
            welcomeMsg.remove();
        }

        chatContainer.appendChild(messageDiv);
        scrollToBottom();

        return messageDiv;
    }

    function addUserMessage(text) {
        createMessage('user', text);
        saveToHistory('user', text);
    }

    function addSystemMessage(text, type = 'info') {
        const messageDiv = document.createElement('div');
        messageDiv.className = `message system ${type}`;
        messageDiv.innerHTML = `<div class="message-content">${escapeHtml(text)}</div>`;
        chatContainer.appendChild(messageDiv);
        scrollToBottom();
    }

    function formatMessage(text) {
        // Escape HTML
        text = escapeHtml(text);

        // Format code blocks (```language\ncode\n```)
        text = text.replace(/```(\w+)?\n([\s\S]*?)```/g, (match, lang, code) => {
            return `<pre><code>${code.trim()}</code></pre>`;
        });

        // Format inline code (`code`)
        text = text.replace(/`([^`]+)`/g, '<code>$1</code>');

        // Format paragraphs
        const paragraphs = text.split('\n\n');
        text = paragraphs.map(p => p.trim() ? `<p>${p.replace(/\n/g, '<br>')}</p>` : '').join('');

        return text;
    }

    function escapeHtml(text) {
        const div = document.createElement('div');
        div.textContent = text;
        return div.innerHTML;
    }

    function updateStatus(text, state) {
        statusDiv.textContent = text;
        statusDiv.className = `status ${state}`;
    }

    function scrollToBottom() {
        chatContainer.scrollTop = chatContainer.scrollHeight;
    }

    // Event Listeners
    function initEventListeners() {
        // Send button
        sendBtn.addEventListener('click', handleSend);

        // Enter key (Ctrl+Enter or just Enter)
        messageInput.addEventListener('keydown', (e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                handleSend();
            }

            // Auto-resize textarea
            messageInput.style.height = 'auto';
            messageInput.style.height = messageInput.scrollHeight + 'px';
        });

        // Project selector
        projectSelector.addEventListener('change', handleProjectChange);

        // Theme toggle
        themeToggle.addEventListener('click', toggleTheme);

        // Keep WebSocket alive with periodic pings
        setInterval(() => {
            if (ws && ws.readyState === WebSocket.OPEN) {
                ws.send(JSON.stringify({ type: 'ping' }));
            }
        }, 30000);  // Every 30 seconds
    }

    function handleSend() {
        const message = messageInput.value.trim();
        if (!message || isProcessing) return;

        // Add user message to UI
        addUserMessage(message);

        // Clear input
        messageInput.value = '';
        messageInput.style.height = 'auto';

        // Disable input while processing
        messageInput.disabled = true;
        sendBtn.disabled = true;

        // Send to server
        sendMessage(message);
    }

    // History Management (localStorage)
    function saveToHistory(role, content) {
        try {
            const history = JSON.parse(localStorage.getItem('chat-history') || '[]');
            history.push({
                role,
                content,
                timestamp: Date.now()
            });

            // Keep only last 100 messages
            if (history.length > 100) {
                history.splice(0, history.length - 100);
            }

            localStorage.setItem('chat-history', JSON.stringify(history));
        } catch (error) {
            console.error('Error saving history:', error);
        }
    }

    function loadHistory() {
        try {
            const history = JSON.parse(localStorage.getItem('chat-history') || '[]');

            // Load last 10 messages
            const recentHistory = history.slice(-10);
            recentHistory.forEach(msg => {
                if (msg.role !== 'system') {
                    createMessage(msg.role, msg.content);
                }
            });
        } catch (error) {
            console.error('Error loading history:', error);
        }
    }

    // Terminal Manager Class
    class TerminalManager {
        constructor() {
            this.terminals = new Map(); // terminalId -> {xterm, socket, fitAddon, wrapper}
            this.activeTerminalId = null;
            this.nextTerminalId = 0;
            this.container = document.getElementById('terminal-container');
            this.tabsContainer = document.getElementById('terminal-tabs');

            this.initResizeHandle();
            this.initEventListeners();
            this.createTerminal(); // Create initial terminal
        }

        createTerminal() {
            const terminalId = this.nextTerminalId++;

            // Create xterm.js instance
            const term = new Terminal({
                cursorBlink: true,
                fontSize: 14,
                fontFamily: 'Consolas, Monaco, "Courier New", monospace',
                theme: {
                    background: '#1e1e1e',
                    foreground: '#d4d4d4',
                    cursor: '#aeafad',
                    black: '#000000',
                    red: '#cd3131',
                    green: '#0dbc79',
                    yellow: '#e5e510',
                    blue: '#2472c8',
                    magenta: '#bc3fbc',
                    cyan: '#11a8cd',
                    white: '#e5e5e5',
                },
                cols: 80,
                rows: 24,
            });

            // Fit addon for responsive sizing
            const fitAddon = new FitAddon.FitAddon();
            term.loadAddon(fitAddon);

            // Create wrapper element
            const wrapper = document.createElement('div');
            wrapper.id = `terminal-${terminalId}`;
            wrapper.className = 'xterm-wrapper';
            this.container.appendChild(wrapper);

            // Open terminal in wrapper
            term.open(wrapper);
            fitAddon.fit();

            // Connect to WebSocket
            const socket = this.connectTerminalWebSocket(terminalId, term);

            // Store terminal info
            this.terminals.set(terminalId, {
                term,
                socket,
                fitAddon,
                wrapper,
                cwd: null,
            });

            // Create tab
            this.createTab(terminalId);

            // Set as active
            this.setActiveTerminal(terminalId);

            // Handle terminal input
            term.onData(data => {
                if (socket.readyState === WebSocket.OPEN) {
                    socket.send(JSON.stringify({
                        type: 'input',
                        data: data,
                    }));
                }
            });

            // Handle resize
            const resizeObserver = new ResizeObserver(() => {
                fitAddon.fit();
                const dims = { cols: term.cols, rows: term.rows };
                if (socket.readyState === WebSocket.OPEN) {
                    socket.send(JSON.stringify({
                        type: 'resize',
                        ...dims,
                    }));
                }
            });
            resizeObserver.observe(wrapper);

            return terminalId;
        }

        connectTerminalWebSocket(terminalId, term) {
            const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
            const socket = new WebSocket(
                `${wsProtocol}//${window.location.host}/ws/term?id=${terminalId}`
            );

            socket.onopen = () => {
                console.log(`Terminal ${terminalId} WebSocket connected`);
                // Send initial dimensions and project path
                // Use currentProject variable which was loaded by initProjectSelector
                const projectPath = currentProject || '';
                console.log(`Terminal ${terminalId} starting in directory: ${projectPath}`);
                socket.send(JSON.stringify({
                    type: 'init',
                    cols: term.cols,
                    rows: term.rows,
                    cwd: projectPath,
                }));
            };

            socket.onmessage = (event) => {
                const data = JSON.parse(event.data);

                if (data.type === 'output') {
                    term.write(data.data);
                } else if (data.type === 'exit') {
                    term.write(`\r\n\n[Process exited with code ${data.code}]\r\n`);
                } else if (data.type === 'error') {
                    term.write(`\r\n[Error: ${data.error}]\r\n`);
                }
            };

            socket.onerror = (error) => {
                console.error(`Terminal ${terminalId} WebSocket error:`, error);
                term.write('\r\n[Connection error]\r\n');
            };

            socket.onclose = () => {
                console.log(`Terminal ${terminalId} WebSocket closed`);
                term.write('\r\n[Connection closed]\r\n');
            };

            return socket;
        }

        createTab(terminalId) {
            const tab = document.createElement('button');
            tab.className = 'terminal-tab';
            tab.dataset.terminalId = terminalId;
            tab.innerHTML = `Terminal ${terminalId + 1} <span class="tab-close">&times;</span>`;

            tab.addEventListener('click', (e) => {
                if (e.target.classList.contains('tab-close')) {
                    this.closeTerminal(terminalId);
                } else {
                    this.setActiveTerminal(terminalId);
                }
            });

            // Insert before the "+" button
            const addButton = document.getElementById('new-terminal-btn');
            this.tabsContainer.insertBefore(tab, addButton);
        }

        setActiveTerminal(terminalId) {
            // Hide all terminals
            this.terminals.forEach((term, id) => {
                term.wrapper.classList.remove('active');
                document.querySelector(`[data-terminal-id="${id}"]`)?.classList.remove('active');
            });

            // Show active terminal
            const terminal = this.terminals.get(terminalId);
            if (terminal) {
                terminal.wrapper.classList.add('active');
                document.querySelector(`[data-terminal-id="${terminalId}"]`)?.classList.add('active');
                this.activeTerminalId = terminalId;
                terminal.term.focus();
                terminal.fitAddon.fit();
            }
        }

        closeTerminal(terminalId) {
            const terminal = this.terminals.get(terminalId);
            if (!terminal) return;

            // Close socket
            terminal.socket.close();

            // Dispose terminal
            terminal.term.dispose();

            // Remove wrapper
            terminal.wrapper.remove();

            // Remove tab
            document.querySelector(`[data-terminal-id="${terminalId}"]`)?.remove();

            // Remove from map
            this.terminals.delete(terminalId);

            // Switch to another terminal if this was active
            if (this.activeTerminalId === terminalId) {
                const remainingIds = Array.from(this.terminals.keys());
                if (remainingIds.length > 0) {
                    this.setActiveTerminal(remainingIds[0]);
                } else {
                    // Create a new terminal if none remain
                    this.createTerminal();
                }
            }
        }

        initResizeHandle() {
            const handle = document.getElementById('resize-handle');
            const chatPanel = document.getElementById('chat-panel');
            const termPanel = document.getElementById('terminal-panel');

            let isResizing = false;

            handle.addEventListener('mousedown', (e) => {
                isResizing = true;
                e.preventDefault();
            });

            document.addEventListener('mousemove', (e) => {
                if (!isResizing) return;

                const containerRect = document.querySelector('.main-content').getBoundingClientRect();
                const offsetX = e.clientX - containerRect.left;
                const percentage = (offsetX / containerRect.width) * 100;

                if (percentage > 20 && percentage < 80) {
                    chatPanel.style.flex = `0 0 ${percentage}%`;
                    termPanel.style.flex = `0 0 ${100 - percentage}%`;

                    // Trigger resize for active terminal
                    if (this.activeTerminalId !== null) {
                        const terminal = this.terminals.get(this.activeTerminalId);
                        if (terminal) {
                            setTimeout(() => terminal.fitAddon.fit(), 10);
                        }
                    }
                }
            });

            document.addEventListener('mouseup', () => {
                isResizing = false;
            });
        }

        initEventListeners() {
            // New terminal button
            document.getElementById('new-terminal-btn').addEventListener('click', () => {
                this.createTerminal();
            });

            // Clear terminal
            document.getElementById('terminal-clear').addEventListener('click', () => {
                if (this.activeTerminalId !== null) {
                    const terminal = this.terminals.get(this.activeTerminalId);
                    terminal?.term.clear();
                }
            });

            // Toggle terminal panel
            document.getElementById('terminal-toggle').addEventListener('click', () => {
                const panel = document.getElementById('terminal-panel');
                panel.classList.toggle('collapsed');
            });
        }
    }

    // Initialize terminal manager (will be created after project selector is ready)
    let terminalManager;

    // Modified init to create terminal after project is loaded
    async function startApp() {
        initTheme();
        await initProjectSelector();  // Wait for project to load
        initWebSocket();
        initEventListeners();
        loadHistory();

        // Now create terminal with proper project path
        if (typeof Terminal !== 'undefined' && typeof FitAddon !== 'undefined') {
            terminalManager = new TerminalManager();
        } else {
            console.error('xterm.js or FitAddon not loaded');
        }
    }

    // Start the application
    startApp();
})();
