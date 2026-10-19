import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  build: { outDir: 'dist', emptyOutDir: true, target: 'es2022' },
  // The page's own name is passed on, so Spool sees a change come from the page it was asked by.
  server: { proxy: { '/api': { target: 'http://127.0.0.1:7979', changeOrigin: false } } },
});
