<script setup lang="ts">
import { computed, nextTick, onActivated, onMounted, onUnmounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { open, save } from '@tauri-apps/plugin-dialog'
import { useRouter } from 'vue-router'
import {
  newTaskId,
  normaliseVideoPath,
  processedVideos,
  takeTaskEditRequest,
  videoTasks,
  type ClipMethod,
  type CombineMode,
  type TaskKind,
  type TaskPreviewTimeline,
  type VideoSegment,
  type VideoTask,
} from '../taskStore'
import { clearWorkspaceDraft, loadWorkspaceDraft, saveWorkspaceDraft } from '../workspaceStore'
import { directoryOf as dirName, formatTimecode as formatSeconds, taskSegmentDurationSeconds } from '../videoUtils'
import {
  getThumbnailSettings,
  isLegacyFastSettings,
  normaliseThumbnailSettings,
  saveThumbnailSettings,
  settingsForPreset,
  type ThumbnailPreset,
  type ThumbnailSeekMode,
  type ThumbnailSettings,
} from '../thumbnailSettings'

interface ThumbnailItem {
  time: number
  timeStr: string
  base64: string
}

interface BatchSplitSource {
  path: string
  duration: number
  loading: boolean
  error: string
}

interface SimilarThumbnailGroup {
  id: number
  times: number[]
}

type ExpansionMode = 'interval' | 'single' | 'count'
type QueueStatus = '等待中' | '處理中' | '完成' | '失敗' | '已取消'
type ModalImageSize = 'fit' | 'original'
type SimilarityKeepMode = 'front' | 'back' | 'both'

interface ExpansionJob {
  id: string
  anchorStartTime: number
  anchorEndTime: number
  mode: ExpansionMode
  interval: number
  singleAnchor: 'start' | 'end'
  count: number
  label: string
  status: QueueStatus
  message: string
  cancelRequested: boolean
  times: number[]
}

const ffmpegPath = ref('')
const ffprobePath = ref('')
const videoPath = ref('')
const originalSourcePath = ref('')
const isProcessing = ref(false)
const initialCaptureCancelRequested = ref(false)
const isEndpointProcessing = ref(false)
const isExpanding = ref(false)
const isDraggingFile = ref(false)
const showPreprocessHint = ref(false)
const filmstripVisible = ref(true)
const logs = ref<string[]>([])
const progressText = ref('')
const progressPercent = ref(0)
const thumbnailPreset = ref<ThumbnailPreset>('legacy')
const thumbnailMaxHeight = ref(720)
const thumbnailOriginalSize = ref(true)
const thumbnailQuality = ref(5)
const thumbnailSeekMode = ref<ThumbnailSeekMode>('fast')
const thumbnailHardwareAcceleration = ref(false)
const showAdvancedCaptureSettings = ref(false)
const thumbnailSettingsLocked = computed(() => isProcessing.value || isEndpointProcessing.value || isExpanding.value)
const legacyFastCaptureActive = computed(() => isLegacyFastSettings(currentThumbnailSettings()))
const workspaceRecovered = ref(false)
let workspaceReady = false
let workspaceSaveTimer: ReturnType<typeof setTimeout> | null = null

const initialCaptureMode = ref<'count' | 'interval'>('count')
const initialThumbnailCount = ref(33)
const initialInterval = ref(4)
const intervalThumbnailCount = computed(() => {
  const interval = Number(initialInterval.value)
  if (!Number.isFinite(interval) || interval <= 0 || !Number.isFinite(videoDuration.value) || videoDuration.value <= 0) return 0
  return Math.max(0, Math.ceil((videoDuration.value - 0.0005) / interval) - 1)
})
const plannedThumbnailCount = computed(() => initialCaptureMode.value === 'count' ? Math.max(0, Math.floor(Number(initialThumbnailCount.value) || 0)) : intervalThumbnailCount.value)
const thumbnails = ref<ThumbnailItem[]>([])
const startThumbnail = ref<string | null>(null)
const endThumbnail = ref<string | null>(null)
const showStartEndpoint = ref(true)
const showEndEndpoint = ref(true)
const activeModalIndex = ref<number | null>(null)
const modalImageSize = ref<ModalImageSize>('fit')
const selectedTimes = ref<number[]>([])
const expansionMode = ref<ExpansionMode>('interval')
const expansionInterval = ref(1)
const singleInsertAnchor = ref<'start' | 'end'>('start')
const expansionCount = ref(4)
const expansionQueue = ref<ExpansionJob[]>([])
const videoDuration = ref(0)
const durationLoading = ref(false)
const durationLoadFailed = ref(false)
let durationRequestSerial = 0
const activePanel = ref<'load' | 'task' | 'combine'>('load')
const combineFiles = ref<string[]>([])
const combineDragFromIndex = ref<number | null>(null)
const combineMode = ref<CombineMode>('copy')
const combineOutputName = ref('merged_video')
const combineExtension = ref('mp4')
const loadSubPanel = ref<'source' | 'init' | 'expand'>('source')
const logOpen = ref(false)
const exportingLog = ref(false)
const showDeleteProcessedConfirm = ref(false)
const isDeletingProcessed = ref(false)
const router = useRouter()
const taskKind = ref<TaskKind>('clip')
const taskSourceMode = ref<'original' | 'processed' | 'batch'>('original')
const taskSourceProcessedId = ref('')
const batchSplitSources = ref<BatchSplitSource[]>([])
const taskSourcePath = computed(() => {
  if (taskSourceMode.value === 'batch') return ''
  if (taskSourceMode.value === 'processed') {
    const match = processedVideos.value.find((task) => task.id === taskSourceProcessedId.value)
    return match?.resultPath || ''
  }
  return originalSourcePath.value
})
const batchSourceModeAvailable = computed(() => taskKind.value === 'clip' && taskMethod.value !== 'range' && !editingTaskId.value)
const batchSourcesReady = computed(() => batchSplitSources.value.length > 0 && batchSplitSources.value.every((source) => !source.loading && !source.error && source.duration > 0))
// The default output name should always reflect whichever source is actually selected (original vs.
// a specific processed video) — otherwise switching sources can leave a misleading filename behind
// from whatever was loaded first, even though the correct file is the one actually used to cut.
watch(taskSourcePath, (path) => {
  if (taskOutputNameAuto.value && path) taskOutputName.value = `${baseNameWithoutExt(path)}${taskNameSuffix.value}`
})
const taskMethod = ref<ClipMethod>('range')
const taskDurationHours = ref(6)
const taskDurationMinutes = ref(0)
const taskDurationSeconds = ref(0)
const taskSplitCount = ref(5)
const mergeSegments = ref<VideoSegment[]>([])
const mergeEditingIndex = ref<number | null>(null)
const taskNameSuffix = ref('_edit')
const taskOutputName = ref('processed_video')
const taskOutputNameAuto = ref(true)
const taskExtension = ref('mp4')
const taskExtraArgs = ref('')
const taskPreciseSeek = ref(true)
const taskKeepSegments = ref(false)
const configuredSegmentDuration = computed(() => taskSegmentDurationSeconds({
  durationHours: taskDurationHours.value,
  durationMinutes: taskDurationMinutes.value,
  durationSeconds: taskDurationSeconds.value,
}))
const durationInputIsValid = computed(() => {
  const hours = Number(taskDurationHours.value)
  const minutes = Number(taskDurationMinutes.value)
  const seconds = Number(taskDurationSeconds.value)
  return [hours, minutes, seconds].every(Number.isFinite)
    && Number.isInteger(hours) && hours >= 0
    && Number.isInteger(minutes) && minutes >= 0 && minutes <= 59
    && Number.isInteger(seconds) && seconds >= 0 && seconds <= 59
})
const durationSplitCount = computed(() => {
  const segmentDuration = configuredSegmentDuration.value
  const duration = videoDuration.value
  return segmentDuration && duration ? Math.ceil(duration / segmentDuration) : 0
})
const visibleDurationSplitPreview = computed(() => {
  const count = durationSplitCount.value
  const indexes = count <= 8 ? Array.from({ length: count }, (_, index) => index) : [...Array.from({ length: 7 }, (_, index) => index), count - 1]
  return indexes.map((index) => ({
    start: index * configuredSegmentDuration.value,
    end: Math.min(videoDuration.value, (index + 1) * configuredSegmentDuration.value),
  }))
})
const editingTaskId = ref<string | null>(null)
const editingCreatesCopy = ref(false)
const editingTask = computed(() => (editingTaskId.value ? videoTasks.find((task) => task.id === editingTaskId.value) ?? null : null))
watch([taskKind, taskMethod], () => {
  if (taskSourceMode.value === 'batch' && !batchSourceModeAvailable.value) taskSourceMode.value = 'original'
})

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
  if (showStartEndpoint.value) list.push({ time: 0, src: startThumbnail.value || '', label: '起點' })
  for (const item of thumbnails.value) list.push({ time: item.time, src: item.base64, label: item.timeStr })
  if (showEndEndpoint.value) list.push({ time: videoDuration.value, src: endThumbnail.value || '', label: '終點' })
  return list
})
const activeModalItem = computed(() => (activeModalIndex.value !== null ? (modalSequence.value[activeModalIndex.value] ?? null) : null))

// The grid and filmstrip retain every time point, but only mount images near the viewport.
const gridRef = ref<HTMLElement | null>(null)
const gridScrollTop = ref(0)
const gridWidth = ref(0)
const gridHeight = ref(0)
const previewColumns = ref(4)
let gridResizeObserver: ResizeObserver | null = null
const gridSequence = computed(() => [
  ...(showStartEndpoint.value ? [{ time: 0, src: startThumbnail.value, label: '00:00:00', endpoint: true }] : []),
  ...thumbnails.value.map((item) => ({ time: item.time, src: item.base64, label: item.timeStr, endpoint: false })),
  ...(showEndEndpoint.value ? [{ time: videoDuration.value, src: endThumbnail.value, label: formatSeconds(videoDuration.value), endpoint: true }] : []),
])
const hasPreviewContent = computed(() => Boolean(thumbnails.value.length || startThumbnail.value || endThumbnail.value || isProcessing.value || isEndpointProcessing.value))
const previewDisplayCount = computed(() => (hasPreviewContent.value ? gridSequence.value.length : 0))
const gridColumnCount = computed(() => Math.max(1, Math.min(12, Math.floor(Number(previewColumns.value) || 4))))
const gridRowPitch = computed(() => {
  const contentWidth = Math.max(1, gridWidth.value - 9)
  const itemWidth = Math.max(1, (contentWidth - (gridColumnCount.value - 1) * 3) / gridColumnCount.value)
  return Math.ceil(itemWidth * 9 / 16 + 26 + 3)
})
const gridTotalRows = computed(() => Math.ceil(gridSequence.value.length / gridColumnCount.value))
const gridFirstRow = computed(() => Math.max(0, Math.floor(gridScrollTop.value / gridRowPitch.value) - 3))
const gridLastRow = computed(() => Math.min(gridTotalRows.value, Math.ceil((gridScrollTop.value + gridHeight.value) / gridRowPitch.value) + 3))
const showGridBackToTop = computed(() => gridScrollTop.value > Math.max(240, gridHeight.value * 0.75))
const visibleGridItems = computed(() => gridSequence.value.slice(gridFirstRow.value * gridColumnCount.value, gridLastRow.value * gridColumnCount.value)
  .map((item, offset) => ({ ...item, index: gridFirstRow.value * gridColumnCount.value + offset })))
const gridCanvasHeight = computed(() => gridTotalRows.value * gridRowPitch.value)
const similarityLevel = ref<'strict' | 'normal' | 'loose'>('normal')
const similarityMinCount = ref(3)
const similarityPanelOpen = ref(false)
const similarityAnalyzing = ref(false)
const similarityProgress = ref(0)
const similarityGroups = ref<SimilarThumbnailGroup[]>([])
const similarityAnalyzedCount = ref(0)
const hoveredSimilarityGroup = ref<number | null>(null)
const selectedSimilarityGroupIds = ref<number[]>([])
const similarityKeepMode = ref<SimilarityKeepMode>('both')
const similarityKeepCount = ref(1)
const showSimilarityMarkers = ref(true)
const showSmartReplaceConfirm = ref(false)
let similarityRunId = 0
const similarityThreshold = computed(() => ({ strict: 3, normal: 7, loose: 12 })[similarityLevel.value])
const similarityMetaByTime = computed(() => {
  const result = new Map<number, { id: number; first: boolean; last: boolean }>()
  for (const group of similarityGroups.value) {
    group.times.forEach((time, index) => result.set(time, {
      id: group.id,
      first: index === 0,
      last: index === group.times.length - 1,
    }))
  }
  return result
})
const similarityResultOutdated = computed(() => Boolean(similarityGroups.value.length && similarityAnalyzedCount.value !== gridSequence.value.length))
const smartRemovalPlan = computed(() => {
  const keepCount = Math.max(1, Math.floor(Number(similarityKeepCount.value) || 1))
  const enabledIds = new Set(selectedSimilarityGroupIds.value)
  return similarityGroups.value.filter((group) => enabledIds.has(group.id)).map((group) => {
    const count = group.times.length
    const overlaps = similarityKeepMode.value === 'both' ? keepCount * 2 >= count : keepCount >= count
    if (overlaps) return { group, overlaps: true, removal: null as VideoSegment | null }
    const startIndex = similarityKeepMode.value === 'back' ? 0 : keepCount
    const endIndex = similarityKeepMode.value === 'front' ? count : count - keepCount
    const afterGroupIndex = gridSequence.value.findIndex((item) => item.time === group.times[count - 1]) + 1
    const end = endIndex < count ? group.times[endIndex] : (gridSequence.value[afterGroupIndex]?.time ?? videoDuration.value)
    return { group, overlaps: false, removal: { start: group.times[startIndex], end } }
  })
})
const smartRemovalRanges = computed(() => smartRemovalPlan.value
  .flatMap((item) => item.removal ? [item.removal] : [])
  .filter((range) => range.end > range.start)
  .sort((a, b) => a.start - b.start)
  .reduce<VideoSegment[]>((merged, range) => {
    const previous = merged[merged.length - 1]
    if (previous && range.start <= previous.end) previous.end = Math.max(previous.end, range.end)
    else merged.push({ ...range })
    return merged
  }, []))
const smartKeepSegments = computed(() => {
  if (!videoDuration.value || !smartRemovalRanges.value.length) return []
  const kept: VideoSegment[] = []
  let cursor = 0
  for (const removal of smartRemovalRanges.value) {
    if (removal.start > cursor) kept.push({ start: cursor, end: removal.start })
    cursor = Math.max(cursor, removal.end)
  }
  if (cursor < videoDuration.value) kept.push({ start: cursor, end: videoDuration.value })
  return kept.filter((segment) => segment.end - segment.start >= 0.001)
})

function measureGrid() {
  const container = gridRef.value
  if (!container) return
  gridWidth.value = container.clientWidth
  gridHeight.value = container.clientHeight
  gridScrollTop.value = container.scrollTop
}
function onGridScroll() {
  if (gridRef.value) gridScrollTop.value = gridRef.value.scrollTop
}

function scrollGridToTop() {
  gridRef.value?.scrollTo({ top: 0, behavior: 'smooth' })
}

function clearSimilarityAnalysis() {
  similarityRunId += 1
  similarityAnalyzing.value = false
  similarityProgress.value = 0
  similarityGroups.value = []
  similarityAnalyzedCount.value = 0
  hoveredSimilarityGroup.value = null
  selectedSimilarityGroupIds.value = []
  showSmartReplaceConfirm.value = false
}

watch(videoPath, clearSimilarityAnalysis)

function thumbnailDifference(first: Uint8Array, second: Uint8Array) {
  let difference = 0
  for (let index = 0; index < first.length; index += 1) if (first[index] !== second[index]) difference += 1
  return difference
}

