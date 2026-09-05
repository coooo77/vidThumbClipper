<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { open } from '@tauri-apps/plugin-dialog'
import { useRouter } from 'vue-router'
import { newTaskId, normaliseVideoPath, processedVideos, videoTasks, type ClipMethod, type CombineMode, type TaskKind, type VideoSegment, type VideoTask } from '../taskStore'

interface ThumbnailItem {
  time: number
  timeStr: string
  base64: string
}

type ExpansionMode = 'interval' | 'single' | 'grid'
type QueueStatus = '等待中' | '處理中' | '完成' | '失敗' | '已取消'

interface ExpansionJob {
  id: string
  anchorStartTime: number
  anchorEndTime: number
  mode: ExpansionMode
  interval: number
  singleAnchor: 'start' | 'end'
  cols: number
  rows: number
  label: string
  status: QueueStatus
  message: string
  cancelRequested: boolean
}

const ffmpegPath = ref('')
const ffprobePath = ref('')
const videoPath = ref('')
const originalSourcePath = ref('')
const isProcessing = ref(false)
const isEndpointProcessing = ref(false)
const isExpanding = ref(false)
const isDraggingFile = ref(false)
const showPreprocessHint = ref(false)
const filmstripVisible = ref(true)
const logs = ref<string[]>([])
const progressText = ref('')
const progressPercent = ref(0)

// The visible layout is always editable. Rows are increased automatically when more images are inserted.
const gridCols = ref(7)
const gridRows = ref(5)
const initialGridSlotCount = computed(() => Math.max(1, Math.floor(Number(gridCols.value) || 1) * Math.floor(Number(gridRows.value) || 1)))
const initialThumbnailCount = computed(() => Math.max(0, initialGridSlotCount.value - 2))
const thumbnails = ref<ThumbnailItem[]>([])
const startThumbnail = ref<string | null>(null)
const endThumbnail = ref<string | null>(null)
const activeModalIndex = ref<number | null>(null)
const selectedTimes = ref<number[]>([])
const expansionMode = ref<ExpansionMode>('interval')
const expansionInterval = ref(1)
const singleInsertAnchor = ref<'start' | 'end'>('start')
const expansionCols = ref(2)
const expansionRows = ref(2)
const expansionQueue = ref<ExpansionJob[]>([])
const videoDuration = ref(0)
const activePanel = ref<'load' | 'task' | 'combine'>('load')
const combineFiles = ref<string[]>([])
const combineDragFromIndex = ref<number | null>(null)
const combineMode = ref<CombineMode>('copy')
const combineOutputName = ref('merged_video')
const combineExtension = ref('mp4')
const loadSubPanel = ref<'init' | 'expand'>('init')
const logOpen = ref(false)
const router = useRouter()
const taskKind = ref<TaskKind>('clip')
const taskSourceMode = ref<'original' | 'processed'>('original')
const taskSourceProcessedId = ref('')
const taskSourcePath = computed(() => {
  if (taskSourceMode.value === 'processed') {
    const match = processedVideos.value.find((task) => task.id === taskSourceProcessedId.value)
    return match?.resultPath || ''
  }
  return originalSourcePath.value
})
// The default output name should always reflect whichever source is actually selected (original vs.
// a specific processed video) — otherwise switching sources can leave a misleading filename behind
// from whatever was loaded first, even though the correct file is the one actually used to cut.
watch(taskSourcePath, (path) => {
  if (taskOutputNameAuto.value && path) taskOutputName.value = `${baseNameWithoutExt(path)}${taskNameSuffix.value}`
})
const taskMethod = ref<ClipMethod>('range')
const taskDurationHours = ref(6)
const taskSplitCount = ref(5)
const mergeSegments = ref<VideoSegment[]>([])
const taskNameSuffix = ref('_edit')
const taskOutputName = ref('processed_video')
const taskOutputNameAuto = ref(true)
const taskExtension = ref('mp4')
const taskExtraArgs = ref('')
const taskPreciseSeek = ref(true)
const taskKeepSegments = ref(false)

// Requires exactly two ADJACENT thumbnails (by CURRENT array position — resolved live, since another
// job inserting thumbnails elsewhere can shift indices while this selection is still pending) — only
// meaningful for "擴充兩張縮圖之間" (inserting between two neighbours).
const selectedPairIsAdjacent = computed(() => {
  if (selectedTimes.value.length !== 2) return false
  const indices = selectedTimes.value.map((time) => indexForTime(time)).sort((a, b) => a - b)
  return indices[1] - indices[0] === 1
})

const selectedPair = computed(() => {
  if (!selectedPairIsAdjacent.value) return null
  const indices = selectedTimes.value.map((time) => indexForTime(time)).sort((a, b) => a - b)
  return { first: indices[0], second: indices[1] }
})

// Any two selected thumbnails (adjacent or not) — this is what task creation / the merge list should
// use. Selection is stored as the actual time values (not array indices), so it stays pointed at the
// same two frames even if other expansion jobs insert thumbnails elsewhere while this is pending.
const selectedTimeRange = computed(() => {
  if (selectedTimes.value.length !== 2) return null
  const [start, end] = [...selectedTimes.value].sort((a, b) => a - b)
  return { start, end }
})

// True when the currently selected adjacent pair already has a pending/running expansion job —
// the same pair can't be queued twice until the existing job finishes.
const isPairLocked = computed(() => {
  if (!selectedPairIsAdjacent.value) return false
  const [start, end] = [...selectedTimes.value].sort((a, b) => a - b)
  return expansionQueue.value.some((job) => (job.status === '等待中' || job.status === '處理中') && job.anchorStartTime === start && job.anchorEndTime === end)
})

// Thumbnail time values that currently bound a pending/running job, for highlighting on the grid.
const lockedTimeSet = computed(() => {
  const set = new Set<number>()
  for (const job of expansionQueue.value) {
    if (job.status === '等待中' || job.status === '處理中') {
      set.add(job.anchorStartTime)
      set.add(job.anchorEndTime)
    }
  }
  return set
})

// Ordered start → thumbnails → end sequence backing the enlarged-preview prev/next navigation and filmstrip.
const modalSequence = computed(() => {
  const list: { time: number; src: string; label: string }[] = []
  if (startThumbnail.value) list.push({ time: 0, src: startThumbnail.value, label: '起點' })
  for (const item of thumbnails.value) list.push({ time: item.time, src: item.base64, label: item.timeStr })
  if (endThumbnail.value) list.push({ time: videoDuration.value, src: endThumbnail.value, label: '終點' })
  return list
})
const activeModalItem = computed(() => (activeModalIndex.value !== null ? (modalSequence.value[activeModalIndex.value] ?? null) : null))

// Auto-scroll the filmstrip so the active item is in view — covers opening the modal, prev/next
// navigation, and clicking a different filmstrip thumbnail, since all of those just change this index.
const filmstripItemRefs = ref<HTMLElement[]>([])
watch(activeModalIndex, async (index) => {
  if (index === null) return
  await nextTick()
  filmstripItemRefs.value[index]?.scrollIntoView({ behavior: 'smooth', inline: 'center', block: 'nearest' })
})

const newCaptureCount = computed(() => {
  if (expansionMode.value === 'interval') return Math.max(0, Math.ceil((endTime.value - startTime.value) / Math.max(1, expansionInterval.value)) - 1)
  if (expansionMode.value === 'single') return buildSingleInsertTime() === null ? 0 : 1
  return Math.max(1, expansionCols.value) * Math.max(1, expansionRows.value)
})

// Used only by the adjacency-bound expansion feature below.
const startTime = computed(() => (selectedPair.value ? Math.min(...selectedTimes.value) : 0))
const endTime = computed(() => (selectedPair.value ? Math.max(...selectedTimes.value) : 0))

function loadPaths() {
  ffmpegPath.value = localStorage.getItem('vid_ffmpeg_path') || ''
  ffprobePath.value = localStorage.getItem('vid_ffprobe_path') || ''
  taskNameSuffix.value = localStorage.getItem('vid_task_suffix') || '_edit'
}

function baseName(path: string) {
  const index = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'))
  return index >= 0 ? path.slice(index + 1) : path
}

function baseNameWithoutExt(path: string) {
  const name = baseName(path)
  const dot = name.lastIndexOf('.')
  return dot > 0 ? name.slice(0, dot) : name
}

function dirName(path: string) {
  const index = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'))
  return index >= 0 ? path.slice(0, index + 1) : ''
}

let unlistenDragDrop: (() => void) | null = null
const infoHintWrap = ref<HTMLElement | null>(null)

