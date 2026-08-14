import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Renderer (React) apenas. O processo main do Electron é JS puro em electron/main.js.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: 'dist-renderer',
    emptyOutDir: true,
  },
});
