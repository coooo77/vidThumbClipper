<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { videoTasks, type VideoSegment, type VideoTask } from '../taskStore'
import { getNotifySettings, notifyResult } from '../notify'

const ffmpegPath = ref('')
const ffprobePath = ref('')
const outputDirectory = ref(localStorage.getItem('vid_output_directory') || '')
const running = ref(false)
const showStopConfirm = ref(false)
const pendingCancelTaskId = ref<string | null>(null)
const hasPendingTasks = computed(() => videoTasks.some((task) => task.status === '等待中'))

onMounted(() => {
  ffmpegPath.value = localStorage.getItem('vid_ffmpeg_path') || ''
  ffprobePath.value = localStorage.getItem('vid_ffprobe_path') || ''
})
function folderOf(path: string) {
  return path.slice(0, Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/')) + 1)
}
function resolveOutputFolder(task: VideoTask) {
  const own = task.outputDir.trim()
  if (own) return `${own.replace(/[\\/]$/, '')}\\`
  const global = outputDirectory.value.trim()
  if (global) return `${global.replace(/[\\/]$/, '')}\\`
  return folderOf(task.source)
}
function destination(task: VideoTask, suffix = '') {
  return `${resolveOutputFolder(task)}${task.outputName}${suffix}.${task.extension.replace(/^\./, '')}`
}
function parseExtraArgs(value: string) {
  return value.trim() ? value.trim().split(/\s+/) : []
}
function formatTime(seconds: number) {
  const total = Math.floor(seconds)
  return [Math.floor(total / 3600), Math.floor((total % 3600) / 60), total % 60].map((value) => String(value).padStart(2, '0')).join(':')
}

async function chooseOutputDirectory() {
  const selected = await open({ directory: true, multiple: false, title: '選擇任務輸出資料夾' })
  if (typeof selected === 'string') {
    outputDirectory.value = selected
    localStorage.setItem('vid_output_directory', selected)
  }
}
function requestStopCurrentTask() {
  const current = videoTasks.find((task) => task.status === '處理中')
  if (current) requestStopTask(current)
}
function requestStopTask(task: VideoTask) {
  if (task.status !== '處理中') return
  pendingCancelTaskId.value = task.id
  showStopConfirm.value = true
}
function cancelStopConfirm() {
  showStopConfirm.value = false
  pendingCancelTaskId.value = null
}
async function confirmStopCurrentTask() {
  showStopConfirm.value = false
  const taskId = pendingCancelTaskId.value
  const current = videoTasks.find((task) => task.id === taskId && task.status === '處理中')
  if (!current) {
    pendingCancelTaskId.value = null
    return
  }
  try {
    const processWasRunning = await invoke<boolean>('cancel_current_task')
    if (!processWasRunning) {
      // After restarting the app, the persisted task may still say "處理中" even
      // though this app instance no longer owns the ffmpeg process.
      current.status = '已取消'
      current.message = '此任務已停止；若輸出資料夾中留下不完整檔案，請手動刪除。'
      pendingCancelTaskId.value = null
      if (!running.value) void runTasks()
    }
  } catch {
    // The running task's own catch block in runTasks() handles the status update either way.
  }
}
function clearFinishedTasks() {
  for (let index = videoTasks.length - 1; index >= 0; index -= 1) {
    const task = videoTasks[index]
    if (task.kind !== 'preprocess' && (task.status === '完成' || task.status === '失敗' || task.status === '已取消')) videoTasks.splice(index, 1)
  }
}
async function chooseTaskOutputDirectory(task: VideoTask) {
  const selected = await open({ directory: true, multiple: false, title: '選擇此任務的輸出資料夾' })
  if (typeof selected === 'string') task.outputDir = selected
}
function clearTaskOutputDirectory(task: VideoTask) {
  task.outputDir = ''
}
async function clipRange(task: VideoTask, range: VideoSegment, path: string) {
  const args = task.preciseSeek
    ? ['-i', task.source, '-ss', range.start.toFixed(3), '-to', range.end.toFixed(3), '-c', 'copy', path, '-y']
    : ['-ss', range.start.toFixed(3), '-i', task.source, '-to', range.end.toFixed(3), '-c', 'copy', path, '-y']
  const ffmpegOutput = await invoke<string>('run_ffmpeg', { ffmpegPath: ffmpegPath.value || null, args })
  const expected = range.end - range.start
  // A truncated/broken copy can still end up with a container header that claims the requested
  // duration (get_video_duration reads that metadata, so it isn't reliable here) — actual file
  // size on disk is a much harder thing to fake, so check that instead.
  let sizeBytes = 0
  try {
    sizeBytes = await invoke<number>('get_file_size', { path })
  } catch {
    sizeBytes = 0
  }
  const minSizeBytes = 200 * 1024
  if (expected > 2 && sizeBytes < minSizeBytes) {
    const command = `ffmpeg ${args.map((a) => (a.includes(' ') ? `"${a}"` : a)).join(' ')}`
    throw new Error(
      `剪輯出的片段檔案異常過小（僅 ${(sizeBytes / 1024).toFixed(1)} KB，要求長度 ${expected.toFixed(1)} 秒）：${formatTime(range.start)} → ${formatTime(range.end)}\n實際指令：${command}\nffmpeg 輸出：${ffmpegOutput.trim() || '(無輸出)'}`,
    )
  }
}
async function runPreprocessTask(task: VideoTask) {
  const path = destination(task)
  await invoke('run_ffmpeg', { ffmpegPath: ffmpegPath.value || null, args: ['-i', task.source, '-c', 'copy', path, '-y'] })
  task.resultPath = path
}
async function runClipTask(task: VideoTask) {
  const duration = task.method === 'range' ? 0 : await invoke<number>('get_video_duration', { ffprobePath: ffprobePath.value || null, videoPath: task.source })
  const ranges: VideoSegment[] =
    task.method === 'range'
      ? task.segments
      : task.method === 'duration'
        ? Array.from({ length: Math.ceil(duration / (task.durationHours * 3600)) }, (_, index) => ({
            start: index * task.durationHours * 3600,
            end: Math.min(duration, (index + 1) * task.durationHours * 3600),
          }))
        : Array.from({ length: task.splitCount }, (_, index) => ({
            start: (duration * index) / task.splitCount,
            end: (duration * (index + 1)) / task.splitCount,
          }))
  for (let index = 0; index < ranges.length; index += 1)
    await clipRange(task, ranges[index], destination(task, ranges.length > 1 ? `_${String(index + 1).padStart(3, '0')}` : ''))
}
async function runMergeTask(task: VideoTask) {
  const clips: string[] = []
  for (let index = 0; index < task.segments.length; index += 1) {
    const path = destination(task, `_segment_${String(index + 1).padStart(3, '0')}`)
    await clipRange(task, task.segments[index], path)
    clips.push(path)
  }
  await invoke('concat_videos', {
    ffmpegPath: ffmpegPath.value || null,
    files: clips,
    outputPath: destination(task),
    extraArgs: parseExtraArgs(task.extraArgs),
    deleteInputs: !task.keepSegments,
  })
}
async function runCombineTask(task: VideoTask) {
  const path = destination(task)
  if (task.combineMode === 'copy') {
    await invoke('concat_videos', { ffmpegPath: ffmpegPath.value || null, files: task.combineSources, outputPath: path, extraArgs: [], deleteInputs: false })
  } else {
    const inputArgs = task.combineSources.flatMap((file) => ['-i', file])
    const n = task.combineSources.length
    const filterInputs = task.combineSources.map((_, index) => `[${index}:v:0][${index}:a:0]`).join('')
    const filter = `${filterInputs}concat=n=${n}:v=1:a=1[outv][outa]`
    const args = [...inputArgs, '-filter_complex', filter, '-map', '[outv]', '-map', '[outa]', '-c:v', 'libx264', '-c:a', 'aac', path, '-y']
    await invoke('run_ffmpeg', { ffmpegPath: ffmpegPath.value || null, args })
  }
  let sizeBytes = 0
  try {
    sizeBytes = await invoke<number>('get_file_size', { path })
  } catch {
    sizeBytes = 0
  }
  if (sizeBytes < 200 * 1024) {
    throw new Error(
      `合併出的檔案異常過小（僅 ${(sizeBytes / 1024).toFixed(1)} KB）。來源影片格式可能不相容${task.combineMode === 'copy' ? '，可以試試「相容合併」模式' : ''}。`,
    )
  }
  task.resultPath = path
}
async function runTasks() {
  if (running.value) return
  if (outputDirectory.value.trim()) localStorage.setItem('vid_output_directory', outputDirectory.value.trim())
  running.value = true
  const { trigger } = getNotifySettings()
  let processedAny = false
  let hadFailure = false
  try {
    for (;;) {
      const task = videoTasks.find((item) => item.status === '等待中')
      if (!task) break
      processedAny = true
      task.status = '處理中'
      try {
        if (task.kind === 'clip') await runClipTask(task)
        else if (task.kind === 'merge') await runMergeTask(task)
        else if (task.kind === 'combine') await runCombineTask(task)
        else await runPreprocessTask(task)
        task.status = '完成'
        task.message = `已輸出至 ${outputDirectory.value || folderOf(task.source)}`
        if (trigger === 'each') notifyResult(true)
      } catch (error) {
        if (pendingCancelTaskId.value === task.id) {
          task.status = '已取消'
          task.message = '使用者已停止此任務，可能留下不完整的輸出檔案，需要手動清理。'
          pendingCancelTaskId.value = null
        } else {
          task.status = '失敗'
          task.message = String(error)
          hadFailure = true
          if (trigger === 'each') notifyResult(false)
        }
      }
    }
  } finally {
    running.value = false
    if (trigger === 'batch' && processedAny) notifyResult(!hadFailure)
  }
}
</script>

<template>
  <div class="tasks-view">
    <section class="card">
      <div class="card-header-with-btn">
        <h2 class="card-title">影片處理任務</h2>
        <div class="task-actions">
          <button class="btn btn-secondary" @click="clearFinishedTasks">清除已完成</button
          ><button v-if="running" class="btn btn-danger" @click="requestStopCurrentTask">停止執行中的任務</button
          ><button class="btn btn-primary" :disabled="running || !hasPendingTasks" @click="runTasks">{{ running ? '處理中…' : '開始依序執行' }}</button>
        </div>
      </div>
      <div v-if="showStopConfirm" class="stop-confirm">
        <span>確定要強制停止目前執行中的任務嗎？已經處理的部分可能留下不完整的輸出檔案，此動作無法復原。</span>
        <div class="stop-confirm-actions">
          <button class="btn btn-secondary compact" @click="cancelStopConfirm">取消</button
          ><button class="btn btn-danger compact" @click="confirmStopCurrentTask">確定停止</button>
        </div>
      </div>
      <p class="info">任務會依建立順序執行。原始影片只會被讀取，絕不覆寫。</p>
      <div class="input-group">
        <label>輸出位置（留空則輸出到原始影片所在資料夾）</label>
        <div class="input-with-btn">
          <input v-model="outputDirectory" placeholder="選擇所有任務的輸出資料夾" /><button class="btn btn-secondary" @click="chooseOutputDirectory">
            選擇資料夾
          </button>
        </div>
      </div>
    </section>
    <section class="card">
      <p v-if="!videoTasks.length" class="info">尚無等待任務。請回到「影片擷取與剪輯」從 Grid 建立任務。</p>
      <div v-for="(task, index) in videoTasks" :key="task.id" class="task-row">
        <div class="task-content w-full">
          <div class="task-header">
            <div class="task-heading">
              <strong class="task-title truncate"
                >{{ index + 1 }}.
                {{ task.kind === 'clip' ? '剪輯' : task.kind === 'merge' ? '剪輯＋合併' : task.kind === 'combine' ? '合併影片' : '預先處理' }}</strong
              ><span class="status" :class="task.status">{{ task.status }}</span>
            </div>
            <button v-if="task.kind === 'preprocess' && task.status === '處理中'" class="btn btn-danger compact task-stop" @click="requestStopTask(task)">停止</button>
            <button v-else-if="task.status !== '處理中'" class="remove" @click="videoTasks.splice(index, 1)">移除</button>
          </div>
          <p>
            {{
              task.kind === 'preprocess'
                ? '整支影片 remux 為 mp4'
                : task.kind === 'combine'
                  ? `${task.combineSources.length} 支影片（${task.combineMode === 'copy' ? '快速' : '相容'}合併）`
                  : task.kind === 'merge'
                    ? `${task.segments.length} 個片段`
                    : task.method === 'duration'
                      ? `每 ${task.durationHours} 小時分割`
                      : task.method === 'count'
                        ? `平均分割 ${task.splitCount} 段`
                        : `${formatTime(task.segments[0]?.start || 0)} → ${formatTime(task.segments[0]?.end || 0)}`
            }}
          </p>
          <p class="task-file-name truncate w-full" :title="`${task.outputName}.${task.extension}`">{{ task.outputName }}.{{ task.extension }}</p>
          <div class="task-output-dir">
            <label>輸出位置</label>
            <input class="w-full" v-model="task.outputDir" :disabled="task.status !== '等待中'" :placeholder="`預設：${outputDirectory || '原始影片所在資料夾'}`" />
            <button v-if="task.status === '等待中'" class="btn btn-secondary compact" @click="chooseTaskOutputDirectory(task)">選擇</button>
            <button v-if="task.status === '等待中' && task.outputDir" class="btn-clear" @click="clearTaskOutputDirectory(task)">清除</button>
          </div>
          <p v-if="task.message" class="message">{{ task.message }}</p>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.tasks-view {
  display: flex;
  flex-direction: column;
  gap: 1.25rem;
  max-width: 1000px;
  margin: 0 auto;
}
.task-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.6rem;
}
.info {
  color: var(--text-muted);
  font-size: 0.9rem;
  line-height: 1.5;
  margin: 0 0 1.1rem;
}
.task-row {
  display: flex;
  gap: 1rem;
  padding: 1rem;
  margin: 0.6rem 0;
  border: 1px solid var(--border-color);
  border-radius: 10px;
  background: rgba(15, 23, 42, 0.45);
}
.task-content {
  min-width: 0;
}
.task-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.75rem;
  width: 100%;
  min-width: 0;
}
.task-heading {
  display: flex;
  align-items: center;
  min-width: 0;
  flex: 1 1 auto;
}
.task-title {
  min-width: 0;
}
.truncate {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.task-row p {
  color: var(--text-muted);
  font-size: 0.85rem;
  margin-top: 0.3rem;
}
.status {
  display: inline-block;
  margin-left: 0.65rem;
  padding: 0.15rem 0.45rem;
  border-radius: 999px;
  background: #334155;
  color: #e2e8f0;
  font-size: 0.76rem;
  flex-shrink: 0;
}
.完成 {
  background: rgba(16, 185, 129, 0.2);
  color: #6ee7b7;
}
.失敗 {
  background: rgba(239, 68, 68, 0.2);
  color: #fca5a5;
}
.處理中 {
  background: rgba(99, 102, 241, 0.25);
  color: #c7d2fe;
}
.已取消 {
  background: rgba(148, 163, 184, 0.2);
  color: #cbd5e1;
}
.message {
  white-space: pre-wrap;
  color: #fca5a5 !important;
}
.remove {
  border: 0;
  align-self: start;
  background: none;
  color: #fca5a5;
  cursor: pointer;
  flex-shrink: 0;
}
.btn-danger {
  background: #7f1d1d;
  color: #fecaca;
  border: 0;
}
.btn-danger:hover {
  background: #991b1b;
}
.stop-confirm {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  margin: -0.5rem 0 1.1rem;
  padding: 0.8rem 1rem;
  border-radius: 10px;
  background: rgba(127, 29, 29, 0.35);
  border: 1px solid rgba(239, 68, 68, 0.4);
  color: #fecaca;
  font-size: 0.85rem;
  line-height: 1.5;
}
.stop-confirm-actions {
  display: flex;
  gap: 0.5rem;
  flex-shrink: 0;
}
.compact {
  padding: 0.45rem 0.75rem;
  font-size: 0.8rem;
}
.task-output-dir {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-top: 0.5rem;
  width: 100%;
  min-width: 0;
}
.task-output-dir label {
  color: var(--text-muted);
  font-size: 0.8rem;
  white-space: nowrap;
}
.task-output-dir input {
  flex: 1 1 0;
  width: 100%;
  min-width: 0;
  padding: 0.4rem 0.6rem;
  font-size: 0.82rem;
}
.task-output-dir .compact {
  flex-shrink: 0;
  padding: 0.4rem 0.7rem;
  font-size: 0.78rem;
}
.task-output-dir .btn-clear {
  border: 0;
  background: none;
  color: var(--text-muted);
  font-size: 0.78rem;
  cursor: pointer;
}
</style>
