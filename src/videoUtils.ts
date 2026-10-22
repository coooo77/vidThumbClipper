import type { VideoSegment, VideoTask } from './taskStore'

export function formatTimecode(seconds: number) {
  const totalMs = Math.max(0, Math.round(seconds * 1000))
  const totalSeconds = Math.floor(totalMs / 1000)
  const base = [Math.floor(totalSeconds / 3600), Math.floor((totalSeconds % 3600) / 60), totalSeconds % 60]
    .map((value) => String(value).padStart(2, '0'))
    .join(':')
  const milliseconds = totalMs % 1000
  return milliseconds ? `${base}.${String(milliseconds).padStart(3, '0')}` : base
}

export function directoryOf(path: string) {
  return path.slice(0, Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/')) + 1)
}

export function resolveTaskOutputFolder(task: VideoTask, globalOutputDirectory: string) {
  const own = task.outputDir.trim()
  if (own) return `${own.replace(/[\\/]$/, '')}\\`
  const global = globalOutputDirectory.trim()
  if (global) return `${global.replace(/[\\/]$/, '')}\\`
  return directoryOf(task.source)
}

export function taskDestination(task: VideoTask, globalOutputDirectory: string, suffix = '') {
  return `${resolveTaskOutputFolder(task, globalOutputDirectory)}${task.outputName}${suffix}.${task.extension.replace(/^\./, '')}`
}

export function buildClipRanges(task: VideoTask, knownDuration = task.previewTimeline?.duration || 0): VideoSegment[] {
  if (task.segments.length) return task.segments
  if (!knownDuration || task.kind !== 'clip') return []
  if (task.method === 'duration') {
    const segmentDuration = task.durationHours * 3600
    if (segmentDuration <= 0) return []
    return Array.from({ length: Math.ceil(knownDuration / segmentDuration) }, (_, index) => ({
      start: index * segmentDuration,
      end: Math.min(knownDuration, (index + 1) * segmentDuration),
    }))
  }
  if (task.method === 'count') {
    const count = Math.max(1, Math.floor(task.splitCount))
    return Array.from({ length: count }, (_, index) => ({
      start: (knownDuration * index) / count,
      end: (knownDuration * (index + 1)) / count,
    }))
  }
  return []
}
