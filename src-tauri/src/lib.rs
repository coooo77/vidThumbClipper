use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

use base64::{engine::general_purpose, Engine as _};

fn thumbnail_cache_dir() -> Result<PathBuf, String> {
    let directory = std::env::temp_dir().join("vidThumbClipper").join("thumbnails");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Unable to create thumbnail cache directory: {error}"))?;
    Ok(directory)
}

// A GUI app (this one) has no console of its own, so on Windows, spawning a console app like
// ffmpeg/ffprobe makes the OS allocate a brand-new console window for it — the flash the person
// is seeing. CREATE_NO_WINDOW tells it not to. This only affects Windows; other platforms are
// unaffected since they never had this behavior to begin with.
fn spawn_hidden(executable: &str) -> Command {
    let mut command = Command::new(executable);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[tauri::command]
fn get_thumbnail_cache_dir() -> Result<String, String> {
    Ok(thumbnail_cache_dir()?.to_string_lossy().to_string())
}

// Deletes every file currently sitting in the thumbnail temp cache (does not touch anything
// outside vidThumbClipper's own temp subfolder).
#[tauri::command]
fn clear_thumbnail_cache() -> Result<(), String> {
    let directory = thumbnail_cache_dir()?;
    let entries = std::fs::read_dir(&directory).map_err(|error| format!("Unable to read thumbnail cache: {error}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

#[tauri::command]
fn get_file_size(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|meta| meta.len())
        .map_err(|error| format!("Unable to read file size: {error}"))
}

#[tauri::command]
async fn run_ffmpeg(ffmpeg_path: Option<String>, args: Vec<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let executable = ffmpeg_path.filter(|path| !path.trim().is_empty()).unwrap_or_else(|| "ffmpeg".into());
        let output = spawn_hidden(&executable).args(args).output().map_err(|error| format!("Unable to run ffmpeg: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.success() { Ok(stdout) } else { Err(format!("ffmpeg failed:\n{stdout}{stderr}")) }
    })
    .await
    .map_err(|error| format!("ffmpeg task panicked: {error}"))?
}

#[tauri::command]
async fn get_video_duration(ffprobe_path: Option<String>, video_path: String) -> Result<f64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let executable = ffprobe_path.filter(|path| !path.trim().is_empty()).unwrap_or_else(|| "ffprobe".into());
        let output = spawn_hidden(&executable)
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1", &video_path])
            .output()
            .map_err(|error| format!("Unable to run ffprobe: {error}"))?;
        if !output.status.success() { return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&output.stderr))); }
        String::from_utf8_lossy(&output.stdout).trim().parse::<f64>().map_err(|_| "Could not read the video's duration.".into())
    })
    .await
    .map_err(|error| format!("ffprobe task panicked: {error}"))?
}

#[tauri::command]
async fn capture_single_thumbnail(
    ffmpeg_path: Option<String>, video_path: String, time: String, index: i64,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let executable = ffmpeg_path.filter(|path| !path.trim().is_empty()).unwrap_or_else(|| "ffmpeg".into());
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let image_path = thumbnail_cache_dir()?.join(format!("thumbnail_{index}_{timestamp}.jpg"));
        let image_path_string = image_path.to_string_lossy().to_string();
        let output = spawn_hidden(&executable)
            .args(["-ss", &time, "-i", &video_path, "-vframes", "1", "-q:v", "5", "-pix_fmt", "yuvj420p", "-strict", "unofficial", &image_path_string, "-y"])
            .output()
            .map_err(|error| format!("Unable to run ffmpeg: {error}"))?;
        if !output.status.success() || !image_path.exists() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Thumbnail capture at {time} failed:\n{stdout}{stderr}"));
        }
        let mut file = File::open(&image_path).map_err(|error| format!("Unable to read generated thumbnail: {error}"))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| format!("Unable to read generated thumbnail: {error}"))?;
        // The frame is only ever needed as the base64 payload returned below (the grid lives in the
        // browser as data URLs) — delete the temp jpg immediately so the cache never accumulates.
        let _ = std::fs::remove_file(&image_path);
        Ok(general_purpose::STANDARD.encode(bytes))
    })
    .await
    .map_err(|error| format!("Thumbnail task panicked: {error}"))?
}

#[tauri::command]
async fn concat_videos(
    ffmpeg_path: Option<String>, files: Vec<String>, output_path: String, extra_args: Vec<String>, delete_inputs: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if files.is_empty() { return Err("No clips were supplied for merging.".into()); }
        let executable = ffmpeg_path.filter(|path| !path.trim().is_empty()).unwrap_or_else(|| "ffmpeg".into());
        let list_path = thumbnail_cache_dir()?.join(format!("concat_{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
        let list_body = files.iter().map(|path| format!("file '{}'", path.replace('\'', "'\\''"))).collect::<Vec<_>>().join("\n");
        std::fs::write(&list_path, list_body).map_err(|error| format!("Unable to create merge list: {error}"))?;
        let list_path_string = list_path.to_string_lossy().to_string();
        let mut args = vec!["-f".into(), "concat".into(), "-safe".into(), "0".into(), "-i".into(), list_path_string, "-c".into(), "copy".into()];
        args.extend(extra_args);
        args.push(output_path);
        args.push("-y".into());
        let output = spawn_hidden(&executable).args(args).output().map_err(|error| format!("Unable to run ffmpeg: {error}"));
        let _ = std::fs::remove_file(list_path);
        let output = output?;
        if output.status.success() {
            if delete_inputs { for path in files { let _ = std::fs::remove_file(path); } }
            Ok(())
        } else { Err(format!("Video merge failed: {}", String::from_utf8_lossy(&output.stderr))) }
    })
    .await
    .map_err(|error| format!("Merge task panicked: {error}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![run_ffmpeg, get_video_duration, capture_single_thumbnail, get_thumbnail_cache_dir, clear_thumbnail_cache, get_file_size, concat_videos])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}