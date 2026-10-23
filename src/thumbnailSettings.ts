export type ThumbnailPreset = 'legacy' | 'compact' | 'quality'
export type ThumbnailSeekMode = 'fast' | 'precise'

export interface ThumbnailSettings {
  preset: ThumbnailPreset
  maxHeight: number
  originalSize: boolean
  quality: number
  seekMode: ThumbnailSeekMode
  hardwareAcceleration: boolean
}

const storageKey = 'vid_thumbnail_settings_v4'

const presets: Record<ThumbnailPreset, Omit<ThumbnailSettings, 'preset'>> = {
  // This must stay byte-for-byte equivalent to the restored legacy FFmpeg path.
  legacy: { maxHeight: 720, originalSize: true, quality: 5, seekMode: 'fast', hardwareAcceleration: false },
  compact: { maxHeight: 480, originalSize: false, quality: 7, seekMode: 'fast', hardwareAcceleration: false },
  quality: { maxHeight: 720, originalSize: false, quality: 3, seekMode: 'precise', hardwareAcceleration: false },
}

export function settingsForPreset(preset: ThumbnailPreset): ThumbnailSettings {
  return { preset, ...presets[preset] }
}

export function normaliseThumbnailSettings(value: Partial<ThumbnailSettings>): ThumbnailSettings {
  const preset: ThumbnailPreset = value.preset === 'compact' || value.preset === 'quality' ? value.preset : 'legacy'
  const fallback = settingsForPreset(preset)
  return {
    preset,
    maxHeight: Math.min(2160, Math.max(144, Math.round(Number(value.maxHeight) || fallback.maxHeight))),
    originalSize: value.originalSize === undefined ? fallback.originalSize : Boolean(value.originalSize),
    quality: Math.min(12, Math.max(2, Math.round(Number(value.quality) || fallback.quality))),
    seekMode: value.seekMode === 'precise' ? 'precise' : 'fast',
    hardwareAcceleration: Boolean(value.hardwareAcceleration),
  }
}

export function isLegacyFastSettings(value: ThumbnailSettings) {
  return value.originalSize && value.quality === 5 && value.seekMode === 'fast' && !value.hardwareAcceleration
}

export function getThumbnailSettings(): ThumbnailSettings {
  try {
    const stored = localStorage.getItem(storageKey)
    return stored ? normaliseThumbnailSettings(JSON.parse(stored)) : settingsForPreset('legacy')
  } catch {
    return settingsForPreset('legacy')
  }
}

export function saveThumbnailSettings(value: ThumbnailSettings) {
  localStorage.setItem(storageKey, JSON.stringify(normaliseThumbnailSettings(value)))
}
