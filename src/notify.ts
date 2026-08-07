// Processing-finished sound notifications. Tones are generated on the fly via the Web Audio API,
// so no audio file needs to be bundled or shipped with the app.

export type NotifyTrigger = 'each' | 'batch'

const KEYS = {
  enabled: 'vid_notify_enabled',
  volume: 'vid_notify_volume',
  trigger: 'vid_notify_trigger',
  onFailure: 'vid_notify_on_failure',
}

export function getNotifySettings() {
  const volumeRaw = Number(localStorage.getItem(KEYS.volume))
  return {
    enabled: localStorage.getItem(KEYS.enabled) === 'true',
    volume: Number.isFinite(volumeRaw) && volumeRaw >= 0 && volumeRaw <= 100 ? volumeRaw : 70,
    trigger: (localStorage.getItem(KEYS.trigger) as NotifyTrigger) || 'batch',
    onFailure: localStorage.getItem(KEYS.onFailure) !== 'false',
  }
}

export function saveNotifySettings(settings: { enabled: boolean; volume: number; trigger: NotifyTrigger; onFailure: boolean }) {
  localStorage.setItem(KEYS.enabled, String(settings.enabled))
  localStorage.setItem(KEYS.volume, String(settings.volume))
  localStorage.setItem(KEYS.trigger, settings.trigger)
  localStorage.setItem(KEYS.onFailure, String(settings.onFailure))
}

function playTone(frequencies: number[], volumePercent: number, type: OscillatorType, noteDuration: number, gap: number) {
  const AudioContextClass = window.AudioContext || (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
  if (!AudioContextClass) return
  const ctx = new AudioContextClass()

  const schedule = () => {
    const gainScale = Math.max(0, Math.min(100, volumePercent)) / 100
    let startTime = ctx.currentTime
    frequencies.forEach((freq) => {
      const oscillator = ctx.createOscillator()
      const gainNode = ctx.createGain()
      oscillator.type = type
      oscillator.frequency.value = freq
      oscillator.connect(gainNode)
      gainNode.connect(ctx.destination)
      gainNode.gain.setValueAtTime(0, startTime)
      gainNode.gain.linearRampToValueAtTime(0.35 * gainScale, startTime + 0.015)
      gainNode.gain.linearRampToValueAtTime(0, startTime + noteDuration)
      oscillator.start(startTime)
      oscillator.stop(startTime + noteDuration + 0.02)
      startTime += noteDuration + gap
    })
    const closeAfter = (startTime - ctx.currentTime + 0.3) * 1000
    setTimeout(() => ctx.close(), closeAfter)
  }

  if (ctx.state === 'suspended') {
    ctx
      .resume()
      .then(schedule)
      .catch(() => schedule())
  } else {
    schedule()
  }
}

// Two quick rising notes — the "success" chime.
export function playCompletionSound(volumePercent: number) {
  playTone([880, 1318.5], volumePercent, 'sine', 0.12, 0.04)
}

// A lower, flatter double buzz — distinctly different from the completion chime.
export function playFailureSound(volumePercent: number) {
  playTone([220, 220], volumePercent, 'sawtooth', 0.16, 0.08)
}

// Called by task/job runners after each result. Reads persisted settings itself since callers
// (Tasks.vue, Home.vue) don't have direct access to the Settings page's live refs.
export function notifyResult(success: boolean) {
  const settings = getNotifySettings()
  if (!settings.enabled) return
  if (success) playCompletionSound(settings.volume)
  else if (settings.onFailure) playFailureSound(settings.volume)
}
