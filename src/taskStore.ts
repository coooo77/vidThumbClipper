import { computed, reactive, watch } from 'vue'

export type TaskKind = 'clip' | 'merge' | 'preprocess' | 'combine'
export type ClipMethod = 'range' | 'duration' | 'count'
export type CombineMode = 'copy' | 'reencode'
export type TaskStatus = '等待中' | '處理中' | '完成' | '失敗' | '已取消'

export interface VideoSegment {
  start: number
  end: number
}
export interface VideoTask {
  id: string
  kind: TaskKind
  method: ClipMethod
  source: string
  segments: VideoSegment[]
  durationHours: number
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
}

const storageKey = 'vid_thumb_clipper_tasks'
let initialTasks: VideoTask[] = []
try {
  initialTasks = JSON.parse(localStorage.getItem(storageKey) || '[]') as VideoTask[]
} catch {
  initialTasks = []
}

export const videoTasks = reactive<VideoTask[]>(initialTasks)

watch(videoTasks, (value) => localStorage.setItem(storageKey, JSON.stringify(value)), { deep: true })

export function newTaskId() {
  return `${Date.now()}-${Math.random().toString(16).slice(2)}`
}

// Preprocess tasks that finished successfully — selectable back on the Home page as a video source.
export const processedVideos = computed(() => videoTasks.filter((task) => task.kind === 'preprocess' && task.status === '完成'))
