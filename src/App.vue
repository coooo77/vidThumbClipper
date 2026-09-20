<script setup lang="ts">
import { ref } from 'vue'
import { useRoute } from 'vue-router'
import { getCurrentWindow } from '@tauri-apps/api/window'

const route = useRoute()
const isAlwaysOnTop = ref(false)

function handleHomeNavigation() {
  // A router-link to the current route does not trigger a route update. Notify the
  // Home view explicitly so clicking the current navigation item still resets its
  // visible panel.
  window.dispatchEvent(new Event('home-navigation'))
}

async function toggleAlwaysOnTop() {
  const next = !isAlwaysOnTop.value
  try {
    await getCurrentWindow().setAlwaysOnTop(next)
    isAlwaysOnTop.value = next
  } catch (error) {
    console.error('Unable to toggle always-on-top:', error)
  }
}
</script>

<template>
  <div class="app-container">
    <header class="app-header">
      <div class="header-main">
        <div class="logo-title">
          <span class="pulse-dot"></span>
          <h1>vidThumbClipper</h1>
        </div>
        <div class="header-actions">
          <nav class="nav-links">
            <router-link to="/" class="nav-item" active-class="nav-active" @click="handleHomeNavigation"> 影片擷取與剪輯 </router-link>
            <router-link to="/tasks" class="nav-item" active-class="nav-active">處理影片任務</router-link>
            <router-link to="/settings" class="nav-item" active-class="nav-active"> 設定 </router-link>
          </nav>
          <button class="pin-btn" :class="{ active: isAlwaysOnTop }" :title="isAlwaysOnTop ? '取消視窗置頂' : '視窗置頂'" @click="toggleAlwaysOnTop">
            <span class="pin-icon">📌</span>
          </button>
        </div>
      </div>
    </header>

    <!-- 路由視圖，使用 keep-alive 保持分頁狀態 -->
    <main class="app-main-content">
      <router-view v-slot="{ Component }">
        <keep-alive>
          <component :is="Component" :key="route.fullPath" />
        </keep-alive>
      </router-view>
    </main>
  </div>
</template>

<style>
/* 全域變數與重置 */
:root {
  --bg-color: #0b0f19;
  --panel-bg: rgba(20, 26, 45, 0.6);
  --border-color: rgba(255, 255, 255, 0.08);
  --primary-color: #6366f1;
  --primary-hover: #4f46e5;
  --secondary-color: #334155;
  --secondary-hover: #475569;
  --text-color: #f1f5f9;
  --text-muted: #94a3b8;
  --accent-color: #10b981;
}

* {
  box-sizing: border-box;
  margin: 0;
  padding: 0;
}

body {
  background-color: var(--bg-color);
  color: var(--text-color);
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
  overflow-x: hidden;
}

.app-container {
  width: 100%;
  max-width: none;
  margin: 0;
  padding: 0 16px 2rem;
  min-height: 100vh;
  display: flex;
  flex-direction: column;
}

.app-header {
  position: fixed;
  z-index: 100;
  top: 0;
  left: 0;
  width: 100%;
  height: 64px;
  margin: 0;
  border-bottom: 1px solid var(--border-color);
  padding: 0.65rem 16px;
  overflow: hidden;
  background: rgba(11, 15, 25, 0.94);
  backdrop-filter: blur(16px);
}

.header-main {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 15px;
}

.logo-title {
  display: flex;
  align-items: center;
  gap: 10px;
}

.pulse-dot {
  width: 12px;
  height: 12px;
  background-color: var(--accent-color);
  border-radius: 50%;
  box-shadow: 0 0 10px var(--accent-color);
  animation: pulse 2s infinite;
}

@keyframes pulse {
  0% {
    transform: scale(0.9);
    opacity: 0.6;
  }
  50% {
    transform: scale(1.1);
    opacity: 1;
  }
  100% {
    transform: scale(0.9);
    opacity: 0.6;
  }
}

.app-header h1 {
  font-size: 1.8rem;
  font-weight: 800;
  background: linear-gradient(135deg, #a5b4fc, #6366f1);
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
}

/* 導覽列 + 置頂按鈕 */
.header-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  position: absolute;
  right: 16px;
  top: 50%;
  transform: translateY(-50%);
}

