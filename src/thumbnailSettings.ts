export type ThumbnailPreset = 'fast' | 'balanced' | 'high'
export type ThumbnailSeekMode = 'fast' | 'precise'

export interface ThumbnailSettings {
  preset: ThumbnailPreset
  maxHeight: number
  originalSize: boolean
  quality: number
  seekMode: ThumbnailSeekMode
  decoderThreads: number
  concurrency: 1 | 2
  hardwareAcceleration: boolean
}

const storageKey = 'vid_thumbnail_settings_v3'
const previousStorageKey = 'vid_thumbnail_settings_v2'

const presets: Record<ThumbnailPreset, Omit<ThumbnailSettings, 'preset'>> = {
  fast: { maxHeight: 360, originalSize: false, quality: 7, seekMode: 'fast', decoderThreads: 0, concurrency: 1, hardwareAcceleration: false },
  balanced: { maxHeight: 720, originalSize: false, quality: 5, seekMode: 'precise', decoderThreads: 0, concurrency: 1, hardwareAcceleration: false },
  high: { maxHeight: 1080, originalSize: false, quality: 2, seekMode: 'precise', decoderThreads: 0, concurrency: 1, hardwareAcceleration: false },
}

export function settingsForPreset(preset: ThumbnailPreset): ThumbnailSettings {
  return { preset, ...presets[preset] }
}

export function normaliseThumbnailSettings(value: Partial<ThumbnailSettings>): ThumbnailSettings {
  const preset: ThumbnailPreset = value.preset === 'fast' || value.preset === 'high' ? value.preset : 'balanced'
  const fallback = settingsForPreset(preset)
  const decoderThreads = Number(value.decoderThreads)
  return {
    preset,
    maxHeight: Math.min(2160, Math.max(144, Math.round(Number(value.maxHeight) || fallback.maxHeight))),
    originalSize: Boolean(value.originalSize),
    quality: Math.min(12, Math.max(2, Math.round(Number(value.quality) || fallback.quality))),
    seekMode: value.seekMode === 'fast' || value.seekMode === 'precise' ? value.seekMode : fallback.seekMode,
    decoderThreads: Number.isFinite(decoderThreads) ? Math.min(4, Math.max(0, Math.round(decoderThreads))) : fallback.decoderThreads,
    concurrency: value.concurrency === 2 ? 2 : 1,
    hardwareAcceleration: Boolean(value.hardwareAcceleration),
  }
}

export function getThumbnailSettings() {
  try {
    const stored = localStorage.getItem(storageKey)
    if (stored) return normaliseThumbnailSettings(JSON.parse(stored))

    const previous = localStorage.getItem(previousStorageKey)
    if (previous) {
      const migrated = normaliseThumbnailSettings({
        ...JSON.parse(previous),
        decoderThreads: 0,
        concurrency: 1,
      })
      localStorage.setItem(storageKey, JSON.stringify(migrated))
      return migrated
    }
    return settingsForPreset('balanced')
  } catch {
    return settingsForPreset('balanced')
  }
}

export function saveThumbnailSettings(value: ThumbnailSettings) {
  localStorage.setItem(storageKey, JSON.stringify(normaliseThumbnailSettings(value)))
}
