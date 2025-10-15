<script lang="ts">
	import { onMount } from 'svelte';
	import { activeTab, switchToTab, themeIcon, toggleTheme, currentProject } from '$lib/stores/app';
	import type { ActiveTab } from '$lib/types';
	import Chat from '$lib/components/Chat.svelte';
	import Terminal from '$lib/components/Terminal.svelte';
	import DiffViewer from '$lib/components/DiffViewer.svelte';
	import History from '$lib/components/History.svelte';
	import Settings from '$lib/components/Settings.svelte';

	let projectPath = '';

	onMount(async () => {
		// Fetch current working directory from server
		try {
			const response = await fetch('/api/cwd');
			const data = await response.json();
			projectPath = data.cwd;
			// Update store
			currentProject.set(projectPath);
		} catch (error) {
			console.error('Error fetching current directory:', error);
			projectPath = 'Current Directory';
		}
	});

	function handleTabClick(tab: ActiveTab): void {
		switchToTab(tab);
	}

	function getProjectDisplayName(path: string): string {
		if (!path) return 'Loading...';
		const parts = path.split(/[\\/]/).filter(Boolean);
		return parts[parts.length - 1] || path;
	}
</script>

<div class="app-container">
	<!-- Header -->
	<header class="app-header">
		<div class="header-left">
			<h1 class="app-title">Claude Code</h1>
			<div class="project-selector">
				<span class="project-label">Project:</span>
				<span class="project-name" title={projectPath}>{getProjectDisplayName(projectPath)}</span>
			</div>
		</div>
		<div class="header-right">
			<button class="theme-toggle" on:click={toggleTheme} title="Toggle Theme">
				<span class="theme-icon">{$themeIcon}</span>
			</button>
		</div>
	</header>

	<!-- Tab Navigation -->
	<nav class="tab-nav">
		<button
			class="tab-button"
			class:active={$activeTab === 'chat'}
			on:click={() => handleTabClick('chat')}
		>
			Chat
		</button>
		<button
			class="tab-button"
			class:active={$activeTab === 'terminal'}
			on:click={() => handleTabClick('terminal')}
		>
			Terminal
		</button>
		<button
			class="tab-button"
			class:active={$activeTab === 'diff'}
			on:click={() => handleTabClick('diff')}
		>
			Diff
		</button>
		<button
			class="tab-button"
			class:active={$activeTab === 'history'}
			on:click={() => handleTabClick('history')}
		>
			History
		</button>
		<button
			class="tab-button"
			class:active={$activeTab === 'settings'}
			on:click={() => handleTabClick('settings')}
		>
			Settings
		</button>
	</nav>

	<!-- Tab Content -->
	<main class="tab-content">
		<div class="tab-panel" class:active={$activeTab === 'chat'}>
			<Chat />
		</div>
		<div class="tab-panel" class:active={$activeTab === 'terminal'}>
			<Terminal />
		</div>
		<div class="tab-panel" class:active={$activeTab === 'diff'}>
			<DiffViewer />
		</div>
		<div class="tab-panel" class:active={$activeTab === 'history'}>
			<History />
		</div>
		<div class="tab-panel" class:active={$activeTab === 'settings'}>
			<Settings />
		</div>
	</main>
</div>

<style>
	.app-container {
		display: flex;
		flex-direction: column;
		height: 100vh;
		width: 100vw;
		overflow: hidden;
		background: var(--bg-color);
	}

	/* Header */
	.app-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 12px 24px;
		background: var(--surface-color);
		border-bottom: 1px solid var(--border-color);
		flex-shrink: 0;
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: 24px;
	}

	.app-title {
		margin: 0;
		font-size: 20px;
		font-weight: 600;
		color: var(--text-color);
	}

	.project-selector {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 12px;
		background: var(--bg-color);
		border: 1px solid var(--border-color);
		border-radius: 4px;
	}

	.project-label {
		font-size: 12px;
		font-weight: 500;
		color: var(--text-secondary);
	}

	.project-name {
		font-size: 14px;
		font-weight: 500;
		color: var(--text-color);
		max-width: 300px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.header-right {
		display: flex;
		gap: 12px;
	}

	.theme-toggle {
		padding: 8px 12px;
		background: var(--bg-color);
		border: 1px solid var(--border-color);
		border-radius: 4px;
		cursor: pointer;
		transition: all 0.2s;
		font-size: 16px;
	}

	.theme-toggle:hover {
		background: var(--item-hover);
	}

	.theme-icon {
		display: inline-block;
	}

	/* Tab Navigation */
	.tab-nav {
		display: flex;
		background: var(--surface-color);
		border-bottom: 1px solid var(--border-color);
		flex-shrink: 0;
	}

	.tab-button {
		padding: 12px 24px;
		background: transparent;
		border: none;
		border-bottom: 3px solid transparent;
		cursor: pointer;
		font-size: 14px;
		font-weight: 500;
		color: var(--text-secondary);
		transition: all 0.2s;
	}

	.tab-button:hover {
		color: var(--text-color);
		background: var(--item-hover);
	}

	.tab-button.active {
		color: var(--primary-color);
		border-bottom-color: var(--primary-color);
	}

	/* Tab Content */
	.tab-content {
		flex: 1;
		overflow: hidden;
		background: var(--bg-color);
		position: relative;
	}

	.tab-panel {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		bottom: 0;
		width: 100%;
		height: 100%;
		display: none;
	}

	.tab-panel.active {
		display: block;
	}

	/* Responsive design */
	@media (max-width: 768px) {
		.header-left {
			gap: 12px;
		}

		.app-title {
			font-size: 16px;
		}

		.project-selector {
			padding: 4px 8px;
		}

		.project-name {
			max-width: 150px;
		}

		.tab-button {
			padding: 10px 16px;
			font-size: 13px;
		}
	}

	@media (max-width: 480px) {
		.app-header {
			padding: 8px 12px;
		}

		.project-label {
			display: none;
		}

		.tab-nav {
			overflow-x: auto;
		}

		.tab-button {
			padding: 10px 12px;
			font-size: 12px;
			white-space: nowrap;
		}
	}
</style>
