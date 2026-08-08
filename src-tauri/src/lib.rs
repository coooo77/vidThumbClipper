use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose, Engine as _};

// Tracks the PID of whichever long-running ffmpeg process (clip/merge/combine — not the quick
// thumbnail/duration calls) is currently in flight, so a "stop task" request from the frontend has
// something to actually kill. Only one video-processing task runs at a time (Tasks.vue's queue is
// sequential), so a single slot is enough — no need for a full task-id-keyed registry.
type RunningPid = Arc<Mutex<Option<u32>>>;

fn thumbnail_cache_dir() -> Result<PathBuf, String> {
    let directory = std::env::temp_dir()
        .join("vidThumbClipper")
        .join("thumbnails");
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

// Runs a spawned child to completion with a hard time limit. stdout/stderr are drained on
// dedicated threads the whole time (not just after exit) — reading them only after the process
// ends risks the OS pipe buffer filling up and the child blocking on its own write(), which would
// turn "wait for exit" into its own indefinite hang. If the timeout elapses, the child is killed
// so the caller is guaranteed a result either way instead of hanging forever.
fn run_with_timeout(
    mut child: std::process::Child,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(format!("Unable to wait for process: {error}"));
            }
        }
        if start.elapsed() > timeout {
            let kill_result = child.kill();
            let wait_result = child.wait();
            let _ = stdout_handle.join();
            let _ = stderr_handle.join();
            return Err(format!(
                "Process timed out after {} seconds and was stopped (kill={kill_result:?}, wait={wait_result:?}).",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[tauri::command]
fn cancel_current_task(running: tauri::State<RunningPid>) -> Result<(), String> {
    let pid = *running
        .lock()
        .map_err(|_| "State lock poisoned".to_string())?;
    let Some(pid) = pid else { return Ok(()) };
    #[cfg(target_os = "windows")]
    {
        let _ = spawn_hidden("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = spawn_hidden("kill").args(["-9", &pid.to_string()]).output();
    }
    Ok(())
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
    let entries = std::fs::read_dir(&directory)
        .map_err(|error| format!("Unable to read thumbnail cache: {error}"))?;
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
async fn run_ffmpeg(
    ffmpeg_path: Option<String>,
    args: Vec<String>,
    running: tauri::State<'_, RunningPid>,
) -> Result<String, String> {
    let running = running.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let executable = ffmpeg_path
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| "ffmpeg".into());
        let child = spawn_hidden(&executable)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| format!("Unable to run ffmpeg: {error}"))?;
        let pid = child.id();
        if let Ok(mut guard) = running.lock() {
            *guard = Some(pid);
        }
        let output = run_with_timeout(child, std::time::Duration::from_secs(3600));
        if let Ok(mut guard) = running.lock() {
            *guard = None;
        }
        let output = output?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.success() {
            Ok(format!("{stdout}{stderr}"))
        } else {
            Err(format!("ffmpeg failed:\n{stdout}{stderr}"))
        }
    })
    .await
    .map_err(|error| format!("ffmpeg task panicked: {error}"))?
}

#[tauri::command]
async fn get_video_duration(
    ffprobe_path: Option<String>,
    video_path: String,
) -> Result<f64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let executable = ffprobe_path
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| "ffprobe".into());
        let output = spawn_hidden(&executable)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                &video_path,
            ])
            .output()
            .map_err(|error| format!("Unable to run ffprobe: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "ffprobe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .map_err(|_| "Could not read the video's duration.".into())
    })
    .await
    .map_err(|error| format!("ffprobe task panicked: {error}"))?
}