async function thumbnailHash(src: string, context: CanvasRenderingContext2D) {
  const image = new Image()
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve()
    image.onerror = () => reject(new Error('縮圖圖片無法讀取'))
    image.src = src
  })
  context.drawImage(image, 0, 0, 9, 8)
  const pixels = context.getImageData(0, 0, 9, 8).data
  const hash = new Uint8Array(64)
  for (let row = 0; row < 8; row += 1) {
    for (let column = 0; column < 8; column += 1) {
      const left = (row * 9 + column) * 4
      const right = left + 4
      const leftGray = pixels[left] * 0.299 + pixels[left + 1] * 0.587 + pixels[left + 2] * 0.114
      const rightGray = pixels[right] * 0.299 + pixels[right + 1] * 0.587 + pixels[right + 2] * 0.114
      hash[row * 8 + column] = leftGray > rightGray ? 1 : 0
    }
  }
  return hash
}

async function analyzeSimilarThumbnails() {
  if (similarityAnalyzing.value) return
  const sequence = gridSequence.value.map((item) => ({ ...item }))
  const minimumCount = Math.max(2, Math.min(100, Math.floor(Number(similarityMinCount.value) || 3)))
  similarityMinCount.value = minimumCount
  if (sequence.filter((item) => item.src).length < minimumCount) return addLog('已載入的縮圖數量不足，無法分析相似區段。')
  const runId = ++similarityRunId
  similarityAnalyzing.value = true
  similarityProgress.value = 0
  similarityGroups.value = []
  const canvas = document.createElement('canvas')
  canvas.width = 9
  canvas.height = 8
  const context = canvas.getContext('2d', { willReadFrequently: true })
  if (!context) {
    similarityAnalyzing.value = false
    return addLog('瀏覽器無法建立縮圖分析工具。')
  }
  const groups: SimilarThumbnailGroup[] = []
  let currentTimes: number[] = []
  let previousHash: Uint8Array | null = null
  let anchorHash: Uint8Array | null = null
  const finishGroup = () => {
    if (currentTimes.length >= minimumCount) groups.push({ id: groups.length + 1, times: [...currentTimes] })
    currentTimes = []
    previousHash = null
    anchorHash = null
  }
  try {
    for (let index = 0; index < sequence.length; index += 1) {
      if (runId !== similarityRunId) return
      const item = sequence[index]
      if (!item.src) {
        finishGroup()
      } else {
        try {
          const hash = await thumbnailHash(item.src, context)
          const continues = previousHash && anchorHash
            && thumbnailDifference(previousHash, hash) <= similarityThreshold.value
            && thumbnailDifference(anchorHash, hash) <= similarityThreshold.value + 2
          if (continues) currentTimes.push(item.time)
          else {
            finishGroup()
            currentTimes = [item.time]
            anchorHash = hash
          }
          previousHash = hash
          if (!anchorHash) anchorHash = hash
        } catch {
          finishGroup()
        }
      }
      similarityProgress.value = Math.round(((index + 1) / sequence.length) * 100)
      if (index % 40 === 39) await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
    }
    finishGroup()
    if (runId !== similarityRunId) return
    similarityGroups.value = groups
    selectedSimilarityGroupIds.value = groups.map((group) => group.id)
    similarityAnalyzedCount.value = sequence.length
    addLog(groups.length ? `相似縮圖分析完成：找到 ${groups.length} 個連續區段。` : '相似縮圖分析完成：沒有找到符合條件的連續區段。')
  } finally {
    if (runId === similarityRunId) similarityAnalyzing.value = false
  }
}

function cancelSimilarityAnalysis() {
  similarityRunId += 1
  similarityAnalyzing.value = false
  addLog('已取消相似縮圖分析。')
}

function selectSimilarityGroup(group: SimilarThumbnailGroup) {
  if (!group.times.length) return
  selectedTimes.value = [group.times[0], group.times[group.times.length - 1]]
  const firstIndex = gridSequence.value.findIndex((item) => item.time === group.times[0])
  if (firstIndex >= 0) {
    const row = Math.floor(firstIndex / gridColumnCount.value)
    gridRef.value?.scrollTo({ top: row * gridRowPitch.value, behavior: 'smooth' })
  }
}

function similarityColorClass(groupId: number) {
  return `similarity-color-${((groupId - 1) % 4) + 1}`
}

function similarityItemClasses(time: number) {
  if (!showSimilarityMarkers.value) return []
  const meta = similarityMetaByTime.value.get(time)
  if (!meta) return []
  return [
    'similar',
    similarityColorClass(meta.id),
    meta.first ? 'similar-first' : '',
    meta.last ? 'similar-last' : '',
    hoveredSimilarityGroup.value !== null && hoveredSimilarityGroup.value !== meta.id ? 'similar-muted' : '',
    hoveredSimilarityGroup.value === meta.id ? 'similar-focused' : '',
  ]
}

function toggleSimilarityGroup(groupId: number) {
  selectedSimilarityGroupIds.value = selectedSimilarityGroupIds.value.includes(groupId)
    ? selectedSimilarityGroupIds.value.filter((id) => id !== groupId)
    : [...selectedSimilarityGroupIds.value, groupId]
}

function applySmartKeepSegments() {
  if (!smartRemovalRanges.value.length || !smartKeepSegments.value.length) return addLog('目前設定沒有需要排除的相似內容。')
  mergeSegments.value = smartKeepSegments.value.map((segment) => ({ ...segment }))
  mergeEditingIndex.value = null
  selectedTimes.value = []
  taskKind.value = 'merge'
  activePanel.value = 'task'
  similarityPanelOpen.value = false
  showSmartReplaceConfirm.value = false
  addLog(`已依相似縮圖建立 ${mergeSegments.value.length} 個合併片段草稿，排除 ${smartRemovalRanges.value.length} 個時間範圍。`)
}

function requestSmartKeepSegments() {
  if (!smartRemovalRanges.value.length) return addLog('目前設定沒有需要排除的相似內容。')
  if (mergeSegments.value.length) showSmartReplaceConfirm.value = true
  else applySmartKeepSegments()
}
watch(gridRef, (element) => {
  gridResizeObserver?.disconnect()
  gridResizeObserver = null
  if (!element) return
  gridResizeObserver = new ResizeObserver(measureGrid)
  gridResizeObserver.observe(element)
  nextTick(measureGrid)
})

const filmstripRef = ref<HTMLElement | null>(null)
const filmstripScrollLeft = ref(0)
const filmstripWidth = ref(0)
const filmstripPitch = 286
let filmstripResizeObserver: ResizeObserver | null = null
const filmstripFirst = computed(() => Math.max(0, Math.floor(filmstripScrollLeft.value / filmstripPitch) - 3))
const filmstripLast = computed(() => Math.min(modalSequence.value.length, Math.ceil((filmstripScrollLeft.value + filmstripWidth.value) / filmstripPitch) + 3))
const visibleFilmstripItems = computed(() => modalSequence.value.slice(filmstripFirst.value, filmstripLast.value)
  .map((item, offset) => ({ ...item, index: filmstripFirst.value + offset })))
function measureFilmstrip() {
  if (!filmstripRef.value) return
  filmstripWidth.value = filmstripRef.value.clientWidth
  filmstripScrollLeft.value = filmstripRef.value.scrollLeft
}
function onFilmstripScroll() {
  if (filmstripRef.value) filmstripScrollLeft.value = filmstripRef.value.scrollLeft
}
watch(filmstripRef, (element) => {
  filmstripResizeObserver?.disconnect()
  filmstripResizeObserver = null
  if (!element) return
  filmstripResizeObserver = new ResizeObserver(measureFilmstrip)
  filmstripResizeObserver.observe(element)
  nextTick(measureFilmstrip)
})
let filmstripScrollFrame: number | null = null

function cancelFilmstripScrollAnimation() {
  if (filmstripScrollFrame !== null) {
    cancelAnimationFrame(filmstripScrollFrame)
    filmstripScrollFrame = null
  }
}

function scrollFilmstripToItem(index: number) {
  const container = filmstripRef.value
  if (!container) return

  cancelFilmstripScrollAnimation()
  const maxScrollLeft = Math.max(0, container.scrollWidth - container.clientWidth)
  const targetLeft = Math.max(0, Math.min(index * filmstripPitch + 140 - container.clientWidth / 2, maxScrollLeft))
  const startLeft = container.scrollLeft
  const distance = targetLeft - startLeft
  if (Math.abs(distance) < 1) return

  // scrollIntoView({ behavior: 'smooth' }) does not expose a duration and can take too long when
  // the filmstrip contains many thumbnails. Use an explicit animation capped at two seconds.
  const duration = Math.min(2000, Math.max(200, Math.abs(distance) * 1.5))
  const startedAt = performance.now()
  const animate = (now: number) => {
    const progress = Math.min(1, (now - startedAt) / duration)
    const easedProgress = 1 - Math.pow(1 - progress, 3)
    container.scrollLeft = startLeft + distance * easedProgress
    if (progress < 1) filmstripScrollFrame = requestAnimationFrame(animate)
    else filmstripScrollFrame = null
  }
  filmstripScrollFrame = requestAnimationFrame(animate)
}

watch(activeModalIndex, async (index) => {
  if (index === null) return
  await nextTick()
  scrollFilmstripToItem(index)
})

const newCaptureCount = computed(() => {
  if (expansionMode.value === 'interval') return Math.max(0, Math.ceil((endTime.value - startTime.value) / Math.max(1, expansionInterval.value)) - 1)
  if (expansionMode.value === 'single') return buildSingleInsertTime() === null ? 0 : 1
  return Math.max(1, Math.floor(Number(expansionCount.value) || 1))
})

// Used only by the adjacency-bound expansion feature below.
const startTime = computed(() => (selectedPair.value ? Math.min(...selectedTimes.value) : 0))
const endTime = computed(() => (selectedPair.value ? Math.max(...selectedTimes.value) : 0))

function loadPaths() {
  ffmpegPath.value = localStorage.getItem('vid_ffmpeg_path') || ''
  ffprobePath.value = localStorage.getItem('vid_ffprobe_path') || ''
  taskNameSuffix.value = localStorage.getItem('vid_task_suffix') || '_edit'
  const thumbnail = getThumbnailSettings()
  thumbnailPreset.value = thumbnail.preset
  thumbnailMaxHeight.value = thumbnail.maxHeight
  thumbnailOriginalSize.value = thumbnail.originalSize
  thumbnailQuality.value = thumbnail.quality
  thumbnailSeekMode.value = thumbnail.seekMode
  thumbnailHardwareAcceleration.value = thumbnail.hardwareAcceleration
}

function currentThumbnailSettings(): ThumbnailSettings {
  return normaliseThumbnailSettings({
    preset: thumbnailPreset.value,
    maxHeight: thumbnailMaxHeight.value,
    originalSize: thumbnailOriginalSize.value,
    quality: thumbnailQuality.value,
    seekMode: thumbnailSeekMode.value,
    hardwareAcceleration: thumbnailHardwareAcceleration.value,
  })
}

function persistThumbnailSettings() {
  const settings = currentThumbnailSettings()
  saveThumbnailSettings(settings)
}

function applyThumbnailPreset() {
  const settings = settingsForPreset(thumbnailPreset.value)
  thumbnailMaxHeight.value = settings.maxHeight
  thumbnailOriginalSize.value = settings.originalSize
  thumbnailQuality.value = settings.quality
  thumbnailSeekMode.value = settings.seekMode
  thumbnailHardwareAcceleration.value = settings.hardwareAcceleration
  persistThumbnailSettings()
}

function restoreSavedWorkspace() {
  const draft = loadWorkspaceDraft()
  if (!draft?.videoPath) return
  videoPath.value = draft.videoPath
  originalSourcePath.value = draft.originalSourcePath || draft.videoPath
  videoDuration.value = Math.max(0, Number(draft.videoDuration) || 0)
  const legacyInitialCount = Math.max(0, Math.floor(Number(draft.gridCols) || 7) * Math.floor(Number(draft.gridRows) || 5) - 2)
  const savedInitialCount = Number(draft.initialThumbnailCount)
  initialThumbnailCount.value = Number.isFinite(savedInitialCount) && savedInitialCount >= 0
    ? Math.floor(savedInitialCount)
    : legacyInitialCount
  initialCaptureMode.value = draft.initialCaptureMode === 'interval' ? 'interval' : 'count'
  previewColumns.value = Math.max(1, Math.min(12, Math.floor(Number(draft.gridCols) || Number(localStorage.getItem('vid_preview_columns')) || 4)))
  initialInterval.value = Math.max(0.1, Number(draft.initialInterval) || 4)
  taskKind.value = draft.taskKind === 'merge' ? 'merge' : 'clip'
  taskMethod.value = ['range', 'duration', 'count'].includes(draft.taskMethod) ? draft.taskMethod : 'range'
  const savedSegmentSeconds = draft.taskDurationMinutes === undefined && draft.taskDurationSeconds === undefined
    ? Math.max(1, Math.round((Number(draft.taskDurationHours) || 6) * 3600))
    : Math.max(1, Math.floor(Number(draft.taskDurationHours) || 0) * 3600 + Math.floor(Number(draft.taskDurationMinutes) || 0) * 60 + Math.floor(Number(draft.taskDurationSeconds) || 0))
  taskDurationHours.value = Math.floor(savedSegmentSeconds / 3600)
  taskDurationMinutes.value = Math.floor((savedSegmentSeconds % 3600) / 60)
  taskDurationSeconds.value = savedSegmentSeconds % 60
  taskSplitCount.value = Math.max(2, Math.floor(Number(draft.taskSplitCount) || 5))
  taskOutputName.value = draft.taskOutputName || `${baseNameWithoutExt(draft.videoPath)}${taskNameSuffix.value}`
  taskOutputNameAuto.value = !draft.taskOutputName
  taskExtension.value = draft.taskExtension || 'mp4'
  taskExtraArgs.value = draft.taskExtraArgs || ''
  taskPreciseSeek.value = draft.taskPreciseSeek !== false
  taskKeepSegments.value = Boolean(draft.taskKeepSegments)
  mergeSegments.value = Array.isArray(draft.mergeSegments) ? draft.mergeSegments.map((segment) => ({ ...segment })) : []
  selectedTimes.value = Array.isArray(draft.selectedTimes) ? draft.selectedTimes.slice(0, 2) : []

  const times = [...new Set((draft.previewTimes || []).filter((time) => Number.isFinite(time) && time >= 0))].sort((a, b) => a - b)
  showStartEndpoint.value = times.some((time) => time === 0)
  showEndEndpoint.value = videoDuration.value > 0 && times.some((time) => Math.abs(time - videoDuration.value) <= 0.0005)
  thumbnails.value = times
    .filter((time) => time > 0 && time < videoDuration.value)
    .map((time) => ({ time, timeStr: formatSeconds(time), base64: '' }))
  workspaceRecovered.value = times.length > 0
  addLog(workspaceRecovered.value ? '已復原上次工作區；縮圖圖片尚未載入，可自行選擇何時重新建立。' : '已復原上次工作區設定。')
}

