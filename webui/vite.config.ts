import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import pkg from './package.json'

// 构建信息只注入到页脚/系统信息展示，不依赖任何外网资源。
// 注意：**不使用构建时刻的时间戳**——dist 是入库产物，带时间戳会让每次构建产生无法复现的 hash 漂移。
// 需要真实构建标识时由 CI/发布脚本传 KHAEN_WEBUI_BUILD_ID（缺省 local）。

export default defineConfig({
  base: '/',
  plugins: [vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url))
    }
  },
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
    __APP_BUILD_ID__: JSON.stringify(process.env.KHAEN_WEBUI_BUILD_ID || 'local')
  },
  server: {
    host: '0.0.0.0',
    port: 5173,
    proxy: {
      // 开发期把 /api 转给本机 daemon（生产由 daemon 同源托管，无代理）。
      '/api': {
        target: process.env.KHAEN_DEV_API || 'http://127.0.0.1:8730',
        changeOrigin: false
      },
      // 契约 mock 的 dev-only fixture 切换端点（真实 daemon 不存在该路径）。
      '/__mock': {
        target: process.env.KHAEN_DEV_API || 'http://127.0.0.1:8730',
        changeOrigin: false
      },
      // 匿名 /healthz（release 版本来源）；daemon 同源时由真实后端返回，dev/preview 代理到后端或 mock。
      '/healthz': {
        target: process.env.KHAEN_DEV_API || 'http://127.0.0.1:8730',
        changeOrigin: false
      }
    }
  },
  preview: {
    // 视觉/浏览器验收用：直接预览 dist，同样把 /api 转给本机 daemon 或契约 mock。
    host: '127.0.0.1',
    port: 4173,
    proxy: {
      '/api': {
        target: process.env.KHAEN_DEV_API || 'http://127.0.0.1:8730',
        changeOrigin: false
      },
      '/__mock': {
        target: process.env.KHAEN_DEV_API || 'http://127.0.0.1:8730',
        changeOrigin: false
      },
      '/healthz': {
        target: process.env.KHAEN_DEV_API || 'http://127.0.0.1:8730',
        changeOrigin: false
      }
    }
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    emptyOutDir: true,
    sourcemap: false,
    target: 'es2020',
    chunkSizeWarningLimit: 1600,
    rollupOptions: {
      output: {
        manualChunks: {
          vue: ['vue', 'vue-router', 'vue-i18n'],
          'element-plus': ['element-plus', '@element-plus/icons-vue']
        }
      }
    }
  }
})
