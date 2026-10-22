import type { ClipMethod, TaskKind, VideoSegment } from './taskStore'

export interface WorkspaceDraft {
  version: 1
  savedAt: number
  videoPath: string
  originalSourcePath: string
  videoDuration: number
  previewTimes: number[]
  selectedTimes: number[]
  mergeSegments: VideoSegment[]
  gridCols: number
  gridRows: number
  initialCaptureMode: 'grid' | 'interval'
  initialInterval: number
  taskKind: TaskKind
  taskMethod: ClipMethod
  taskDurationHours: number
  taskSplitCount: number
  taskOutputName: string
  taskExtension: string
  taskExtraArgs: string
  taskPreciseSeek: boolean
  taskKeepSegments: boolean
}

const workspaceStorageKey = 'vid_thumb_clipper_workspace_v1'

export function loadWorkspaceDraft(): WorkspaceDraft | null {
  try {
    const parsed = JSON.parse(localStorage.getItem(workspaceStorageKey) || 'null') as WorkspaceDraft | null
    return parsed?.version === 1 && typeof parsed.videoPath === 'string' ? parsed : null
  } catch {
    return null
  }
}

export function saveWorkspaceDraft(draft: Omit<WorkspaceDraft, 'version' | 'savedAt'>) {
  localStorage.setItem(workspaceStorageKey, JSON.stringify({ ...draft, version: 1, savedAt: Date.now() }))
}

export function clearWorkspaceDraft() {
  localStorage.removeItem(workspaceStorageKey)
}