function scheduleWorkspaceSave() {
  if (!workspaceReady) return
  if (workspaceSaveTimer) clearTimeout(workspaceSaveTimer)
  workspaceSaveTimer = setTimeout(() => {
    const previewTimes = [
      ...(showStartEndpoint.value ? [0] : []),
      ...thumbnails.value.map((item) => item.time),
      ...(showEndEndpoint.value && videoDuration.value > 0 ? [videoDuration.value] : []),
    ]
    saveWorkspaceDraft({
      videoPath: videoPath.value,
      originalSourcePath: originalSourcePath.value,
      videoDuration: videoDuration.value,
      previewTimes,
      selectedTimes: [...selectedTimes.value],
      mergeSegments: mergeSegments.value.map((segment) => ({ ...segment })),
      initialCaptureMode: initialCaptureMode.value,
      initialThumbnailCount: initialThumbnailCount.value,
      gridCols: previewColumns.value,
      initialInterval: initialInterval.value,
      taskKind: taskKind.value,
      taskMethod: taskMethod.value,
      taskDurationHours: taskDurationHours.value,
      taskDurationMinutes: taskDurationMinutes.value,
      taskDurationSeconds: taskDurationSeconds.value,
      taskSplitCount: taskSplitCount.value,
      taskOutputName: taskOutputName.value,
      taskExtension: taskExtension.value,
      taskExtraArgs: taskExtraArgs.value,
      taskPreciseSeek: taskPreciseSeek.value,
      taskKeepSegments: taskKeepSegments.value,
    })
  }, 500)
}

watch([
  videoPath, originalSourcePath, videoDuration, initialCaptureMode, initialThumbnailCount, initialInterval, previewColumns,
  taskKind, taskMethod, taskDurationHours, taskDurationMinutes, taskDurationSeconds, taskSplitCount, taskOutputName, taskExtension, taskExtraArgs,
  taskPreciseSeek, taskKeepSegments, showStartEndpoint, showEndEndpoint,
  () => thumbnails.value.map((item) => item.time).join(','),
  () => selectedTimes.value.join(','),
  () => JSON.stringify(mergeSegments.value),
], scheduleWorkspaceSave)

async function reloadRecoveredWorkspaceThumbnails() {
  if (!workspaceRecovered.value || isProcessing.value) return
  const middleTimes = thumbnails.value.map((item) => item.time)
  isProcessing.value = true
  initialCaptureCancelRequested.value = false
  try {
    await captureAtTimes(middleTimes, 0, () => initialCaptureCancelRequested.value, (item) => {
      const target = thumbnails.value.find((thumbnail) => thumbnail.time === item.time)
      if (target) target.base64 = item.base64
    })
    thumbnails.value = thumbnails.value.filter((item) => item.base64)
    if (!initialCaptureCancelRequested.value) {
      workspaceRecovered.value = false
      void captureEndpointThumbnails(videoDuration.value)
      addLog('工作區縮圖已重新載入。')
    }
  } catch (error) {
    thumbnails.value = thumbnails.value.filter((item) => item.base64)
    addLog(`重新載入工作區縮圖失敗：${String(error)}`)
  } finally {
    isProcessing.value = false
    initialCaptureCancelRequested.value = false
    progressText.value = ''
    progressPercent.value = 0
  }
}