function handleDocumentClick(event: MouseEvent) {
  if (showPreprocessHint.value && infoHintWrap.value && !infoHintWrap.value.contains(event.target as Node)) {
    showPreprocessHint.value = false
  }
}

onMounted(async () => {
  loadPaths()
  document.addEventListener('click', handleDocumentClick)
  try {
    unlistenDragDrop = await getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === 'over') {
        isDraggingFile.value = true
      } else if (event.payload.type === 'drop') {
        isDraggingFile.value = false
        const paths = event.payload.paths
        if (paths.length > 1) queuePreprocessTasks(paths)
        else if (paths.length === 1) handleDroppedVideo(paths[0])
      } else {
        isDraggingFile.value = false
      }
    })
  } catch (error) {
    addLog(`無法啟用拖曳載入：${String(error)}`)
  }
})

onUnmounted(() => {
  unlistenDragDrop?.()
  document.removeEventListener('click', handleDocumentClick)
  if (autoClearFinishedTimer) clearTimeout(autoClearFinishedTimer)
})

defineExpose({ loadPaths })

function formatSeconds(seconds: number) {
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const s = Math.floor(seconds % 60)
  const ms = Math.round((seconds - Math.floor(seconds)) * 1000)
  const base = [h, m, s].map((value) => String(value).padStart(2, '0')).join(':')
  return ms ? `${base}.${String(ms).padStart(3, '0')}` : base
}

function addLog(message: string) {
  logs.value.unshift(`[${new Date().toLocaleTimeString()}] ${message}`)
}

function clearLogs() {
  logs.value = []
}

function modalGoPrev() {
  if (activeModalIndex.value === null || activeModalIndex.value <= 0) return
  activeModalIndex.value -= 1
}

function modalGoNext() {
  if (activeModalIndex.value === null || activeModalIndex.value >= modalSequence.value.length - 1) return
  activeModalIndex.value += 1
}

function normaliseLayout() {
  gridCols.value = Math.max(1, Math.floor(gridCols.value || 1))
  gridRows.value = Math.max(1, Math.ceil(thumbnails.value.length / gridCols.value))
}

function updateLayoutFromUser() {
  gridCols.value = Math.max(1, Math.floor(gridCols.value || 1))
  gridRows.value = Math.max(1, Math.floor(gridRows.value || 1))
  localStorage.setItem('vid_grid_cols', String(gridCols.value))
  localStorage.setItem('vid_grid_rows', String(gridRows.value))
}

// The user's last-chosen grid size should carry over to the next video's initial capture — only the
// automatic row growth from "擴充兩張縮圖之間" should reset (that's handled separately per video).
function loadGridPreference() {
  const cols = Number(localStorage.getItem('vid_grid_cols'))
  const rows = Number(localStorage.getItem('vid_grid_rows'))
  gridCols.value = Number.isFinite(cols) && cols > 0 ? cols : 7
  gridRows.value = Number.isFinite(rows) && rows > 0 ? rows : 5
}

function applyCurrentLayout() {
  updateLayoutFromUser()
  const minimumRows = Math.max(1, Math.ceil(thumbnails.value.length / gridCols.value))
  if (gridRows.value < minimumRows) gridRows.value = minimumRows
  addLog(`目前縮圖排列已設為 ${gridCols.value} × ${gridRows.value}。`)
}

function loadVideoFromPath(path: string) {
  loadPaths()
  videoPath.value = path
  originalSourcePath.value = path
  loadGridPreference()
  loadSubPanel.value = 'init'
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  videoDuration.value = 0
  expansionQueue.value = []
  selectedTimes.value = []
  taskOutputName.value = `${baseNameWithoutExt(path)}${taskNameSuffix.value}`
  taskOutputNameAuto.value = true
  taskSourceMode.value = 'original'
  taskSourceProcessedId.value = ''
  addLog(`已選擇影片：${path}`)
}

async function selectVideo() {
  if (isProcessing.value || isEndpointProcessing.value || isExpanding.value) return addLog('目前有縮圖正在處理，請等待完成後再切換影片。')
  const selected = await open({
    multiple: false,
    directory: false,
    title: '選擇影片檔案',
    filters: [{ name: 'Video', extensions: ['mp4', 'mkv', 'avi', 'mov', 'flv', 'webm', 'm4v', 'ts', 'mpeg', 'mpg', 'wmv'] }],
  })
  if (typeof selected !== 'string') return
  loadVideoFromPath(selected)
}

// Dropped files aren't extension-checked here — anything can be dropped, and if ffprobe/ffmpeg can't
// actually parse it as a video, that surfaces as a clear error toast once thumbnail capture runs.
function handleDroppedVideo(path: string) {
  if (isProcessing.value || isEndpointProcessing.value || isExpanding.value) return addLog('目前有縮圖正在處理，請等待完成後再切換影片。')
  activePanel.value = 'load'
  loadVideoFromPath(path)
}

async function selectVideosForPreprocessing() {
  const selected = await open({
    multiple: true,
    directory: false,
    title: '選擇要預先處理的影片（可多選）',
    filters: [{ name: 'Video', extensions: ['mp4', 'mkv', 'avi', 'mov', 'flv', 'webm', 'm4v', 'ts', 'mpeg', 'mpg', 'wmv'] }],
  })
  const paths = Array.isArray(selected) ? selected : typeof selected === 'string' ? [selected] : []
  if (!paths.length) return
  queuePreprocessTasks(paths)
}

function queuePreprocessTasks(paths: string[]) {
  const seenSources = new Set<string>()
  const pathsToQueue = paths.filter((path) => {
    const source = path.trim()
    if (!source) return false
    const sourceKey = normaliseVideoPath(source)
    if (seenSources.has(sourceKey)) return false
    seenSources.add(sourceKey)

    // Selecting the same file again while an existing preprocess task is still in the queue
    // would create the same output twice. Keep one task per source instead.
    return !videoTasks.some((task) => task.kind === 'preprocess' && normaliseVideoPath(task.source) === sourceKey)
  })

  for (const path of pathsToQueue) {
    videoTasks.push({
      id: newTaskId(),
      kind: 'preprocess',
      method: 'range',
      source: path,
      segments: [],
      durationHours: taskDurationHours.value,
      splitCount: taskSplitCount.value,
      outputName: `${baseNameWithoutExt(path)}_preprocessed`,
      extension: 'mp4',
      extraArgs: '',
      keepSegments: false,
      outputDir: dirName(path),
      resultPath: '',
      preciseSeek: false,
      combineSources: [],
      combineMode: 'copy',
      status: '等待中',
      message: '',
    })
  }
  if (!pathsToQueue.length) {
    addLog('選取的影片都已在預先處理佇列中。')
    return
  }
  addLog(`已加入 ${pathsToQueue.length} 筆預先處理任務，前往「影片處理任務」執行。`)
  router.push('/tasks')
}

function useProcessedVideo(task: VideoTask) {
  if (!task.resultPath) return
  if (isProcessing.value || isEndpointProcessing.value || isExpanding.value) return addLog('目前有縮圖正在處理，請等待完成後再切換影片。')
  loadPaths()
  videoPath.value = task.resultPath
  originalSourcePath.value = task.source
  loadGridPreference()
  loadSubPanel.value = 'init'
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  videoDuration.value = 0
  expansionQueue.value = []
  selectedTimes.value = []
  taskOutputName.value = `${baseNameWithoutExt(task.source)}${taskNameSuffix.value}`
  taskOutputNameAuto.value = true
  taskSourceMode.value = 'original'
  taskSourceProcessedId.value = ''
  addLog(`已選擇處理過的影片：${task.resultPath}`)
}

function removeProcessedVideo(id: string) {
  const index = videoTasks.findIndex((task) => task.id === id)
  if (index !== -1) videoTasks.splice(index, 1)
}

function clearProcessedVideos() {
  for (let index = videoTasks.length - 1; index >= 0; index -= 1) {
    if (videoTasks[index].kind === 'preprocess' && videoTasks[index].status === '完成') videoTasks.splice(index, 1)
  }
}

async function captureAtTimes(times: number[], offset: number, shouldStop?: () => boolean) {
  const result: ThumbnailItem[] = []
  for (let i = 0; i < times.length; i += 1) {
    if (shouldStop && shouldStop()) break
    // Do not show 100% until the awaited backend capture has actually returned. Previously the
    // last item displayed "40/40 (100%)" while ffmpeg could still be blocked, which made it look
    // safe to start an expansion even though isProcessing was still true.
    progressPercent.value = Math.round((i / times.length) * 100)
    progressText.value = `正在擷取 ${i + 1}/${times.length} 張（${progressPercent.value}%）`
    const base64 = await invoke<string>('capture_single_thumbnail', {
      ffmpegPath: ffmpegPath.value || null,
      videoPath: videoPath.value,
      time: times[i].toFixed(3),
      index: offset + i,
    })
    result.push({ time: times[i], timeStr: formatSeconds(times[i]), base64: `data:image/jpeg;base64,${base64}` })
    progressPercent.value = Math.round(((i + 1) / times.length) * 100)
    progressText.value = `已完成擷取 ${i + 1}/${times.length} 張（${progressPercent.value}%）`
  }
  return result
}

