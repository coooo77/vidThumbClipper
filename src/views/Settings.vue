<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import { openPath } from '@tauri-apps/plugin-opener'
import { invoke } from '@tauri-apps/api/core'
import { getNotifySettings, playCompletionSound, playFailureSound, saveNotifySettings, type NotifyTrigger } from '../notify'

const ffmpegPath = ref('')
const ffprobePath = ref('')
const taskNameSuffix = ref('_edit')
const statusMsg = ref('')
const notifyEnabled = ref(false)
const notifyVolume = ref(70)
const notifyTrigger = ref<NotifyTrigger>('batch')
const notifyOnFailure = ref(true)
const diskSafetyEnabled = ref(false)
const diskSafetyMinGb = ref(100)

onMounted(() => {
  ffmpegPath.value = localStorage.getItem('vid_ffmpeg_path') || ''
  ffprobePath.value = localStorage.getItem('vid_ffprobe_path') || ''
  taskNameSuffix.value = localStorage.getItem('vid_task_suffix') || '_edit'
  const notify = getNotifySettings()
  notifyEnabled.value = notify.enabled
  notifyVolume.value = notify.volume
  notifyTrigger.value = notify.trigger
  notifyOnFailure.value = notify.onFailure
  diskSafetyEnabled.value = localStorage.getItem('vid_disk_safety_enabled') === 'true'
  diskSafetyMinGb.value = Math.max(1, Number(localStorage.getItem('vid_disk_safety_min_gb')) || 100)
})

function showStatus(message: string) {
  statusMsg.value = message
  window.setTimeout(() => {
    statusMsg.value = ''
  }, 3000)
}
function saveSettings() {
  localStorage.setItem('vid_ffmpeg_path', ffmpegPath.value.trim())
  localStorage.setItem('vid_ffprobe_path', ffprobePath.value.trim())
  localStorage.setItem('vid_task_suffix', taskNameSuffix.value.trim() || '_edit')
  saveNotifySettings({ enabled: notifyEnabled.value, volume: notifyVolume.value, trigger: notifyTrigger.value, onFailure: notifyOnFailure.value })
  localStorage.setItem('vid_disk_safety_enabled', String(diskSafetyEnabled.value))
  localStorage.setItem('vid_disk_safety_min_gb', String(Math.max(1, Number(diskSafetyMinGb.value) || 100)))
  showStatus('設定已儲存。')
}
function testCompletionSound() {
  playCompletionSound(notifyVolume.value)
}
function testFailureSound() {
  playFailureSound(notifyVolume.value)
}
async function chooseExecutable(target: 'ffmpeg' | 'ffprobe') {
  const selected = await open({ multiple: false, directory: false, title: `選擇 ${target}.exe`, filters: [{ name: 'Executable', extensions: ['exe'] }] })
  if (typeof selected !== 'string') return
  if (target === 'ffmpeg') ffmpegPath.value = selected
  else ffprobePath.value = selected
  saveSettings()
}
async function openThumbnailCache() {
  try {
    const path = await invoke<string>('get_thumbnail_cache_dir')
    await openPath(path)
  } catch (error) {
    showStatus(`無法開啟暫存資料夾：${String(error)}`)
  }
}
async function clearThumbnailCache() {
  try {
    await invoke('clear_thumbnail_cache')
    showStatus('已清除縮圖暫存。')
  } catch (error) {
    showStatus(`清除暫存失敗：${String(error)}`)
  }
}
</script>

