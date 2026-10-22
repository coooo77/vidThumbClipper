use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

type ThumbnailRuns = Mutex<HashMap<String, Arc<AtomicBool>>>;

#[tauri::command]
fn begin_thumbnail(request_id: String, runs: tauri::State<ThumbnailRuns>) -> Result<(), String> {
    let mut runs = runs.lock().map_err(|_| "Thumbnail state lock poisoned")?;
    if runs.contains_key(&request_id) {
        return Err("Duplicate thumbnail request".into());
    }
    runs.insert(request_id, Arc::new(AtomicBool::new(false)));
    Ok(())
}

#[tauri::command]
fn cancel_thumbnail(request_id: String, runs: tauri::State<ThumbnailRuns>) -> Result<(), String> {
    if let Some(cancel) = runs
        .lock()
        .map_err(|_| "Thumbnail state lock poisoned")?
        .get(&request_id)
    {
        cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
fn end_thumbnail(request_id: String, runs: tauri::State<ThumbnailRuns>) -> Result<(), String> {
    runs.lock()
        .map_err(|_| "Thumbnail state lock poisoned")?
        .remove(&request_id);
    Ok(())
}

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
    child: std::process::Child,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    run_controlled(
        child,
        std::time::Instant::now() + timeout,
        &AtomicBool::new(false),
    )
}

fn run_controlled(
    mut child: std::process::Child,
    deadline: std::time::Instant,
    cancelled: &AtomicBool,
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

    let outcome = loop {
        if cancelled.load(Ordering::SeqCst) {
            break Err("Thumbnail cancelled".to_string());
        }
        if std::time::Instant::now() >= deadline {
            break Err("Process timed out: deadline exceeded".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => {
                break Err(format!("Unable to wait for process: {error}"));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    };

    // Reaping and pipe EOF must not keep the caller blocked indefinitely.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let kill = if outcome.is_err() {
            child.kill()
        } else {
            Ok(())
        };
        let _ = child.wait();
        let stdout = stdout_handle.join().unwrap_or_default();
        let stderr = stderr_handle.join().unwrap_or_default();
        let result = match outcome {
            Ok(status) => Ok((status, stdout, stderr)),
            Err(error) => Err(format!(
                "{error}; stop result: {kill:?}\n{}{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            )),
        };
        let _ = sender.send(result);
    });
    let (status, stdout, stderr) = receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| {
            "Process cleanup exceeded 2 seconds; background cleanup is still pending".to_string()
        })??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[tauri::command]
fn cancel_current_task(running: tauri::State<RunningPid>) -> Result<bool, String> {
    let pid = *running
        .lock()
        .map_err(|_| "State lock poisoned".to_string())?;
    let Some(pid) = pid else { return Ok(false) };
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
    Ok(true)
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
fn delete_file(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Unable to delete file: path is empty".to_string());
    }

    let path = PathBuf::from(trimmed);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        // A missing output is already in the desired state, so stale task records can still be
        // removed from the processed-video list.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Unable to delete file '{}': {error}",
            path.display()
        )),
    }
}

#[tauri::command]
fn get_file_size(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|meta| meta.len())
        .map_err(|error| format!("Unable to read file size: {error}"))
}

#[cfg(target_os = "windows")]
fn available_space_for_path(path: &str) -> Result<u64, String> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available: *mut u64,
            total_number_of_bytes: *mut u64,
            total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }

    let wide_path: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0_u64;
    let success = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        Err(format!(
            "Unable to read available disk space for '{}': {}",
            path,
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(available)
    }
}

