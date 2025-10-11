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

    // Initialize
    function init() {
        initTheme();
        initProjectSelector();
        initWebSocket();
        initEventListeners();
        loadHistory();
    }

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
    function initProjectSelector() {
        // Set current directory as default
        currentProject = window.location.pathname.replace(/\\/g, '/');
        projectSelector.innerHTML = `<option value="${currentProject}">Current Directory</option>`;
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
            project_path: currentProject || process.cwd()
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

    // Start the application
    init();
})();
