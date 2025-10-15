<script lang="ts">
	import { onMount } from 'svelte';
	import { theme } from '$lib/stores/app';
	import favicon from '$lib/assets/favicon.svg';
	import '../app.css';

	let { children } = $props();

	// Initialize theme on mount
	onMount(() => {
		// Apply theme to document element
		if (typeof document !== 'undefined') {
			document.documentElement.setAttribute('data-theme', $theme);
		}
	});

	// Reactively update theme
	$effect(() => {
		if (typeof document !== 'undefined') {
			document.documentElement.setAttribute('data-theme', $theme);
		}
	});
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>Claude Code - Web UI</title>
</svelte:head>

{@render children?.()}
