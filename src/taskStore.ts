import { computed, reactive, watch } from 'vue'

export type TaskKind = 'clip' | 'merge' | 'preprocess' | 'combine'
export type ClipMethod = 'range' | 'duration' | 'count'
export type CombineMode = 'copy' | 'reencode'
export type TaskStatus = '等待中' | '處理中' | '完成' | '失敗' | '已取消'

export interface VideoSegment {
  start: number
  end: number
}
export interface TaskPreviewTimeline {
  duration: number
  times: number[]
  /** @deprecated Kept only so timelines saved by older versions remain readable. */
  gridCols?: number
}
export interface TaskProgress {
  completed: number
  total: number
  startedAt: number
  label: string
}
export interface VideoTask {
  id: string
  kind: TaskKind
  method: ClipMethod
  source: string
  segments: VideoSegment[]
  durationHours: number
  durationMinutes?: number
  durationSeconds?: number
  splitCount: number
  outputName: string
  extension: string
  extraArgs: string
  keepSegments: boolean
  outputDir: string
  resultPath: string
  preciseSeek: boolean
  combineSources: string[]
  combineMode: CombineMode
  status: TaskStatus
  message: string
  // Only timeline metadata is persisted. Thumbnail image data is intentionally excluded so even
  // very large previews do not consume hundreds of megabytes on disk.
  previewTimeline?: TaskPreviewTimeline
  progress?: TaskProgress
}

const storageKey = 'vid_thumb_clipper_tasks'

export function normaliseVideoPath(path: string) {
  return path.replace(/[\\/]+$/, '').replace(/\//g, '\\').toLowerCase()
}

function deduplicatePreprocessTasks(tasks: VideoTask[]) {
  const seenSources = new Set<string>()
  return tasks.filter((task) => {
    if (task.kind !== 'preprocess') return true
    const sourceKey = normaliseVideoPath(task.source)
    if (seenSources.has(sourceKey)) return false
    seenSources.add(sourceKey)
    return true
  })
}

let initialTasks: VideoTask[] = []
try {
  const storedTasks = JSON.parse(localStorage.getItem(storageKey) || '[]') as VideoTask[]
  initialTasks = deduplicatePreprocessTasks(storedTasks).map((task) => task.status === '處理中'
    ? {
        ...task,
        status: '等待中',
        message: '上次程式關閉時此任務仍在處理，已安全移回等待佇列，請確認輸出檔案後再重新執行。',
        progress: undefined,
      }
    : task)
} catch {
  initialTasks = []
}

export const videoTasks = reactive<VideoTask[]>(initialTasks)

watch(videoTasks, (value) => localStorage.setItem(storageKey, JSON.stringify(value)), { deep: true })

export function newTaskId() {
  return `${Date.now()}-${Math.random().toString(16).slice(2)}`
}

let pendingTaskEditId: string | null = null

export function queueTaskEdit(id: string) {
  pendingTaskEditId = id
}

export function takeTaskEditRequest() {
  const id = pendingTaskEditId
  pendingTaskEditId = null
  return id
}

// Preprocess tasks that finished successfully — selectable back on the Home page as a video source.
export const processedVideos = computed(() => {
  const seenSources = new Set<string>()
  return videoTasks.filter((task) => {
    if (task.kind !== 'preprocess' || task.status !== '完成') return false
    const sourceKey = normaliseVideoPath(task.source)
    if (seenSources.has(sourceKey)) return false
    seenSources.add(sourceKey)
    return true
  })
})
