<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { Channel, invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { useRouter } from 'vue-router'
import { queueTaskEdit, videoTasks, type VideoSegment, type VideoTask } from '../taskStore'
import { getNotifySettings, notifyResult } from '../notify'
import { buildClipRanges, directoryOf, formatTimecode, resolveTaskOutputFolder, taskDestination, taskSegmentDurationSeconds } from '../videoUtils'

const ffmpegPath = ref('')
const ffprobePath = ref('')
const outputDirectory = ref(localStorage.getItem('vid_output_directory') || '')
const running = ref(false)
const stopBatchRequested = ref(false)
const showStopConfirm = ref(false)
const pendingEditTaskId = ref<string | null>(null)
const expandedTaskIds = ref<string[]>([])
const draggedTaskId = ref<string | null>(null)
const dragTargetTaskId = ref<string | null>(null)
const dragTargetAfter = ref(false)
let pointerDragSourceId: string | null = null
let pointerDragStartX = 0
let pointerDragStartY = 0
let pointerDragActive = false
const safetyWarning = ref('')
const clockNow = ref(Date.now())
let clockTimer: ReturnType<typeof setInterval> | null = null
const scheduleMode = ref<'now' | 'delay' | 'clock'>('now')
const delayHours = ref(0)
const delayMinutes = ref(30)
const scheduledHour = ref(6)
const scheduledMinute = ref(30)
const scheduledSecond = ref(0)
const hourOptions = Array.from({ length: 24 }, (_, index) => index)
const minuteSecondOptions = Array.from({ length: 60 }, (_, index) => index)
const scheduledStartAt = ref<number | null>(null)
const scheduledNow = ref(Date.now())
let scheduleTimer: ReturnType<typeof setInterval> | null = null
const hasPendingTasks = computed(() => videoTasks.some((task) => task.status === '等待中'))
const router = useRouter()

onMounted(() => {
  ffmpegPath.value = localStorage.getItem('vid_ffmpeg_path') || ''
  ffprobePath.value = localStorage.getItem('vid_ffprobe_path') || ''
  const storedSchedule = Number(localStorage.getItem('vid_task_schedule_at'))
  scheduledStartAt.value = Number.isFinite(storedSchedule) && storedSchedule > 0 ? storedSchedule : null
  const suggestedTime = new Date(Date.now() + 60 * 60 * 1000)
  scheduledHour.value = suggestedTime.getHours()
  scheduledMinute.value = suggestedTime.getMinutes()
  scheduledSecond.value = 0
  startScheduleTimer()
  window.addEventListener('mousemove', onTaskMouseMove, { passive: false })
  window.addEventListener('mouseup', onTaskMouseUp)
  window.addEventListener('blur', finishTaskDrag)
})
onUnmounted(() => {
  if (clockTimer) clearInterval(clockTimer)
  if (scheduleTimer) clearInterval(scheduleTimer)
  window.removeEventListener('mousemove', onTaskMouseMove)
  window.removeEventListener('mouseup', onTaskMouseUp)
  window.removeEventListener('blur', finishTaskDrag)
  finishTaskDrag()
})
function resolveOutputFolder(task: VideoTask) {
  return resolveTaskOutputFolder(task, outputDirectory.value)
}
function destination(task: VideoTask, suffix = '') {
  return taskDestination(task, outputDirectory.value, suffix)
}
function diskSafetySettings() {
  return {
    enabled: localStorage.getItem('vid_disk_safety_enabled') === 'true',
    minGb: Math.max(1, Number(localStorage.getItem('vid_disk_safety_min_gb')) || 100),
  }
}
async function ensureEnoughDiskSpace(task: VideoTask) {
  const settings = diskSafetySettings()
  if (!settings.enabled) return true
  const folder = resolveOutputFolder(task).replace(/[\\/]$/, '')
  try {
    const availableBytes = await invoke<number>('get_available_space', { path: folder })
    const availableGb = availableBytes / 1024 ** 3
    if (availableGb >= settings.minGb) return true
    safetyWarning.value = `已停止執行：輸出磁碟僅剩 ${availableGb.toFixed(1)} GB，低於安全門檻 ${settings.minGb} GB。請釋放空間、調整輸出位置，或到設定變更門檻。`
  } catch (error) {
    safetyWarning.value = `無法確認輸出磁碟容量，因此未開始任務：${String(error)}`
  }
  return false
}
function parseExtraArgs(value: string) {
  return value.trim() ? value.trim().split(/\s+/) : []
}
function formatDuration(seconds: number) {
  const rounded = Math.max(0, Math.round(seconds))
  const hours = Math.floor(rounded / 3600)
  const minutes = Math.floor((rounded % 3600) / 60)
  const remainingSeconds = rounded % 60
  if (hours) return `${hours} 小時 ${minutes} 分`
  if (minutes) return `${minutes} 分 ${remainingSeconds} 秒`
  return `${remainingSeconds} 秒`
}
function formatTaskRunDuration(milliseconds: number) {
  const seconds = Math.max(0, Math.round(milliseconds / 1000))
  if (seconds < 60) return `${seconds} 秒`
  const hours = Math.floor(seconds / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  const remainingSeconds = seconds % 60
  const clock = `${String(minutes).padStart(2, '0')}:${String(remainingSeconds).padStart(2, '0')}`
  return hours ? `${String(hours).padStart(2, '0')}:${clock}` : clock
}
function nextClockStartAt() {
  const start = new Date()
  start.setHours(scheduledHour.value, scheduledMinute.value, scheduledSecond.value, 0)
  if (start.getTime() <= Date.now()) start.setDate(start.getDate() + 1)
  return start.getTime()
}
function scheduledClockPreview() {
  return new Date(nextClockStartAt()).toLocaleString()
}
function scheduleCountdownText() {
  if (!scheduledStartAt.value) return ''
  const seconds = Math.max(0, Math.ceil((scheduledStartAt.value - scheduledNow.value) / 1000))
  return `${new Date(scheduledStartAt.value).toLocaleString()}（剩餘 ${formatDuration(seconds)}）`
}
function clearSchedule() {
  scheduledStartAt.value = null
  localStorage.removeItem('vid_task_schedule_at')
}
function startScheduleTimer() {
  if (scheduleTimer) clearInterval(scheduleTimer)
  const tick = () => {
    scheduledNow.value = Date.now()
    if (scheduledStartAt.value && scheduledStartAt.value <= scheduledNow.value && !running.value) {
      clearSchedule()
      void runTasks()
    }
  }
  tick()
  scheduleTimer = setInterval(tick, 1000)
}
function requestTaskRun() {
  if (scheduleMode.value === 'now') {
    clearSchedule()
    void runTasks()
    return
  }
  let startAt = 0
  if (scheduleMode.value === 'delay') {
    const hours = Math.max(0, Number(delayHours.value) || 0)
    const minutes = Math.max(0, Number(delayMinutes.value) || 0)
    const delayMs = (hours * 60 + minutes) * 60 * 1000
    if (delayMs < 1000) {
      safetyWarning.value = '請輸入至少 1 分鐘的延後時間。'
      return
    }
    startAt = Date.now() + delayMs
  } else {
    startAt = nextClockStartAt()
  }
  safetyWarning.value = ''
  scheduledStartAt.value = startAt
  localStorage.setItem('vid_task_schedule_at', String(startAt))
  startScheduleTimer()
}
function startTaskProgress(task: VideoTask, total: number, label: string) {
  task.progress = { completed: 0, total: Math.max(1, total), startedAt: Date.now(), label }
}
function completeTaskStep(task: VideoTask, label: string) {
  if (!task.progress) return
  task.progress.completed = Math.min(task.progress.total, task.progress.completed + 1)
  task.progress.label = label
}
function updateMediaProgress(task: VideoTask, completedBefore: number, expectedDuration: number, processedSeconds: number) {
  if (!task.progress || task.status !== '處理中') return
  const bounded = Math.min(Math.max(0, expectedDuration), Math.max(0, processedSeconds))
  task.progress.completed = Math.min(task.progress.total, completedBefore + bounded)
}
async function runFfmpegWithProgress(task: VideoTask, args: string[], completedBefore: number, expectedDuration: number) {
  const onProgress = new Channel<number>()
  onProgress.onmessage = (seconds) => updateMediaProgress(task, completedBefore, expectedDuration, seconds)
  const result = await invoke<string>('run_ffmpeg', { ffmpegPath: ffmpegPath.value || null, args, onProgress })
  updateMediaProgress(task, completedBefore, expectedDuration, expectedDuration)
  return result
}
async function concatWithProgress(task: VideoTask, files: string[], outputPath: string, extraArgs: string[], deleteInputs: boolean, completedBefore: number, expectedDuration: number) {
  const onProgress = new Channel<number>()
  onProgress.onmessage = (seconds) => updateMediaProgress(task, completedBefore, expectedDuration, seconds)
  await invoke('concat_videos', { ffmpegPath: ffmpegPath.value || null, files, outputPath, extraArgs, deleteInputs, onProgress })
  updateMediaProgress(task, completedBefore, expectedDuration, expectedDuration)
}
async function videoDuration(path: string) {
  return await invoke<number>('get_video_duration', { ffprobePath: ffprobePath.value || null, videoPath: path })
}
function taskProgressPercent(task: VideoTask) {
  if (!task.progress?.total) return 0
  return Math.round((task.progress.completed / task.progress.total) * 100)
}
function taskElapsedSeconds(task: VideoTask) {
  if (!task.progress?.startedAt) return 0
  const end = task.status === '處理中' ? clockNow.value : task.progress.startedAt + Math.max(0, task.progress.completed ? 1 : 0)
  return Math.max(0, (end - task.progress.startedAt) / 1000)
}
function taskEtaText(task: VideoTask) {
  const progress = task.progress
  if (!progress || task.status !== '處理中') return ''
  if (!progress.completed) return '剩餘時間估算中'
  const elapsed = (clockNow.value - progress.startedAt) / 1000
  const remaining = (elapsed / progress.completed) * (progress.total - progress.completed)
  return remaining > 0 ? `預估剩餘 ${formatDuration(remaining)}` : '即將完成'
}

function taskSummary(task: VideoTask) {
  if (task.kind === 'preprocess') return '整支影片 remux 為 mp4'
  if (task.kind === 'combine') return `${task.combineSources.length} 支影片・${task.combineMode === 'copy' ? '快速合併' : '相容合併'}`
  const seekMode = task.preciseSeek ? '精細定位' : '快速定位'
  if (task.kind === 'merge') return `${task.segments.length} 個片段・${seekMode}`
  if (task.method === 'duration') return `每 ${formatTimecode(taskSegmentDurationSeconds(task))} 分割${task.segments.length ? `・共 ${task.segments.length} 段` : ''}・${seekMode}`
  if (task.method === 'count') return `平均分割 ${task.splitCount} 段・${seekMode}`
  return `${formatTimecode(task.segments[0]?.start || 0)} → ${formatTimecode(task.segments[0]?.end || 0)}・${seekMode}`
}

function taskRanges(task: VideoTask): VideoSegment[] {
  return buildClipRanges(task)
}

function waitingNeighborIndex(index: number, direction: -1 | 1) {
  for (let candidate = index + direction; candidate >= 0 && candidate < videoTasks.length; candidate += direction) {
    if (videoTasks[candidate].status === '等待中') return candidate
  }
  return -1
}

function canMoveTask(index: number, direction: -1 | 1) {
  return !running.value && videoTasks[index]?.status === '等待中' && waitingNeighborIndex(index, direction) !== -1
}

function moveTask(index: number, direction: -1 | 1) {
  if (!canMoveTask(index, direction)) return
  const targetIndex = waitingNeighborIndex(index, direction)
  const task = videoTasks[index]
  videoTasks[index] = videoTasks[targetIndex]
  videoTasks[targetIndex] = task
}

function canDragTask(task: VideoTask) {
  return !running.value && task.status === '等待中'
}

function reorderTaskRelative(sourceId: string, targetId: string, insertAfter: boolean) {
  const sourceIndex = videoTasks.findIndex((item) => item.id === sourceId)
  const targetIndex = videoTasks.findIndex((item) => item.id === targetId)
  const targetTask = videoTasks[targetIndex]
  if (sourceIndex !== -1 && targetIndex !== -1 && sourceIndex !== targetIndex && targetTask && canDragTask(targetTask)) {
    let insertAt = targetIndex + (insertAfter ? 1 : 0)
    const [moved] = videoTasks.splice(sourceIndex, 1)
    if (sourceIndex < insertAt) insertAt -= 1
    videoTasks.splice(insertAt, 0, moved)
  }
}

function onTaskMouseDown(event: MouseEvent, task: VideoTask) {
  if (event.button !== 0 || !canDragTask(task)) return
  pointerDragSourceId = task.id
  pointerDragStartX = event.clientX
  pointerDragStartY = event.clientY
  pointerDragActive = false
  event.preventDefault()
}

function onTaskMouseMove(event: MouseEvent) {
  if (!pointerDragSourceId) return
  if (!pointerDragActive && Math.hypot(event.clientX - pointerDragStartX, event.clientY - pointerDragStartY) < 5) return
  if (!pointerDragActive) {
    pointerDragActive = true
    draggedTaskId.value = pointerDragSourceId
    document.body.classList.add('task-reordering')
  }
  event.preventDefault()
  const row = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>('[data-task-id]')
  const targetId = row?.dataset.taskId || null
  const targetTask = videoTasks.find((task) => task.id === targetId)
  if (!row || !targetTask || targetId === pointerDragSourceId || !canDragTask(targetTask)) {
    dragTargetTaskId.value = null
    return
  }
  const bounds = row.getBoundingClientRect()
  dragTargetTaskId.value = targetId
  dragTargetAfter.value = event.clientY >= bounds.top + bounds.height / 2
  const edge = 56
  if (event.clientY < edge) window.scrollBy({ top: -12 })
  else if (event.clientY > window.innerHeight - edge) window.scrollBy({ top: 12 })
}

function onTaskMouseUp(event: MouseEvent) {
  // Some WebView/input combinations coalesce mouse-move events during a drag.
  // Re-evaluate the final pointer position so a valid drag still commits on release.
  if (pointerDragSourceId) onTaskMouseMove(event)
  if (pointerDragActive && pointerDragSourceId && dragTargetTaskId.value) {
    reorderTaskRelative(pointerDragSourceId, dragTargetTaskId.value, dragTargetAfter.value)
  }
  finishTaskDrag()
}

function finishTaskDrag() {
  pointerDragSourceId = null
  pointerDragActive = false
  draggedTaskId.value = null
  dragTargetTaskId.value = null
  dragTargetAfter.value = false
  document.body.classList.remove('task-reordering')
}

function toggleTaskRanges(taskId: string) {
  expandedTaskIds.value = expandedTaskIds.value.includes(taskId)
    ? expandedTaskIds.value.filter((id) => id !== taskId)
    : [...expandedTaskIds.value, taskId]
}

function requestEditTask(task: VideoTask) {
  if (task.status === '處理中' || (task.kind !== 'clip' && task.kind !== 'merge')) return
  pendingEditTaskId.value = task.id
}

function cancelEditTask() {
  pendingEditTaskId.value = null
}

function confirmEditTask(task: VideoTask) {
  if (pendingEditTaskId.value !== task.id || task.status === '處理中' || (task.kind !== 'clip' && task.kind !== 'merge')) return
  pendingEditTaskId.value = null
  queueTaskEdit(task.id)
  router.push('/')
}

async function chooseOutputDirectory() {
  const selected = await open({ directory: true, multiple: false, title: '選擇任務輸出資料夾' })
  if (typeof selected === 'string') {
    outputDirectory.value = selected
    localStorage.setItem('vid_output_directory', selected)
  }
}
function requestStopBatch() {
  if (!running.value || stopBatchRequested.value) return
  showStopConfirm.value = true
}
function requestStopTask(task: VideoTask) {
  if (task.status !== '處理中' || stopBatchRequested.value) return
  showStopConfirm.value = true
}
function cancelStopConfirm() {
  showStopConfirm.value = false
}
async function confirmStopBatch() {
  showStopConfirm.value = false
  stopBatchRequested.value = true
  const current = videoTasks.find((task) => task.status === '處理中')
  if (!current) {
    if (!running.value) stopBatchRequested.value = false
    return
  }
  current.message = '正在停止目前任務；後續任務會保留在等待佇列。'
  try {
    await invoke<boolean>('cancel_current_task')
  } catch (error) {
    safetyWarning.value = `停止要求暫時無法傳送；本批仍會在目前處理步驟結束後停止：${String(error)}`
  }
}
function retryTask(task: VideoTask) {
  if (running.value || task.status === '等待中' || task.status === '處理中') return
  task.status = '等待中'
  task.message = ''
  task.progress = undefined
  task.lastRunStartedAt = undefined
  task.lastRunDurationMs = undefined
  task.resultPath = ''
  if (pendingEditTaskId.value === task.id) pendingEditTaskId.value = null
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
function throwIfStopRequested(task: VideoTask) {
  if (stopBatchRequested.value && task.status === '處理中') throw new Error('Task stopped by user')
}
function isProcessCancellation(error: unknown) {
  return String(error).includes('Process cancelled')
}
async function throwIfBatchStopLatched(task: VideoTask) {
  throwIfStopRequested(task)
  try {
    if (!(await invoke<boolean>('is_task_batch_cancelled'))) return
  } catch {
    // The local flag and tracked-process error still protect the normal path. A read failure here
    // must not turn an otherwise successful video task into a false failure.
    return
  }
  stopBatchRequested.value = true
  throw new Error('Task stopped by user')
}
async function clipRange(task: VideoTask, range: VideoSegment, path: string, completedBefore: number) {
  const duration = Math.max(0.001, range.end - range.start)
  const args = task.preciseSeek
    ? ['-i', task.source, '-ss', range.start.toFixed(3), '-t', duration.toFixed(3), '-c', 'copy', path, '-y']
    : ['-ss', range.start.toFixed(3), '-i', task.source, '-t', duration.toFixed(3), '-c', 'copy', path, '-y']
  const ffmpegOutput = await runFfmpegWithProgress(task, args, completedBefore, duration)
  throwIfStopRequested(task)
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
      `剪輯出的片段檔案異常過小（僅 ${(sizeBytes / 1024).toFixed(1)} KB，要求長度 ${expected.toFixed(1)} 秒）：${formatTimecode(range.start)} → ${formatTimecode(range.end)}\n實際指令：${command}\nffmpeg 輸出：${ffmpegOutput.trim() || '(無輸出)'}`,
    )
  }
}
async function runPreprocessTask(task: VideoTask) {
  const duration = Math.max(0.001, await videoDuration(task.source))
  startTaskProgress(task, duration, '正在預先處理')
  const path = destination(task)
  throwIfStopRequested(task)
  await runFfmpegWithProgress(task, ['-i', task.source, '-c', 'copy', path, '-y'], 0, duration)
  throwIfStopRequested(task)
  completeTaskStep(task, '預先處理完成')
  task.resultPath = path
}
async function runClipTask(task: VideoTask) {
  throwIfStopRequested(task)
  const duration = task.method === 'range' || task.segments.length
    ? 0
    : await invoke<number>('get_video_duration', { ffprobePath: ffprobePath.value || null, videoPath: task.source })
  throwIfStopRequested(task)
  const ranges = buildClipRanges(task, duration)
  const totalDuration = Math.max(0.001, ranges.reduce((sum, range) => sum + Math.max(0, range.end - range.start), 0))
  startTaskProgress(task, totalDuration, `準備剪輯 ${ranges.length} 個片段`)
  let completedDuration = 0
  for (let index = 0; index < ranges.length; index += 1) {
    throwIfStopRequested(task)
    if (task.progress) task.progress.label = `正在剪輯片段 ${index + 1}/${ranges.length}`
    await clipRange(task, ranges[index], destination(task, ranges.length > 1 ? `_${String(index + 1).padStart(3, '0')}` : ''), completedDuration)
    throwIfStopRequested(task)
    completedDuration += Math.max(0, ranges[index].end - ranges[index].start)
    if (task.progress) {
      task.progress.completed = Math.min(task.progress.total, completedDuration)
      task.progress.label = `已完成片段 ${index + 1}/${ranges.length}`
    }
  }
}
async function runMergeTask(task: VideoTask) {
  const clipsDuration = Math.max(0.001, task.segments.reduce((sum, segment) => sum + Math.max(0, segment.end - segment.start), 0))
  const totalDuration = clipsDuration * 2
  startTaskProgress(task, totalDuration, `準備剪輯 ${task.segments.length} 個片段`)
  const clips: string[] = []
  let completedDuration = 0
  for (let index = 0; index < task.segments.length; index += 1) {
    throwIfStopRequested(task)
    if (task.progress) task.progress.label = `正在剪輯片段 ${index + 1}/${task.segments.length}`
    const path = destination(task, `_segment_${String(index + 1).padStart(3, '0')}`)
    await clipRange(task, task.segments[index], path, completedDuration)
    throwIfStopRequested(task)
    clips.push(path)
    completedDuration += Math.max(0, task.segments[index].end - task.segments[index].start)
    if (task.progress) {
      task.progress.completed = completedDuration
      task.progress.label = `已完成片段 ${index + 1}/${task.segments.length}`
    }
  }
  throwIfStopRequested(task)
  if (task.progress) task.progress.label = '正在合併片段'
  await concatWithProgress(task, clips, destination(task), parseExtraArgs(task.extraArgs), !task.keepSegments, clipsDuration, clipsDuration)
  throwIfStopRequested(task)
  completeTaskStep(task, '合併完成')
}
async function runCombineTask(task: VideoTask) {
  let totalDuration = 0
  for (const source of task.combineSources) totalDuration += Math.max(0, await videoDuration(source))
  totalDuration = Math.max(0.001, totalDuration)
  startTaskProgress(task, totalDuration, '正在合併影片')
  const path = destination(task)
  throwIfStopRequested(task)
  if (task.combineMode === 'copy') {
    await concatWithProgress(task, task.combineSources, path, [], false, 0, totalDuration)
  } else {
    const inputArgs = task.combineSources.flatMap((file) => ['-i', file])
    const n = task.combineSources.length
    const filterInputs = task.combineSources.map((_, index) => `[${index}:v:0][${index}:a:0]`).join('')
    const filter = `${filterInputs}concat=n=${n}:v=1:a=1[outv][outa]`
    const args = [...inputArgs, '-filter_complex', filter, '-map', '[outv]', '-map', '[outa]', '-c:v', 'libx264', '-c:a', 'aac', path, '-y']
    await runFfmpegWithProgress(task, args, 0, totalDuration)
  }
  throwIfStopRequested(task)
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
  completeTaskStep(task, '合併完成')
}
async function runTasks() {
  if (running.value) return
  if (outputDirectory.value.trim()) localStorage.setItem('vid_output_directory', outputDirectory.value.trim())
  stopBatchRequested.value = false
  running.value = true
  safetyWarning.value = ''
  try {
    await invoke('begin_task_batch')
  } catch (error) {
    running.value = false
    safetyWarning.value = `無法初始化任務佇列，因此沒有開始執行：${String(error)}`
    return
  }
  clockTimer = setInterval(() => { clockNow.value = Date.now() }, 1000)
  const { trigger } = getNotifySettings()
  let processedAny = false
  let hadFailure = false
  try {
    for (;;) {
      if (stopBatchRequested.value) break
      const task = videoTasks.find((item) => item.status === '等待中')
      if (!task) break
      if (!(await ensureEnoughDiskSpace(task))) break
      if (stopBatchRequested.value) break
      processedAny = true
      task.lastRunStartedAt = Date.now()
      task.lastRunDurationMs = undefined
      task.status = '處理中'
      try {
        if (task.kind === 'clip') await runClipTask(task)
        else if (task.kind === 'merge') await runMergeTask(task)
        else if (task.kind === 'combine') await runCombineTask(task)
        else await runPreprocessTask(task)
        await throwIfBatchStopLatched(task)
        task.status = '完成'
        task.message = `已輸出至 ${outputDirectory.value || directoryOf(task.source)}`
        if (trigger === 'each') notifyResult(true)
      } catch (error) {
        if (stopBatchRequested.value || isProcessCancellation(error)) {
          stopBatchRequested.value = true
          task.status = '已取消'
          task.message = '已停止此任務，後續任務仍保留等待。若留下不完整的輸出檔案，重新執行時會覆寫；也可以手動清理。'
        } else {
          task.status = '失敗'
          task.message = String(error)
          hadFailure = true
          if (trigger === 'each') notifyResult(false)
        }
      } finally {
        if (task.lastRunStartedAt) task.lastRunDurationMs = Math.max(0, Date.now() - task.lastRunStartedAt)
      }
      if (stopBatchRequested.value) break
    }
  } finally {
    const wasStopped = stopBatchRequested.value
    running.value = false
    if (clockTimer) clearInterval(clockTimer)
    clockTimer = null
    showStopConfirm.value = false
    stopBatchRequested.value = false
    if (trigger === 'batch' && processedAny && !wasStopped) notifyResult(!hadFailure)
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
          ><button v-if="running" class="btn btn-danger" :disabled="stopBatchRequested" @click="requestStopBatch">{{ stopBatchRequested ? '正在停止…' : '停止整批執行' }}</button
          ><button v-if="scheduledStartAt" class="btn btn-secondary" @click="clearSchedule">取消排程</button
          ><button class="btn btn-primary" :disabled="running || !hasPendingTasks || Boolean(scheduledStartAt)" @click="requestTaskRun">{{ running ? '處理中…' : scheduledStartAt ? '已排程' : scheduleMode === 'now' ? '開始依序執行' : '設定執行時間' }}</button>
        </div>
      </div>
      <div v-if="showStopConfirm" class="stop-confirm">
        <span>確定要停止整批執行嗎？目前任務會標記為已取消，後續任務會保留在等待佇列；已經處理的部分可能留下不完整檔案。</span>
        <div class="stop-confirm-actions">
          <button class="btn btn-secondary compact" @click="cancelStopConfirm">取消</button
          ><button class="btn btn-danger compact" @click="confirmStopBatch">確定停止整批</button>
        </div>
      </div>
      <p class="info">任務會依建立順序執行。原始影片只會被讀取，絕不覆寫。</p>
      <div v-if="safetyWarning" class="safety-warning">{{ safetyWarning }}</div>
      <div class="schedule-panel">
        <div class="schedule-modes">
          <label><input v-model="scheduleMode" type="radio" value="now" :disabled="Boolean(scheduledStartAt)" /> 立即執行</label>
          <label><input v-model="scheduleMode" type="radio" value="delay" :disabled="Boolean(scheduledStartAt)" /> 延後執行</label>
          <label><input v-model="scheduleMode" type="radio" value="clock" :disabled="Boolean(scheduledStartAt)" /> 指定時間</label>
        </div>
        <div v-if="!scheduledStartAt && scheduleMode === 'delay'" class="schedule-fields">
          <label>小時 <input v-model.number="delayHours" type="number" min="0" step="1" /></label>
          <label>分鐘 <input v-model.number="delayMinutes" type="number" min="0" step="1" /></label>
        </div>
        <div v-else-if="!scheduledStartAt && scheduleMode === 'clock'" class="schedule-clock-wrap">
          <div class="schedule-fields clock-fields">
            <label>時 <select v-model.number="scheduledHour"><option v-for="hour in hourOptions" :key="hour" :value="hour">{{ String(hour).padStart(2, '0') }}</option></select></label>
            <span>:</span>
            <label>分 <select v-model.number="scheduledMinute"><option v-for="minute in minuteSecondOptions" :key="minute" :value="minute">{{ String(minute).padStart(2, '0') }}</option></select></label>
            <span>:</span>
            <label>秒 <select v-model.number="scheduledSecond"><option v-for="second in minuteSecondOptions" :key="second" :value="second">{{ String(second).padStart(2, '0') }}</option></select></label>
          </div>
          <small>將安排在 {{ scheduledClockPreview() }}；若今天的時間已過，會自動改為明天。</small>
        </div>
        <div v-if="scheduledStartAt" class="scheduled-summary"><strong>已排程</strong><span>{{ scheduleCountdownText() }}</span><small>程式需保持開啟；若電腦休眠，喚醒後會立即補執行。</small></div>
      </div>
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
      <div
        v-for="(task, index) in videoTasks"
        :key="task.id"
        class="task-row"
        :data-task-id="task.id"
        :class="{
          'task-dragging': draggedTaskId === task.id,
          'task-drag-before': dragTargetTaskId === task.id && !dragTargetAfter,
          'task-drag-after': dragTargetTaskId === task.id && dragTargetAfter,
        }"
      >
        <div class="task-content w-full">
          <div class="task-header">
            <div class="task-heading">
              <strong class="task-title truncate"
                >{{ index + 1 }}.
                {{ task.kind === 'clip' ? '剪輯' : task.kind === 'merge' ? '剪輯＋合併' : task.kind === 'combine' ? '合併影片' : '預先處理' }}</strong
              ><span class="status" :class="task.status">{{ task.status }}</span><span class="task-summary">{{ taskSummary(task) }}</span>
            </div>
            <div class="task-row-actions">
              <div v-if="task.status === '等待中'" class="task-order-controls" aria-label="調整任務順序">
                <button class="task-order-button task-drag-handle" :disabled="running" title="按住並拖曳以調整順序" aria-label="拖曳調整任務順序" @mousedown="onTaskMouseDown($event, task)">⠿</button>
                <button class="task-order-button" :disabled="!canMoveTask(index, -1)" title="向上移動" aria-label="向上移動任務" @click="moveTask(index, -1)">↑</button>
                <button class="task-order-button" :disabled="!canMoveTask(index, 1)" title="向下移動" aria-label="向下移動任務" @click="moveTask(index, 1)">↓</button>
              </div>
              <button
                v-if="task.kind === 'clip' || task.kind === 'merge'"
                class="task-control task-control-range"
                :aria-expanded="expandedTaskIds.includes(task.id)"
                @click="toggleTaskRanges(task.id)"
              >
                {{ expandedTaskIds.includes(task.id) ? '收合範圍' : '查看範圍' }}
              </button>
              <button
                v-if="(task.kind === 'clip' || task.kind === 'merge') && task.status !== '處理中'"
                class="task-control task-control-edit"
                @click="requestEditTask(task)"
              >再編輯</button>
              <button
                v-if="task.status === '完成' || task.status === '失敗' || task.status === '已取消'"
                class="task-control task-control-retry"
                :disabled="running"
                @click="retryTask(task)"
              >{{ task.status === '完成' ? '再次執行' : '重新執行' }}</button>
              <button v-if="task.status === '處理中'" class="btn btn-danger compact task-stop" :disabled="stopBatchRequested" @click="requestStopTask(task)">{{ stopBatchRequested ? '停止中…' : '停止' }}</button>
              <button v-else class="task-control task-control-remove" @click="videoTasks.splice(index, 1)">移除</button>
            </div>
          </div>
          <div v-if="pendingEditTaskId === task.id" class="edit-confirm">
            <span>
              {{ task.status === '等待中' ? '確定要再編輯這筆任務嗎？儲存後會直接更新原任務。' : '確定要再編輯這筆任務嗎？原任務會保留，儲存後將另建新的等待任務。' }}
            </span>
            <div class="edit-confirm-actions">
              <button class="task-control task-control-range" @click="cancelEditTask">取消</button>
              <button class="task-control task-control-edit" @click="confirmEditTask(task)">確定再編輯</button>
            </div>
          </div>
          <div v-if="expandedTaskIds.includes(task.id) && (task.kind === 'clip' || task.kind === 'merge')" class="range-details">
            <div class="range-details-header">
              <strong>剪輯範圍</strong>
              <span v-if="task.previewTimeline">保存 {{ task.previewTimeline.times.length }} 個縮圖時間點（不含圖片）</span>
            </div>
            <div v-if="taskRanges(task).length" class="range-list">
              <div v-for="(range, rangeIndex) in taskRanges(task)" :key="`${range.start}-${range.end}-${rangeIndex}`" class="range-item">
                <span>片段 {{ rangeIndex + 1 }}</span>
                <code>{{ formatTimecode(range.start) }} → {{ formatTimecode(range.end) }}</code>
              </div>
            </div>
            <p v-else class="range-empty">這筆舊任務沒有保存可展開的實際範圍，執行時會依影片長度計算。</p>
          </div>
          <p class="task-file-name truncate w-full" :title="`${task.outputName}.${task.extension}`">{{ task.outputName }}.{{ task.extension }}</p>
          <div v-if="task.progress || task.lastRunDurationMs !== undefined" class="task-progress">
            <div class="task-progress-meta">
              <span v-if="task.progress">{{ task.progress.label }}</span>
              <span v-if="task.status === '處理中'">{{ taskProgressPercent(task) }}% · 已執行 {{ formatDuration(taskElapsedSeconds(task)) }} · {{ taskEtaText(task) }}</span>
              <span v-else-if="task.lastRunDurationMs !== undefined">{{ task.status === '完成' ? '100% · ' : '' }}總處理時間 {{ formatTaskRunDuration(task.lastRunDurationMs) }}</span>
            </div>
            <div v-if="task.progress" class="task-progress-track" :class="{ indeterminate: task.status === '處理中' && task.progress.completed === 0 }">
              <span :style="{ width: `${taskProgressPercent(task)}%` }"></span>
            </div>
          </div>
          <div class="task-output-dir">
            <label>輸出位置</label>
            <input class="w-full" v-model="task.outputDir" :disabled="task.status !== '等待中'" :placeholder="`預設：${outputDirectory || '原始影片所在資料夾'}`" />
            <button v-if="task.status === '等待中'" class="btn btn-secondary compact" @click="chooseTaskOutputDirectory(task)">選擇</button>
            <button v-if="task.status === '等待中' && task.outputDir" class="btn-clear" @click="clearTaskOutputDirectory(task)">清除</button>
          </div>
          <p
            v-if="task.message"
            class="message"
            :class="{ 'message-success': task.status === '完成', 'message-error': task.status === '失敗', 'message-cancelled': task.status === '已取消' }"
          >{{ task.message }}</p>
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
.safety-warning {
  margin: 0 0 1rem;
  padding: 0.8rem 1rem;
  border: 1px solid rgba(245, 158, 11, 0.45);
  border-radius: 9px;
  background: rgba(120, 53, 15, 0.22);
  color: #fcd34d;
  font-size: 0.86rem;
  line-height: 1.5;
}
.schedule-panel {
  margin-bottom: 1rem;
  padding: 0.8rem 0.9rem;
  border: 1px solid rgba(148, 163, 184, 0.16);
  border-radius: 9px;
  background: rgba(15, 23, 42, 0.34);
}
.schedule-modes,
.schedule-fields {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.8rem 1.1rem;
}
.schedule-modes label,
.schedule-fields label {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  color: var(--text-muted);
  font-size: 0.83rem;
}
.schedule-fields {
  margin-top: 0.75rem;
}
.schedule-fields input {
  width: auto;
  min-width: 5.5rem;
  padding: 0.45rem 0.6rem;
}
.schedule-clock-wrap {
  margin-top: 0.75rem;
}
.schedule-clock-wrap .schedule-fields {
  margin-top: 0;
}
.clock-fields select {
  min-width: 4.4rem;
  padding: 0.45rem 0.55rem;
  border: 1px solid var(--border-color);
  border-radius: 7px;
  background: rgba(15, 23, 42, 0.75);
  color: var(--text-color);
}
.schedule-clock-wrap small {
  display: block;
  margin-top: 0.55rem;
  color: var(--text-muted);
}
.scheduled-summary {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.4rem 0.7rem;
  color: #c7d2fe;
  font-size: 0.85rem;
}
.scheduled-summary small {
  width: 100%;
  color: var(--text-muted);
}
.task-row {
  position: relative;
  display: flex;
  gap: 1rem;
  padding: 1rem;
  margin: 0.6rem 0;
  border: 1px solid var(--border-color);
  border-radius: 10px;
  background: rgba(15, 23, 42, 0.45);
  transition: border-color 0.16s ease, opacity 0.16s ease, transform 0.16s ease;
}
.task-row.task-dragging {
  opacity: 0.45;
}
.task-row.task-drag-before::before,
.task-row.task-drag-after::after {
  content: '';
  position: absolute;
  left: 0.75rem;
  right: 0.75rem;
  height: 3px;
  border-radius: 999px;
  background: #818cf8;
  box-shadow: 0 0 10px rgba(129, 140, 248, 0.55);
}
.task-row.task-drag-before::before {
  top: -0.45rem;
}
.task-row.task-drag-after::after {
  bottom: -0.45rem;
}
.task-content {
  min-width: 0;
}
.task-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  width: 100%;
  min-width: 0;
  margin-bottom: 0.7rem;
  padding-bottom: 0.75rem;
  border-bottom: 1px solid rgba(148, 163, 184, 0.12);
}
.task-heading {
  display: flex;
  align-items: center;
  gap: 0.55rem;
  overflow: hidden;
  min-width: 0;
  flex: 1 1 auto;
}
.task-title {
  min-width: 0;
  flex-shrink: 0;
}
.task-summary {
  min-width: 0;
  overflow: hidden;
  color: var(--text-muted);
  font-size: 0.78rem;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.task-summary::before {
  content: '·';
  margin-right: 0.55rem;
  color: rgba(148, 163, 184, 0.55);
}
.task-row-actions {
  display: flex;
  flex-wrap: nowrap;
  justify-content: flex-end;
  align-items: center;
  gap: 0.45rem;
  flex-shrink: 0;
}
.task-order-controls {
  display: inline-flex;
  gap: 0.2rem;
  padding-right: 0.2rem;
  border-right: 1px solid rgba(148, 163, 184, 0.18);
}
.task-order-button {
  display: inline-grid;
  width: 29px;
  height: 34px;
  padding: 0;
  place-items: center;
  border: 1px solid rgba(129, 140, 248, 0.18);
  border-radius: 7px;
  background: rgba(30, 41, 59, 0.55);
  color: #cbd5e1;
  font: inherit;
  cursor: pointer;
}
.task-order-button:hover:not(:disabled) {
  border-color: rgba(129, 140, 248, 0.48);
  background: rgba(99, 102, 241, 0.18);
  color: #fff;
}
.task-order-button:disabled {
  cursor: not-allowed;
  opacity: 0.3;
}
.task-drag-handle:not(:disabled) {
  cursor: grab;
  font-size: 1rem;
  touch-action: none;
  user-select: none;
  -webkit-user-drag: none;
}
.task-drag-handle:not(:disabled):active {
  cursor: grabbing;
}
:global(body.task-reordering) {
  cursor: grabbing;
  user-select: none;
}
.task-control {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-height: 34px;
  padding: 0.42rem 0.72rem;
  border: 1px solid transparent;
  border-radius: 7px;
  font-family: inherit;
  font-size: 0.8rem;
  font-weight: 600;
  line-height: 1;
  cursor: pointer;
  white-space: nowrap;
  transition: border-color 0.18s ease, background-color 0.18s ease, color 0.18s ease;
}
.task-control-range {
  border-color: rgba(129, 140, 248, 0.22);
  background: rgba(99, 102, 241, 0.06);
  color: #c7d2fe;
}
.task-control-range:hover {
  border-color: rgba(129, 140, 248, 0.42);
  background: rgba(99, 102, 241, 0.13);
  color: #eef2ff;
}
.task-control-edit {
  border-color: rgba(129, 140, 248, 0.42);
  background: rgba(99, 102, 241, 0.24);
  color: #eef2ff;
}
.task-control-edit:hover {
  border-color: rgba(165, 180, 252, 0.58);
  background: rgba(99, 102, 241, 0.38);
}
.task-control-retry {
  border-color: rgba(52, 211, 153, 0.32);
  background: rgba(6, 78, 59, 0.16);
  color: #a7f3d0;
}
.task-control-retry:hover:not(:disabled) {
  border-color: rgba(52, 211, 153, 0.56);
  background: rgba(6, 95, 70, 0.32);
  color: #d1fae5;
}
.task-control-remove {
  border-color: rgba(248, 113, 113, 0.2);
  background: rgba(127, 29, 29, 0.05);
  color: #fda4af;
}
.task-control-remove:hover {
  border-color: rgba(248, 113, 113, 0.42);
  background: rgba(127, 29, 29, 0.22);
  color: #fecdd3;
}
.task-control:focus-visible {
  outline: 2px solid rgba(165, 180, 252, 0.75);
  outline-offset: 2px;
}
.task-control:disabled {
  cursor: not-allowed;
  opacity: 0.4;
}
.edit-confirm {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  margin-bottom: 0.7rem;
  padding: 0.7rem 0.8rem;
  border: 1px solid rgba(129, 140, 248, 0.28);
  border-radius: 8px;
  background: rgba(99, 102, 241, 0.09);
  color: #c7d2fe;
  font-size: 0.82rem;
  line-height: 1.45;
}
.edit-confirm-actions {
  display: flex;
  gap: 0.45rem;
  flex-shrink: 0;
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
.task-file-name {
  margin-top: 0 !important;
}
.task-progress {
  margin: 0.65rem 0 0.2rem;
}
.task-progress-meta {
  display: flex;
  justify-content: space-between;
  gap: 0.75rem;
  margin-bottom: 0.35rem;
  color: var(--text-muted);
  font-size: 0.75rem;
}
.task-progress-track {
  position: relative;
  height: 5px;
  overflow: hidden;
  border-radius: 999px;
  background: rgba(148, 163, 184, 0.16);
}
.task-progress-track span {
  display: block;
  height: 100%;
  border-radius: inherit;
  background: linear-gradient(90deg, #6366f1, #22c55e);
  transition: width 0.3s ease;
}
.task-progress-track.indeterminate span {
  width: 35% !important;
  animation: task-progress-slide 1.4s ease-in-out infinite;
}
@keyframes task-progress-slide {
  from { transform: translateX(-110%); }
  to { transform: translateX(310%); }
}
.task-row .message {
  margin-top: 0.55rem;
  white-space: pre-wrap;
  color: var(--text-muted);
}
.task-row .message-success {
  color: #6ee7b7;
}
.task-row .message-error {
  color: #fca5a5;
}
.task-row .message-cancelled {
  color: #fbbf24;
}
.range-details {
  margin-top: 0.75rem;
  padding: 0.75rem;
  border: 1px solid rgba(99, 102, 241, 0.25);
  border-radius: 8px;
  background: rgba(99, 102, 241, 0.08);
}
.range-details-header {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
  color: #e0e7ff;
  font-size: 0.82rem;
}
.range-details-header span {
  color: var(--text-muted);
  font-size: 0.76rem;
}
.range-list {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  max-height: 14rem;
  margin-top: 0.6rem;
  overflow-y: auto;
}
.range-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.4rem 0.5rem;
  border-radius: 6px;
  background: rgba(15, 23, 42, 0.55);
  color: var(--text-muted);
  font-size: 0.8rem;
}
.range-item code {
  color: #e2e8f0;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
}
.range-empty {
  margin-top: 0.6rem !important;
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
@media (max-width: 720px) {
  .task-header {
    align-items: flex-start;
    flex-wrap: wrap;
  }
  .task-row-actions {
    margin-left: auto;
  }
}
</style>