async function generateGridThumbnails() {
  if (!videoPath.value) return addLog('請先選擇影片檔案。')
  updateLayoutFromUser()
  // The selected N × M layout includes the two endpoint cells. Only the remaining cells need
  // regular timeline captures; otherwise a 4 × 5 layout incorrectly renders 22 images.
  const count = initialThumbnailCount.value
  loadPaths()
  isProcessing.value = true
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  selectedTimes.value = []
  let initialCaptureSucceeded = false
  let duration = 0
  try {
    duration = await invoke<number>('get_video_duration', { ffprobePath: ffprobePath.value || null, videoPath: videoPath.value })
    videoDuration.value = duration
    const times = Array.from({ length: count }, (_, index) => ((index + 1) * duration) / (count + 1))
    thumbnails.value = await captureAtTimes(times, 0)
    normaliseLayout()
    initialCaptureSucceeded = true
    addLog(`已建立 ${thumbnails.value.length} 張中間縮圖，起點與終點將在背景擷取。`)
  } catch (error) {
    addLog(`擷取失敗：${String(error)}`)
  } finally {
    isProcessing.value = false
    progressText.value = ''
    progressPercent.value = 0
    // If an older UI state queued an expansion while the initial capture was still running,
    // resume it now that the source thumbnails are genuinely ready.
    if (initialCaptureSucceeded && expansionQueue.value.some((job) => job.status === '等待中')) processExpansionQueue()
    if (initialCaptureSucceeded) void captureEndpointThumbnails(duration)
  }
}

async function captureEndpointThumbnails(duration: number) {
  isEndpointProcessing.value = true
  addLog('開始背景擷取起點與終點縮圖；這不會阻塞擴充佇列。')
  try {
    const endpointResults = await Promise.allSettled([
      invoke<string>('capture_single_thumbnail', {
        ffmpegPath: ffmpegPath.value || null,
        videoPath: videoPath.value,
        time: '0.000',
        index: -1,
      }),
      invoke<string>('capture_single_thumbnail', {
        ffmpegPath: ffmpegPath.value || null,
        videoPath: videoPath.value,
        // Stay a little farther inside the file than the container's reported end. Some long MP4s
        // report a final timestamp for which input-side seek produces an empty image.
        time: Math.max(0, duration - 2).toFixed(3),
        index: -2,
      }),
    ])
    const [startResult, endResult] = endpointResults
    if (startResult.status === 'fulfilled') startThumbnail.value = `data:image/jpeg;base64,${startResult.value}`
    else addLog(`起點縮圖擷取失敗：${String(startResult.reason)}`)
    if (endResult.status === 'fulfilled') endThumbnail.value = `data:image/jpeg;base64,${endResult.value}`
    else addLog(`終點縮圖擷取失敗：${String(endResult.reason)}`)
    if (startResult.status === 'fulfilled' && endResult.status === 'fulfilled') addLog('起點與終點縮圖已完成。')
  } finally {
    isEndpointProcessing.value = false
  }
}

function toggleSelection(time: number) {
  if (selectedTimes.value.includes(time)) {
    selectedTimes.value = selectedTimes.value.filter((item) => item !== time)
    return
  }
  if (selectedTimes.value.length === 2) selectedTimes.value = [time]
  else selectedTimes.value = [...selectedTimes.value, time]
}

// Live preview only (uses the current UI selections) — actual execution uses buildTimesForJob below.
function buildSingleInsertTime(): number | null {
  if (!selectedPair.value) return null
  const interval = Math.max(1, Math.floor(expansionInterval.value || 1))
  const time = singleInsertAnchor.value === 'start' ? startTime.value + interval : endTime.value - interval
  if (time <= startTime.value || time >= endTime.value) return null
  return time
}

// Times to capture for a queued job, computed from the settings snapshotted when it was added
// (not the live UI refs, which may have moved on to configuring a different pair by the time this runs).
function buildTimesForJob(job: ExpansionJob): number[] {
  const start = job.anchorStartTime
  const end = job.anchorEndTime
  if (job.mode === 'interval') {
    const interval = Math.max(1, Math.floor(job.interval || 1))
    const times: number[] = []
    for (let time = start + interval; time < end; time += interval) times.push(time)
    return times
  }
  if (job.mode === 'single') {
    const interval = Math.max(1, Math.floor(job.interval || 1))
    const time = job.singleAnchor === 'start' ? start + interval : end - interval
    return time > start && time < end ? [time] : []
  }
  const count = Math.max(1, Math.floor(job.cols || 1)) * Math.max(1, Math.floor(job.rows || 1))
  const gap = (end - start) / (count + 1)
  return Array.from({ length: count }, (_, index) => start + gap * (index + 1))
}

// A job's own anchors never move (existing thumbnails are never touched), but its right-anchor's
// position in the array can shift as other jobs elsewhere insert thumbnails — resolve it live.
function indexForTime(time: number): number {
  if (time <= 0) return -1
  if (time >= videoDuration.value) return thumbnails.value.length
  const found = thumbnails.value.findIndex((item) => item.time === time)
  return found === -1 ? thumbnails.value.length : found
}

function queueExpansionJob() {
  if (isProcessing.value) return addLog('初始縮圖仍在處理中，請等進度完成後再加入擴充任務。')
  if (!selectedPairIsAdjacent.value || !selectedPair.value) return addLog('請選擇兩張相鄰的縮圖後再加入截圖任務。')
  if (isPairLocked.value) return addLog('這個範圍正在截圖佇列中，請等待完成後再選取。')
  const start = startTime.value
  const end = endTime.value
  const interval = Math.max(1, Math.floor(Number(expansionInterval.value) || 1))
  if ((expansionMode.value === 'interval' || expansionMode.value === 'single') && end - start <= interval) {
    return addLog(
      `無法加入截圖任務：選取範圍只有 ${formatSeconds(end - start)}，不超過目前設定的 ${interval} 秒，因此沒有可插入的時間點。請選擇更大的範圍或較小的秒數。`,
    )
  }
  const job: ExpansionJob = {
    id: newTaskId(),
    anchorStartTime: start,
    anchorEndTime: end,
    mode: expansionMode.value,
    interval: expansionInterval.value,
    singleAnchor: singleInsertAnchor.value,
    cols: expansionCols.value,
    rows: expansionRows.value,
    label: `${formatSeconds(start)} → ${formatSeconds(end)}`,
    status: '等待中',
    message: '',
    cancelRequested: false,
  }
  expansionQueue.value.push(job)
  selectedTimes.value = []
  addLog(`已加入截圖任務：${job.label}`)
  processExpansionQueue()
}

function removeExpansionJob(id: string) {
  const index = expansionQueue.value.findIndex((job) => job.id === id && job.status === '等待中')
  if (index !== -1) expansionQueue.value.splice(index, 1)
}

// Signals the in-flight capture loop to stop after its current step — whatever thumbnails were
// already captured are still inserted (see runExpansionJob), nothing already done is thrown away.
function cancelExpansionJob(id: string) {
  const job = expansionQueue.value.find((item) => item.id === id && item.status === '處理中')
  if (job) job.cancelRequested = true
}

function clearFinishedExpansionJobs() {
  for (let index = expansionQueue.value.length - 1; index >= 0; index -= 1) {
    const status = expansionQueue.value[index].status
    if (status === '完成' || status === '失敗' || status === '已取消') expansionQueue.value.splice(index, 1)
  }
}

// Auto-cleanup: any change to the queue (a job starting, finishing, being added, etc.) resets a 5s
// timer; once the queue has been quiet for 5s, finished/failed jobs are cleared automatically so a
// long batch doesn't leave the list full of stale entries requiring a manual click.
let autoClearFinishedTimer: ReturnType<typeof setTimeout> | null = null
watch(
  expansionQueue,
  () => {
    if (autoClearFinishedTimer) clearTimeout(autoClearFinishedTimer)
    autoClearFinishedTimer = setTimeout(() => {
      clearFinishedExpansionJobs()
      autoClearFinishedTimer = null
    }, 5000)
  },
  { deep: true },
)

