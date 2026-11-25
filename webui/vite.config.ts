import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import AutoImport from 'unplugin-auto-import/vite'
import Components from 'unplugin-vue-components/vite'
import { ElementPlusResolver } from 'unplugin-vue-components/resolvers'
import pkg from './package.json'

// 构建信息：注入到前端「系统信息」页（不依赖任何外网/CDN）。
const buildTime = new Date().toISOString()
const gitish = process.env.KHAEN_WEBUI_BUILD_ID || 'local'

export default defineConfig({
  base: '/',
  plugins: [
    vue(),
    AutoImport({
      imports: ['vue', 'vue-router'],
      resolvers: [ElementPlusResolver({ importStyle: false })],
      dts: 'src/auto-imports.d.ts',
      eslintrc: { enabled: false }
    }),
    Components({
      resolvers: [ElementPlusResolver({ importStyle: false })],
      dts: 'src/components.d.ts'
    })
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url))
    }
  },
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
    __APP_BUILD_TIME__: JSON.stringify(buildTime),
    __APP_BUILD_ID__: JSON.stringify(gitish)
  },
  server: {
    host: '0.0.0.0',
    port: 5173,
    proxy: {
      '/api': { target: 'http://192.168.63.222:80', changeOrigin: false },
      '/healthz': { target: 'http://192.168.63.222:80', changeOrigin: false },
      '/ws': { target: 'ws://192.168.63.222:80', ws: true }
    }
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: false,
    chunkSizeWarningLimit: 1600,
    rollupOptions: {
      output: {
        manualChunks: {
          vue: ['vue', 'vue-router'],
          'element-plus': ['element-plus', '@element-plus/icons-vue']
        }
      }
    }
  }
})