.nav-links {
  display: flex;
  gap: 10px;
}

.pin-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 2.4rem;
  height: 2.4rem;
  flex-shrink: 0;
  border-radius: 8px;
  border: 1px solid var(--border-color);
  background: rgba(255, 255, 255, 0.03);
  color: var(--text-muted);
  font-size: 1.1rem;
  cursor: pointer;
  transition: all 0.25s ease;
}

.pin-btn:hover {
  color: var(--text-color);
  background: rgba(255, 255, 255, 0.08);
}

.pin-btn.active {
  background: rgba(99, 102, 241, 0.2);
  border-color: rgba(99, 102, 241, 0.4);
  color: #a5b4fc;
  box-shadow: 0 0 10px rgba(99, 102, 241, 0.3);
}

.pin-btn .pin-icon {
  transform: rotate(45deg);
  transition: transform 0.25s ease;
}

.pin-btn.active .pin-icon {
  transform: rotate(0deg);
}

.nav-item {
  color: var(--text-muted);
  text-decoration: none;
  font-weight: 600;
  font-size: 0.95rem;
  padding: 0.5rem 1rem;
  border-radius: 8px;
  border: 1px solid transparent;
  transition: all 0.25s ease;
}

.nav-item:hover {
  color: var(--text-color);
  background: rgba(255, 255, 255, 0.05);
}

.nav-active {
  color: #fff;
  background: rgba(99, 102, 241, 0.15);
  border-color: rgba(99, 102, 241, 0.3);
}

.app-main-content {
  flex: 1;
  margin-top: 80px;
}

/* 共用卡片與容器元件 */
.card {
  background: var(--panel-bg);
  backdrop-filter: blur(16px);
  border: 1px solid var(--border-color);
  border-radius: 16px;
  padding: 1.5rem;
  box-shadow: 0 8px 32px 0 rgba(0, 0, 0, 0.3);
}

.card-title {
  font-size: 1.1rem;
  font-weight: 600;
  margin-bottom: 1.25rem;
  color: var(--text-color);
  border-left: 3px solid var(--primary-color);
  padding-left: 10px;
}

.card-header-with-btn {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  row-gap: 0.6rem;
  margin-bottom: 1rem;
}

.card-header-with-btn .card-title {
  margin-bottom: 0;
}

.input-group {
  margin-bottom: 1.25rem;
}

.input-group label {
  display: block;
  font-size: 0.85rem;
  color: var(--text-muted);
  margin-bottom: 0.5rem;
}

input[type='text'],
input[type='number'],
input:not([type]) {
  width: 100%;
  background: rgba(15, 23, 42, 0.6);
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 0.75rem 1rem;
  color: var(--text-color);
  font-size: 0.95rem;
  transition: all 0.25s ease;
}

input[type='text']:focus,
input[type='number']:focus,
input:not([type]):focus {
  border-color: var(--primary-color);
  box-shadow: 0 0 0 2px rgba(99, 102, 241, 0.2);
  outline: none;
}

.input-with-btn {
  display: flex;
  gap: 8px;
}

.input-with-btn input {
  flex: 1;
}

.btn {
  border-radius: 8px;
  padding: 0.75rem 1.5rem;
  font-size: 0.95rem;
  font-weight: 600;
  border: none;
  cursor: pointer;
  transition: all 0.25s ease;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  white-space: nowrap;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-primary {
  background-color: var(--primary-color);
  color: #fff;
}

.btn-primary:hover:not(:disabled) {
  background-color: var(--primary-hover);
}

.btn-glow:hover:not(:disabled) {
  box-shadow: 0 0 15px rgba(99, 102, 241, 0.4);
}

.btn-secondary {
  background-color: var(--secondary-color);
  color: var(--text-color);
}

.btn-secondary:hover:not(:disabled) {
  background-color: var(--secondary-hover);
}

.w-full {
  width: 100%;
}

@media (max-width: 600px) {
  .app-header {
    height: 94px;
  }
  .app-main-content {
    margin-top: 110px;
  }
}
</style>