function discardRecoveredPreview() {
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  showStartEndpoint.value = false
  showEndEndpoint.value = false
  selectedTimes.value = []
  workspaceRecovered.value = false
  clearWorkspaceDraft()
  scheduleWorkspaceSave()
  addLog('已捨棄上次保存的縮圖時間軸，影片與任務欄位仍保留。')
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

let unlistenDragDrop: (() => void) | null = null
const infoHintWrap = ref<HTMLElement | null>(null)
const similarityPopoverWrap = ref<HTMLElement | null>(null)

function selectVideoFilePanel() {
  activePanel.value = 'load'
  loadSubPanel.value = 'source'
}

function handleDocumentClick(event: MouseEvent) {
  if (showPreprocessHint.value && infoHintWrap.value && !infoHintWrap.value.contains(event.target as Node)) {
    showPreprocessHint.value = false
  }
  if (similarityPanelOpen.value && similarityPopoverWrap.value && !similarityPopoverWrap.value.contains(event.target as Node)) {
    similarityPanelOpen.value = false
  }
}

onMounted(async () => {
  loadPaths()
  restoreSavedWorkspace()
  await nextTick()
  workspaceReady = true
  window.addEventListener('home-navigation', selectVideoFilePanel)
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

// Home is kept alive between routes. A task edit request takes priority; otherwise returning to
// this page restores the ordinary load/source view.
onActivated(() => {
  loadPaths()
  const taskId = takeTaskEditRequest()
  if (taskId) void restoreTaskForEditing(taskId)
  else selectVideoFilePanel()
})

onUnmounted(() => {
  unlistenDragDrop?.()
  window.removeEventListener('home-navigation', selectVideoFilePanel)
  document.removeEventListener('click', handleDocumentClick)
  if (autoClearFinishedTimer) clearTimeout(autoClearFinishedTimer)
  if (workspaceSaveTimer) clearTimeout(workspaceSaveTimer)
  cancelFilmstripScrollAnimation()
  gridResizeObserver?.disconnect()
  filmstripResizeObserver?.disconnect()
})

defineExpose({ loadPaths })

function addLog(message: string) {
  logs.value.unshift(`[${new Date().toLocaleTimeString()}] ${message}`)
  if (logs.value.length > 1000) logs.value.length = 1000
}

function clearLogs() {
  logs.value = []
}

function debugLogContent() {
  const taskSummary = videoTasks.map((task) => ({
    id: task.id,
    kind: task.kind,
    status: task.status,
    source: task.source,
    outputDir: task.outputDir,
    outputName: `${task.outputName}.${task.extension}`,
    segments: task.segments,
    message: task.message,
  }))
  const diagnostics = {
    exportedAt: new Date().toISOString(),
    userAgent: navigator.userAgent,
    currentVideo: videoPath.value,
    originalSource: originalSourcePath.value,
    videoDurationSeconds: videoDuration.value,
    preview: {
      totalSlots: gridSequence.value.length,
      loadedMiddleThumbnails: thumbnails.value.filter((item) => item.base64).length,
      gridCols: previewColumns.value,
      renderedColumns: gridColumnCount.value,
      selectedTimes: selectedTimes.value,
      processing: isProcessing.value || isEndpointProcessing.value || isExpanding.value,
    },
    thumbnailSettings: currentThumbnailSettings(),
    configuredTools: {
      ffmpegPath: ffmpegPath.value || '(PATH/default)',
      ffprobePath: ffprobePath.value || '(PATH/default)',
    },
    tasks: taskSummary,
  }
  return [
    '注意：此檔案可能包含本機影片與輸出資料夾的完整路徑，分享前請自行確認。',
    '',
    '=== Diagnostics ===',
    JSON.stringify(diagnostics, null, 2),
    '',
    `=== Operation log (${logs.value.length}, oldest to newest) ===`,
    ...[...logs.value].reverse(),
    '',
  ].join('\n')
}

async function exportDebugLog() {
  if (exportingLog.value) return
  const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)
  const selected = await save({
    title: '匯出除錯紀錄（可能包含本機檔案路徑）',
    defaultPath: `vidThumbClipper-debug-${stamp}.txt`,
    filters: [{ name: 'Text log', extensions: ['txt', 'log'] }],
  })
  if (!selected) return
  exportingLog.value = true
  try {
    await invoke('export_debug_log', { path: selected, content: debugLogContent() })
    addLog(`已匯出除錯紀錄：${selected}`)
  } catch (error) {
    addLog(`匯出除錯紀錄失敗：${String(error)}`)
  } finally {
    exportingLog.value = false
  }
}

function openModal(index: number) {
  if (index < 0 || index >= modalSequence.value.length) return
  modalImageSize.value = 'fit'
  activeModalIndex.value = index
}

function openModalForTime(time: number) {
  const index = modalSequence.value.findIndex((item) => item.time === time)
  if (index !== -1) openModal(index)
}

function toggleModalImageSize() {
  modalImageSize.value = modalImageSize.value === 'fit' ? 'original' : 'fit'
}

function modalGoPrev() {
  if (activeModalIndex.value === null || activeModalIndex.value <= 0) return
  activeModalIndex.value -= 1
}

function modalGoNext() {
  if (activeModalIndex.value === null || activeModalIndex.value >= modalSequence.value.length - 1) return
  activeModalIndex.value += 1
}

function persistPreviewColumns() {
  previewColumns.value = Math.max(1, Math.min(12, Math.floor(Number(previewColumns.value) || 4)))
  localStorage.setItem('vid_preview_columns', String(previewColumns.value))
  nextTick(measureGrid)
}

function loadPreviewColumnsPreference() {
  const saved = Number(localStorage.getItem('vid_preview_columns'))
  previewColumns.value = Number.isFinite(saved) ? Math.max(1, Math.min(12, Math.floor(saved))) : 4
}

function loadVideoFromPath(path: string) {
  loadPaths()
  videoPath.value = path
  originalSourcePath.value = path
  loadPreviewColumnsPreference()
  loadSubPanel.value = 'init'
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  showStartEndpoint.value = true
  showEndEndpoint.value = true
  videoDuration.value = 0
  expansionQueue.value = []
  selectedTimes.value = []
  gridScrollTop.value = 0
  taskOutputName.value = `${baseNameWithoutExt(path)}${taskNameSuffix.value}`
  taskOutputNameAuto.value = true
  taskSourceMode.value = 'original'
  taskSourceProcessedId.value = ''
  editingTaskId.value = null
  editingCreatesCopy.value = false
  addLog(`已選擇影片：${path}`)
  void loadVideoDuration(path)
}

async function loadVideoDuration(path: string) {
  const serial = ++durationRequestSerial
  durationLoading.value = true
  durationLoadFailed.value = false
  try {
    const duration = await invoke<number>('get_video_duration', { ffprobePath: ffprobePath.value || null, videoPath: path })
    if (serial === durationRequestSerial && videoPath.value === path) videoDuration.value = duration
  } catch (error) {
    if (serial === durationRequestSerial) {
      durationLoadFailed.value = true
      addLog(`無法預先讀取影片長度：${String(error)}`)
    }
  } finally {
    if (serial === durationRequestSerial) durationLoading.value = false
  }
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
      durationMinutes: taskDurationMinutes.value,
      durationSeconds: taskDurationSeconds.value,
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
  loadPreviewColumnsPreference()
  loadSubPanel.value = 'init'
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  showStartEndpoint.value = true
  showEndEndpoint.value = true
  videoDuration.value = 0
  expansionQueue.value = []
  selectedTimes.value = []
  gridScrollTop.value = 0
  taskOutputName.value = `${baseNameWithoutExt(task.source)}${taskNameSuffix.value}`
  taskOutputNameAuto.value = true
  taskSourceMode.value = 'original'
  taskSourceProcessedId.value = ''
  editingTaskId.value = null
  editingCreatesCopy.value = false
  addLog(`已選擇處理過的影片：${task.resultPath}`)
  void loadVideoDuration(task.resultPath)
}

function removeProcessedVideo(id: string) {
  const index = videoTasks.findIndex((task) => task.id === id)
  if (index !== -1) videoTasks.splice(index, 1)
}

function clearProcessedVideos() {
  showDeleteProcessedConfirm.value = false
  for (let index = videoTasks.length - 1; index >= 0; index -= 1) {
    if (videoTasks[index].kind === 'preprocess' && videoTasks[index].status === '完成') videoTasks.splice(index, 1)
  }
}

function requestDeleteAllProcessedVideos() {
  if (!processedVideos.value.length) return
  if (isProcessing.value || isEndpointProcessing.value || isExpanding.value) {
    addLog('目前有縮圖正在處理，請等待完成後再刪除已處理影片。')
    return
  }
  showDeleteProcessedConfirm.value = true
}

function cancelDeleteAllProcessedVideos() {
  showDeleteProcessedConfirm.value = false
}

function resetThumbnailPreview() {
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  showStartEndpoint.value = true
  showEndEndpoint.value = true
  videoDuration.value = 0
  expansionQueue.value = []
  selectedTimes.value = []
  gridScrollTop.value = 0
  progressText.value = ''
  progressPercent.value = 0
}

async function deleteAllProcessedVideos() {
  if (isDeletingProcessed.value) return
  showDeleteProcessedConfirm.value = false

  const tasksToDelete = videoTasks.filter((task) => task.kind === 'preprocess' && task.status === '完成')
  if (!tasksToDelete.length) return

  isDeletingProcessed.value = true
  const pathsByKey = new Map<string, string>()
  const failures: string[] = []
  const sourceKeys = new Set(tasksToDelete.map((task) => normaliseVideoPath(task.source)))

  for (const task of tasksToDelete) {
    const displayName = `${task.outputName}.${task.extension}`
    const resultPath = task.resultPath.trim()
    if (!resultPath) {
      failures.push(`${displayName}（沒有輸出檔案路徑）`)
      continue
    }
    const resultKey = normaliseVideoPath(resultPath)
    if (sourceKeys.has(resultKey)) {
      failures.push(`${displayName}（輸出檔案路徑與原始影片相同，為避免誤刪已略過）`)
      continue
    }
    if (!pathsByKey.has(resultKey)) pathsByKey.set(resultKey, resultPath)
  }

  const deletedPathKeys = new Set<string>()
  try {
    for (const [pathKey, path] of pathsByKey) {
      try {
        await invoke('delete_file', { path })
        deletedPathKeys.add(pathKey)
      } catch (error) {
        failures.push(`${path}（${String(error)}）`)
      }
    }

    let removedTaskCount = 0
    for (let index = videoTasks.length - 1; index >= 0; index -= 1) {
      const task = videoTasks[index]
      if (task.kind !== 'preprocess' || task.status !== '完成') continue
      const resultPath = task.resultPath.trim()
      if (resultPath && !deletedPathKeys.has(normaliseVideoPath(resultPath))) continue
      if (!resultPath && failures.some((failure) => failure.startsWith(`${task.outputName}.${task.extension}（`))) continue
      videoTasks.splice(index, 1)
      removedTaskCount += 1
    }

    const currentPathKey = normaliseVideoPath(videoPath.value)
    if (deletedPathKeys.has(currentPathKey)) {
      const deletedTask = tasksToDelete.find((task) => normaliseVideoPath(task.resultPath) === currentPathKey)
      const fallbackPath = deletedTask?.source || (normaliseVideoPath(originalSourcePath.value) !== currentPathKey ? originalSourcePath.value : '')
      videoPath.value = fallbackPath
      originalSourcePath.value = fallbackPath
      taskSourceMode.value = 'original'
      taskSourceProcessedId.value = ''
      resetThumbnailPreview()
      addLog(fallbackPath ? '目前載入的已處理影片已刪除，已切回原始影片。' : '目前載入的已處理影片已刪除，請重新選擇影片。')
    }

    if (failures.length) {
      addLog(`已刪除 ${deletedPathKeys.size} 個已處理影片檔案，另有 ${failures.length} 個項目刪除失敗，已保留在清單中。`)
      for (const failure of failures) addLog(`刪除失敗：${failure}`)
    } else {
      addLog(`已刪除全部已處理影片檔案，共 ${deletedPathKeys.size} 個檔案，並清除 ${removedTaskCount} 筆紀錄。`)
    }
  } finally {
    isDeletingProcessed.value = false
  }
}

const activeThumbnailRequests = new Set<string>()

async function captureThumbnail(args: Record<string, unknown>, shouldStop?: () => boolean, settings = currentThumbnailSettings()) {
  const requestId = newTaskId()
  await invoke('begin_thumbnail', { requestId })
  try {
    if (shouldStop?.()) throw new Error('Thumbnail cancelled')
    if (shouldStop) activeThumbnailRequests.add(requestId)
    const captureArgs = {
      ...args,
      requestId,
      maxHeight: settings.maxHeight,
      originalSize: settings.originalSize,
      quality: settings.quality,
      seekMode: settings.seekMode,
      hardwareAcceleration: settings.hardwareAcceleration,
    }
    try {
      return await invoke<string>('capture_single_thumbnail', captureArgs)
    } catch (error) {
      if (!settings.hardwareAcceleration) throw error
      addLog('硬體解碼無法完成此張縮圖，已自動改用軟體解碼。')
      return await invoke<string>('capture_single_thumbnail', { ...captureArgs, hardwareAcceleration: false })
    }
  } finally {
    activeThumbnailRequests.delete(requestId)
    await invoke('end_thumbnail', { requestId })
  }
}

function stopActiveThumbnail() {
  for (const requestId of activeThumbnailRequests) {
    void invoke('cancel_thumbnail', { requestId })
      .catch((error) => addLog(`無法傳送取消要求：${String(error)}`))
  }
}

async function captureAtTimes(times: number[], offset: number, shouldStop?: () => boolean, onCaptured?: (item: ThumbnailItem, index: number) => void) {
  const result: ThumbnailItem[] = []
  const settings = currentThumbnailSettings()
  for (let index = 0; index < times.length; index += 1) {
    if (shouldStop?.()) break
    progressPercent.value = Math.round((index / times.length) * 100)
    progressText.value = `正在擷取 ${index + 1}/${times.length} 張（${progressPercent.value}%）`
    let base64: string
    try {
      base64 = await captureThumbnail({
        ffmpegPath: ffmpegPath.value || null,
        videoPath: videoPath.value,
        time: times[index].toFixed(3),
        index: offset + index,
      }, shouldStop, settings)
    } catch (error) {
      if (shouldStop?.()) {
        if (String(error).includes('cleanup') || String(error).includes('stop result: Err')) addLog(`取消後程序清理異常：${String(error)}`)
        break
      }
      throw error
    }
    const item = { time: times[index], timeStr: formatSeconds(times[index]), base64: `data:image/jpeg;base64,${base64}` }
    result.push(item)
    onCaptured?.(item, index)
    progressPercent.value = Math.round(((index + 1) / times.length) * 100)
    progressText.value = `已完成擷取 ${index + 1}/${times.length} 張（${progressPercent.value}%）`
  }
  return result
}

async function generateInitialThumbnails() {
  if (!videoPath.value) return addLog('請先選擇影片檔案。')
  const interval = Number(initialInterval.value)
  if (initialCaptureMode.value === 'interval' && (!Number.isFinite(interval) || interval < 0.1)) {
    return addLog('請輸入至少 0.1 秒的有效截圖間隔。')
  }
  const count = Math.max(0, Math.floor(Number(initialThumbnailCount.value) || 0))
  loadPaths()
  isProcessing.value = true
  initialCaptureCancelRequested.value = false
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  showStartEndpoint.value = true
  showEndEndpoint.value = true
  selectedTimes.value = []
  gridScrollTop.value = 0
  let initialCaptureSucceeded = false
  let duration = 0
  try {
    duration = await invoke<number>('get_video_duration', { ffprobePath: ffprobePath.value || null, videoPath: videoPath.value })
    videoDuration.value = duration
    const actualCount = initialCaptureMode.value === 'count' ? count : Math.max(0, Math.ceil((duration - 0.0005) / interval) - 1)
    const times = Array.from({ length: actualCount }, (_, index) => initialCaptureMode.value === 'count'
      ? ((index + 1) * duration) / (actualCount + 1)
      : (index + 1) * interval)
    thumbnails.value = times.map((time) => ({ time, timeStr: formatSeconds(time), base64: '' }))
    await captureAtTimes(times, 0, () => initialCaptureCancelRequested.value, (item) => {
      const target = thumbnails.value.find((thumbnail) => thumbnail.time === item.time)
      if (target) target.base64 = item.base64
    })
    thumbnails.value = thumbnails.value.filter((item) => item.base64)
    if (initialCaptureCancelRequested.value) {
      addLog(`已取消建立初始縮圖，已保留取消前完成的 ${thumbnails.value.length} 張縮圖。`)
    } else {
      initialCaptureSucceeded = true
      addLog(`已建立 ${thumbnails.value.length} 張中間縮圖，起點與終點將在背景擷取。`)
    }
  } catch (error) {
    thumbnails.value = thumbnails.value.filter((item) => item.base64)
    addLog(`擷取失敗：${String(error)}`)
  } finally {
    isProcessing.value = false
    initialCaptureCancelRequested.value = false
    progressText.value = ''
    progressPercent.value = 0
    // If an older UI state queued an expansion while the initial capture was still running,
    // resume it now that the source thumbnails are genuinely ready.
    if (initialCaptureSucceeded && expansionQueue.value.some((job) => job.status === '等待中')) processExpansionQueue()
    if (initialCaptureSucceeded) void captureEndpointThumbnails(duration)
  }
}

function cancelInitialThumbnailCapture() {
  if (!isProcessing.value || initialCaptureCancelRequested.value) return
  initialCaptureCancelRequested.value = true
  progressText.value = '正在取消建立縮圖…'
  stopActiveThumbnail()
  addLog('已要求中止目前截圖，已完成的縮圖會保留。')
}

async function captureEndpointThumbnails(duration: number) {
  if (!showStartEndpoint.value && !showEndEndpoint.value) return
  isEndpointProcessing.value = true
  addLog('開始背景擷取起點與終點縮圖；這不會阻塞擴充佇列。')
  try {
    const endpointResults = await Promise.allSettled([
      showStartEndpoint.value
        ? captureThumbnail({
            ffmpegPath: ffmpegPath.value || null,
            videoPath: videoPath.value,
            time: '0.000',
            index: -1,
          })
        : Promise.resolve(null),
      showEndEndpoint.value
        ? captureThumbnail({
            ffmpegPath: ffmpegPath.value || null,
            videoPath: videoPath.value,
            // Stay a little farther inside the file than the container's reported end. Some long MP4s
            // report a final timestamp for which input-side seek produces an empty image.
            time: Math.max(0, duration - 2).toFixed(3),
            index: -2,
          })
        : Promise.resolve(null),
    ])
    const [startResult, endResult] = endpointResults
    if (showStartEndpoint.value) {
      if (startResult.status === 'fulfilled' && startResult.value) startThumbnail.value = `data:image/jpeg;base64,${startResult.value}`
      else if (startResult.status === 'rejected') addLog(`起點縮圖擷取失敗：${String(startResult.reason)}`)
    }
    if (showEndEndpoint.value) {
      if (endResult.status === 'fulfilled' && endResult.value) endThumbnail.value = `data:image/jpeg;base64,${endResult.value}`
      else if (endResult.status === 'rejected') addLog(`終點縮圖擷取失敗：${String(endResult.reason)}`)
    }
    const startDone = !showStartEndpoint.value || (startResult.status === 'fulfilled' && Boolean(startResult.value))
    const endDone = !showEndEndpoint.value || (endResult.status === 'fulfilled' && Boolean(endResult.value))
    if (startDone && endDone) addLog('端點縮圖已完成。')
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

function removeSelectedPreviewRange() {
  if (!selectedTimeRange.value) return addLog('請先選擇兩張縮圖作為移除範圍。')
  if (isProcessing.value || isEndpointProcessing.value || isExpanding.value || expansionQueue.value.some((job) => job.status === '等待中' || job.status === '處理中')) {
    return addLog('目前仍有縮圖任務進行中，請等待完成或取消後再移除範圍。')
  }
  const { start, end } = selectedTimeRange.value
  const before = thumbnails.value.length
  thumbnails.value = thumbnails.value.filter((item) => item.time < start || item.time > end)
  let removedCount = before - thumbnails.value.length
  if (showStartEndpoint.value && start <= 0 && end >= 0) {
    showStartEndpoint.value = false
    startThumbnail.value = null
    removedCount += 1
  }
  if (showEndEndpoint.value && start <= videoDuration.value && end >= videoDuration.value) {
    showEndEndpoint.value = false
    endThumbnail.value = null
    removedCount += 1
  }
  selectedTimes.value = []
  activeModalIndex.value = null
  gridScrollTop.value = 0
  if (gridRef.value) gridRef.value.scrollTop = 0
  addLog(`已移除 ${formatSeconds(start)} → ${formatSeconds(end)} 範圍內共 ${removedCount} 張縮圖（包含兩端）。`)
}

async function restoreTaskForEditing(taskId: string) {
  const task = videoTasks.find((item) => item.id === taskId)
  if (!task || (task.kind !== 'clip' && task.kind !== 'merge')) {
    addLog('找不到可再編輯的剪輯任務。')
    selectVideoFilePanel()
    return
  }
  if (task.status === '處理中') {
    addLog('處理中的任務無法再編輯，請先等待完成或停止任務。')
    selectVideoFilePanel()
    return
  }
  if (isProcessing.value || isEndpointProcessing.value || isExpanding.value) {
    addLog('目前仍有縮圖正在處理，請等待完成或取消後再開啟任務。')
    return
  }

  loadPaths()
  editingTaskId.value = task.id
  editingCreatesCopy.value = task.status !== '等待中'
  videoPath.value = task.source
  originalSourcePath.value = task.source
  thumbnails.value = []
  startThumbnail.value = null
  endThumbnail.value = null
  expansionQueue.value = []
  activeModalIndex.value = null
  gridScrollTop.value = 0
  taskKind.value = task.kind
  taskMethod.value = task.method
  const segmentDuration = taskSegmentDurationSeconds(task)
  taskDurationHours.value = Math.floor(segmentDuration / 3600)
  taskDurationMinutes.value = Math.floor((segmentDuration % 3600) / 60)
  taskDurationSeconds.value = Math.floor(segmentDuration % 60)
  taskSplitCount.value = task.splitCount
  taskOutputName.value = editingCreatesCopy.value ? `${task.outputName}${taskNameSuffix.value}` : task.outputName
  taskOutputNameAuto.value = false
  taskExtension.value = task.extension
  taskExtraArgs.value = task.extraArgs
  taskPreciseSeek.value = task.preciseSeek
  taskKeepSegments.value = task.keepSegments
  taskSourceMode.value = 'original'
  taskSourceProcessedId.value = ''
  mergeSegments.value = task.kind === 'merge' ? task.segments.map((segment) => ({ ...segment })) : []
  selectedTimes.value = task.kind === 'clip' && task.method === 'range' && task.segments[0]
    ? [task.segments[0].start, task.segments[0].end]
    : []

  const timeline = task.previewTimeline
  if (!timeline?.times.length || !timeline.duration) {
    showStartEndpoint.value = true
    showEndEndpoint.value = true
    videoDuration.value = 0
    loadPreviewColumnsPreference()
    activePanel.value = 'load'
    loadSubPanel.value = 'init'
    addLog('這筆舊任務沒有保存縮圖時間軸；請重新建立縮圖後再修改任務。')
    void loadVideoDuration(task.source)
    return
  }

  videoDuration.value = timeline.duration
  previewColumns.value = Math.max(1, Math.min(12, Math.floor(timeline.gridCols || 4)))
  const times = [...new Set(timeline.times.map(roundPreviewTime))].sort((a, b) => a - b)
  showStartEndpoint.value = times.some((time) => time === 0)
  showEndEndpoint.value = times.some((time) => Math.abs(time - timeline.duration) <= 0.0005)
  const middleTimes = times.filter((time) => time > 0 && time < timeline.duration)
  thumbnails.value = middleTimes.map((time) => ({ time, timeStr: formatSeconds(time), base64: '' }))
  activePanel.value = 'load'
  loadSubPanel.value = 'init'
  isProcessing.value = true
  initialCaptureCancelRequested.value = false
  addLog(`正在依任務保存的 ${times.length} 個時間點重新建立縮圖。`)
  let completed = false
  try {
    await captureAtTimes(middleTimes, 0, () => initialCaptureCancelRequested.value, (item) => {
      const target = thumbnails.value.find((thumbnail) => thumbnail.time === item.time)
      if (target) target.base64 = item.base64
    })
    thumbnails.value = thumbnails.value.filter((item) => item.base64)
    completed = !initialCaptureCancelRequested.value
    if (completed) addLog(`已恢復 ${thumbnails.value.length} 張中間縮圖，可繼續調整任務。`)
    else addLog(`已取消恢復縮圖，保留已完成的 ${thumbnails.value.length} 張。`)
  } catch (error) {
    thumbnails.value = thumbnails.value.filter((item) => item.base64)
    addLog(`恢復任務縮圖失敗：${String(error)}`)
  } finally {
    isProcessing.value = false
    initialCaptureCancelRequested.value = false
    progressText.value = ''
    progressPercent.value = 0
    activePanel.value = 'task'
    if (completed) void captureEndpointThumbnails(timeline.duration)
  }
}

function cancelTaskEditing() {
  editingTaskId.value = null
  editingCreatesCopy.value = false
  router.push('/tasks')
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
  const count = Math.max(1, Math.floor(job.count || 1))
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
    count: expansionCount.value,
    label: `${formatSeconds(start)} → ${formatSeconds(end)}`,
    status: '等待中',
    message: '',
    cancelRequested: false,
    times: [],
  }
  job.times = buildTimesForJob(job)
  if (!job.times.length) return addLog('無法加入截圖任務：目前設定沒有可插入的時間點。')
  const insertAt = indexForTime(job.anchorEndTime)
  thumbnails.value.splice(
    insertAt,
    0,
    ...job.times.map((time) => ({ time, timeStr: formatSeconds(time), base64: '' })),
  )
  expansionQueue.value.push(job)
  selectedTimes.value = []
  addLog(`已加入截圖任務：${job.label}`)
  processExpansionQueue()
}

function removeExpansionJob(id: string) {
  const index = expansionQueue.value.findIndex((job) => job.id === id && job.status === '等待中')
  if (index === -1) return
  removeUnfinishedExpansionPlaceholders(expansionQueue.value[index])
  expansionQueue.value.splice(index, 1)
}

function removeUnfinishedExpansionPlaceholders(job: ExpansionJob) {
  const timeSet = new Set(job.times)
  thumbnails.value = thumbnails.value.filter((item) => item.base64 || !timeSet.has(item.time))
}

// Stop the current backend capture as well as subsequent frames; completed frames stay visible.
function cancelExpansionJob(id: string) {
  const job = expansionQueue.value.find((item) => item.id === id && item.status === '處理中')
  if (job) {
    job.cancelRequested = true
    progressText.value = '正在取消截圖…'
    stopActiveThumbnail()
  }
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
  const times = job.times
  if (!times.length) {
    const span = Math.max(0, job.anchorEndTime - job.anchorStartTime)
    const interval = Math.max(1, Math.floor(Number(job.interval) || 1))
    const message = `沒有符合條件的新時間點（範圍 ${formatSeconds(span)}，設定 ${interval} 秒）`
    job.message = message
    addLog(`擴充失敗（${job.label}）：${message}`)
    throw new Error(message)
  }
  let additions: ThumbnailItem[] = []
  try {
    additions = await captureAtTimes(times, thumbnails.value.length, () => job.cancelRequested, (item) => {
      const target = thumbnails.value.find((thumbnail) => thumbnail.time === item.time)
      if (target) target.base64 = item.base64
    })
  } catch (error) {
    removeUnfinishedExpansionPlaceholders(job)
    throw error
  }
  if (job.cancelRequested) {
    removeUnfinishedExpansionPlaceholders(job)
  }
  if (job.cancelRequested) {
    addLog(`已取消「${job.label}」，已插入取消前完成的 ${additions.length} 張縮圖。`)
  } else {
    addLog(`已在「${job.label}」之間插入 ${additions.length} 張新縮圖。`)
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
  if (mergeEditingIndex.value === index) {
    mergeEditingIndex.value = null
    selectedTimes.value = []
  } else if (mergeEditingIndex.value !== null && mergeEditingIndex.value > index) {
    mergeEditingIndex.value -= 1
  }
}

function moveMergeSegment(index: number, direction: -1 | 1) {
  const target = index + direction
  if (target < 0 || target >= mergeSegments.value.length) return
  const [segment] = mergeSegments.value.splice(index, 1)
  mergeSegments.value.splice(target, 0, segment)
  if (mergeEditingIndex.value === index) mergeEditingIndex.value = target
  else if (mergeEditingIndex.value === target) mergeEditingIndex.value = index
}

function editMergeSegment(index: number) {
  const segment = mergeSegments.value[index]
  if (!segment) return
  mergeEditingIndex.value = index
  selectedTimes.value = [segment.start, segment.end]
  const firstIndex = gridSequence.value.findIndex((item) => item.time === segment.start)
  if (firstIndex >= 0) gridRef.value?.scrollTo({ top: Math.floor(firstIndex / gridColumnCount.value) * gridRowPitch.value, behavior: 'smooth' })
}

function applyMergeSegmentEdit() {
  if (mergeEditingIndex.value === null || !selectedTimeRange.value) return
  mergeSegments.value[mergeEditingIndex.value] = { ...selectedTimeRange.value }
  addLog(`已更新合併片段 ${mergeEditingIndex.value + 1}。`)
  mergeEditingIndex.value = null
  selectedTimes.value = []
}

function cancelMergeSegmentEdit() {
  mergeEditingIndex.value = null
  selectedTimes.value = []
}

function roundPreviewTime(time: number) {
  return Math.round(time * 1000) / 1000
}

function buildPreviewTimeline(): TaskPreviewTimeline | undefined {
  if (!videoDuration.value || !hasPreviewContent.value) return undefined
  const times = [...new Set(gridSequence.value.map((item) => roundPreviewTime(item.time)))].sort((a, b) => a - b)
  return {
    duration: roundPreviewTime(videoDuration.value),
    times,
    gridCols: previewColumns.value,
  }
}

function buildCalculatedClipSegments(method: ClipMethod): VideoSegment[] {
  const duration = videoDuration.value
  if (!Number.isFinite(duration) || duration <= 0) return []
  if (method === 'duration') {
    const segmentDuration = configuredSegmentDuration.value
    if (!Number.isFinite(segmentDuration) || segmentDuration <= 0) return []
    return Array.from({ length: Math.ceil(duration / segmentDuration) }, (_, index) => ({
      start: index * segmentDuration,
      end: Math.min(duration, (index + 1) * segmentDuration),
    }))
  }
  if (method === 'count') {
    const count = Math.max(1, Math.floor(Number(taskSplitCount.value) || 1))
    return Array.from({ length: count }, (_, index) => ({
      start: (duration * index) / count,
      end: (duration * (index + 1)) / count,
    }))
  }
  return []
}

function saveTaskFromEditor(kind: 'clip' | 'merge', segments: VideoSegment[]) {
  const sourceTask = editingTask.value
  const task: VideoTask = {
    id: newTaskId(),
    kind,
    method: kind === 'merge' ? 'range' : taskMethod.value,
    source: taskSourcePath.value,
    segments: segments.map((segment) => ({ start: roundPreviewTime(segment.start), end: roundPreviewTime(segment.end) })),
    durationHours: taskDurationHours.value,
    durationMinutes: taskDurationMinutes.value,
    durationSeconds: taskDurationSeconds.value,
    splitCount: taskSplitCount.value,
    outputName: taskOutputName.value.trim() || (kind === 'clip' ? 'clip' : 'merged_video'),
    extension: taskExtension.value.trim() || 'mp4',
    extraArgs: taskExtraArgs.value,
    keepSegments: taskKeepSegments.value,
    outputDir: sourceTask?.outputDir || '',
    resultPath: '',
    preciseSeek: taskPreciseSeek.value,
    combineSources: [],
    combineMode: 'copy',
    status: '等待中',
    message: '',
    previewTimeline: buildPreviewTimeline(),
  }

  const shouldUpdateExisting = Boolean(sourceTask && !editingCreatesCopy.value && sourceTask.status === '等待中')
  if (shouldUpdateExisting && sourceTask) {
    Object.assign(sourceTask, task, { id: sourceTask.id })
    addLog(`已更新任務：${task.outputName}。`)
  } else {
    videoTasks.push(task)
    addLog(editingTaskId.value ? `已從原任務建立新的編輯任務：${task.outputName}。` : `已建立任務：${task.outputName}。`)
  }

  editingTaskId.value = null
  editingCreatesCopy.value = false
  selectedTimes.value = []
  mergeSegments.value = []
  router.push('/tasks')
}

function createClipTask() {
  if (taskSourceMode.value === 'batch') return createBatchClipTasks()
  if (!taskSourcePath.value) return addLog('請先選擇影片檔案，或選擇要使用的已處理影片。')
  let segments: VideoSegment[] = []
  if (taskMethod.value === 'range') {
    if (!selectedTimeRange.value) return addLog('請先在縮圖中選擇兩個時間點作為剪輯範圍。')
    segments = [{ start: selectedTimeRange.value.start, end: selectedTimeRange.value.end }]
  } else {
    if (taskMethod.value === 'duration' && (!durationInputIsValid.value || configuredSegmentDuration.value <= 0)) {
      return addLog('請輸入有效的分段時間長度；分鐘與秒鐘必須介於 0 到 59。')
    }
    if (taskMethod.value === 'count' && (!Number.isFinite(Number(taskSplitCount.value)) || Number(taskSplitCount.value) < 2)) {
      return addLog('分割數量至少需要 2 段。')
    }
    segments = buildCalculatedClipSegments(taskMethod.value)
  }
  saveTaskFromEditor('clip', segments)
}

function batchSplitCount(source: BatchSplitSource) {
  if (!source.duration) return 0
  if (taskMethod.value === 'duration') return configuredSegmentDuration.value > 0 ? Math.ceil(source.duration / configuredSegmentDuration.value) : 0
  return taskMethod.value === 'count' ? Math.max(1, Math.floor(Number(taskSplitCount.value) || 1)) : 0
}

async function selectBatchSplitVideos() {
  const selected = await open({
    multiple: true,
    directory: false,
    title: '選擇要套用相同設定的影片',
    filters: [{ name: 'Video', extensions: ['mp4', 'mkv', 'avi', 'mov', 'flv', 'webm', 'm4v', 'ts', 'mpeg', 'mpg', 'wmv'] }],
  })
  const paths = Array.isArray(selected) ? selected : typeof selected === 'string' ? [selected] : []
  const existing = new Set(batchSplitSources.value.map((source) => normaliseVideoPath(source.path)))
  const additions = paths
    .filter((path) => {
      const key = normaliseVideoPath(path)
      if (existing.has(key)) return false
      existing.add(key)
      return true
    })
    .map((path) => ({ path, duration: 0, loading: true, error: '' }))
  batchSplitSources.value.push(...additions)
  for (const source of additions) {
    try {
      source.duration = await invoke<number>('get_video_duration', { ffprobePath: ffprobePath.value || null, videoPath: source.path })
    } catch (error) {
      source.error = String(error)
    } finally {
      source.loading = false
    }
  }
}

function removeBatchSplitSource(index: number) {
  batchSplitSources.value.splice(index, 1)
}

function moveBatchSplitSource(index: number, direction: -1 | 1) {
  const target = index + direction
  if (target < 0 || target >= batchSplitSources.value.length) return
  const [source] = batchSplitSources.value.splice(index, 1)
  batchSplitSources.value.splice(target, 0, source)
}

function createBatchClipTasks() {
  if (!batchSourceModeAvailable.value) return addLog('批次分割只適用於時間長度或依數量分割。')
  if (!batchSourcesReady.value) return addLog('請先選擇影片，並等待所有影片長度讀取完成。')
  if (taskMethod.value === 'duration' && (!durationInputIsValid.value || configuredSegmentDuration.value <= 0)) {
    return addLog('請輸入有效的分段時間長度；分鐘與秒鐘必須介於 0 到 59。')
  }
  if (taskMethod.value === 'count' && (!Number.isFinite(Number(taskSplitCount.value)) || Number(taskSplitCount.value) < 2)) {
    return addLog('分割數量至少需要 2 段。')
  }
  const outputNames = batchSplitSources.value.map((source) => `${baseNameWithoutExt(source.path)}${taskNameSuffix.value}.${taskExtension.value.trim() || 'mp4'}`.toLowerCase())
  if (new Set(outputNames).size !== outputNames.length) {
    return addLog('批次影片中有重複的輸出檔名。請移除同名來源，或分開建立任務，避免輸出互相覆蓋。')
  }
  for (const source of batchSplitSources.value) {
    const segmentDuration = configuredSegmentDuration.value
    const segmentCount = taskMethod.value === 'duration' ? Math.ceil(source.duration / segmentDuration) : Math.max(1, Math.floor(taskSplitCount.value))
    const segments = Array.from({ length: segmentCount }, (_, index) => taskMethod.value === 'duration'
      ? { start: index * segmentDuration, end: Math.min(source.duration, (index + 1) * segmentDuration) }
      : { start: (source.duration * index) / segmentCount, end: (source.duration * (index + 1)) / segmentCount })
    videoTasks.push({
      id: newTaskId(),
      kind: 'clip',
      method: taskMethod.value,
      source: source.path,
      segments: segments.map((segment) => ({ start: roundPreviewTime(segment.start), end: roundPreviewTime(segment.end) })),
      durationHours: taskDurationHours.value,
      durationMinutes: taskDurationMinutes.value,
      durationSeconds: taskDurationSeconds.value,
      splitCount: taskSplitCount.value,
      outputName: `${baseNameWithoutExt(source.path)}${taskNameSuffix.value}`,
      extension: taskExtension.value.trim() || 'mp4',
      extraArgs: taskExtraArgs.value,
      keepSegments: false,
      outputDir: '',
      resultPath: '',
      preciseSeek: taskPreciseSeek.value,
      combineSources: [],
      combineMode: 'copy',
      status: '等待中',
      message: '',
    })
  }
  addLog(`已建立 ${batchSplitSources.value.length} 筆批次分割任務。`)
  batchSplitSources.value = []
  router.push('/tasks')
}

function createMergeTask() {
  if (!taskSourcePath.value) return addLog('請先選擇影片檔案，或選擇要使用的已處理影片。')
  if (mergeSegments.value.length < 2) return addLog('請至少加入兩個片段才能建立剪輯＋合併任務。')
  saveTaskFromEditor('merge', mergeSegments.value)
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
    durationMinutes: taskDurationMinutes.value,
    durationSeconds: taskDurationSeconds.value,
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

      <section
        v-if="activePanel === 'load'"
        class="card load-panel-card"
        :class="{ 'initial-mode': loadSubPanel === 'init', 'has-processed-videos': processedVideos.length > 0 }"
      >
        <div class="panel-tabs sub-tabs">
          <button type="button" class="panel-tab" :class="{ active: loadSubPanel === 'source' }" @click="loadSubPanel = 'source'">影片檔案</button>
          <button type="button" class="panel-tab" :class="{ active: loadSubPanel === 'init' }" @click="loadSubPanel = 'init'">建立初始縮圖</button>
          <button type="button" class="panel-tab" :class="{ active: loadSubPanel === 'expand' }" @click="loadSubPanel = 'expand'">
            <span>擴充縮圖</span><span v-if="expansionQueue.length" class="sub-tab-count">{{ expansionQueue.length }}</span>
          </button>
        </div>

          <div v-if="loadSubPanel === 'source'" class="load-panel-source-area">
            <div class="subsection source-section">
              <div class="input-group no-bottom" :class="{ 'drop-target': isDraggingFile }">
                <div class="video-path-row">
                  <div class="video-path-display" :title="videoPath">{{ videoPath || '請選擇影片，或將影片檔案拖曳到視窗中' }}</div>
                </div>
                <div class="source-actions-row">
                  <button class="btn btn-secondary compact" @click="selectVideo">選擇影片</button>
                  <button class="btn btn-secondary compact" title="選擇影片進行預先處理（可多選）" @click="selectVideosForPreprocessing">選擇影片預先處理</button>
                  <div class="info-hint-wrap" ref="infoHintWrap">
                    <button type="button" class="info-hint" @click="showPreprocessHint = !showPreprocessHint">說明</button>
                    <div v-if="showPreprocessHint" class="info-popover">
                      若擷取的秒數跟原始影片對不上，可以多選幾支影片加入預先處理（remux 成 mp4，修正時間軸），到「影片處理任務」執行後回來這裡選用。
                    </div>
                  </div>
                </div>
                <p class="field-hint">也可以直接把影片檔案拖曳到這個視窗中載入。</p>
              </div>
          </div>
          <div v-if="processedVideos.length" class="processed-list">
            <div class="processed-list-header">
              <span>已處理影片（{{ processedVideos.length }}）</span>
              <div class="processed-list-actions">
                <button class="btn-clear" @click="clearProcessedVideos">清除全部</button>
                <button class="btn btn-danger compact" :disabled="isDeletingProcessed" @click="requestDeleteAllProcessedVideos">刪除全部</button>
              </div>
            </div>
            <div v-if="showDeleteProcessedConfirm" class="processed-delete-confirm">
              <span>確定要刪除全部已處理影片嗎？實際影片檔案也會刪除，且無法復原。</span>
              <div class="processed-delete-confirm-actions">
                <button class="btn btn-secondary compact" @click="cancelDeleteAllProcessedVideos">取消</button>
                <button class="btn btn-danger compact" @click="deleteAllProcessedVideos">確定刪除</button>
              </div>
            </div>
            <div class="processed-items">
              <div v-for="task in processedVideos" :key="task.id" class="processed-item">
                <span class="processed-name" :title="task.resultPath">{{ task.outputName }}.{{ task.extension }}</span>
                <button class="btn btn-primary compact" @click="useProcessedVideo(task)">使用</button>
                <button class="btn-clear" @click="removeProcessedVideo(task.id)">移除</button>
              </div>
            </div>
          </div>
        </div>

        <div v-if="loadSubPanel !== 'source'" class="load-panel-capture-area">
          <div v-if="loadSubPanel === 'init'" class="subsection capture-section">
          <div class="capture-settings-box">
            <div class="capture-settings-header">
              <div>
                <strong>縮圖擷取設定</strong>
                <span :class="['capture-speed-badge', { fast: legacyFastCaptureActive }]">{{ legacyFastCaptureActive ? '舊版高速路徑' : '自訂處理' }}</span>
              </div>
              <button type="button" class="btn-clear" @click="showAdvancedCaptureSettings = !showAdvancedCaptureSettings">
                {{ showAdvancedCaptureSettings ? '收合' : '進階設定' }}
              </button>
            </div>
            <div v-if="showAdvancedCaptureSettings" class="capture-settings-grid">
              <div class="input-group capture-setting-wide">
                <label>方案</label>
                <select v-model="thumbnailPreset" :disabled="thumbnailSettingsLocked" @change="applyThumbnailPreset">
                  <option value="legacy">高速相容（原始尺寸、快速定位）</option>
                  <option value="compact">節省空間（480p）</option>
                  <option value="quality">高品質（720p、精確定位）</option>
                </select>
              </div>
              <div class="input-group">
                <label>最大高度（px）</label>
                <input v-model.number="thumbnailMaxHeight" :disabled="thumbnailSettingsLocked || thumbnailOriginalSize" type="number" min="144" max="2160" step="40" @change="persistThumbnailSettings" />
              </div>
              <div class="input-group">
                <label>JPEG 品質（2 最佳、12 最快）</label>
                <input v-model.number="thumbnailQuality" :disabled="thumbnailSettingsLocked" type="number" min="2" max="12" @change="persistThumbnailSettings" />
              </div>
              <div class="input-group">
                <label>定位模式</label>
                <select v-model="thumbnailSeekMode" :disabled="thumbnailSettingsLocked" @change="persistThumbnailSettings"><option value="fast">快速</option><option value="precise">精確（較慢）</option></select>
              </div>
              <div class="input-group capture-setting-toggle">
                <label>圖片尺寸</label>
                <button type="button" class="original-size-button" :class="{ active: thumbnailOriginalSize }" :disabled="thumbnailSettingsLocked" @click="thumbnailOriginalSize = !thumbnailOriginalSize; persistThumbnailSettings()">
                  {{ thumbnailOriginalSize ? '✓ 使用原始尺寸' : `限制為 ${thumbnailMaxHeight}p` }}
                </button>
              </div>
              <label class="checkbox-row capture-setting-wide"><input v-model="thumbnailHardwareAcceleration" :disabled="thumbnailSettingsLocked" type="checkbox" @change="persistThumbnailSettings" /> 實驗性硬體解碼（失敗時自動改用軟體解碼）</label>
              <p class="capture-setting-note capture-setting-wide">縮放、精確定位或硬體解碼會離開舊版高速路徑；擷取固定依序進行，不會同時搶用磁碟。</p>
            </div>
          </div>
          <div class="mode-row">
            <label><input v-model="initialCaptureMode" type="radio" value="count" /> 指定張數</label>
            <label><input v-model="initialCaptureMode" type="radio" value="interval" /> 指定截圖間隔</label>
          </div>
          <div v-if="initialCaptureMode === 'count'" class="input-group">
            <label>中間縮圖張數</label><input v-model.number="initialThumbnailCount" type="number" min="0" step="1" />
          </div>
          <div v-else class="input-group">
            <label>每隔幾秒擷取一張</label><input v-model.number="initialInterval" type="number" min="0.1" step="0.1" />
          </div>
          <p v-if="initialCaptureMode === 'count'" class="info-text">將影片等分擷取 {{ plannedThumbnailCount }} 張中間縮圖，連同開頭與結尾共 {{ plannedThumbnailCount + 2 }} 張。</p>
          <p v-else class="info-text">
            {{ durationLoadFailed ? '無法預先讀取影片長度；請確認影片與 ffprobe 設定。' : durationLoading || !videoDuration ? '正在讀取影片長度，以計算縮圖張數…' : `影片長度 ${formatSeconds(videoDuration)}；預計擷取 ${plannedThumbnailCount} 張中間縮圖，加上起點與終點共 ${plannedThumbnailCount + 2} 格。` }}
          </p>
          <p v-if="initialCaptureMode === 'interval' && plannedThumbnailCount >= 1000" class="info-text">大量縮圖需要較長擷取時間；可隨時取消，已完成的縮圖會保留。</p>
          <button
            v-if="isProcessing"
            class="btn btn-danger w-full"
            :disabled="initialCaptureCancelRequested"
            @click="cancelInitialThumbnailCapture"
          >
            {{ initialCaptureCancelRequested ? '取消中…' : '取消建立縮圖' }}
          </button>
          <button v-else class="btn btn-primary btn-glow w-full" :disabled="isEndpointProcessing || isExpanding" @click="generateInitialThumbnails">
            建立縮圖
          </button>
        </div>

        <div v-else class="subsection capture-section expansion-section">
          <p class="info-text">在右側依序選擇兩張相鄰縮圖，再選擇插入方式。</p>
          <div class="mode-row">
            <label><input v-model="expansionMode" type="radio" value="interval" /> 固定秒數</label
            ><label><input v-model="expansionMode" type="radio" value="single" /> 插入單張</label
            ><label><input v-model="expansionMode" type="radio" value="count" /> 指定張數</label>
          </div>
          <div v-if="expansionMode === 'interval' || expansionMode === 'single'" class="input-group">
            <label>秒數（至少 1 秒）</label><input v-model.number="expansionInterval" type="number" min="1" />
            <div class="quick-set-row">
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 1 }" @click="expansionInterval = 1">1 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 3 }" @click="expansionInterval = 3">3 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 5 }" @click="expansionInterval = 5">5 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 10 }" @click="expansionInterval = 10">10 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 30 }" @click="expansionInterval = 30">30 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 60 }" @click="expansionInterval = 60">60 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 120 }" @click="expansionInterval = 120">120 秒</button>
              <button type="button" class="quick-set-btn" :class="{ active: expansionInterval === 300 }" @click="expansionInterval = 300">300 秒</button>
            </div>
          </div>
          <div v-if="expansionMode === 'single'" class="mode-row">
            <label><input v-model="singleInsertAnchor" type="radio" value="start" /> 從起點（左側縮圖）往後 {{ expansionInterval }} 秒</label
            ><label><input v-model="singleInsertAnchor" type="radio" value="end" /> 從終點（右側縮圖）往前 {{ expansionInterval }} 秒</label>
          </div>
          <div v-if="expansionMode === 'count'" class="input-group">
            <label>在範圍內等距插入幾張</label><input v-model.number="expansionCount" type="number" min="1" step="1" />
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
                <span class="job-status" :class="job.status">{{ job.status === '處理中' && job.cancelRequested ? '正在取消…' : job.status }}</span>
                <button v-if="job.status === '等待中'" class="btn-clear" @click="removeExpansionJob(job.id)">移除</button>
                <button v-else-if="job.status === '處理中'" class="btn-clear" :disabled="job.cancelRequested" @click="cancelExpansionJob(job.id)">取消</button>
              </div>
            </div>
          </div>
        </div>

          <div v-if="progressText" class="progress-container">
            <div class="progress-bar-bg"><div class="progress-bar-fill" :style="{ width: `${progressPercent}%` }"></div></div>
            <div class="progress-text">{{ progressText }}</div>
          </div>
        </div>
      </section>

      <section v-if="activePanel === 'task'" class="card task-builder-card">
        <div class="card-header-with-btn">
          <h2 class="card-title">{{ editingTaskId ? '再編輯處理任務' : '建立處理任務' }}</h2>
          <button v-if="editingTaskId" class="btn-clear" @click="cancelTaskEditing">取消再編輯</button>
        </div>
        <p v-if="editingTaskId" class="edit-task-notice">
          {{ editingCreatesCopy ? '原任務會保留；儲存後將建立一筆新的等待任務，輸出檔名已自動加上後綴。' : '儲存後會直接更新這筆等待中的任務。' }}
        </p>
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
            <label v-if="batchSourceModeAvailable"><input v-model="taskSourceMode" type="radio" value="batch" /> 多個影片（批次）</label>
          </div>
          <select v-if="taskSourceMode === 'processed'" v-model="taskSourceProcessedId">
            <option value="" disabled>請選擇已處理影片</option>
            <option v-for="task in processedVideos" :key="task.id" :value="task.id">{{ task.outputName }}.{{ task.extension }}</option>
          </select>
          <div v-if="taskSourceMode === 'batch'" class="batch-source-panel">
            <div class="batch-source-actions">
              <button type="button" class="btn btn-secondary compact" @click="selectBatchSplitVideos">選擇多支影片</button>
              <button v-if="batchSplitSources.length" type="button" class="btn-clear" @click="batchSplitSources = []">清空</button>
            </div>
            <div v-if="batchSplitSources.length" class="batch-source-list">
              <div v-for="(source, index) in batchSplitSources" :key="source.path" class="batch-source-item">
                <span class="batch-source-index">{{ index + 1 }}</span>
                <div class="batch-source-info">
                  <strong :title="source.path">{{ baseName(source.path) }}</strong>
                  <span v-if="source.loading">正在讀取影片長度…</span>
                  <span v-else-if="source.error" class="batch-source-error">無法讀取：{{ source.error }}</span>
                  <span v-else>{{ formatSeconds(source.duration) }}・預計 {{ batchSplitCount(source) }} 段</span>
                </div>
                <button type="button" class="btn-clear" :disabled="index === 0" aria-label="向上移動" @click="moveBatchSplitSource(index, -1)">↑</button>
                <button type="button" class="btn-clear" :disabled="index === batchSplitSources.length - 1" aria-label="向下移動" @click="moveBatchSplitSource(index, 1)">↓</button>
                <button type="button" class="btn-clear danger-text" @click="removeBatchSplitSource(index)">移除</button>
              </div>
            </div>
            <p v-else class="selection-summary muted">尚未選擇批次分割影片；不會切換目前預覽中的影片。</p>
          </div>
          <p v-else class="selection-summary break-all" :class="{ muted: !taskSourcePath }">
            {{ taskSourcePath ? `將對「${baseName(taskSourcePath)}」${editingTaskId ? '儲存修改' : '建立任務'}` : '尚未選擇來源影片' }}
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
            <label>每段時間長度</label>
            <div class="duration-input-grid">
              <label><input v-model.number="taskDurationHours" type="number" min="0" step="1" /> 小時</label>
              <label><input v-model.number="taskDurationMinutes" type="number" min="0" max="59" step="1" /> 分鐘</label>
              <label><input v-model.number="taskDurationSeconds" type="number" min="0" max="59" step="1" /> 秒</label>
            </div>
            <div v-if="durationSplitCount" class="duration-split-preview">
              <strong>預計輸出 {{ durationSplitCount }} 段</strong>
              <p v-if="configuredSegmentDuration >= videoDuration">設定長度大於或等於影片長度，因此只會輸出一個完整片段。</p>
              <ol>
                <li v-for="(segment, index) in visibleDurationSplitPreview" :key="`${segment.start}-${segment.end}`">
                  <span v-if="durationSplitCount > 8 && index === 7">… 最後一段：</span>{{ formatSeconds(segment.start) }} → {{ formatSeconds(segment.end) }}
                </li>
              </ol>
            </div>
            <p v-else class="field-hint">時間長度必須大於 0；最後不足指定長度的部分仍會保留。</p>
          </div>
          <div v-else class="input-group">
            <label>平均分割數量</label>
            <input v-model.number="taskSplitCount" type="number" min="2" step="1" />
          </div>
        </template>

        <template v-else>
          <p class="info-text">在右側縮圖選擇任意兩個時間點加入片段；使用上下按鈕調整合併順序。</p>
          <div v-if="mergeEditingIndex !== null" class="merge-edit-notice">
            <span>正在編輯片段 {{ mergeEditingIndex + 1 }}，請在右側重新選擇頭尾時間，再按「套用到片段」。</span>
            <button type="button" class="btn btn-secondary compact" @click="cancelMergeSegmentEdit">取消編輯</button>
          </div>
          <div v-if="mergeSegments.length" class="merge-list merge-segment-list">
            <div
              v-for="(segment, index) in mergeSegments"
              :key="index"
              class="merge-item"
              :class="{ editing: mergeEditingIndex === index }"
            >
              <span class="merge-index">{{ index + 1 }}</span>
              <span class="merge-range">{{ formatSeconds(segment.start) }} → {{ formatSeconds(segment.end) }}</span>
              <div class="merge-item-actions">
                <button class="merge-action" :disabled="index === 0" title="向上移動" @click="moveMergeSegment(index, -1)">↑</button>
                <button class="merge-action" :disabled="index === mergeSegments.length - 1" title="向下移動" @click="moveMergeSegment(index, 1)">↓</button>
                <button class="merge-action" @click="editMergeSegment(index)">編輯</button>
                <button class="merge-remove" @click="removeMergeSegment(index)">移除</button>
              </div>
            </div>
          </div>
          <p v-else class="selection-summary muted">尚未加入任何片段。</p>
          <label class="checkbox-row"><input v-model="taskKeepSegments" type="checkbox" /> 合併後保留個別剪輯片段</label>
        </template>

        <div v-if="taskSourceMode !== 'batch'" class="row">
          <div class="col input-group"><label>輸出檔名</label><input v-model="taskOutputName" @input="taskOutputNameAuto = false" /></div>
          <div class="col input-group"><label>副檔名</label><input v-model="taskExtension" /></div>
        </div>
        <div v-else class="input-group">
          <label>輸出副檔名</label><input v-model="taskExtension" />
          <p class="field-hint">輸出檔名會使用各來源檔名加上「{{ taskNameSuffix }}」，每個分段再附加流水號。</p>
        </div>
        <div class="input-group"><label>額外 ffmpeg 參數（合併時套用，以空白分隔）</label><input v-model="taskExtraArgs" placeholder="例如：-crf 20" /></div>
        <div class="input-group">
          <label>切割定位方式</label>
          <select v-model="taskPreciseSeek">
            <option :value="false">快速定位（建議，適合長影片）</option>
            <option :value="true">精細定位（較慢，適合時間戳異常的影片）</option>
          </select>
          <p class="field-hint">兩種方式都不會重新編碼，速度較快；切點仍受關鍵影格限制，可能有數秒誤差。</p>
        </div>

        <button v-if="taskKind === 'clip'" class="btn btn-primary btn-glow w-full" @click="createClipTask">
          {{ taskSourceMode === 'batch' ? `建立 ${batchSplitSources.length} 筆分割任務` : editingTaskId ? (editingCreatesCopy ? '另存新的剪輯任務' : '儲存剪輯任務修改') : '建立剪輯任務' }}
        </button>
        <button v-else class="btn btn-primary btn-glow w-full" :disabled="mergeSegments.length < 2" @click="createMergeTask">
          {{ editingTaskId ? (editingCreatesCopy ? '另存新的剪輯＋合併任務' : '儲存剪輯＋合併任務修改') : '建立剪輯＋合併任務' }}
        </button>
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
          <h2 class="card-title">縮圖列表（{{ previewDisplayCount }} 張）</h2>
        </div>
        <div v-if="hasPreviewContent" class="current-layout-controls">
          <label>每列欄數
            <input v-model.number="previewColumns" class="preview-column-input" type="number" min="1" max="12" step="1" @change="persistPreviewColumns" />
          </label>
          <template v-if="selectedTimeRange">
            <span class="row-divider">|</span>
            <span>已選擇範圍：{{ formatSeconds(selectedTimeRange.start) }} → {{ formatSeconds(selectedTimeRange.end) }}</span>
            <button v-if="mergeEditingIndex === null" class="btn btn-danger compact" @click="removeSelectedPreviewRange">移除範圍內縮圖（含兩端）</button>
            <button v-if="taskKind === 'merge' && mergeEditingIndex === null" class="btn btn-primary compact" @click="addSegmentToMergeList">＋ 加入合併清單</button>
          </template>
          <template v-if="taskKind === 'merge' && mergeEditingIndex !== null">
            <span v-if="!selectedTimeRange" class="merge-edit-selection-hint">{{ selectedTimes.length ? '已選新起點，請再選終點' : '請選擇新的起點與終點' }}</span>
            <button class="btn btn-primary compact" :disabled="!selectedTimeRange" @click="applyMergeSegmentEdit">套用到片段 {{ mergeEditingIndex + 1 }}</button>
            <button class="btn btn-secondary compact" @click="cancelMergeSegmentEdit">取消編輯</button>
          </template>
          <div ref="similarityPopoverWrap" class="similarity-tool">
            <button
              type="button"
              class="similarity-toggle"
              :class="{ active: similarityPanelOpen }"
              :aria-expanded="similarityPanelOpen"
              @click="similarityPanelOpen = !similarityPanelOpen"
            >
              <span>相似縮圖</span>
              <span v-if="similarityAnalyzing" class="similarity-toggle-status">{{ similarityProgress }}%</span>
              <span v-else-if="similarityGroups.length" class="similarity-toggle-status">{{ similarityGroups.length }}</span>
              <span>{{ similarityPanelOpen ? '▲' : '▼' }}</span>
            </button>
            <div v-if="similarityPanelOpen" class="similarity-panel" @click.stop>
              <div class="similarity-controls">
                <label>相似判定
                  <select v-model="similarityLevel" :disabled="similarityAnalyzing">
                    <option value="strict">嚴格</option>
                    <option value="normal">一般</option>
                    <option value="loose">寬鬆</option>
                  </select>
                </label>
                <label>最少連續
                  <input v-model.number="similarityMinCount" :disabled="similarityAnalyzing" type="number" min="2" max="100" step="1" /> 張
                </label>
                <label class="similarity-marker-toggle"><input v-model="showSimilarityMarkers" type="checkbox" /> 顯示區段標記</label>
                <button v-if="!similarityAnalyzing" type="button" class="btn btn-secondary compact" @click="analyzeSimilarThumbnails">分析相似縮圖</button>
                <button v-else type="button" class="btn btn-danger compact" @click="cancelSimilarityAnalysis">取消分析（{{ similarityProgress }}%）</button>
                <button v-if="similarityGroups.length && !similarityAnalyzing" type="button" class="btn-clear" @click="clearSimilarityAnalysis">清除結果</button>
              </div>
              <div v-if="similarityAnalyzing" class="similarity-progress"><span :style="{ width: `${similarityProgress}%` }"></span></div>
              <p v-if="similarityResultOutdated" class="field-hint warning-text">縮圖數量已改變，目前標記可能已過期，建議重新分析。</p>
              <div v-if="similarityGroups.length" class="similarity-results">
                <strong>找到 {{ similarityGroups.length }} 個相似區段</strong>
                <div
                  v-for="group in similarityGroups"
                  :key="group.id"
                  class="similarity-result-wrap"
                  :class="similarityColorClass(group.id)"
                  @mouseenter="hoveredSimilarityGroup = group.id"
                  @mouseleave="hoveredSimilarityGroup = null"
                >
                  <label class="similarity-result-check"><input type="checkbox" :checked="selectedSimilarityGroupIds.includes(group.id)" @change="toggleSimilarityGroup(group.id)" />套用</label>
                  <button type="button" class="similarity-result" @focus="hoveredSimilarityGroup = group.id" @blur="hoveredSimilarityGroup = null" @click="selectSimilarityGroup(group)">
                    <span>區段 {{ group.id }}</span>
                    <span>{{ formatSeconds(group.times[0]) }} → {{ formatSeconds(group.times[group.times.length - 1]) }}</span>
                    <span>{{ group.times.length }} 張・選取頭尾</span>
                  </button>
                </div>
              </div>
              <div v-if="similarityGroups.length" class="smart-removal-panel">
                <strong>建立智慧合併草稿</strong>
                <div class="smart-removal-controls">
                  <label>保留方式
                    <select v-model="similarityKeepMode">
                      <option value="front">保留前段</option>
                      <option value="back">保留後段</option>
                      <option value="both">保留前後</option>
                    </select>
                  </label>
                  <label>{{ similarityKeepMode === 'both' ? '前後各保留' : '保留' }}
                    <input v-model.number="similarityKeepCount" type="number" min="1" max="100" step="1" /> 張
                  </label>
                  <button type="button" class="btn btn-primary compact" :disabled="!smartRemovalRanges.length" @click="requestSmartKeepSegments">建立合併片段草稿</button>
                </div>
                <div class="smart-removal-preview">
                  <span v-if="!selectedSimilarityGroupIds.length">尚未勾選相似區段。</span>
                  <template v-else>
                    <span v-for="item in smartRemovalPlan" :key="item.group.id" :class="{ 'warning-text': item.overlaps }">
                      區段 {{ item.group.id }}：{{ item.overlaps ? '保留範圍重疊，不移除' : `${formatSeconds(item.removal!.start)} → ${formatSeconds(item.removal!.end)}` }}
                    </span>
                  </template>
                </div>
                <div v-if="showSmartReplaceConfirm" class="smart-replace-confirm">
                  <span>目前已有 {{ mergeSegments.length }} 個合併片段。要以智慧分析結果取代嗎？</span>
                  <button type="button" class="btn btn-danger compact" @click="applySmartKeepSegments">取代現有片段</button>
                  <button type="button" class="btn btn-secondary compact" @click="showSmartReplaceConfirm = false">取消</button>
                </div>
              </div>
            </div>
          </div>
        </div>
        <p class="info-text">點一下選取，點兩下檢視原圖。縮圖會依可用寬度自動排列；選取任意兩張後可移除範圍內所有縮圖。</p>
        <div v-if="workspaceRecovered" class="workspace-recovery">
          <div><strong>已復原上次工作區</strong><span>時間軸位置已保留，但縮圖圖片不會占用硬碟；可在方便時重新擷取。</span></div>
          <div class="workspace-recovery-actions">
            <button class="btn btn-secondary compact" @click="discardRecoveredPreview">捨棄時間軸</button>
            <button class="btn btn-primary compact" :disabled="isProcessing" @click="reloadRecoveredWorkspaceThumbnails">重新載入縮圖</button>
          </div>
        </div>
        <div v-if="hasPreviewContent" class="grid-shell">
          <div ref="gridRef" class="grid-container" @scroll.passive="onGridScroll">
            <div class="grid-virtual-canvas" :style="{ height: `${gridCanvasHeight}px` }">
              <div class="grid-visible-rows" :style="{ top: `${gridFirstRow * gridRowPitch}px`, gridTemplateColumns: `repeat(${gridColumnCount}, minmax(0, 1fr))`, gridAutoRows: `${gridRowPitch - 3}px` }">
                <div
                  v-for="item in visibleGridItems"
                  :key="`${item.time}-${item.index}`"
                  class="grid-item-img"
                  :class="[{
                    endpoint: item.endpoint,
                    selected: selectedTimes.includes(item.time),
                    'in-segment': mergeSegments.length > 0 && isInMergeSegment(item.time),
                    locked: lockedTimeSet.has(item.time),
                  }, similarityItemClasses(item.time)]"
                  @click="toggleSelection(item.time)"
                  @dblclick="openModalForTime(item.time)"
                >
                  <div class="img-wrapper"><img v-if="item.src" :src="item.src" :alt="`縮圖 ${item.label}`" draggable="false" /><div v-else class="thumbnail-loading"><span></span><small>載入中</small></div><span v-if="item.endpoint" class="endpoint-label">{{ item.time === 0 ? '起點' : '終點' }}</span><span v-if="showSimilarityMarkers && similarityMetaByTime.get(item.time)" class="similarity-badge">相似 {{ similarityMetaByTime.get(item.time)?.id }}</span></div>
                  <div class="time-tag">{{ item.label }}</div>
                </div>
              </div>
            </div>
          </div>
          <button v-if="showGridBackToTop" type="button" class="grid-back-to-top" title="回到縮圖列表頂端" aria-label="回到縮圖列表頂端" @click="scrollGridToTop">↑<span>回頂端</span></button>
        </div>
        <div v-else class="preview-placeholder">建立縮圖後會顯示在這裡。</div>
      </section>
    </div>
    <div v-if="activeModalItem" class="modal-overlay" @click.self="activeModalIndex = null">
      <button class="modal-close-btn" @click="activeModalIndex = null">×</button>
      <div class="modal-main">
        <button class="modal-nav-btn modal-nav-prev" :disabled="activeModalIndex === 0" @click.stop="modalGoPrev">‹</button>
        <div class="modal-content-scroller" :class="{ 'fit-mode': modalImageSize === 'fit' }">
          <div class="modal-time-label">{{ activeModalItem.label }}</div>
          <div class="modal-image-viewport">
            <img v-if="activeModalItem.src" :src="activeModalItem.src" class="original-size-img" alt="原始縮圖" />
            <div v-else class="modal-loading"><span></span><strong>縮圖載入中…</strong><small>完成後會自動顯示，不需要關閉視窗。</small></div>
          </div>
        </div>
        <button class="modal-nav-btn modal-nav-next" :disabled="activeModalIndex === modalSequence.length - 1" @click.stop="modalGoNext">›</button>
        <div class="modal-display-controls">
          <button class="modal-image-size-toggle" :aria-pressed="modalImageSize === 'fit'" @click.stop="toggleModalImageSize">
            {{ modalImageSize === 'fit' ? '原始大小' : '貼合容器' }}
          </button>
          <button class="modal-filmstrip-toggle" @click.stop="filmstripVisible = !filmstripVisible">{{ filmstripVisible ? '隱藏縮圖列 ▾' : '顯示縮圖列 ▸' }}</button>
        </div>
      </div>
      <div v-if="filmstripVisible" ref="filmstripRef" class="modal-filmstrip" @scroll.passive="onFilmstripScroll">
        <div class="filmstrip-spacer" :style="{ width: `${filmstripFirst * filmstripPitch}px` }"></div>
        <div
          v-for="item in visibleFilmstripItems"
          :key="item.index"
          class="filmstrip-item"
          :class="{ active: item.index === activeModalIndex }"
          @click="activeModalIndex = item.index"
        >
          <img v-if="item.src" :src="item.src" :alt="item.label" />
          <div v-else class="filmstrip-loading"><span></span></div>
          <span class="filmstrip-time">{{ item.label }}</span>
        </div>
        <div class="filmstrip-spacer" :style="{ width: `${(modalSequence.length - filmstripLast) * filmstripPitch}px` }"></div>
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
          <button class="btn btn-secondary compact" :disabled="exportingLog" title="會包含錯誤全文、設定及本機檔案路徑" @click="exportDebugLog">{{ exportingLog ? '匯出中…' : '匯出除錯紀錄' }}</button>
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
  min-height: 0;
  min-width: 0;
}
.settings-panel,
.preview-log-panel {
  display: flex;
  flex-direction: column;
  gap: 1.25rem;
  min-height: 0;
  min-width: 0;
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
  padding: 0.3rem;
}
.panel-tabs.sub-tabs .panel-tab {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.35rem;
  min-width: 0;
  font-size: 0.82rem;
  padding: 0.5rem 0.4rem;
  white-space: nowrap;
}
.sub-tab-count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.15rem;
  height: 1.15rem;
  padding: 0 0.25rem;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.16);
  color: #e0e7ff;
  font-size: 0.7rem;
  line-height: 1;
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
.load-panel-card {
  display: flex;
  flex: 1 1 0;
  flex-direction: column;
  gap: 1rem;
  min-height: 0;
  min-width: 0;
  padding-top: 0.75rem;
  padding-bottom: 0.75rem;
  overflow: hidden;
}
.load-panel-card:not(.has-processed-videos) {
  flex: 0 0 auto;
}
.load-panel-card > .panel-tabs.sub-tabs {
  flex: 0 0 auto;
  margin: 0;
}
.source-section {
  flex: 0 0 auto;
  min-height: 0;
  min-width: 0;
}
.load-panel-source-area {
  display: flex;
  flex: 1 1 0;
  flex-direction: column;
  min-height: 0;
  min-width: 0;
}
.load-panel-card:not(.has-processed-videos) .load-panel-source-area {
  flex: 0 0 auto;
}
.capture-section {
  flex: 1 1 auto;
  min-height: 0;
  min-width: 0;
}
.load-panel-capture-area {
  display: flex;
  flex: 1 1 0;
  flex-direction: column;
  min-height: 0;
  min-width: 0;
  overflow: hidden;
}
.expansion-section {
  display: flex;
  flex-direction: column;
  min-height: 0;
  min-width: 0;
  overflow: hidden;
}
.load-panel-card:not(.has-processed-videos) .load-panel-capture-area {
  flex: 0 0 auto;
}
.load-panel-card:not(.has-processed-videos) .capture-section {
  flex: 0 0 auto;
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
  min-width: 0;
}
.col {
  flex: 1;
  min-width: 0;
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
.video-path-display {
  min-width: 0;
  padding: 0.45rem 0;
  color: var(--text-color);
  font-size: 14px;
  line-height: 1.45;
  overflow-wrap: anywhere;
  word-break: break-all;
}
.source-actions-row {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  min-width: 0;
  margin-top: 0.6rem;
  white-space: nowrap;
}
.source-actions-row .btn {
  flex: 0 0 auto;
  padding: 0.45rem 0.7rem;
  font-size: 0.78rem;
}
.input-group.drop-target .video-path-row {
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
  display: flex;
  flex: 1 1 auto;
  flex-direction: column;
  min-height: 9rem;
  margin-top: 0.9rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 0.6rem 0.75rem 0;
  overflow: hidden;
}
.processed-list-header {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.8rem;
  color: var(--text-muted);
  margin-bottom: 0.4rem;
}
.processed-list-actions {
  display: flex;
  align-items: center;
  gap: 0.65rem;
}
.btn-danger {
  border: 0;
  background: #7f1d1d;
  color: #fecaca;
}
.btn-danger:hover:not(:disabled) {
  background: #991b1b;
}
.processed-delete-confirm {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  margin: 0 0 0.75rem;
  padding: 0.7rem 0.8rem;
  border: 1px solid rgba(239, 68, 68, 0.4);
  border-radius: 8px;
  background: rgba(127, 29, 29, 0.35);
  color: #fecaca;
  font-size: 0.8rem;
  line-height: 1.5;
}
.processed-delete-confirm-actions {
  display: flex;
  flex-shrink: 0;
  gap: 0.5rem;
}
.processed-items {
  flex: 1 1 auto;
  min-height: 0;
  padding-right: 0.5rem;
  overflow-y: auto;
  scrollbar-gutter: stable;
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
  flex-wrap: wrap;
  gap: 1.2rem;
  color: var(--text-muted);
  font-size: 0.9rem;
  margin: 0 0 1rem;
}
.mode-row label {
  min-width: 0;
}
.mode-row input {
  margin-right: 0.35rem;
}
.duration-input-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 0.65rem;
}
.duration-input-grid label {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: center;
  gap: 0.4rem;
  margin: 0;
}
.duration-split-preview {
  margin-top: 0.8rem;
  padding: 0.7rem 0.8rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  background: rgba(15, 23, 42, 0.4);
  color: var(--text-muted);
  font-size: 0.78rem;
}
.duration-split-preview strong { color: var(--text-color); }
.duration-split-preview p { margin: 0.35rem 0 0; }
.duration-split-preview ol {
  margin: 0.55rem 0 0;
  padding-left: 1.4rem;
  font-family: ui-monospace, monospace;
  line-height: 1.55;
}
.quick-set-row {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(4rem, 1fr));
  gap: 0.4rem;
  margin-top: 0.5rem;
  min-width: 0;
}
.quick-set-btn {
  min-width: 0;
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
.edit-task-notice {
  margin: -0.35rem 0 1rem;
  padding: 0.7rem 0.8rem;
  border: 1px solid rgba(99, 102, 241, 0.3);
  border-radius: 8px;
  background: rgba(99, 102, 241, 0.1);
  color: #c7d2fe;
  font-size: 0.82rem;
  line-height: 1.5;
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
.merge-segment-list {
  max-height: 320px;
  padding-right: 0.35rem;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-gutter: stable;
}
.batch-source-panel {
  display: flex;
  flex-direction: column;
  gap: 0.7rem;
}
.batch-source-actions {
  display: flex;
  align-items: center;
  gap: 0.7rem;
}
.batch-source-list {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  max-height: 240px;
  overflow-y: auto;
}
.batch-source-item {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  padding: 0.55rem 0.65rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  background: rgba(15, 23, 42, 0.45);
}
.batch-source-index {
  flex: 0 0 1.5rem;
  color: var(--text-muted);
  text-align: center;
}
.batch-source-info {
  display: flex;
  flex: 1;
  min-width: 0;
  flex-direction: column;
  gap: 0.18rem;
}
.batch-source-info strong {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.batch-source-info span {
  color: var(--text-muted);
  font-size: 0.75rem;
}
.batch-source-info .batch-source-error,
.danger-text { color: #fca5a5; }
.merge-item {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.55rem 0.75rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  background: rgba(15, 23, 42, 0.5);
}
.merge-item.editing {
  border-color: #818cf8;
  box-shadow: 0 0 0 2px rgba(99, 102, 241, 0.2);
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
.merge-item-actions {
  display: flex;
  align-items: center;
  flex-shrink: 0;
  gap: 0.3rem;
}
.merge-action {
  min-width: 1.8rem;
  padding: 0.25rem 0.4rem;
  border: 1px solid var(--border-color);
  border-radius: 5px;
  color: #cbd5e1;
  background: rgba(30, 41, 59, 0.75);
  font-size: 0.75rem;
  cursor: pointer;
}
.merge-action:hover:not(:disabled) { border-color: #818cf8; color: #fff; }
.merge-action:disabled { opacity: 0.35; cursor: default; }
.merge-edit-notice {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.6rem;
  margin-bottom: 0.75rem;
  padding: 0.6rem 0.7rem;
  border: 1px solid rgba(99, 102, 241, 0.45);
  border-radius: 8px;
  color: #c7d2fe;
  background: rgba(99, 102, 241, 0.12);
  font-size: 0.78rem;
}
.merge-edit-selection-hint {
  color: #c7d2fe;
  font-size: 0.78rem;
}
.expansion-queue {
  display: flex;
  flex: 1 1 0;
  flex-direction: column;
  min-height: 0;
  min-width: 0;
  margin-top: 0.9rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 0.6rem 0.75rem;
  overflow: hidden;
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
  flex: 1 1 auto;
  min-height: 0;
  padding-right: 0.5rem;
  overflow-y: auto;
  scrollbar-gutter: stable;
}
.load-panel-card:not(.has-processed-videos) .expansion-queue {
  flex: 0 0 auto;
}
.load-panel-card:not(.has-processed-videos) .expansion-queue-list {
  flex: 0 1 auto;
  max-height: 260px;
}
.expansion-queue-item {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.4rem 0;
  border-top: 1px solid var(--border-color);
  font-size: 0.82rem;
  min-width: 0;
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
.load-panel-card.initial-mode .progress-container {
  margin-top: 0.75rem;
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
.grid-shell {
  position: relative;
  flex: 1;
  min-height: 0;
}
.grid-container {
  height: 100%;
  overflow-y: auto;
  padding: 3px;
  background: #020617;
  user-select: none;
  -webkit-user-select: none;
}
.grid-back-to-top {
  position: absolute;
  left: 14px;
  bottom: 14px;
  z-index: 4;
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  padding: 0.5rem 0.7rem;
  border: 1px solid rgba(165, 180, 252, 0.55);
  border-radius: 999px;
  color: #eef2ff;
  background: rgba(30, 41, 59, 0.94);
  box-shadow: 0 8px 22px rgba(0, 0, 0, 0.38);
  cursor: pointer;
  backdrop-filter: blur(8px);
}
.grid-back-to-top:hover {
  border-color: #a5b4fc;
  background: rgba(55, 65, 81, 0.98);
}
.grid-virtual-canvas {
  position: relative;
}
.grid-visible-rows {
  position: absolute;
  left: 0;
  right: 0;
  display: grid;
  gap: 3px;
}
.grid-item-img {
  background: #000;
  cursor: pointer;
  border: 2px solid transparent;
  min-width: 0;
}
.grid-item-img.selected {
  border-color: #f59e0b;
  outline: 4px solid #f59e0b;
  outline-offset: -4px;
  box-shadow: 0 0 0 2px rgba(245, 158, 11, 0.42);
}
.grid-item-img.in-segment {
  outline: 4px solid #34d399;
  outline-offset: -4px;
}
.grid-item-img.in-segment.selected {
  outline: 4px solid #f59e0b;
  outline-offset: -4px;
}
.grid-item-img.locked {
  box-shadow: inset 0 0 0 3px rgba(96, 165, 250, 0.85);
}
.similarity-color-1 { --similar-color: #c2b6e5; --similar-bg: #4f3379; --similar-soft: rgba(128, 87, 194, 0.28); }
.similarity-color-2 { --similar-color: #93c8e2; --similar-bg: #245873; --similar-soft: rgba(59, 151, 193, 0.26); }
.similarity-color-3 { --similar-color: #8cd5b8; --similar-bg: #235d4b; --similar-soft: rgba(59, 165, 129, 0.25); }
.similarity-color-4 { --similar-color: #e4b98e; --similar-bg: #7e422a; --similar-soft: rgba(203, 121, 64, 0.26); }
.grid-item-img.similar {
  position: relative;
  transition: opacity 0.15s ease, filter 0.15s ease;
}
.grid-item-img.similar .time-tag {
  color: #fff;
  background: var(--similar-bg);
}
.grid-item-img.similar-first::before,
.grid-item-img.similar-last::after {
  position: absolute;
  top: 0;
  bottom: 0;
  z-index: 2;
  width: 4px;
  content: '';
  background: var(--similar-color);
  pointer-events: none;
}
.grid-item-img.similar-first::before { left: 0; }
.grid-item-img.similar-last::after { right: 0; }
.grid-item-img.similar-muted { opacity: 0.38; }
.grid-item-img.similar-focused { filter: brightness(1.16); }
.similarity-badge {
  position: absolute;
  top: 5px;
  right: 5px;
  z-index: 3;
  padding: 0.15rem 0.35rem;
  border: 1px solid var(--similar-color);
  border-radius: 999px;
  color: #fff;
  background: var(--similar-bg);
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.45);
  font-size: 0.62rem;
  font-weight: 700;
  line-height: 1;
  pointer-events: none;
}
.img-wrapper {
  position: relative;
  height: calc(100% - 22px);
}
.img-wrapper img {
  width: 100%;
  height: 100%;
  display: block;
  object-fit: cover;
  -webkit-user-drag: none;
}
.thumbnail-loading,
.modal-loading,
.filmstrip-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(110deg, #111827 25%, #1e293b 45%, #111827 65%);
  background-size: 220% 100%;
  animation: thumbnail-shimmer 1.4s ease-in-out infinite;
  color: #94a3b8;
}
.thumbnail-loading {
  width: 100%;
  height: 100%;
  flex-direction: column;
  gap: 0.35rem;
  font-size: 0.72rem;
}
.thumbnail-loading span,
.modal-loading span,
.filmstrip-loading span {
  width: 1.25rem;
  height: 1.25rem;
  border: 2px solid rgba(148, 163, 184, 0.35);
  border-top-color: #a5b4fc;
  border-radius: 50%;
  animation: thumbnail-spin 0.8s linear infinite;
}
@keyframes thumbnail-shimmer {
  to { background-position: -220% 0; }
}
@keyframes thumbnail-spin {
  to { transform: rotate(360deg); }
}
.time-tag {
  height: 22px;
  box-sizing: border-box;
  padding: 3px;
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
.workspace-recovery {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 0.75rem;
  padding: 0.75rem 0.9rem;
  border: 1px solid rgba(99, 102, 241, 0.35);
  border-radius: 9px;
  background: rgba(99, 102, 241, 0.1);
  color: #c7d2fe;
  font-size: 0.82rem;
}
.workspace-recovery > div:first-child {
  display: flex;
  flex-direction: column;
  gap: 0.2rem;
}
.workspace-recovery span {
  color: var(--text-muted);
}
.workspace-recovery-actions {
  display: flex;
  gap: 0.5rem;
  flex-shrink: 0;
}
.current-layout-controls {
  position: relative;
  z-index: 12;
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
.current-layout-controls .preview-column-input {
  width: 5rem;
  font-variant-numeric: tabular-nums;
}
.similarity-panel {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  left: 0;
  z-index: 20;
  padding: 0.65rem 0.75rem;
  border: 1px solid rgba(139, 92, 246, 0.3);
  border-radius: 8px;
  background: rgba(15, 23, 42, 0.98);
  box-shadow: 0 14px 32px rgba(0, 0, 0, 0.45);
  backdrop-filter: blur(10px);
}
.similarity-tool {
  display: flex;
  margin-left: auto;
}
.similarity-toggle {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  min-height: 2rem;
  padding: 0.4rem 0.65rem;
  border: 1px solid rgba(139, 92, 246, 0.3);
  border-radius: 7px;
  color: #ddd6fe;
  background: rgba(76, 29, 149, 0.1);
  font-size: 0.78rem;
  white-space: nowrap;
  cursor: pointer;
}
.similarity-toggle:hover,
.similarity-toggle.active {
  border-color: rgba(167, 139, 250, 0.65);
  background: rgba(76, 29, 149, 0.2);
}
.similarity-toggle-status {
  padding: 0.16rem 0.45rem;
  border-radius: 999px;
  color: #ede9fe;
  background: rgba(139, 92, 246, 0.35);
  font-size: 0.7rem;
}
.similarity-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.55rem;
}
.similarity-controls label {
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  color: var(--text-muted);
  font-size: 0.8rem;
}
.similarity-controls select {
  width: auto;
  min-width: 5rem;
  padding: 0.4rem 0.55rem;
}
.similarity-controls input {
  width: 4rem;
  padding: 0.4rem 0.5rem;
}
.similarity-controls .similarity-marker-toggle input {
  width: auto;
  margin: 0;
  padding: 0;
}
.similarity-progress {
  height: 5px;
  margin-top: 0.6rem;
  overflow: hidden;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.08);
}
.similarity-progress span {
  display: block;
  height: 100%;
  border-radius: inherit;
  background: #8b5cf6;
  transition: width 0.15s ease;
}
.similarity-results {
  display: flex;
  gap: 0.4rem;
  margin-top: 0.6rem;
  padding-bottom: 0.2rem;
  overflow-x: auto;
}
.similarity-results > strong {
  align-self: center;
  flex: 0 0 auto;
  margin-right: 0.2rem;
  color: #ddd6fe;
  font-size: 0.78rem;
}
.similarity-result {
  display: inline-flex;
  flex: 0 0 auto;
  flex-direction: column;
  gap: 0.12rem;
  padding: 0.4rem 0.55rem;
  border: 1px solid var(--similar-color);
  border-radius: 7px;
  color: #fff;
  background: var(--similar-bg);
  font-size: 0.7rem;
  text-align: left;
  cursor: pointer;
}
.similarity-result-wrap {
  display: flex;
  flex: 0 0 auto;
  align-items: stretch;
  overflow: hidden;
  border: 1px solid var(--similar-color);
  border-radius: 7px;
  background: var(--similar-bg);
}
.similarity-result-wrap .similarity-result {
  border: 0;
  border-radius: 0;
}
.similarity-result-check {
  display: inline-flex;
  align-items: center;
  gap: 0.25rem;
  padding: 0.35rem 0.45rem;
  border-right: 1px solid var(--similar-color);
  color: #fff;
  font-size: 0.68rem;
  cursor: pointer;
}
.similarity-result-check input { width: auto; margin: 0; }
.similarity-result:hover {
  box-shadow: 0 0 0 2px var(--similar-soft);
  filter: brightness(1.15);
}
.smart-removal-panel {
  margin-top: 0.7rem;
  padding-top: 0.65rem;
  border-top: 1px solid rgba(148, 163, 184, 0.18);
}
.smart-removal-panel > strong { color: #e2e8f0; font-size: 0.82rem; }
.smart-removal-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: end;
  gap: 0.55rem;
  margin-top: 0.5rem;
}
.smart-removal-controls label {
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  color: var(--text-muted);
  font-size: 0.75rem;
}
.smart-removal-controls select { width: auto; min-width: 7rem; padding: 0.4rem 0.5rem; }
.smart-removal-controls input { width: 3.8rem; padding: 0.4rem 0.5rem; }
.smart-removal-preview {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem 0.8rem;
  margin-top: 0.55rem;
  color: #cbd5e1;
  font: 0.7rem ui-monospace, monospace;
}
.smart-replace-confirm {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.5rem;
  margin-top: 0.6rem;
  padding: 0.55rem;
  border: 1px solid rgba(239, 68, 68, 0.4);
  border-radius: 7px;
  color: #fecaca;
  background: rgba(127, 29, 29, 0.3);
  font-size: 0.75rem;
}
.warning-text { color: #fbbf24; }
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
.modal-display-controls {
  position: absolute;
  right: 1rem;
  bottom: 1rem;
  z-index: 202;
  display: flex;
  gap: 0.5rem;
  align-items: center;
}
.modal-image-size-toggle,
.modal-filmstrip-toggle {
  padding: 0.5rem 0.9rem;
  border-radius: 999px;
  border: 1px solid rgba(255, 255, 255, 0.15);
  background: rgba(51, 65, 85, 0.85);
  color: #fff;
  font-size: 0.8rem;
  cursor: pointer;
}
.modal-image-size-toggle:hover,
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
.modal-content-scroller.fit-mode {
  width: 100%;
  height: 100%;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.modal-content-scroller.fit-mode .modal-image-viewport {
  flex: 1;
  min-width: 0;
  min-height: 0;
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
}
.modal-time-label {
  color: #e5e7eb;
  font:
    0.85rem ui-monospace,
    monospace;
  margin-bottom: 0.6rem;
}
.modal-loading {
  width: min(720px, 80vw);
  min-height: min(420px, 60vh);
  flex-direction: column;
  gap: 0.8rem;
  border-radius: 10px;
}
.modal-loading small {
  color: #64748b;
}
.original-size-img {
  display: block;
  max-width: none;
  margin: 0 auto;
}
.fit-mode .original-size-img {
  width: 100%;
  height: 100%;
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
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
.filmstrip-spacer {
  flex: 0 0 auto;
  height: 1px;
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
.filmstrip-loading {
  width: 100%;
  flex: 1;
  min-height: 0;
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
.capture-settings-box {
  margin-bottom: 1rem;
  padding: 0.8rem;
  border: 1px solid var(--border-color);
  border-radius: 9px;
  background: rgba(15, 23, 42, 0.35);
}
.capture-settings-header,
.capture-settings-header > div {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.6rem;
}
.capture-speed-badge {
  padding: 0.18rem 0.45rem;
  border-radius: 999px;
  background: rgba(245, 158, 11, 0.15);
  color: #fbbf24;
  font-size: 0.7rem;
}
.capture-speed-badge.fast {
  background: rgba(16, 185, 129, 0.15);
  color: #6ee7b7;
}
.capture-settings-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.85rem 0.75rem;
  margin-top: 0.9rem;
  align-items: end;
}
.capture-setting-wide { grid-column: 1 / -1; }
.capture-settings-grid .input-group {
  min-width: 0;
  margin: 0;
}
.capture-settings-grid .input-group > label {
  display: flex;
  align-items: flex-end;
  min-height: 2.45rem;
  margin-bottom: 0.45rem;
  line-height: 1.35;
}
.capture-settings-grid input,
.capture-settings-grid select {
  box-sizing: border-box;
  height: 44px;
  width: 100%;
  padding: 0.65rem 0.7rem;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  background: rgba(15, 23, 42, 0.6);
  color: var(--text-color);
}
.capture-setting-toggle .original-size-button { width: 100%; height: 44px; }
.original-size-button {
  padding: 0.6rem 0.7rem;
  border: 1px solid rgba(129, 140, 248, 0.35);
  border-radius: 8px;
  background: rgba(30, 41, 59, 0.8);
  color: #c7d2fe;
  cursor: pointer;
}
.original-size-button.active {
  border-color: rgba(129, 140, 248, 0.75);
  background: rgba(79, 70, 229, 0.32);
}
.capture-setting-note {
  margin: -0.35rem 0 0;
  color: var(--text-muted);
  font-size: 0.76rem;
  line-height: 1.45;
}
@media (max-width: 420px) {
  .capture-settings-grid { grid-template-columns: 1fr; }
  .capture-setting-wide { grid-column: 1; }
  .capture-settings-grid .input-group > label { min-height: 0; }
}
@media (min-width: 901px) {
  .main-layout {
    height: calc(100vh - 112px);
  }
  .load-panel-card.has-processed-videos {
    height: 0;
  }
}
@media (max-width: 900px) {
  .main-layout {
    grid-template-columns: 1fr;
  }
  .settings-panel {
    min-height: calc(100vh - 112px);
  }
  .load-panel-card.has-processed-videos {
    flex: 1 1 0;
    min-height: 0;
  }
}
@media (max-width: 600px) {
  .preview-card {
    height: calc(100vh - 142px);
  }
}
</style>
