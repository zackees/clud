<script lang="ts">
	import Terminal from '$lib/components/Terminal.svelte';
	import { onMount } from 'svelte';
	import { currentProject } from '$lib/stores/app';

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
			projectPath = '';
		}
	});
</script>

<svelte:head>
	<title>Claude Code - Terminal</title>
</svelte:head>

<div class="terminal-only-page">
	<div class="terminal-header">
		<h1>Terminal</h1>
		{#if projectPath}
			<span class="project-path">{projectPath}</span>
		{/if}
	</div>
	<div class="terminal-wrapper">
		<Terminal />
	</div>
</div>

<style>
	.terminal-only-page {
		display: flex;
		flex-direction: column;
		height: 100vh;
		width: 100vw;
		overflow: hidden;
		background: #1e1e1e;
	}

	.terminal-header {
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 12px 16px;
		background: #252526;
		border-bottom: 1px solid #3e3e42;
		flex-shrink: 0;
	}

	.terminal-header h1 {
		margin: 0;
		font-size: 16px;
		font-weight: 600;
		color: #cccccc;
	}

	.project-path {
		font-size: 13px;
		color: #858585;
		font-family: 'Consolas', 'Monaco', monospace;
	}

	.terminal-wrapper {
		flex: 1;
		overflow: hidden;
		position: relative;
	}
</style>