async function runExpansionJob(job: ExpansionJob) {
  const times = buildTimesForJob(job)
  if (!times.length) {
    const span = Math.max(0, job.anchorEndTime - job.anchorStartTime)
    const interval = Math.max(1, Math.floor(Number(job.interval) || 1))
    const message = `沒有符合條件的新時間點（範圍 ${formatSeconds(span)}，設定 ${interval} 秒）`
    job.message = message
    addLog(`擴充失敗（${job.label}）：${message}`)
    throw new Error(message)
  }
  const insertAt = indexForTime(job.anchorEndTime)
  const additions = await captureAtTimes(times, thumbnails.value.length, () => job.cancelRequested)
  thumbnails.value.splice(insertAt, 0, ...additions)
  normaliseLayout()
  if (job.cancelRequested) {
    addLog(`已取消「${job.label}」，已插入取消前完成的 ${additions.length} 張縮圖。`)
  } else {
    addLog(`已在「${job.label}」之間插入 ${additions.length} 張新縮圖，版面已自動調整為 ${gridCols.value} × ${gridRows.value}。`)
  }
}

// Sequential worker: keeps draining "等待中" jobs (in the order they were added) for as long as
// there are any, so jobs queued while this is already running just get picked up on the next loop.
async function processExpansionQueue() {
  if (isProcessing.value || isExpanding.value) return
  isExpanding.value = true
  loadPaths()
  try {
    for (;;) {
      const job = expansionQueue.value.find((item) => item.status === '等待中')
      if (!job) break
      job.status = '處理中'
      try {
        await runExpansionJob(job)
        job.status = job.cancelRequested ? '已取消' : '完成'
      } catch (error) {
        job.status = '失敗'
        job.message = String(error)
        addLog(`擴充失敗（${job.label}）：${String(error)}`)
      }
    }
  } finally {
    isExpanding.value = false
    progressText.value = ''
    progressPercent.value = 0
  }
}

function addSegmentToMergeList() {
  if (!selectedTimeRange.value) return addLog('請先選擇兩個時間點作為片段。')
  const { start, end } = selectedTimeRange.value
  mergeSegments.value.push({ start, end })
  selectedTimes.value = []
  addLog(`已加入片段 ${formatSeconds(start)} → ${formatSeconds(end)}，目前共 ${mergeSegments.value.length} 段。`)
}

// Used to outline every thumbnail that currently falls inside an already-added merge segment.
function isInMergeSegment(time: number): boolean {
  return mergeSegments.value.some((segment) => time >= segment.start && time <= segment.end)
}

function removeMergeSegment(index: number) {
  mergeSegments.value.splice(index, 1)
}

const dragFromIndex = ref<number | null>(null)
function onSegmentDragStart(index: number) {
  dragFromIndex.value = index
}
function onSegmentDrop(index: number) {
  if (dragFromIndex.value === null || dragFromIndex.value === index) return
  const [moved] = mergeSegments.value.splice(dragFromIndex.value, 1)
  mergeSegments.value.splice(index, 0, moved)
  dragFromIndex.value = null
}

function createClipTask() {
  if (!taskSourcePath.value) return addLog('請先選擇影片檔案，或選擇要使用的已處理影片。')
  let segments: VideoSegment[] = []
  if (taskMethod.value === 'range') {
    if (!selectedTimeRange.value) return addLog('請先在縮圖中選擇兩個時間點作為剪輯範圍。')
    segments = [{ start: selectedTimeRange.value.start, end: selectedTimeRange.value.end }]
  }
  videoTasks.push({
    id: newTaskId(),
    kind: 'clip',
    method: taskMethod.value,
    source: taskSourcePath.value,
    segments,
    durationHours: taskDurationHours.value,
    splitCount: taskSplitCount.value,
    outputName: taskOutputName.value.trim() || 'clip',
    extension: taskExtension.value.trim() || 'mp4',
    extraArgs: taskExtraArgs.value,
    keepSegments: taskKeepSegments.value,
    outputDir: '',
    resultPath: '',
    preciseSeek: taskPreciseSeek.value,
    combineSources: [],
    combineMode: 'copy',
    status: '等待中',
    message: '',
  })
  addLog(`已建立剪輯任務：${taskOutputName.value}，前往「影片處理任務」執行。`)
  selectedTimes.value = []
  router.push('/tasks')
}

function createMergeTask() {
  if (!taskSourcePath.value) return addLog('請先選擇影片檔案，或選擇要使用的已處理影片。')
  if (mergeSegments.value.length < 2) return addLog('請至少加入兩個片段才能建立剪輯＋合併任務。')
  videoTasks.push({
    id: newTaskId(),
    kind: 'merge',
    method: 'range',
    source: taskSourcePath.value,
    segments: mergeSegments.value.map((segment) => ({ ...segment })),
    durationHours: taskDurationHours.value,
    splitCount: taskSplitCount.value,
    outputName: taskOutputName.value.trim() || 'merged_video',
    extension: taskExtension.value.trim() || 'mp4',
    extraArgs: taskExtraArgs.value,
    keepSegments: taskKeepSegments.value,
    outputDir: '',
    resultPath: '',
    preciseSeek: taskPreciseSeek.value,
    combineSources: [],
    combineMode: 'copy',
    status: '等待中',
    message: '',
  })
  addLog(`已建立剪輯＋合併任務，共 ${mergeSegments.value.length} 段，前往「影片處理任務」執行。`)
  mergeSegments.value = []
  router.push('/tasks')
}

async function selectFilesToCombine() {
  const selected = await open({
    multiple: true,
    directory: false,
    title: '選擇要合併的影片（可多選，依選取順序加入）',
    filters: [{ name: 'Video', extensions: ['mp4', 'mkv', 'avi', 'mov', 'flv', 'webm', 'm4v', 'ts', 'mpeg', 'mpg', 'wmv'] }],
  })
  const paths = Array.isArray(selected) ? selected : typeof selected === 'string' ? [selected] : []
  if (!paths.length) return
  combineFiles.value = [...combineFiles.value, ...paths]
}

function removeCombineFile(index: number) {
  combineFiles.value.splice(index, 1)
}

function onCombineDragStart(index: number) {
  combineDragFromIndex.value = index
}

function onCombineDrop(index: number) {
  if (combineDragFromIndex.value === null || combineDragFromIndex.value === index) return
  const [moved] = combineFiles.value.splice(combineDragFromIndex.value, 1)
  combineFiles.value.splice(index, 0, moved)
  combineDragFromIndex.value = null
}

function createCombineTask() {
  if (combineFiles.value.length < 2) return addLog('請至少加入兩支影片才能建立合併任務。')
  videoTasks.push({
    id: newTaskId(),
    kind: 'combine',
    method: 'range',
    source: combineFiles.value[0],
    segments: [],
    durationHours: taskDurationHours.value,
    splitCount: taskSplitCount.value,
    outputName: combineOutputName.value.trim() || 'merged_video',
    extension: combineExtension.value.trim() || 'mp4',
    extraArgs: '',
    keepSegments: false,
    outputDir: dirName(combineFiles.value[0]),
    resultPath: '',
    preciseSeek: false,
    combineSources: [...combineFiles.value],
    combineMode: combineMode.value,
    status: '等待中',
    message: '',
  })
  addLog(`已建立合併任務，共 ${combineFiles.value.length} 支影片，前往「影片處理任務」執行。`)
  combineFiles.value = []
  router.push('/tasks')
}
</script>