<template>
  <div class="settings-view">
    <section class="card settings-card">
      <h2 class="card-title">工具與暫存設定</h2>
      <p class="description">設定 ffmpeg / ffprobe 的位置。每張縮圖擷取後，暫存檔會立即自動刪除；若因程式異常結束而留下殘檔，可用下方按鈕手動清除。</p>
      <div class="input-group">
        <label>ffmpeg 路徑</label>
        <div class="input-with-btn">
          <input v-model="ffmpegPath" placeholder="例如：C:\ffmpeg\bin\ffmpeg.exe" @change="saveSettings" /><button
            class="btn btn-secondary"
            @click="chooseExecutable('ffmpeg')"
          >
            選擇
          </button>
        </div>
      </div>
      <div class="input-group">
        <label>ffprobe 路徑</label>
        <div class="input-with-btn">
          <input v-model="ffprobePath" placeholder="例如：C:\ffmpeg\bin\ffprobe.exe" @change="saveSettings" /><button
            class="btn btn-secondary"
            @click="chooseExecutable('ffprobe')"
          >
            選擇
          </button>
        </div>
      </div>
      <div class="input-group">
        <label>任務預設輸出檔名後綴</label><input v-model="taskNameSuffix" placeholder="_edit" @change="saveSettings" />
        <p class="field-hint">建立任務時，預設輸出檔名為「原始影片檔名 + 此後綴」。</p>
      </div>
      <div class="save-actions">
        <button class="btn btn-primary btn-glow" @click="saveSettings">儲存設定</button
        ><button class="btn btn-secondary" @click="openThumbnailCache">開啟截圖暫存資料夾</button
        ><button class="btn btn-secondary" @click="clearThumbnailCache">清除縮圖暫存</button
        ><transition name="fade"
          ><span v-if="statusMsg" class="status-badge">{{ statusMsg }}</span></transition
        >
      </div>
    </section>

    <section class="card settings-card">
      <h2 class="card-title">硬碟容量安全保護</h2>
      <p class="description">啟用後，每個任務開始前都會檢查該任務實際輸出磁碟的可用空間。低於門檻時會保留任務並停止整批處理，避免輸出到一半耗盡空間。</p>
      <label class="checkbox-row-label"><input type="checkbox" v-model="diskSafetyEnabled" @change="saveSettings" /> 啟用容量安全保護</label>
      <div v-if="diskSafetyEnabled" class="disk-safety-settings">
        <div class="input-group">
          <label>最低可用空間（GB）</label>
          <input v-model.number="diskSafetyMinGb" type="number" min="1" step="1" @change="saveSettings" />
          <p class="field-hint">預設 100 GB。可用空間等於或高於此值時才會開始任務。</p>
        </div>
      </div>
    </section>

    <section class="card settings-card">
      <h2 class="card-title">通知設定</h2>
      <label class="checkbox-row-label"><input type="checkbox" v-model="notifyEnabled" @change="saveSettings" /> 處理結束發出聲音通知</label>

      <div v-if="notifyEnabled" class="notify-sub-settings">
        <div class="input-group">
          <label>音量（{{ notifyVolume }}%）</label>
          <input type="range" min="0" max="100" v-model.number="notifyVolume" @change="saveSettings" />
        </div>

        <div class="input-group">
          <label>觸發時機</label>
          <div class="mode-row">
            <label><input type="radio" v-model="notifyTrigger" value="batch" @change="saveSettings" /> 整批任務全部完成才提示一次</label>
            <label><input type="radio" v-model="notifyTrigger" value="each" @change="saveSettings" /> 每個任務完成都提示</label>
          </div>
        </div>

        <label class="checkbox-row-label"><input type="checkbox" v-model="notifyOnFailure" @change="saveSettings" /> 任務失敗時，用不同音效提示</label>

        <div class="save-actions test-actions">
          <button class="btn btn-secondary" @click="testCompletionSound">測試完成音效</button>
          <button class="btn btn-secondary" @click="testFailureSound">測試失敗音效</button>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.settings-view {
  max-width: 700px;
  margin: 0 auto;
  padding: 1rem 0;
}
.settings-card {
  display: flex;
  flex-direction: column;
  gap: 1.5rem;
}
.description {
  color: var(--text-muted);
  font-size: 0.9rem;
  line-height: 1.6;
}
.field-hint {
  color: var(--text-muted);
  font-size: 0.8rem;
  margin-top: 0.4rem;
}
.save-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.75rem;
}
.status-badge {
  color: var(--accent-color);
  font-size: 0.85rem;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.3s;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
.checkbox-row-label {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  color: var(--text-color);
  font-size: 0.95rem;
  cursor: pointer;
}
.checkbox-row-label input {
  width: auto;
}
.notify-sub-settings {
  display: flex;
  flex-direction: column;
  gap: 1.25rem;
  margin-top: 0.25rem;
  padding: 1rem 1.1rem;
  border-left: 2px solid var(--border-color);
}
.disk-safety-settings {
  padding: 1rem 1.1rem 0;
  border-left: 2px solid var(--border-color);
}
.notify-sub-settings .mode-row {
  display: flex;
  flex-wrap: wrap;
  gap: 1.2rem;
  color: var(--text-muted);
  font-size: 0.9rem;
}
.notify-sub-settings .mode-row label {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  cursor: pointer;
}
.notify-sub-settings input[type='range'] {
  width: 100%;
  accent-color: var(--primary-color);
}
.test-actions {
  margin-top: -0.25rem;
}
</style>