#[tauri::command]
async fn capture_single_thumbnail(
    ffmpeg_path: Option<String>,
    video_path: String,
    time: String,
    index: i64,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let executable = ffmpeg_path
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| "ffmpeg".into());
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let cache_dir = thumbnail_cache_dir()?;
        let fast_path = cache_dir.join(format!("thumbnail_{index}_{timestamp}_fast.jpg"));
        let fast_path_string = fast_path.to_string_lossy().to_string();
        let captured_ok = |path: &std::path::Path| {
            path.is_file() && path.metadata().map(|meta| meta.len() > 0).unwrap_or(false)
        };
        let mut debug = Vec::new();

        // Fast seek is preferred; precise seek is used only when fast seek did not produce a usable
        // file. Both attempts drain their output continuously and have a hard timeout, so a bad
        // timestamp cannot leave the frontend's processing state stuck forever.
        let fast_args = [
            "-y",
            "-ss",
            &time,
            "-i",
            &video_path,
            "-vframes",
            "1",
            "-q:v",
            "5",
            "-pix_fmt",
            "yuvj420p",
            "-strict",
            "unofficial",
            &fast_path_string,
        ];
        let fast_started = std::time::Instant::now();
        let fast_output = spawn_hidden(&executable)
            .args(fast_args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| format!("Unable to run ffmpeg: {error}"))
            .and_then(|child| run_with_timeout(child, std::time::Duration::from_secs(20)));
        let fast_ok = matches!(&fast_output, Ok(output) if output.status.success())
            && captured_ok(&fast_path);
        debug.push(thumbnail_attempt_debug(
            "fast",
            fast_started.elapsed(),
            &fast_output,
            &fast_path,
            &fast_args,
        ));

        let result_path = if fast_ok {
            fast_path.clone()
        } else {
            // Do not leave a zero-byte/invalid fast output around while the fallback runs.
            let _ = std::fs::remove_file(&fast_path);
            let precise_path = cache_dir.join(format!("thumbnail_{index}_{timestamp}_precise.jpg"));
            let precise_path_string = precise_path.to_string_lossy().to_string();
            let precise_args = [
                "-y",
                "-i",
                &video_path,
                "-ss",
                &time,
                "-vframes",
                "1",
                "-q:v",
                "5",
                "-pix_fmt",
                "yuvj420p",
                "-strict",
                "unofficial",
                &precise_path_string,
            ];
            // Precise seek is slower, but the timeout guarantees a failed capture eventually returns
            // to Home instead of leaving the initial/expansion state stuck forever.
            let precise_started = std::time::Instant::now();
            let precise_output = spawn_hidden(&executable)
                .args(precise_args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|error| format!("Unable to run ffmpeg: {error}"))
                .and_then(|child| run_with_timeout(child, std::time::Duration::from_secs(120)));
            let precise_ok = matches!(&precise_output, Ok(output) if output.status.success())
                && captured_ok(&precise_path);
            debug.push(thumbnail_attempt_debug(
                "precise",
                precise_started.elapsed(),
                &precise_output,
                &precise_path,
                &precise_args,
            ));
            if !precise_ok {
                let _ = std::fs::remove_file(&precise_path);
                return Err(format!(
                    "Thumbnail capture at {time} failed (fast + precise seek).\n{}",
                    debug.join("\n"),
                ));
            }
            precise_path
        };

        let mut file = File::open(&result_path).map_err(|error| {
            format!(
                "Unable to read generated thumbnail at {time}: {error}\n{}",
                debug.join("\n")
            )
        })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| {
            format!(
                "Unable to read generated thumbnail at {time}: {error}\n{}",
                debug.join("\n")
            )
        })?;
        drop(file);
        // The frame is only ever needed as the base64 payload returned below (the grid lives in the
        // browser as data URLs) — delete the temp jpg(s) immediately so the cache never accumulates.
        let _ = std::fs::remove_file(&fast_path);
        let _ = std::fs::remove_file(&result_path);
        Ok(general_purpose::STANDARD.encode(bytes))
    })
    .await
    .map_err(|error| format!("Thumbnail task panicked: {error}"))?
}

fn thumbnail_attempt_debug(
    label: &str,
    elapsed: std::time::Duration,
    output: &Result<std::process::Output, String>,
    path: &std::path::Path,
    args: &[&str],
) -> String {
    let file_info = match path.metadata() {
        Ok(metadata) => format!(
            "file_exists=true is_file={} bytes={}",
            metadata.is_file(),
            metadata.len()
        ),
        Err(error) => format!("file_exists=false metadata_error={error}"),
    };
    match output {
        Ok(output) => format!(
            "[thumbnail:{label}] elapsed_ms={} exit_code={:?} success={} {file_info}\nargs: {}\nstdout:\n{}\nstderr:\n{}",
            elapsed.as_millis(),
            output.status.code(),
            output.status.success(),
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ),
        Err(error) => format!(
            "[thumbnail:{label}] elapsed_ms={} process_error={error} {file_info}\nargs: {}",
            elapsed.as_millis(),
            args.join(" "),
        ),
    }
}

#[tauri::command]
async fn concat_videos(
    ffmpeg_path: Option<String>,
    files: Vec<String>,
    output_path: String,
    extra_args: Vec<String>,
    delete_inputs: bool,
    running: tauri::State<'_, RunningPid>,
) -> Result<(), String> {
    let running = running.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if files.is_empty() {
            return Err("No clips were supplied for merging.".into());
        }
        let executable = ffmpeg_path
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| "ffmpeg".into());
        let list_path = thumbnail_cache_dir()?.join(format!(
            "concat_{}.txt",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let list_body = files
            .iter()
            .map(|path| format!("file '{}'", path.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&list_path, list_body)
            .map_err(|error| format!("Unable to create merge list: {error}"))?;
        let list_path_string = list_path.to_string_lossy().to_string();
        let mut args = vec![
            "-f".into(),
            "concat".into(),
            "-safe".into(),
            "0".into(),
            "-i".into(),
            list_path_string,
            "-c".into(),
            "copy".into(),
        ];
        args.extend(extra_args);
        args.push(output_path);
        args.push("-y".into());
        let child = spawn_hidden(&executable)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| format!("Unable to run ffmpeg: {error}"));
        let output = child.and_then(|child| {
            let pid = child.id();
            if let Ok(mut guard) = running.lock() {
                *guard = Some(pid);
            }
            let result = run_with_timeout(child, std::time::Duration::from_secs(3600));
            if let Ok(mut guard) = running.lock() {
                *guard = None;
            }
            result
        });
        let _ = std::fs::remove_file(list_path);
        let output = output?;
        if output.status.success() {
            if delete_inputs {
                for path in files {
                    let _ = std::fs::remove_file(path);
                }
            }
            Ok(())
        } else {
            Err(format!(
                "Video merge failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    })
    .await
    .map_err(|error| format!("Merge task panicked: {error}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(None::<u32>)) as RunningPid)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            run_ffmpeg,
            get_video_duration,
            capture_single_thumbnail,
            get_thumbnail_cache_dir,
            clear_thumbnail_cache,
            get_file_size,
            concat_videos,
            cancel_current_task
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