<template>
  <div class="main-layout">
    <div class="settings-panel">
      <div class="panel-tabs">
        <button class="panel-tab" :class="{ active: activePanel === 'load' }" @click="activePanel = 'load'">載入影片</button>
        <button class="panel-tab" :class="{ active: activePanel === 'task' }" @click="activePanel = 'task'">建立任務</button>
        <button class="panel-tab" :class="{ active: activePanel === 'combine' }" @click="activePanel = 'combine'">合併影片</button>
      </div>

      <section v-if="activePanel === 'load'" class="card">
        <div class="subsection">
          <h3 class="subsection-title">影片來源</h3>
          <div class="input-group no-bottom" :class="{ 'drop-target': isDraggingFile }">
            <label>影片檔案</label>
            <div class="input-with-btn">
              <input v-model="videoPath" placeholder="請選擇影片，或將影片檔案拖曳到視窗中" readonly /><button class="btn btn-secondary" @click="selectVideo">
                選擇影片
              </button>
            </div>
            <p class="field-hint">也可以直接把影片檔案拖曳到這個視窗中載入。</p>
          </div>
          <div class="preprocess-row">
            <button class="btn btn-secondary compact" @click="selectVideosForPreprocessing">選擇影片進行預先處理（可多選）</button>
            <div class="info-hint-wrap" ref="infoHintWrap">
              <button type="button" class="info-hint" @click="showPreprocessHint = !showPreprocessHint">說明</button>
              <div v-if="showPreprocessHint" class="info-popover">
                若擷取的秒數跟原始影片對不上，可以多選幾支影片加入預先處理（remux 成 mp4，修正時間軸），到「影片處理任務」執行後回來這裡選用。
              </div>
            </div>
          </div>
          <div v-if="processedVideos.length" class="processed-list">
            <div class="processed-list-header">
              <span>已處理影片（{{ processedVideos.length }}）</span><button class="btn-clear" @click="clearProcessedVideos">清除全部</button>
            </div>
            <div v-for="task in processedVideos" :key="task.id" class="processed-item">
              <span class="processed-name" :title="task.resultPath">{{ task.outputName }}.{{ task.extension }}</span>
              <button class="btn btn-primary compact" @click="useProcessedVideo(task)">使用</button>
              <button class="btn-clear" @click="removeProcessedVideo(task.id)">移除</button>
            </div>
          </div>
        </div>

        <hr class="divider" />

        <div class="panel-tabs sub-tabs">
          <button class="panel-tab" :class="{ active: loadSubPanel === 'init' }" @click="loadSubPanel = 'init'">建立初始縮圖</button>
          <button class="panel-tab" :class="{ active: loadSubPanel === 'expand' }" @click="loadSubPanel = 'expand'">擴充兩張縮圖之間</button>
        </div>

        <div v-if="loadSubPanel === 'init'" class="subsection">
          <div class="row">
            <div class="col input-group"><label>欄（N）</label><input v-model.number="gridCols" type="number" min="1" @change="updateLayoutFromUser" /></div>
            <div class="col input-group"><label>列（M）</label><input v-model.number="gridRows" type="number" min="1" @change="updateLayoutFromUser" /></div>
          </div>
          <p class="info-text">初始擷取 {{ initialThumbnailCount }} 張中間縮圖，連同起點與終點共 {{ initialGridSlotCount }} 格；之後可隨時自行改為任意 N × M 排列。</p>
          <button class="btn btn-primary btn-glow w-full" :disabled="isProcessing || isEndpointProcessing || isExpanding" @click="generateGridThumbnails">
            {{ isProcessing ? '處理中…' : '建立縮圖' }}
          </button>
        </div>

        <div v-else class="subsection">
          <p class="info-text">在右側依序選擇兩張相鄰縮圖，再選擇插入方式。</p>
          <div class="mode-row">
            <label><input v-model="expansionMode" type="radio" value="interval" /> 固定秒數</label
            ><label><input v-model="expansionMode" type="radio" value="single" /> 插入單張</label
            ><label><input v-model="expansionMode" type="radio" value="grid" /> 等分 Grid</label>
          </div>
          <div v-if="expansionMode === 'interval' || expansionMode === 'single'" class="input-group">
            <label>秒數（至少 1 秒）</label><input v-model.number="expansionInterval" type="number" min="1" />
            <div class="quick-set-row">
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 1 }" @click="expansionInterval = 1">1 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 10 }" @click="expansionInterval = 10">10 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 60 }" @click="expansionInterval = 60">60 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 300 }" @click="expansionInterval = 300">300 秒</button>
            </div>
          </div>
          <div v-if="expansionMode === 'single'" class="mode-row">
            <label><input v-model="singleInsertAnchor" type="radio" value="start" /> 從起點（左側縮圖）往後 {{ expansionInterval }} 秒</label
            ><label><input v-model="singleInsertAnchor" type="radio" value="end" /> 從終點（右側縮圖）往前 {{ expansionInterval }} 秒</label>
          </div>
          <div v-if="expansionMode === 'grid'" class="row">
            <div class="col input-group"><label>Grid 欄（N）</label><input v-model.number="expansionCols" type="number" min="1" /></div>
            <div class="col input-group"><label>Grid 列（M）</label><input v-model.number="expansionRows" type="number" min="1" /></div>
          </div>
          <p v-if="selectedPair" class="selection-summary" :class="{ muted: isPairLocked }">
            範圍：{{ formatSeconds(startTime) }} → {{ formatSeconds(endTime) }}，預計插入 {{ newCaptureCount }} 張{{
              isPairLocked ? '（此範圍正在截圖佇列中）' : ''
            }}
          </p>
          <p v-else class="selection-summary muted">尚未選擇兩張相鄰縮圖</p>
          <button
            class="btn btn-primary w-full"
            :disabled="isProcessing || isPairLocked"
            @click="queueExpansionJob"
          >
            加入截圖任務
          </button>
          <div v-if="expansionQueue.length" class="expansion-queue">
            <div class="expansion-queue-header">
              <span>截圖佇列（{{ expansionQueue.length }}）</span
              ><button
                v-if="expansionQueue.some((job) => job.status === '完成' || job.status === '失敗' || job.status === '已取消')"
                class="btn-clear"
                @click="clearFinishedExpansionJobs"
              >
                清除已完成
              </button>
            </div>
            <div class="expansion-queue-list">
              <div v-for="job in expansionQueue" :key="job.id" class="expansion-queue-item">
                <span class="job-label">{{ job.label }}</span>
                <span class="job-status" :class="job.status">{{ job.status }}</span>
                <button v-if="job.status === '等待中'" class="btn-clear" @click="removeExpansionJob(job.id)">移除</button>
                <button v-else-if="job.status === '處理中'" class="btn-clear" @click="cancelExpansionJob(job.id)">取消</button>
              </div>
            </div>
          </div>
        </div>

        <div v-if="progressText" class="progress-container">
          <div class="progress-bar-bg"><div class="progress-bar-fill" :style="{ width: `${progressPercent}%` }"></div></div>
          <div class="progress-text">{{ progressText }}</div>
        </div>
      </section>

      <section v-if="activePanel === 'task'" class="card task-builder-card">
        <h2 class="card-title">建立處理任務</h2>
        <div class="mode-row">
          <label><input v-model="taskKind" type="radio" value="clip" /> 剪輯</label>
          <label><input v-model="taskKind" type="radio" value="merge" /> 剪輯＋合併</label>
        </div>

        <div class="input-group">
          <label>此任務要處理的影片</label>
          <div class="mode-row">
            <label><input v-model="taskSourceMode" type="radio" value="original" /> 原始影片</label>
            <label :class="{ muted: !processedVideos.length }"
              ><input v-model="taskSourceMode" type="radio" value="processed" :disabled="!processedVideos.length" /> 已處理影片</label
            >
          </div>
          <select v-if="taskSourceMode === 'processed'" v-model="taskSourceProcessedId">
            <option value="" disabled>請選擇已處理影片</option>
            <option v-for="task in processedVideos" :key="task.id" :value="task.id">{{ task.outputName }}.{{ task.extension }}</option>
          </select>
          <p class="selection-summary break-all" :class="{ muted: !taskSourcePath }">
            {{ taskSourcePath ? `將對「${baseName(taskSourcePath)}」建立任務` : '尚未選擇來源影片' }}
          </p>
        </div>

        <template v-if="taskKind === 'clip'">
          <div class="mode-row">
            <label><input v-model="taskMethod" type="radio" value="range" /> 時間範圍</label>
            <label><input v-model="taskMethod" type="radio" value="duration" /> 時間長度</label>
            <label><input v-model="taskMethod" type="radio" value="count" /> 依數量分割</label>
          </div>
          <p v-if="taskMethod === 'range'" class="selection-summary" :class="{ muted: !selectedTimeRange }">
            {{
              selectedTimeRange ? `剪輯範圍：${formatSeconds(selectedTimeRange.start)} → ${formatSeconds(selectedTimeRange.end)}` : '請先在縮圖中選擇兩個時間點'
            }}
          </p>
          <div v-else-if="taskMethod === 'duration'" class="input-group">
            <label>每段時間長度（小時）</label>
            <input v-model.number="taskDurationHours" type="number" min="0.1" step="0.5" />
          </div>
          <div v-else class="input-group">
            <label>平均分割數量</label>
            <input v-model.number="taskSplitCount" type="number" min="2" step="1" />
          </div>
        </template>

        <template v-else>
          <p class="info-text">在右側縮圖選擇任意兩個時間點，點「加入合併清單」加入片段；清單可拖曳調整順序，預設為加入順序。</p>
          <div v-if="mergeSegments.length" class="merge-list">
            <div
              v-for="(segment, index) in mergeSegments"
              :key="index"
              class="merge-item"
              draggable="true"
              @dragstart="onSegmentDragStart(index)"
              @dragover.prevent
              @drop="onSegmentDrop(index)"
            >
              <span class="merge-handle">⠿</span>
              <span class="merge-index">{{ index + 1 }}</span>
              <span class="merge-range">{{ formatSeconds(segment.start) }} → {{ formatSeconds(segment.end) }}</span>
              <button class="merge-remove" @click="removeMergeSegment(index)">移除</button>
            </div>
          </div>
          <p v-else class="selection-summary muted">尚未加入任何片段。</p>
          <label class="checkbox-row"><input v-model="taskKeepSegments" type="checkbox" /> 合併後保留個別剪輯片段</label>
        </template>

        <div class="row">
          <div class="col input-group"><label>輸出檔名</label><input v-model="taskOutputName" @input="taskOutputNameAuto = false" /></div>
          <div class="col input-group"><label>副檔名</label><input v-model="taskExtension" /></div>
        </div>
        <div class="input-group"><label>額外 ffmpeg 參數（合併時套用，以空白分隔）</label><input v-model="taskExtraArgs" placeholder="例如：-crf 20" /></div>
        <label class="checkbox-row"><input v-model="taskPreciseSeek" type="checkbox" /> 精確 seek（較慢，適合時間戳異常的長時間錄影）</label>

        <button v-if="taskKind === 'clip'" class="btn btn-primary btn-glow w-full" @click="createClipTask">建立剪輯任務</button>
        <button v-else class="btn btn-primary btn-glow w-full" :disabled="mergeSegments.length < 2" @click="createMergeTask">建立剪輯＋合併任務</button>
      </section>

      <section v-if="activePanel === 'combine'" class="card">
        <h2 class="card-title">合併多支影片</h2>
        <p class="info-text">選擇幾支現成的影片檔案，依加入順序合併成一支，跟剪輯無關。</p>
        <button class="btn btn-secondary w-full" @click="selectFilesToCombine">選擇影片檔案（可多選）</button>

        <div v-if="combineFiles.length" class="merge-list">
          <div
            v-for="(file, index) in combineFiles"
            :key="`${file}-${index}`"
            class="merge-item"
            draggable="true"
            @dragstart="onCombineDragStart(index)"
            @dragover.prevent
            @drop="onCombineDrop(index)"
          >
            <span class="merge-handle">⠿</span>
            <span class="merge-index">{{ index + 1 }}</span>
            <span class="merge-range combine-filename" :title="file">{{ file }}</span>
            <button class="merge-remove" @click="removeCombineFile(index)">移除</button>
          </div>
        </div>
        <p v-else class="selection-summary muted">尚未加入任何影片。</p>

        <div class="mode-row">
          <label><input v-model="combineMode" type="radio" value="copy" /> 快速合併（不重新編碼，要求所有檔案格式一致）</label>
          <label><input v-model="combineMode" type="radio" value="reencode" /> 相容合併（重新編碼，較慢但格式不同也能合併）</label>
        </div>
        <p v-if="combineMode === 'reencode'" class="info-text">相容合併需要每支影片都有音訊軌，純無聲的影片檔案會失敗。</p>

        <div class="row">
          <div class="col input-group"><label>輸出檔名</label><input v-model="combineOutputName" /></div>
          <div class="col input-group"><label>副檔名</label><input v-model="combineExtension" /></div>
        </div>

        <button class="btn btn-primary btn-glow w-full" :disabled="combineFiles.length < 2" @click="createCombineTask">建立合併任務</button>
      </section>
    </div>

    <div class="preview-log-panel">
      <section class="card preview-card">
        <div class="card-header-with-btn">
          <h2 class="card-title">縮圖列表（{{ thumbnails.length }} 張）</h2>
        </div>
        <div v-if="thumbnails.length" class="current-layout-controls">
          <span>目前顯示排列</span>
          <label>欄 N <input v-model.number="gridCols" type="number" min="1" /></label>
          <span>×</span>
          <label>列 M <input v-model.number="gridRows" type="number" min="1" /></label>
          <button class="btn btn-primary compact" @click="applyCurrentLayout">套用排列</button>
          <template v-if="taskKind === 'merge' && selectedTimeRange">
            <span class="row-divider">|</span>
            <span>已選擇範圍：{{ formatSeconds(selectedTimeRange.start) }} → {{ formatSeconds(selectedTimeRange.end) }}</span>
            <button class="btn btn-primary compact" @click="addSegmentToMergeList">＋ 加入合併清單</button>
          </template>
        </div>
        <p class="info-text">點一下選取，點兩下檢視原圖。擴充後會自動增加列數以容納新縮圖。</p>
        <div v-if="thumbnails.length" class="grid-container" :style="{ gridTemplateColumns: `repeat(${gridCols}, minmax(0, 1fr))` }">
          <div
            class="grid-item-img endpoint"
            :class="{ selected: selectedTimes.includes(0), 'in-segment': mergeSegments.length > 0 && isInMergeSegment(0), locked: lockedTimeSet.has(0) }"
            @click="toggleSelection(0)"
            @dblclick="startThumbnail && (activeModalIndex = 0)"
          >
            <div class="img-wrapper"><img v-if="startThumbnail" :src="startThumbnail" alt="起點" /><span class="endpoint-label">起點</span></div>
            <div class="time-tag">00:00:00</div>
          </div>
          <div
            v-for="(item, index) in thumbnails"
            :key="`${item.time}-${index}`"
            class="grid-item-img"
            :class="{
              selected: selectedTimes.includes(item.time),
              'in-segment': mergeSegments.length > 0 && isInMergeSegment(item.time),
              locked: lockedTimeSet.has(item.time),
            }"
            @click="toggleSelection(item.time)"
            @dblclick="activeModalIndex = (startThumbnail ? 1 : 0) + index"
          >
            <div class="img-wrapper"><img :src="item.base64" :alt="`縮圖 ${item.timeStr}`" /></div>
            <div class="time-tag">{{ item.timeStr }}</div>
          </div>
          <div
            class="grid-item-img endpoint"
            :class="{
              selected: selectedTimes.includes(videoDuration),
              'in-segment': mergeSegments.length > 0 && isInMergeSegment(videoDuration),
              locked: lockedTimeSet.has(videoDuration),
            }"
            @click="toggleSelection(videoDuration)"
            @dblclick="endThumbnail && (activeModalIndex = modalSequence.length - 1)"
          >
            <div class="img-wrapper"><img v-if="endThumbnail" :src="endThumbnail" alt="終點" /><span class="endpoint-label">終點</span></div>
            <div class="time-tag">{{ formatSeconds(videoDuration) }}</div>
          </div>
        </div>
        <div v-else class="preview-placeholder">建立縮圖後會顯示在這裡。</div>
      </section>
    </div>
    <div v-if="activeModalItem" class="modal-overlay" @click.self="activeModalIndex = null">
      <button class="modal-close-btn" @click="activeModalIndex = null">×</button>
      <div class="modal-main">
        <button class="modal-nav-btn modal-nav-prev" :disabled="activeModalIndex === 0" @click.stop="modalGoPrev">‹</button>
        <div class="modal-content-scroller">
          <div class="modal-time-label">{{ activeModalItem.label }}</div>
          <img :src="activeModalItem.src" class="original-size-img" alt="原始縮圖" />
        </div>
        <button class="modal-nav-btn modal-nav-next" :disabled="activeModalIndex === modalSequence.length - 1" @click.stop="modalGoNext">›</button>
        <button class="modal-filmstrip-toggle" @click="filmstripVisible = !filmstripVisible">{{ filmstripVisible ? '隱藏縮圖列 ▾' : '顯示縮圖列 ▸' }}</button>
      </div>
      <div v-if="filmstripVisible" class="modal-filmstrip">
        <div
          v-for="(item, index) in modalSequence"
          :key="index"
          ref="filmstripItemRefs"
          class="filmstrip-item"
          :class="{ active: index === activeModalIndex }"
          @click="activeModalIndex = index"
        >
          <img :src="item.src" :alt="item.label" />
          <span class="filmstrip-time">{{ item.label }}</span>
        </div>
      </div>
    </div>

    <div v-if="isDraggingFile" class="drag-overlay">
      <div class="drag-overlay-inner">放開滑鼠以載入影片<br /><span class="drag-overlay-hint">單一檔案：直接載入／多個檔案：加入預先處理任務</span></div>
    </div>

    <button class="log-fab" :class="{ active: logOpen }" title="操作紀錄" @click="logOpen = !logOpen">
      <span class="log-fab-icon">☰</span>
      <span v-if="logs.length && !logOpen" class="log-fab-badge">{{ logs.length }}</span>
    </button>
    <div v-if="logOpen" class="log-fullscreen">
      <div class="log-fullscreen-header">
        <h2>操作紀錄</h2>
        <div class="log-fullscreen-actions">
          <button class="btn-clear" @click="clearLogs">清除</button>
          <button class="log-close-btn" @click="logOpen = false">×</button>
        </div>
      </div>
      <div class="log-fullscreen-body">
        <div v-if="!logs.length" class="log-empty">尚無操作紀錄。</div>
        <div v-for="(log, index) in logs" :key="index" class="log-item">{{ log }}</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.main-layout {
  display: grid;
  grid-template-columns: minmax(300px, 390px) minmax(0, 1fr);
  gap: 1.25rem;
}
.settings-panel,
.preview-log-panel {
  display: flex;
  flex-direction: column;
  gap: 1.25rem;
}
.settings-panel {
  position: relative;
  z-index: 5;
}
.panel-tabs {
  display: flex;
  gap: 0.5rem;
  padding: 0.3rem;
  border-radius: 10px;
  background: rgba(15, 23, 42, 0.5);
  border: 1px solid var(--border-color);
}
.panel-tabs.sub-tabs {
  margin-bottom: 1.1rem;
  padding: 0.2rem;
}
.panel-tabs.sub-tabs .panel-tab {
  font-size: 0.82rem;
  padding: 0.5rem 0.4rem;
}
.panel-tab {
  flex: 1;
  border: 0;
  background: transparent;
  color: var(--text-muted);
  font-weight: 600;
  font-size: 0.9rem;
  padding: 0.6rem 0.5rem;
  border-radius: 7px;
  cursor: pointer;
  transition: all 0.2s ease;
}
.panel-tab:hover {
  color: var(--text-color);
}
.panel-tab.active {
  background: var(--primary-color);
  color: #fff;
}
.subsection-title {
  font-size: 0.95rem;
  font-weight: 600;
  color: var(--text-color);
  margin-bottom: 0.9rem;
}
.subsection:first-child {
  margin-top: 0;
}
.divider {
  border: none;
  border-top: 1px solid var(--border-color);
  margin: 1.5rem 0;
}
.row {
  display: flex;
  gap: 0.75rem;
}
.col {
  flex: 1;
}
.no-bottom,
.row .input-group {
  margin-bottom: 0;
}
.info-text,
.selection-summary {
  color: var(--text-muted);
  font-size: 0.85rem;
  line-height: 1.5;
  margin: 0.45rem 0 1rem;
}
.muted {
  opacity: 0.7;
}
.break-all {
  word-break: break-all;
}
select {
  width: 100%;
  background: rgba(15, 23, 42, 0.6);
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 0.75rem 1rem;
  color: var(--text-color);
  font-size: 0.95rem;
  margin-top: 0.5rem;
}
select:focus {
  border-color: var(--primary-color);
  box-shadow: 0 0 0 2px rgba(99, 102, 241, 0.2);
  outline: none;
}
.field-hint {
  color: var(--text-muted);
  font-size: 0.78rem;
  margin-top: 0.4rem;
}
.input-group.drop-target .input-with-btn {
  border-radius: 8px;
  box-shadow: 0 0 0 2px var(--primary-color);
}
.drag-overlay {
  position: fixed;
  inset: 0;
  z-index: 300;
  background: rgba(15, 23, 42, 0.75);
  backdrop-filter: blur(2px);
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}
.drag-overlay-inner {
  padding: 1.5rem 2.5rem;
  border: 3px dashed var(--primary-color);
  border-radius: 16px;
  background: rgba(15, 23, 42, 0.9);
  color: #fff;
  font-size: 1.3rem;
  font-weight: 700;
  text-align: center;
}
.drag-overlay-hint {
  display: inline-block;
  margin-top: 0.5rem;
  color: #cbd5e1;
  font-size: 0.85rem;
  font-weight: 400;
}
.preprocess-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.6rem;
  margin-top: 0.75rem;
}
.info-hint-wrap {
  position: relative;
  display: inline-flex;
}
.info-hint {
  border: 1px solid var(--border-color);
  background: rgba(15, 23, 42, 0.5);
  color: var(--text-muted);
  font-size: 0.75rem;
  padding: 0.2rem 0.6rem;
  border-radius: 999px;
  cursor: pointer;
}
.info-hint:hover {
  color: var(--text-color);
  border-color: rgba(255, 255, 255, 0.3);
}
.info-popover {
  position: absolute;
  top: calc(100% + 8px);
  left: 0;
  z-index: 20;
  width: 260px;
  padding: 0.7rem 0.85rem;
  border-radius: 8px;
  background: #111827;
  border: 1px solid var(--border-color);
  color: var(--text-muted);
  font-size: 0.78rem;
  line-height: 1.55;
  box-shadow: 0 10px 24px rgba(0, 0, 0, 0.45);
}
.processed-list {
  margin-top: 0.9rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 0.6rem 0.75rem;
}
.processed-list-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 0.8rem;
  color: var(--text-muted);
  margin-bottom: 0.4rem;
}
.processed-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.4rem 0;
  border-top: 1px solid var(--border-color);
}
.processed-item:first-of-type {
  border-top: 0;
}
.processed-name {
  flex: 1;
  min-width: 0;
  font-size: 0.82rem;
  color: var(--text-color);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.mode-row {
  display: flex;
  gap: 1.2rem;
  color: var(--text-muted);
  font-size: 0.9rem;
  margin: 0 0 1rem;
}
.mode-row input {
  margin-right: 0.35rem;
}
.quick-set-row {
  display: flex;
  gap: 0.4rem;
  margin-top: 0.5rem;
}
.quick-set-btn {
  flex: 1;
  border: 1px solid var(--border-color);
  background: rgba(15, 23, 42, 0.5);
  color: var(--text-muted);
  font-size: 0.8rem;
  font-weight: 600;
  padding: 0.4rem 0;
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.2s ease;
}
.quick-set-btn:hover {
  color: var(--text-color);
  border-color: rgba(255, 255, 255, 0.2);
}
.quick-set-btn.active {
  background: var(--primary-color);
  border-color: var(--primary-color);
  color: #fff;
}
.task-builder-card .row {
  margin-top: 1rem;
}
.checkbox-row {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  color: var(--text-muted);
  font-size: 0.9rem;
  margin: 0.25rem 0 1.25rem;
}
.checkbox-row input {
  width: auto;
}
.merge-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  margin: 0 0 1rem;
}
.merge-item {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.55rem 0.75rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  background: rgba(15, 23, 42, 0.5);
  cursor: grab;
}
.merge-item:active {
  cursor: grabbing;
}
.merge-handle {
  color: var(--text-muted);
}
.merge-index {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.5rem;
  height: 1.5rem;
  border-radius: 50%;
  background: rgba(99, 102, 241, 0.2);
  color: #c7d2fe;
  font-size: 0.75rem;
  font-weight: 700;
  flex-shrink: 0;
}
.merge-range {
  flex: 1;
  font: 0.85rem monospace;
  color: var(--text-color);
}
.combine-filename {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 0.8rem;
}
.merge-remove {
  border: 0;
  background: none;
  color: #fca5a5;
  font-size: 0.8rem;
  cursor: pointer;
}
.expansion-queue {
  margin-top: 0.9rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 0.6rem 0.75rem;
}
.expansion-queue-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 0.8rem;
  color: var(--text-muted);
  margin-bottom: 0.4rem;
}
.expansion-queue-list {
  max-height: 260px;
  overflow-y: auto;
}
.expansion-queue-item {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.4rem 0;
  border-top: 1px solid var(--border-color);
  font-size: 0.82rem;
}
.expansion-queue-item:first-of-type {
  border-top: 0;
}
.job-label {
  flex: 1;
  min-width: 0;
  font:
    0.8rem ui-monospace,
    monospace;
  color: var(--text-color);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.job-status {
  padding: 0.1rem 0.5rem;
  border-radius: 999px;
  background: #334155;
  color: #e2e8f0;
  font-size: 0.74rem;
  white-space: nowrap;
}
.job-status.完成 {
  background: rgba(16, 185, 129, 0.2);
  color: #6ee7b7;
}
.job-status.失敗 {
  background: rgba(239, 68, 68, 0.2);
  color: #fca5a5;
}
.job-status.處理中 {
  background: rgba(99, 102, 241, 0.25);
  color: #c7d2fe;
}
.job-status.已取消 {
  background: rgba(148, 163, 184, 0.2);
  color: #cbd5e1;
}
.progress-container {
  margin: 0 0.2rem;
}
.progress-bar-bg {
  height: 8px;
  overflow: hidden;
  border-radius: 5px;
  background: rgba(255, 255, 255, 0.08);
}
.progress-bar-fill {
  height: 100%;
  background: linear-gradient(90deg, #818cf8, #6366f1);
  transition: width 0.2s;
}
.progress-text {
  margin-top: 0.4rem;
  color: var(--text-muted);
  font-size: 0.8rem;
  text-align: right;
}
/* Grid height = 100vh minus the fixed header block: 64px header + 16px gap to content + 32px
   app-container bottom padding (see App.vue .app-header / .app-main-content / .app-container). */
.preview-card {
  display: flex;
  flex-direction: column;
  height: calc(100vh - 112px);
  min-height: 420px;
}
.compact {
  padding: 0.45rem 0.75rem;
  font-size: 0.8rem;
}
.grid-container {
  display: grid;
  gap: 3px;
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 3px;
  background: #020617;
}
.grid-item-img {
  background: #000;
  cursor: pointer;
  border: 2px solid transparent;
  min-width: 0;
}
.grid-item-img.selected {
  border-color: #f59e0b;
  box-shadow: 0 0 0 2px rgba(245, 158, 11, 0.3);
}
.grid-item-img.in-segment {
  outline: 3px solid #34d399;
  outline-offset: -3px;
}
.grid-item-img.in-segment.selected {
  outline: none;
}
.grid-item-img.locked {
  box-shadow: inset 0 0 0 3px rgba(96, 165, 250, 0.85);
}
.img-wrapper {
  position: relative;
  aspect-ratio: 16 / 9;
}
.img-wrapper img {
  width: 100%;
  height: 100%;
  display: block;
  object-fit: cover;
}
.time-tag {
  padding: 0.25rem;
  text-align: center;
  font: 0.7rem monospace;
  color: #e5e7eb;
  background: #111827;
}
.preview-placeholder {
  display: grid;
  flex: 1;
  min-height: 260px;
  place-items: center;
  color: var(--text-muted);
  border: 1px dashed var(--border-color);
}
.current-layout-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.45rem;
  margin: 0 0 1rem;
  padding: 0.65rem 0.75rem;
  border-radius: 8px;
  background: rgba(99, 102, 241, 0.1);
  color: var(--text-muted);
  font-size: 0.85rem;
}
.current-layout-controls label {
  display: flex;
  align-items: center;
  gap: 0.25rem;
}
.current-layout-controls input {
  width: 4.5rem;
  padding: 0.35rem 0.45rem;
  font-size: 0.85rem;
}
.row-divider {
  opacity: 0.4;
}
.log-fab {
  position: fixed;
  right: 1.5rem;
  bottom: 1.5rem;
  z-index: 150;
  width: 3.4rem;
  height: 3.4rem;
  border-radius: 50%;
  border: 1px solid var(--border-color);
  background: var(--primary-color);
  color: #fff;
  font-size: 1.3rem;
  cursor: pointer;
  box-shadow: 0 8px 24px 0 rgba(0, 0, 0, 0.4);
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.2s ease;
}
.log-fab:hover {
  background: var(--primary-hover);
}
.log-fab.active {
  background: var(--secondary-color);
}
.log-fab-badge {
  position: absolute;
  top: -2px;
  right: -2px;
  min-width: 1.25rem;
  height: 1.25rem;
  padding: 0 0.3rem;
  border-radius: 999px;
  background: #ef4444;
  color: #fff;
  font-size: 0.68rem;
  font-weight: 700;
  display: flex;
  align-items: center;
  justify-content: center;
}
.log-fullscreen {
  position: fixed;
  inset: 0;
  z-index: 200;
  display: flex;
  flex-direction: column;
  padding: 1.5rem 2rem 2rem;
  background: rgba(3, 7, 18, 0.97);
  backdrop-filter: blur(8px);
}
.log-fullscreen-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 1.25rem;
  padding-bottom: 1rem;
  border-bottom: 1px solid var(--border-color);
}
.log-fullscreen-header h2 {
  font-size: 1.2rem;
}
.log-fullscreen-actions {
  display: flex;
  align-items: center;
  gap: 1.25rem;
}
.log-close-btn {
  border: 0;
  background: none;
  color: var(--text-color);
  font-size: 2rem;
  line-height: 1;
  cursor: pointer;
}
.log-fullscreen-body {
  flex: 1;
  overflow-y: auto;
  color: var(--text-muted);
  font:
    0.85rem ui-monospace,
    monospace;
}
.log-fullscreen-body .log-item {
  padding: 0.5rem 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
}
.btn-clear {
  border: 0;
  background: none;
  color: var(--text-muted);
  cursor: pointer;
  font-size: 0.85rem;
}
.modal-overlay {
  position: fixed;
  inset: 0;
  z-index: 200;
  background: rgba(3, 7, 18, 0.92);
  display: flex;
  flex-direction: column;
}
.modal-main {
  position: relative;
  flex: 1;
  min-height: 0;
  padding: 2rem;
  display: flex;
  align-items: center;
  justify-content: center;
}
.modal-filmstrip-toggle {
  position: absolute;
  right: 1rem;
  bottom: 1rem;
  z-index: 202;
  padding: 0.5rem 0.9rem;
  border-radius: 999px;
  border: 1px solid rgba(255, 255, 255, 0.15);
  background: rgba(51, 65, 85, 0.85);
  color: #fff;
  font-size: 0.8rem;
  cursor: pointer;
}
.modal-filmstrip-toggle:hover {
  background: rgba(71, 85, 105, 0.95);
}
.modal-content-scroller {
  max-width: 100%;
  max-height: 100%;
  overflow: auto;
  padding: 1rem;
  background: #020617;
  text-align: center;
}
.modal-time-label {
  color: #e5e7eb;
  font:
    0.85rem ui-monospace,
    monospace;
  margin-bottom: 0.6rem;
}
.original-size-img {
  display: block;
  max-width: none;
  margin: 0 auto;
}
.modal-close-btn {
  position: fixed;
  top: 1rem;
  right: 1rem;
  width: 2.5rem;
  height: 2.5rem;
  border: 0;
  border-radius: 50%;
  background: #334155;
  color: white;
  font-size: 1.6rem;
  cursor: pointer;
  z-index: 201;
}
.modal-nav-btn {
  position: absolute;
  top: 50%;
  transform: translateY(-50%);
  width: 3rem;
  height: 3rem;
  border-radius: 50%;
  border: 0;
  background: rgba(51, 65, 85, 0.85);
  color: #fff;
  font-size: 1.8rem;
  line-height: 1;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 201;
}
.modal-nav-btn:disabled {
  opacity: 0.25;
  cursor: default;
}
.modal-nav-btn:not(:disabled):hover {
  background: rgba(71, 85, 105, 0.95);
}
.modal-nav-prev {
  left: 1rem;
}
.modal-nav-next {
  right: 1rem;
}
.modal-filmstrip {
  flex-shrink: 0;
  height: 200px;
  display: flex;
  gap: 6px;
  padding: 10px;
  overflow-x: auto;
  overflow-y: hidden;
  background: rgba(0, 0, 0, 0.55);
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}
.filmstrip-item {
  flex: 0 0 auto;
  width: 280px;
  height: 100%;
  cursor: pointer;
  border: 2px solid transparent;
  border-radius: 4px;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  background: #000;
}
.filmstrip-item.active {
  border-color: #f59e0b;
}
.filmstrip-item img {
  width: 100%;
  flex: 1;
  min-height: 0;
  object-fit: cover;
  display: block;
}
.filmstrip-time {
  font:
    0.68rem ui-monospace,
    monospace;
  color: #cbd5e1;
  text-align: center;
  padding: 2px 0;
  background: rgba(0, 0, 0, 0.6);
  flex-shrink: 0;
}
.endpoint .img-wrapper {
  background: linear-gradient(135deg, #1e293b, #334155);
}
.endpoint-label {
  position: absolute;
  top: 3px;
  left: 3px;
  padding: 0.1rem 0.45rem;
  border-radius: 4px;
  background: rgba(0, 0, 0, 0.72);
  color: #fbbf24;
  font-size: 0.68rem;
  font-weight: 700;
  letter-spacing: 0.02em;
  z-index: 1;
}
@media (max-width: 900px) {
  .main-layout {
    grid-template-columns: 1fr;
  }
}
@media (max-width: 600px) {
  .preview-card {
    height: calc(100vh - 142px);
  }
}
</style>