#[cfg(not(target_os = "windows"))]
fn available_space_for_path(path: &str) -> Result<u64, String> {
    let output = Command::new("df")
        .args(["-Pk", path])
        .output()
        .map_err(|error| format!("Unable to run df: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Unable to read available disk space: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .ok_or("Unable to parse disk space result")?
        .to_string();
    let available_kib = line
        .split_whitespace()
        .nth(3)
        .ok_or("Unable to parse available disk space")?
        .parse::<u64>()
        .map_err(|error| format!("Unable to parse available disk space: {error}"))?;
    Ok(available_kib * 1024)
}

#[tauri::command]
fn get_available_space(path: String) -> Result<u64, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Unable to read disk space: path is empty".to_string());
    }
    available_space_for_path(trimmed)
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
    request_id: String,
    runs: tauri::State<'_, ThumbnailRuns>,
    ffmpeg_path: Option<String>,
    video_path: String,
    time: String,
    index: i64,
    width: u32,
    quality: u8,
    seek_mode: String,
    decoder_threads: u8,
    hardware_acceleration: bool,
) -> Result<String, String> {
    let cancelled = runs
        .lock()
        .map_err(|_| "Thumbnail state lock poisoned")?
        .get(&request_id)
        .cloned()
        .ok_or("Unknown thumbnail request")?;
    // A complete precise-seek fallback may need longer on multi-hour transport streams. Each
    // individual attempt is still bounded so one bad timestamp cannot hang the queue forever.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    tauri::async_runtime::spawn_blocking(move || {
        let check = || {
            if cancelled.load(Ordering::SeqCst) {
                Err("Thumbnail cancelled".to_string())
            } else if std::time::Instant::now() >= deadline {
                Err("Thumbnail timed out after 30 seconds".to_string())
            } else {
                Ok(())
            }
        };
        check()?;
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
        let width = width.clamp(160, 1920).to_string();
        let quality = quality.clamp(2, 12).to_string();
        let decoder_threads = decoder_threads.clamp(1, 4).to_string();
        let scale_filter = format!("scale={width}:-2");
        let target_seconds = time
            .parse::<f64>()
            .map_err(|error| format!("Invalid thumbnail timestamp '{time}': {error}"))?;
        let build_args = |mode: &str, output_path: &str| {
            let mut args = vec!["-y".to_string()];
            if hardware_acceleration {
                args.extend(["-hwaccel".to_string(), "auto".to_string()]);
            }
            if mode == "fast" {
                args.extend(["-ss".to_string(), time.clone()]);
            } else if mode == "hybrid" {
                args.extend([
                    "-ss".to_string(),
                    (target_seconds - 30.0).max(0.0).to_string(),
                ]);
            }
            args.extend([
                "-threads".to_string(),
                decoder_threads.clone(),
                "-i".to_string(),
                video_path.clone(),
            ]);
            if mode == "full" {
                args.extend(["-ss".to_string(), time.clone()]);
            } else if mode == "hybrid" {
                args.extend([
                    "-ss".to_string(),
                    target_seconds.min(30.0).to_string(),
                ]);
            }
            args.extend([
                "-vframes".to_string(),
                "1".to_string(),
                "-vf".to_string(),
                scale_filter.clone(),
                "-q:v".to_string(),
                quality.clone(),
                "-pix_fmt".to_string(),
                "yuvj420p".to_string(),
                "-strict".to_string(),
                "unofficial".to_string(),
                output_path.to_string(),
            ]);
            args
        };

        // The selected seek mode is attempted first. Fast mode falls back to precise mode when it
        // cannot produce a usable file. Both attempts drain output continuously and have a hard
        // timeout, so a bad timestamp cannot leave the frontend stuck forever.
        let prefer_precise = seek_mode == "precise";
        let primary_mode = if prefer_precise { "hybrid" } else { "fast" };
        let fast_args = build_args(primary_mode, &fast_path_string);
        let fast_started = std::time::Instant::now();
        let fast_output = spawn_hidden(&executable)
            .args(&fast_args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| format!("Unable to run ffmpeg: {error}"))
            .and_then(|child| {
                run_controlled(
                    child,
                    deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(45)),
                    &cancelled,
                )
            });
        let fast_ok = matches!(&fast_output, Ok(output) if output.status.success())
            && captured_ok(&fast_path);
        debug.push(thumbnail_attempt_debug(
            if prefer_precise { "precise-primary" } else { "fast" },
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
            check().map_err(|error| format!("{error}\n{}", debug.join("\n")))?;
            if matches!(&fast_output, Err(error) if error.contains("cleanup exceeded") || error.contains("stop result: Err")) {
                return Err(format!("Thumbnail process could not be cleaned up; retry stopped.\n{}", debug.join("\n")));
            }
            let precise_path = cache_dir.join(format!("thumbnail_{index}_{timestamp}_precise.jpg"));
            let precise_path_string = precise_path.to_string_lossy().to_string();
            let fallback_mode = if prefer_precise { "full" } else { "hybrid" };
            let precise_args = build_args(fallback_mode, &precise_path_string);
            // Both attempts share the same 90-second deadline; fallback gets only what remains.
            let precise_started = std::time::Instant::now();
            let precise_output = spawn_hidden(&executable)
                .args(&precise_args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|error| format!("Unable to run ffmpeg: {error}"))
                .and_then(|child| run_controlled(child, deadline, &cancelled));
            let precise_ok = matches!(&precise_output, Ok(output) if output.status.success())
                && captured_ok(&precise_path);
            debug.push(thumbnail_attempt_debug(
                if prefer_precise { "full-precise-fallback" } else { "hybrid-precise-fallback" },
                precise_started.elapsed(),
                &precise_output,
                &precise_path,
                &precise_args,
            ));
            if !precise_ok {
                let _ = std::fs::remove_file(&precise_path);
                return Err(format!(
                    "Thumbnail capture at {time} failed ({primary_mode} + {fallback_mode} seek).\n{}",
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
    args: &[String],
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
        .manage(ThumbnailRuns::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            run_ffmpeg,
            get_video_duration,
            capture_single_thumbnail,
            begin_thumbnail,
            cancel_thumbnail,
            end_thumbnail,
            get_thumbnail_cache_dir,
            clear_thumbnail_cache,
            delete_file,
            get_file_size,
            get_available_space,
            concat_videos,
            cancel_current_task
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // Launch this test binary as a controlled child, without depending on installed FFmpeg.
    fn fixture(mode: &str) -> std::process::Child {
        spawn_hidden(std::env::current_exe().unwrap().to_str().unwrap())
            .args([
                "--exact",
                "tests::process_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("THUMBNAIL_TEST_MODE", mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn process_fixture() {
        if std::env::var("THUMBNAIL_TEST_MODE").unwrap() == "output" {
            use std::io::Write;
            std::io::stdout()
                .write_all(&vec![b'x'; 256 * 1024])
                .unwrap();
            std::io::stderr()
                .write_all(&vec![b'y'; 256 * 1024])
                .unwrap();
        } else {
            std::thread::sleep(Duration::from_secs(10));
        }
    }

    #[test]
    fn cancellation_interrupts_running_child() {
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let child = fixture("sleep");
        let started = Instant::now();
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            signal.store(true, Ordering::SeqCst);
        });
        let error = run_controlled(child, started + Duration::from_secs(8), &cancel).unwrap_err();
        setter.join().unwrap();
        assert!(error.contains("cancelled"), "{error}");
        assert!(error.contains("stop result: Ok"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn retry_uses_remaining_deadline() {
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let deadline = started + Duration::from_millis(700);
        let fast = run_controlled(
            fixture("sleep"),
            started + Duration::from_millis(300),
            &cancel,
        )
        .unwrap_err();
        assert!(fast.contains("timed out"), "{fast}");
        let precise = run_controlled(fixture("sleep"), deadline, &cancel).unwrap_err();
        assert!(precise.contains("timed out"), "{precise}");
        assert!(precise.contains("stop result: Ok"), "{precise}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn drains_both_pipes_without_deadlock() {
        let output = run_controlled(
            fixture("output"),
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() >= 256 * 1024);
        assert!(output.stderr.len() >= 256 * 1024);
    }
}
