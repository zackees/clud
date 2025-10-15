import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		port: 5174, // Different port from webui (5173)
		proxy: {
			'/api': {
				target: 'http://localhost:8889', // Telegram server port
				changeOrigin: true,
				ws: false
			},
			'/ws': {
				target: 'ws://localhost:8889',
				changeOrigin: true,
				ws: true
			}
		}
	}
});
