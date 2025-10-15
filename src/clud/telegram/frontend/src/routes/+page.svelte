<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import SessionList from '$lib/components/SessionList.svelte';
	import ChatView from '$lib/components/ChatView.svelte';
	import ConnectionStatus from '$lib/components/ConnectionStatus.svelte';
	import { appStore } from '$lib/stores/app.svelte';
	import { messagesStore } from '$lib/stores/messages.svelte';

	// Get WebSocket service
	const wsService = appStore.getWebSocketService();

	// Setup WebSocket callbacks
	onMount(() => {
		// Load sessions on mount
		appStore.loadSessions();

		// Setup message callback
		wsService.onMessage = (message) => {
			messagesStore.addMessage(message.session_id, message);
		};

		// Setup message history callback
		wsService.onMessages = (messages) => {
			if (appStore.selectedSessionId) {
				messagesStore.setMessages(appStore.selectedSessionId, messages);
			}
		};

		// Setup typing callback
		wsService.onTyping = (isTyping) => {
			messagesStore.setTyping(isTyping);
		};

		// Refresh sessions every 30 seconds
		const refreshInterval = setInterval(() => {
			appStore.loadSessions();
		}, 30000);

		return () => {
			clearInterval(refreshInterval);
		};
	});

	onDestroy(() => {
		// Disconnect WebSocket on component destroy
		wsService.disconnect();
	});

	function handleSelectSession(sessionId: string): void {
		appStore.selectSession(sessionId);
	}

	function handleDeleteSession(sessionId: string): void {
		appStore.deleteSession(sessionId);
	}

	function handleSendMessage(content: string): void {
		wsService.sendMessage(content);
	}

	function handleToggleTheme(): void {
		appStore.toggleTheme();
	}

	// Computed values
	$effect(() => {
		// Update document title
		if (appStore.selectedSession) {
			document.title = `@${appStore.selectedSession.telegram_username} - Claude Code Telegram`;
		} else {
			document.title = 'Claude Code - Telegram Dashboard';
		}
	});

	// Get messages for selected session
	const currentMessages = $derived(
		appStore.selectedSessionId ? messagesStore.getMessages(appStore.selectedSessionId) : []
	);
</script>

<div class="app">
	<header class="app-header">
		<div class="app-header-left">
			<h1 class="app-title">Claude Code</h1>
			<span class="app-subtitle">Telegram Dashboard</span>
		</div>

		<div class="app-header-right">
			<ConnectionStatus state={appStore.connectionState} />

			<button class="theme-toggle" onclick={handleToggleTheme} title="Toggle theme">
				{#if appStore.theme === 'light'}
					<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
						<path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
					</svg>
				{:else}
					<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
						<circle cx="12" cy="12" r="5" />
						<line x1="12" y1="1" x2="12" y2="3" />
						<line x1="12" y1="21" x2="12" y2="23" />
						<line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
						<line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
						<line x1="1" y1="12" x2="3" y2="12" />
						<line x1="21" y1="12" x2="23" y2="12" />
						<line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
						<line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
					</svg>
				{/if}
			</button>

			<a
				href="https://t.me/clud_ckl_bot"
				target="_blank"
				rel="noopener noreferrer"
				class="telegram-link"
				title="Open Telegram bot"
			>
				<svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
					<path
						d="M12 0C5.373 0 0 5.373 0 12s5.373 12 12 12 12-5.373 12-12S18.627 0 12 0zm5.562 8.161l-1.61 7.584c-.12.556-.437.694-.887.432l-2.453-1.806-1.182 1.138c-.131.131-.241.241-.494.241l.176-2.5 4.543-4.102c.197-.176-.043-.274-.307-.098l-5.616 3.535-2.42-.757c-.527-.165-.537-.527.11-.781l9.46-3.646c.439-.16.823.105.68.781z"
					/>
				</svg>
			</a>
		</div>
	</header>

	<main class="app-main">
		<aside class="app-sidebar">
			<SessionList
				sessions={appStore.sessions}
				selectedSessionId={appStore.selectedSessionId}
				onSelectSession={handleSelectSession}
				onDeleteSession={handleDeleteSession}
			/>
		</aside>

		<div class="app-content">
			<ChatView
				session={appStore.selectedSession}
				messages={currentMessages}
				isTyping={messagesStore.isTyping}
				onSendMessage={handleSendMessage}
			/>
		</div>
	</main>

	{#if appStore.error}
		<div class="app-error">
			<span>{appStore.error}</span>
			<button onclick={() => (appStore.error = null)}>Dismiss</button>
		</div>
	{/if}

	{#if appStore.isLoading}
		<div class="app-loading">
			<div class="spinner"></div>
		</div>
	{/if}
</div>

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100vh;
		overflow: hidden;
	}

	.app-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 1rem;
		background-color: var(--primary);
		color: white;
		border-bottom: 2px solid var(--primary-dark);
	}

	.app-header-left {
		display: flex;
		align-items: baseline;
		gap: 0.75rem;
	}

	.app-title {
		font-size: 1.5rem;
		font-weight: 700;
		margin: 0;
	}

	.app-subtitle {
		font-size: 0.875rem;
		opacity: 0.9;
	}

	.app-header-right {
		display: flex;
		align-items: center;
		gap: 1rem;
	}

	.theme-toggle,
	.telegram-link {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 40px;
		height: 40px;
		border-radius: 0.5rem;
		background-color: rgba(255, 255, 255, 0.1);
		color: white;
		transition: background-color 0.15s;
	}

	.theme-toggle:hover,
	.telegram-link:hover {
		background-color: rgba(255, 255, 255, 0.2);
	}

	.app-main {
		display: flex;
		flex: 1;
		overflow: hidden;
	}

	.app-sidebar {
		width: 320px;
		flex-shrink: 0;
		overflow: hidden;
	}

	.app-content {
		flex: 1;
		overflow: hidden;
	}

	.app-error {
		position: fixed;
		bottom: 1rem;
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		align-items: center;
		gap: 1rem;
		padding: 1rem 1.5rem;
		background-color: var(--error);
		color: white;
		border-radius: 0.5rem;
		box-shadow: 0 4px 12px var(--shadow-strong);
		z-index: 1000;
	}

	.app-error button {
		padding: 0.25rem 0.75rem;
		background-color: rgba(255, 255, 255, 0.2);
		color: white;
		border: none;
		border-radius: 0.25rem;
		font-size: 0.875rem;
		cursor: pointer;
		transition: background-color 0.15s;
	}

	.app-error button:hover {
		background-color: rgba(255, 255, 255, 0.3);
	}

	.app-loading {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		bottom: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		background-color: rgba(0, 0, 0, 0.5);
		z-index: 999;
	}

	.spinner {
		width: 40px;
		height: 40px;
		border: 4px solid rgba(255, 255, 255, 0.3);
		border-top-color: white;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	/* Responsive design */
	@media (max-width: 768px) {
		.app-header {
			padding: 0.75rem;
		}

		.app-title {
			font-size: 1.25rem;
		}

		.app-subtitle {
			display: none;
		}

		.app-main {
			flex-direction: column;
		}

		.app-sidebar {
			width: 100%;
			max-height: 40%;
			border-right: none;
			border-bottom: 1px solid var(--border);
		}

		.app-content {
			flex: 1;
		}
	}
</style>
