use crate::tools::{get_category_for_extension, detect_gpu_encoders, get_gpu_encoder_for_format, get_tool_path};
use crate::types::{ConversionOptions, ConversionResult, ConversionProgress, ImageOptions, VideoOptions};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

// Windows constant for hiding console window
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Get absolute path without the \\?\ prefix that canonicalize() adds on Windows
/// This prefix can cause issues with some programs like LibreOffice
pub(crate) fn get_absolute_path(path: &Path) -> std::path::PathBuf {
    match path.canonicalize() {
        Ok(p) => {
            let path_str = p.to_string_lossy();
            // Remove \\?\ prefix on Windows
            if path_str.starts_with(r"\\?\") {
                std::path::PathBuf::from(&path_str[4..])
            } else {
                p
            }
        }
        Err(_) => path.to_path_buf(),
    }
}

// Global process registry for tracking and killing running conversions
lazy_static::lazy_static! {
    pub static ref RUNNING_PROCESSES: Mutex<HashMap<String, Child>> = Mutex::new(HashMap::new());
    pub static ref CANCELLED_JOBS: Mutex<std::collections::HashSet<String>> = Mutex::new(std::collections::HashSet::new());
    pub static ref APP_HANDLE: Mutex<Option<AppHandle>> = Mutex::new(None);
}

/// Set the app handle for emitting events
pub fn set_app_handle(handle: AppHandle) {
    if let Ok(mut app_handle) = APP_HANDLE.lock() {
        *app_handle = Some(handle);
    }
}

/// Get video/audio duration in seconds using ffprobe
pub fn get_media_duration(input_path: &str) -> Option<f64> {
    // Resolved centrally (bundled -> configured -> system) instead of
    // deriving the ffprobe path ad hoc here - see engines::resolver.
    let ffprobe = crate::engines::resolver::resolve(crate::engines::EngineId::Ffprobe).ok()?;

    let mut command = Command::new(&ffprobe.path);
    command.args([
        "-v", "quiet",
        "-show_entries", "format=duration",
        "-of", "default=noprint_wrappers=1:nokey=1",
        input_path,
    ]);
    
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    
    let output = command.output().ok()?;
    
    let duration_str = String::from_utf8_lossy(&output.stdout);
    duration_str.trim().parse::<f64>().ok()
}

/// Parse FFmpeg progress output line and extract time in seconds
fn parse_ffmpeg_time(line: &str) -> Option<f64> {
    // Format: out_time=00:01:23.456789 or out_time_ms=12345678
    if line.starts_with("out_time_ms=") {
        let ms_str = line.trim_start_matches("out_time_ms=");
        if let Ok(ms) = ms_str.parse::<i64>() {
            return Some(ms as f64 / 1000.0);
        }
    } else if line.starts_with("out_time=") {
        let time_str = line.trim_start_matches("out_time=");
        return parse_time_string(time_str);
    }
    None
}

/// Parse FFmpeg speed (e.g., "1.23x" -> 1.23)
fn parse_ffmpeg_speed(line: &str) -> Option<f64> {
    if line.starts_with("speed=") {
        let speed_str = line.trim_start_matches("speed=").trim_end_matches('x').trim();
        if speed_str == "N/A" {
            return None;
        }
        return speed_str.parse::<f64>().ok();
    }
    None
}

/// Parse time string (HH:MM:SS.MS) to seconds
fn parse_time_string(time_str: &str) -> Option<f64> {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() == 3 {
        let hours: f64 = parts[0].parse().ok()?;
        let minutes: f64 = parts[1].parse().ok()?;
        let seconds: f64 = parts[2].parse().ok()?;
        return Some(hours * 3600.0 + minutes * 60.0 + seconds);
    }
    None
}

/// Emit progress event to frontend
fn emit_progress(job_id: &str, progress: f32, current_time: f64, total_duration: f64, speed: f64, eta: Option<f64>) {
    if let Ok(app_handle) = APP_HANDLE.lock() {
        if let Some(ref handle) = *app_handle {
            let progress_event = ConversionProgress {
                job_id: job_id.to_string(),
                progress,
                current_time_secs: current_time,
                total_duration_secs: total_duration,
                speed,
                eta_secs: eta,
                status: if progress >= 100.0 { "completed".to_string() } else { "converting".to_string() },
            };
            let _ = handle.emit("conversion-progress", progress_event);
        }
    }
}

/// Kill a running conversion process by job ID
pub fn kill_process(job_id: &str) -> Result<(), String> {
    // Mark as cancelled
    if let Ok(mut cancelled) = CANCELLED_JOBS.lock() {
        cancelled.insert(job_id.to_string());
    }
    
    // Kill the process if it exists
    if let Ok(mut processes) = RUNNING_PROCESSES.lock() {
        if let Some(mut child) = processes.remove(job_id) {
            // Try to kill the process (ignore error if already finished)
            let _ = child.kill();
            // Wait for the process to avoid zombies
            let _ = child.wait();
            return Ok(());
        }
    }
    
    Ok(())
}

/// Check if a job has been cancelled
pub fn is_cancelled(job_id: &str) -> bool {
    CANCELLED_JOBS
        .lock()
        .map(|cancelled| cancelled.contains(job_id))
        .unwrap_or(false)
}

/// Clear the cancelled flag for a job
pub fn clear_cancelled(job_id: &str) {
    if let Ok(mut cancelled) = CANCELLED_JOBS.lock() {
        cancelled.remove(job_id);
    }
}

/// The main conversion router - the single path a "drop file, pick format,
/// hit Convert" action goes through for every category (video, audio,
/// image, document, spreadsheet, presentation, archive, vector, font).
///
/// Phase 1 additions, in order:
///   1. Centralized validation (`security::path_validation`) of both the
///      input file and the output directory before anything else happens.
///   2. The category converter is handed a path *inside an isolated job
///      temp directory* instead of the real output directory directly.
///      Every converter function in this file already has the invariant
///      of returning exactly the `output` path it was given (verified
///      across all nine category converters), so this substitution is
///      transparent to them - none of them needed to change.
///   3. Only after the converter reports success is the finished file
///      moved into the user's real output directory. A failed or
///      cancelled conversion never leaves a partial/corrupt file there -
///      at most, debris in the ephemeral job directory, which is removed
///      when `job` drops at the end of this function regardless of which
///      branch returned.
pub fn convert_file(
    input_path: &str,
    output_format: &str,
    output_dir: &str,
    options: ConversionOptions,
    job_id: Option<&str>,
) -> ConversionResult {
    let start = Instant::now();

    let fail = |e: String, start: Instant| ConversionResult {
        success: false,
        output_path: None,
        error: Some(e),
        duration_ms: start.elapsed().as_millis() as u64,
    };

    let input_canonical = match crate::security::path_validation::validate_input_file(input_path) {
        Ok(p) => p,
        Err(e) => return fail(e, start),
    };
    let output_dir_canonical = match crate::security::path_validation::validate_output_dir(output_dir) {
        Ok(p) => p,
        Err(e) => return fail(e, start),
    };
    let input = input_canonical.as_path();
    let input_path_str = input_canonical.to_string_lossy().to_string();

    // Isolated per-job working directory. Cleaned up automatically when
    // `job` goes out of scope at the end of this function (Drop), whether
    // we return success, an error, or unwind partway through - see
    // security::temp::JobTempDir.
    let job = match crate::security::temp::JobTempDir::new() {
        Ok(j) => j,
        Err(e) => return fail(format!("Failed to create working directory: {}", e), start),
    };
    
    // Clear any previous cancelled state for this job
    if let Some(jid) = job_id {
        clear_cancelled(jid);
    }
    
    let extension = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    
    let category = get_category_for_extension(&extension);
    let output_format_clean = output_format.trim_start_matches('.');
    
    // Generate the FINAL filename (in the real output directory) up front,
    // so we know what to name the temp-dir working file too.
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let mut final_filename = format!("{}.{}", stem, output_format_clean);
    let mut final_output_path = output_dir_canonical.join(&final_filename);

    // Don't let the final output overwrite the input (can happen when
    // converting to the same extension, e.g. a compress-only PDF pass).
    if final_output_path == input_canonical {
        final_filename = format!("{}_converted.{}", stem, output_format_clean);
        final_output_path = output_dir_canonical.join(&final_filename);
    }

    let temp_output = job.file_path(&final_filename);
    let temp_output_str = temp_output.to_string_lossy().to_string();
    
    let result = match category {
        "video" => convert_video(&input_path_str, &temp_output_str, output_format_clean, &options, job_id),
        "audio" => convert_audio(&input_path_str, &temp_output_str, output_format_clean, &options, job_id),
        "image" => convert_image(&input_path_str, &temp_output_str, output_format_clean, &options, job_id),
        "document" | "ebook" => convert_document(&input_path_str, &temp_output_str, output_format_clean, &options, job_id, job.path()),
        "spreadsheet" => convert_spreadsheet(&input_path_str, &temp_output_str, output_format_clean, job_id, job.path()),
        "presentation" => convert_presentation(&input_path_str, &temp_output_str, output_format_clean, job_id, job.path()),
        "archive" => convert_archive(&input_path_str, &temp_output_str, output_format_clean, &options, job_id),
        "vector" => convert_vector(&input_path_str, &temp_output_str, output_format_clean, &options, job_id),
        "font" => convert_font(&input_path_str, &temp_output_str, output_format_clean),
        _ => Err(format!("Unsupported file category: {}", category)),
    };
    
    // Clean up cancelled state
    if let Some(jid) = job_id {
        clear_cancelled(jid);
    }
    
    let final_result = match result {
        Ok(produced_path) => {
            // Defense in depth: confirm the converter actually wrote
            // inside the job directory we gave it before we move
            // anything, in case a future converter change stops
            // respecting the `output` path it's handed.
            let produced = Path::new(&produced_path);
            match crate::security::path_validation::confirm_within(job.path(), produced) {
                Ok(_) => match crate::security::temp::move_into_place(produced, &final_output_path) {
                    Ok(()) => Ok(final_output_path.to_string_lossy().to_string()),
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    };

    let duration = start.elapsed();
    
    match final_result {
        Ok(path) => ConversionResult {
            success: true,
            output_path: Some(path),
            error: None,
            duration_ms: duration.as_millis() as u64,
        },
        Err(e) => ConversionResult {
            success: false,
            output_path: None,
            error: Some(e),
            duration_ms: duration.as_millis() as u64,
        },
    }
}

fn convert_video(
    input: &str,
    output: &str,
    format: &str,
    options: &ConversionOptions,
    job_id: Option<&str>,
) -> Result<String, String> {
    // Get video duration for progress tracking
    let duration = get_media_duration(input);
    
    if format == "gif" {
        let mut gif_args: Vec<String> = vec![
            "-i".to_string(),
            input.to_string(),
            "-y".to_string(), 
        ];
        if let Some(ref start) = options.start_time {
            gif_args.insert(0, start.clone());
            gif_args.insert(0, "-ss".to_string());
        }
        if let Some(ref end) = options.end_time {
            gif_args.push("-to".to_string());
            gif_args.push(end.clone());
        }
        gif_args.push("-vf".to_string());
        gif_args.push("fps=10,scale=480:-1:flags=lanczos".to_string());
        gif_args.push(output.to_string());
        return run_command_with_job_id("ffmpeg", &gif_args, job_id, duration)
            .map(|_| output.to_string());
    }
    
    let mut args: Vec<String> = vec![
        "-i".to_string(),
        input.to_string(),
        "-y".to_string(),
    ];
    
    // Check for GPU encoding
    let use_gpu = options.use_gpu.unwrap_or(false);
    let gpu_encoder = if use_gpu {
        let gpu_info = detect_gpu_encoders();
        get_gpu_encoder_for_format(format, &gpu_info, options.gpu_encoder.as_deref())
    } else {
        None
    };
    
    // Add time trimming
    if let Some(ref start) = options.start_time {
        args.insert(0, start.clone());
        args.insert(0, "-ss".to_string());
    }
    if let Some(ref end) = options.end_time {
        args.push("-to".to_string());
        args.push(end.clone());
    }
    
    // Add resolution filters
    let mut vf_filters = Vec::new();
    let mut custom_scale = false;
    if let Some(preset) = options.preset_resolution.as_deref() {
        let scale = match preset {
            "4K" => "scale=3840:-2", 
            "1080p" => "scale=1920:-2",
            "720p" => "scale=1280:-2",
            "480p" => "scale=854:-2",
            "Custom" => {
                custom_scale = true;
                ""
            },
            _ => "",
        };
        if !scale.is_empty() {
            vf_filters.push(scale.to_string());
        }
    }
    
    if custom_scale {
        if let (Some(w), Some(h)) = (options.custom_width, options.custom_height) {
            let maintain = options.maintain_aspect_ratio.unwrap_or(true);
            if maintain {
                vf_filters.push(format!("scale={}:-2", w)); // or complex aspect ratio math if both are enforced. FFmpeg `scale=w:h:force_original_aspect_ratio=decrease` is better
                // For simplicity mapping user desire: if lock is on, `scale=w:-2` or `-2:h` might be applied. 
                // Let's implement full aspect ratio force:
                vf_filters.push(format!("scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2", w, h, w, h));
            } else {
                vf_filters.push(format!("scale={}:{}", w, h));
            }
        }
    } else if options.preset_resolution.is_none() {
        if let (Some(w), Some(h)) = (options.width, options.height) {
            vf_filters.push(format!("scale={}:{}", w, h));
        }
    }
    
    // Add Frame Rate
    if let Some(fps_val) = &options.fps {
        if let Some(fps_str) = fps_val.as_str() {
            if fps_str != "Match Source" {
                args.push("-r".to_string());
                args.push(fps_str.to_string());
            }
        } else if let Some(fps_num) = fps_val.as_f64() {
            args.push("-r".to_string());
            args.push(fps_num.to_string());
        }
    }

    // Advanced Codec & Encoding Options
    let video_codec = options.video_codec.as_deref().unwrap_or("H.264");
    let audio_codec = options.audio_codec.as_deref().unwrap_or("AAC");
    let bitrate_mode = options.bitrate_mode.as_deref().unwrap_or("VBR");
    
    // Apply Video Codec
    let ffmpeg_vcodec = match video_codec {
        "H.265" | "HEVC" => {
            if let Some(ref encoder) = gpu_encoder {
                match encoder.as_str() {
                    "hevc_nvenc" | "hevc_amf" | "hevc_qsv" | "hevc_videotoolbox" => encoder.clone(),
                    _ => "libx265".to_string(),
                }
            } else { "libx265".to_string() }
        },
        "VP9" => "libvpx-vp9".to_string(), 
        "AV1" => {
            if let Some(ref encoder) = gpu_encoder {
                match encoder.as_str() {
                    "av1_nvenc" | "av1_amf" | "av1_qsv" => encoder.clone(),
                    _ => "libsvtav1".to_string(),
                }
            } else { "libsvtav1".to_string() }
        },
        _ => { // default H.264
            if let Some(ref encoder) = gpu_encoder {
                match encoder.as_str() {
                    "h264_nvenc" | "h264_amf" | "h264_qsv" | "h264_videotoolbox" => encoder.clone(),
                    _ => "libx264".to_string(),
                }
            } else { "libx264".to_string() }
        }
    };
    
    args.push("-c:v".to_string());
    args.push(ffmpeg_vcodec.clone());
    
    // Quality & Bitrate
    if bitrate_mode == "VBR" {
        let crf = options.crf.unwrap_or(23);
        
        if ffmpeg_vcodec.contains("nvenc") {
            args.push("-rc".to_string());
            args.push("vbr".to_string());
            args.push("-cq".to_string());
            args.push(crf.to_string());
        } else if ffmpeg_vcodec.contains("amf") {
            args.push("-rc".to_string());
            args.push("vbr_latency".to_string());
            args.push("-qp_i".to_string());
            args.push(crf.to_string());
            args.push("-qp_p".to_string());
            args.push(crf.to_string());
        } else if ffmpeg_vcodec.contains("qsv") {
            args.push("-global_quality".to_string());
            args.push(crf.to_string());
        } else if ffmpeg_vcodec.contains("videotoolbox") {
            let qv = ((51 - crf) as f32 / 51.0 * 100.0) as u32; // Map roughly
            args.push("-q:v".to_string());
            args.push(qv.to_string());
        } else {
            args.push("-crf".to_string());
            args.push(crf.to_string());
            if ffmpeg_vcodec == "libvpx-vp9" {
                 args.push("-b:v".to_string());
                 args.push("0".to_string());
            }
        }
    } else {
        // CBR mode
        let bitrate_mbps = options.video_bitrate.unwrap_or(8.0);
        let target_bitrate = format!("{}M", bitrate_mbps);
        
        args.push("-b:v".to_string());
        args.push(target_bitrate.clone());
        args.push("-maxrate".to_string());
        args.push(target_bitrate.clone());
        args.push("-bufsize".to_string());
        args.push(format!("{}M", bitrate_mbps * 2.0));
    }
    
    // Audio Codec
    if audio_codec == "No Audio" {
        args.push("-an".to_string());
    } else {
        let ffmpeg_acodec = match audio_codec {
            "MP3" => "libmp3lame",
            "Opus" => "libopus", // WebM might override this earlier but this covers explicit cases
            _ => "aac",
        };
        args.push("-c:a".to_string());
        args.push(ffmpeg_acodec.to_string());
        
        let audio_bitrate = options.audio_bitrate_kbps.unwrap_or(192);
        args.push("-b:a".to_string());
        args.push(format!("{}k", audio_bitrate));
        
        if let Some(sample_rate) = options.audio_sample_rate.as_deref() {
            if sample_rate != "Match Source" {
                let sr_val = match sample_rate {
                    "44.1kHz" => "44100",
                    "48kHz" => "48000",
                    _ => "",
                };
                if !sr_val.is_empty() {
                    args.push("-ar".to_string());
                    args.push(sr_val.to_string());
                }
            }
        }
        
        if let Some(vol) = options.volume_db {
            if vol != 0 {
                args.push("-af".to_string());
                args.push(format!("volume={}dB", vol));
            }
        }
        
        if let Some(channels) = options.channel_layout.as_deref() {
            if channels != "Auto" {
                let ac = match channels {
                    "Mono" => "1",
                    "Stereo" => "2",
                    "5.1" => "6",
                    _ => "",
                };
                if !ac.is_empty() {
                    args.push("-ac".to_string());
                    args.push(ac.to_string());
                }
            }
        }
    }
    
    // Subtitles
    if let Some(sub_action) = options.subtitle_action.as_deref() {
        match sub_action {
            "Strip All" => {
                args.push("-sn".to_string());
            },
            "Burn Into Video" => {
                if let Some(index) = options.subtitle_stream_index {
                    // Escape the path for FFmpeg filter: replace backslashes and colons (common in Windows paths)
                    let escaped_path = input.replace("\\", "/").replace(":", "\\:");
                    vf_filters.push(format!("subtitles='{}':si={}", escaped_path, index));
                }
            },
            _ => { // "No Change"
                args.push("-c:s".to_string());
                args.push("copy".to_string());
            }
        }
    }

    if !vf_filters.is_empty() {
        args.push("-vf".to_string());
        args.push(vf_filters.join(","));
    }
    
    args.push(output.to_string());
    
    let is_two_pass = options.two_pass.unwrap_or(false) && bitrate_mode == "CBR";
    
    if is_two_pass {
        let mut pass1_args = args.clone();
        pass1_args.pop(); // remove output filename
        pass1_args.push("-pass".to_string());
        pass1_args.push("1".to_string());
        pass1_args.push("-f".to_string());
        pass1_args.push(if format == "webm" { "webm".to_string() } else if format == "mkv" { "matroska".to_string() } else { "mp4".to_string() }); 
        pass1_args.push(if cfg!(windows) { "NUL".to_string() } else { "/dev/null".to_string() });
        
        let _ = run_command_with_job_id("ffmpeg", &pass1_args, job_id, duration);
        
        let out = args.pop().unwrap();
        args.push("-pass".to_string());
        args.push("2".to_string());
        args.push(out);
    }

    run_command_with_job_id("ffmpeg", &args, job_id, duration)
        .map(|_| output.to_string())
}

fn convert_audio(
    input: &str,
    output: &str,
    format: &str,
    options: &ConversionOptions,
    job_id: Option<&str>,
) -> Result<String, String> {
    // Get audio duration for progress tracking
    let duration = get_media_duration(input);
    
    let mut args: Vec<String> = vec![
        "-i".to_string(),
        input.to_string(),
        "-y".to_string(),
    ];
    
    // Add quality/bitrate settings
    if let Some(ref bitrate) = options.bitrate {
        args.push("-b:a".to_string());
        args.push(bitrate.clone());
    } else if let Some(quality) = options.quality {
        let bitrate = match quality {
            90..=100 => "320k",
            70..=89 => "256k",
            50..=69 => "192k",
            30..=49 => "128k",
            _ => "96k",
        };
        args.push("-b:a".to_string());
        args.push(bitrate.to_string());
    }
    
    // Format-specific codec
    match format {
        "mp3" => {
            args.push("-c:a".to_string());
            args.push("libmp3lame".to_string());
        }
        "aac" | "m4a" => {
            args.push("-c:a".to_string());
            args.push("aac".to_string());
        }
        "opus" => {
            args.push("-c:a".to_string());
            args.push("libopus".to_string());
        }
        "flac" => {
            args.push("-c:a".to_string());
            args.push("flac".to_string());
        }
        "ogg" => {
            args.push("-c:a".to_string());
            args.push("libvorbis".to_string());
        }
        _ => {}
    }
    
    args.push(output.to_string());
    
    run_command_with_job_id("ffmpeg", &args, job_id, duration)
        .map(|_| output.to_string())
}

fn convert_image(
    input: &str,
    output: &str,
    format: &str,
    options: &ConversionOptions,
    job_id: Option<&str>,
) -> Result<String, String> {
    use crate::native::image as native_image;

    // Common raster formats (JPEG/PNG/WebP/BMP/GIF/TIFF) go through the
    // in-process pure-Rust pipeline - no external tool, no subprocess, no
    // ImageMagick dependency. Special formats (AVIF/HEIC/PSD, or an
    // extension the native pipeline doesn't recognize) fall back to
    // ImageMagick, which is still required for those - see Step 2 final
    // report classification.
    if let Some(target) = native_image::NativeImageFormat::from_extension(format) {
        let native_options = native_image::NativeConvertOptions {
            width: options.width,
            height: options.height,
            quality: options.quality.map(|q| q.clamp(1, 100) as u8),
            png_compression_level: options.compression_level.map(|c| c.clamp(0, 9) as u8),
        };
        return native_image::convert_file(Path::new(input), Path::new(output), target, &native_options)
            .map(|_| output.to_string())
            .map_err(|e| e.to_string());
    }

    let mut args: Vec<String> = vec![input.to_string()];

    // Add resize
    if let (Some(w), Some(h)) = (options.width, options.height) {
        args.push("-resize".to_string());
        args.push(format!("{}x{}", w, h));
    }

    // Add quality
    if let Some(quality) = options.quality {
        args.push("-quality".to_string());
        args.push(quality.to_string());
    }

    // Strip metadata if requested
    if options.preserve_metadata == Some(false) {
        args.push("-strip".to_string());
    }

    // Format-specific settings
    match format {
        "avif" => {
            args.push("-define".to_string());
            args.push("heic:speed=5".to_string());
        }
        _ => {}
    }

    args.push(output.to_string());

    run_command_with_job_id("magick", &args, job_id, None)
        .map(|_| output.to_string())
}

fn convert_document(
    input: &str,
    output: &str,
    format: &str,
    _options: &ConversionOptions,
    job_id: Option<&str>,
    job_dir: &Path,
) -> Result<String, String> {
    let input_ext = Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    
    let output_format_clean = format.to_lowercase();
    
    // PDF input requires special handling - Pandoc cannot read PDF files
    // Use LibreOffice for PDF to document conversions, Ghostscript for PDF to image
    if input_ext == "pdf" {
        // PDF to image conversions (png, jpg) - use Ghostscript
        if ["png", "jpg", "jpeg"].contains(&output_format_clean.as_str()) {
            let device = match output_format_clean.as_str() {
                "png" => "png16m",
                "jpg" | "jpeg" => "jpeg",
                _ => "png16m",
            };
            
            let args = vec![
                "-dNOPAUSE".to_string(),
                "-dBATCH".to_string(),
                "-dSAFER".to_string(),
                format!("-sDEVICE={}", device),
                "-r300".to_string(),
                format!("-sOutputFile={}", output),
                input.to_string(),
            ];
            
            return run_command_with_job_id("gs", &args, job_id, None)
                .map(|_| output.to_string());
        }
        
        // PDF -> Office is reconstruction, not rendering - kept as an
        // explicit, separate pipeline (see reconstruction.rs) rather than
        // treated as just another LibreOffice `--convert-to` call. In
        // particular, targets nothing here can actually produce (xlsx/
        // pptx) now fail clearly instead of silently falling back to a
        // mislabeled TXT export the way the old `_ => ("txt", "txt")`
        // catch-all did.
        use crate::reconstruction::{PdfToDocx, PdfToPptx, PdfToXlsx};
        return match output_format_clean.as_str() {
            "docx" | "doc" | "odt" | "rtf" | "html" | "txt" | "epub" => {
                PdfToDocx::convert(input, output, output_format_clean.as_str(), job_dir).map_err(|e| e.to_string())
            }
            // LibreOffice doesn't support md directly - reconstruct to txt.
            "md" => PdfToDocx::convert(input, output, "txt", job_dir).map_err(|e| e.to_string()),
            "xlsx" | "xls" | "ods" => PdfToXlsx::convert(input, output, job_dir).map_err(|e| e.to_string()),
            "pptx" | "ppt" | "odp" => PdfToPptx::convert(input, output, job_dir).map_err(|e| e.to_string()),
            other => Err(format!("Unsupported PDF conversion target: {}", other)),
        };
    }
    
    // Use pandoc for markdown, HTML, and text conversions (non-PDF inputs).
    // "docx" is deliberately excluded: a DOCX input must fall through to
    // the bundled `engines::office` (LibreOffice) path below rather than a
    // separately-installed `pandoc`, matching every other Office format
    // (XLSX, PPTX, ODT, DOC, RTF).
    let pandoc_formats = ["md", "markdown", "html", "htm", "txt", "rst", "epub"];
    
    if pandoc_formats.contains(&input_ext.as_str()) || pandoc_formats.contains(&output_format_clean.as_str()) {
        let args = vec![
            input.to_string(),
            "-o".to_string(),
            output.to_string(),
        ];
        
        return run_command_with_job_id("pandoc", &args, job_id, None)
            .map(|_| output.to_string());
    }
    
    // Office document conversion (docx/doc/odt/rtf -> pdf/docx/doc/odt/
    // rtf/html/txt) - routed through the centralized OfficeEngine, never
    // a soffice command built inline here. See engines::office.
    let office_formats = ["docx", "doc", "odt", "rtf", "pdf", "xlsx", "xls", "pptx", "ppt"];
    if office_formats.contains(&input_ext.as_str()) || office_formats.contains(&output_format_clean.as_str()) {
        let filter = match output_format_clean.as_str() {
            "pdf" => "pdf",
            "docx" => "docx",
            "doc" => "doc",
            "odt" => "odt",
            "rtf" => "rtf",
            "html" => "html",
            "txt" => "txt",
            _ => "pdf",
        };

        return crate::engines::office::convert(input, output, filter, false, job_dir).map_err(|e| e.to_string());
    }

    Err("Unsupported document conversion".to_string())
}

fn convert_spreadsheet(
    input: &str,
    output: &str,
    format: &str,
    job_id: Option<&str>,
    job_dir: &Path,
) -> Result<String, String> {
    let _ = job_id;
    let filter = match format {
        "xlsx" => "xlsx",
        "xls" => "xls",
        "csv" => "csv",
        "ods" => "ods",
        "pdf" => "pdf",
        "html" => "html",
        _ => return Err(format!("Unsupported spreadsheet format: {}", format)),
    };

    crate::engines::office::convert(input, output, filter, false, job_dir).map_err(|e| e.to_string())
}

fn convert_presentation(
    input: &str,
    output: &str,
    format: &str,
    job_id: Option<&str>,
    job_dir: &Path,
) -> Result<String, String> {
    let _ = job_id;
    let filter = match format {
        "pptx" => "pptx",
        "ppt" => "ppt",
        "odp" => "odp",
        "pdf" => "pdf",
        _ => return Err(format!("Unsupported presentation format: {}", format)),
    };

    crate::engines::office::convert(input, output, filter, false, job_dir).map_err(|e| e.to_string())
}

fn convert_vector(
    input: &str,
    output: &str,
    format: &str,
    options: &ConversionOptions,
    job_id: Option<&str>,
) -> Result<String, String> {
    let input_ext = Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    
    // SVG to raster using ImageMagick
    if input_ext == "svg" && ["png", "jpg", "jpeg", "webp"].contains(&format) {
        let mut args: Vec<String> = vec![
            "-background".to_string(),
            "none".to_string(),
        ];
        
        if let (Some(w), Some(h)) = (options.width, options.height) {
            args.push("-density".to_string());
            args.push("300".to_string());
            args.push("-resize".to_string());
            args.push(format!("{}x{}", w, h));
        }
        
        args.push(input.to_string());
        args.push(output.to_string());
        
        return run_command_with_job_id("magick", &args, job_id, None)
            .map(|_| output.to_string());
    }
    
    // EPS/PDF conversions using Ghostscript
    if ["eps", "pdf"].contains(&input_ext.as_str()) || ["eps", "pdf"].contains(&format) {
        let device = match format {
            "png" => "png16m",
            "jpg" | "jpeg" => "jpeg",
            "pdf" => "pdfwrite",
            "eps" => "eps2write",
            _ => return Err(format!("Unsupported vector conversion: {} to {}", input_ext, format)),
        };
        
        let args = vec![
            "-dNOPAUSE".to_string(),
            "-dBATCH".to_string(),
            "-dSAFER".to_string(),
            format!("-sDEVICE={}", device),
            "-r300".to_string(),
            format!("-sOutputFile={}", output),
            input.to_string(),
        ];
        
        return run_command_with_job_id("gs", &args, job_id, None)
            .map(|_| output.to_string());
    }
    
    Err(format!("Unsupported vector conversion: {} to {}", input_ext, format))
}

fn convert_font(
    input: &str,
    output: &str,
    _format: &str,
) -> Result<String, String> {
    // Font conversion is limited - we can use fontforge if available
    // For now, copy with extension change for compatible formats
    std::fs::copy(input, output)
        .map_err(|e| format!("Font conversion failed: {}", e))?;
    
    Ok(output.to_string())
}

fn convert_archive(
    input: &str,
    output: &str,
    format: &str,
    _options: &ConversionOptions,
    job_id: Option<&str>,
) -> Result<String, String> {
    // Create a temporary directory for extraction
    let temp_dir = std::env::temp_dir().join(format!("localconvert_archive_{}", 
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("Failed to create temp directory: {}", e))?;

    let input_ext = Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Step 1: Extract the input archive to temp directory. ZIP is handled
    // natively (see native::archive_zip) - no external tool needed. Every
    // other input format still goes through 7-Zip.
    if input_ext == "zip" {
        crate::native::archive_zip::extract_zip(Path::new(input), &temp_dir).map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            format!("Failed to extract archive: {}", e)
        })?;
    } else {
        let extract_args = vec![
            "x".to_string(),           // Extract with full paths
            input.to_string(),
            format!("-o{}", temp_dir.to_string_lossy()),
            "-y".to_string(),          // Yes to all prompts
        ];

        run_command_with_job_id("7z", &extract_args, job_id, None)
            .map_err(|e| {
                // Clean up temp directory on error
                let _ = std::fs::remove_dir_all(&temp_dir);
                format!("Failed to extract archive: {}", e)
            })?;
    }

    // Step 2: Create the output archive in the target format. ZIP output
    // is also handled natively; everything else still goes through 7-Zip.
    if format == "zip" {
        let result = crate::native::archive_zip::create_zip(&temp_dir, Path::new(output));
        let _ = std::fs::remove_dir_all(&temp_dir);
        result.map_err(|e| format!("Failed to create archive: {}", e))?;
        return Ok(output.to_string());
    }

    let archive_type = match format {
        "7z" => "7z",
        "tar" => "tar",
        "tar.gz" | "tgz" => "tgz",
        "tar.bz2" | "tbz2" => "tbz2",
        _ => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(format!("Unsupported archive output format: {}", format));
        }
    };

    // Build compression command
    // 7-Zip uses: 7z a -tzip output.zip source_dir/*
    let create_args = vec![
        "a".to_string(),           // Add to archive
        format!("-t{}", archive_type),
        output.to_string(),
        format!("{}{}*", temp_dir.to_string_lossy(), std::path::MAIN_SEPARATOR),
        "-y".to_string(),
    ];

    let result = run_command_with_job_id("7z", &create_args, job_id, None);

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&temp_dir);

    result.map_err(|e| format!("Failed to create archive: {}", e))?;

    Ok(output.to_string())
}

fn run_command(cmd: &str, args: &[String]) -> Result<(), String> {
    run_command_with_job_id(cmd, args, None, None)
}

/// Resolve the actual executable path for a tool
fn resolve_tool_path(cmd: &str) -> String {
    get_tool_path(cmd)
}

/// Run FFmpeg with progress tracking
fn run_ffmpeg_with_progress(
    executable: &str,
    args: &[String],
    job_id: Option<&str>,
    total_duration: Option<f64>,
) -> Result<(), String> {
    // Check if already cancelled before starting
    if let Some(jid) = job_id {
        if is_cancelled(jid) {
            return Err("Conversion cancelled".to_string());
        }
    }
    
    // Build FFmpeg args with progress output
    let mut full_args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-progress".to_string(),
        "pipe:1".to_string(),
        "-stats_period".to_string(),
        "0.5".to_string(),
    ];
    full_args.extend(args.iter().cloned());
    
    // Spawn FFmpeg process (hide console window on Windows)
    let mut command = Command::new(executable);
    command.args(&full_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    
    let mut child = command.spawn()
        .map_err(|e| format!("Failed to execute ffmpeg: {}", e))?;
    
    let jid_string = job_id.map(|s| s.to_string());
    let duration = total_duration.unwrap_or(0.0);
    
    // Read progress from stdout
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut current_time = 0.0;
        let mut current_speed = 1.0;
        
        for line in reader.lines() {
            // Check for cancellation
            if let Some(ref jid) = jid_string {
                if is_cancelled(jid) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Conversion cancelled".to_string());
                }
            }
            
            if let Ok(line) = line {
                // Parse progress info
                if let Some(time) = parse_ffmpeg_time(&line) {
                    current_time = time;
                }
                if let Some(speed) = parse_ffmpeg_speed(&line) {
                    current_speed = speed;
                }
                
                // Calculate and emit progress
                if line.starts_with("progress=") {
                    let progress = if duration > 0.0 {
                        ((current_time / duration) * 100.0).min(99.0) as f32
                    } else {
                        0.0
                    };
                    
                    let eta = if current_speed > 0.0 && duration > 0.0 {
                        Some((duration - current_time) / current_speed)
                    } else {
                        None
                    };
                    
                    if let Some(ref jid) = jid_string {
                        emit_progress(jid, progress, current_time, duration, current_speed, eta);
                    }
                }
            }
        }
    }
    
    // Wait for process to finish
    let output = child.wait_with_output()
        .map_err(|e| format!("Failed to wait for ffmpeg: {}", e))?;
    
    if output.status.success() {
        // Emit 100% progress on completion
        if let Some(ref jid) = jid_string {
            emit_progress(jid, 100.0, duration, duration, 1.0, Some(0.0));
        }
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("ffmpeg failed: {}", stderr))
    }
}

fn run_command_with_job_id(cmd: &str, args: &[String], job_id: Option<&str>, duration: Option<f64>) -> Result<(), String> {
    // Resolve the actual executable path
    let executable = resolve_tool_path(cmd);
    
    // For FFmpeg, use the specialized progress-tracking function
    if cmd == "ffmpeg" && job_id.is_some() {
        return run_ffmpeg_with_progress(&executable, args, job_id, duration);
    }
    
    // Check if already cancelled before starting
    if let Some(jid) = job_id {
        if is_cancelled(jid) {
            return Err("Conversion cancelled".to_string());
        }
    }
    
    // Spawn the process (hide console window on Windows)
    let mut command = Command::new(&executable);
    command.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    
    let mut child = command.spawn()
        .map_err(|e| format!("Failed to execute {}: {}", cmd, e))?;
    
    // Register the process if we have a job_id
    if let Some(jid) = job_id {
        // We need to store the child, but we also need to wait on it
        // So we'll use a different approach: poll the process while checking cancellation
        let jid_string = jid.to_string();
        
        loop {
            // Check if cancelled
            if is_cancelled(&jid_string) {
                // Kill the process
                let _ = child.kill();
                let _ = child.wait();
                return Err("Conversion cancelled".to_string());
            }
            
            // Try to get exit status without blocking
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Process has finished
                    if status.success() {
                        return Ok(());
                    } else {
                        let stderr = child.stderr.take()
                            .map(|mut s| {
                                let mut buf = String::new();
                                use std::io::Read;
                                let _ = s.read_to_string(&mut buf);
                                buf
                            })
                            .unwrap_or_default();
                        let stdout = child.stdout.take()
                            .map(|mut s| {
                                let mut buf = String::new();
                                use std::io::Read;
                                let _ = s.read_to_string(&mut buf);
                                buf
                            })
                            .unwrap_or_default();
                        return Err(format!(
                            "{} failed:\nStderr: {}\nStdout: {}",
                            cmd, stderr, stdout
                        ));
                    }
                }
                Ok(None) => {
                    // Process still running, sleep briefly then check again
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(format!("Failed to check process status: {}", e));
                }
            }
        }
    } else {
        // No job_id, just wait for completion (original behavior)
        let output = child.wait_with_output()
            .map_err(|e| format!("Failed to wait for {}: {}", cmd, e))?;
        
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(format!(
                "{} failed:\nStderr: {}\nStdout: {}",
                cmd, stderr, stdout
            ))
        }
    }
}

// Additional conversion helpers

/// Output format for the native pipeline is taken from `output`'s
/// extension. `None` means the extension isn't one of the native formats
/// (JPEG/PNG/WebP/BMP/GIF/TIFF), so the caller should fall back to
/// ImageMagick.
fn native_target_format(output: &str) -> Option<crate::native::image::NativeImageFormat> {
    let ext = Path::new(output).extension().and_then(|e| e.to_str()).unwrap_or("");
    crate::native::image::NativeImageFormat::from_extension(ext)
}

#[allow(dead_code)]
pub fn resize_image_helper(
    input: &str,
    output: &str,
    options: &ImageOptions,
) -> Result<String, String> {
    use crate::native::image as native_image;

    let w = options.width.ok_or("Width required")?;
    let h = options.height.ok_or("Height required")?;

    if let Some(target) = native_target_format(output) {
        let quality = options.quality.map(|q| q.clamp(1, 100) as u8);
        native_image::resize_file(Path::new(input), Path::new(output), target, w, h, quality)
            .map_err(|e| e.to_string())?;
        return Ok(output.to_string());
    }

    let mut args = vec![input.to_string()];
    args.push("-resize".to_string());
    args.push(format!("{}x{}!", w, h));
    if let Some(q) = options.quality {
        args.push("-quality".to_string());
        args.push(q.to_string());
    }
    args.push(output.to_string());

    run_command("magick", &args)?;
    Ok(output.to_string())
}

#[allow(dead_code)]
pub fn compress_image_helper(
    input: &str,
    output: &str,
    quality: u32,
) -> Result<String, String> {
    use crate::native::image as native_image;

    if let Some(target) = native_target_format(output) {
        native_image::compress_file(Path::new(input), Path::new(output), target, quality.clamp(1, 100) as u8)
            .map_err(|e| e.to_string())?;
        return Ok(output.to_string());
    }

    let args = vec![
        input.to_string(),
        "-quality".to_string(),
        quality.to_string(),
        "-strip".to_string(),
        output.to_string(),
    ];

    run_command("magick", &args)?;
    Ok(output.to_string())
}

#[allow(dead_code)]
pub fn crop_image_helper(
    input: &str,
    output: &str,
    options: &ImageOptions,
) -> Result<String, String> {
    use crate::native::image as native_image;

    let x = options.crop_x.unwrap_or(0);
    let y = options.crop_y.unwrap_or(0);
    let w = options.crop_width.ok_or("Crop width required")?;
    let h = options.crop_height.ok_or("Crop height required")?;

    if let Some(target) = native_target_format(output) {
        native_image::crop_file(Path::new(input), Path::new(output), target, x, y, w, h)
            .map_err(|e| e.to_string())?;
        return Ok(output.to_string());
    }

    let args = vec![
        input.to_string(),
        "-crop".to_string(),
        format!("{}x{}+{}+{}", w, h, x, y),
        "+repage".to_string(),
        output.to_string(),
    ];

    run_command("magick", &args)?;
    Ok(output.to_string())
}

#[allow(dead_code)]
pub fn rotate_image_helper(
    input: &str,
    output: &str,
    degrees: i32,
) -> Result<String, String> {
    use crate::native::image as native_image;

    if let Some(target) = native_target_format(output) {
        native_image::rotate_file(Path::new(input), Path::new(output), target, degrees)
            .map_err(|e| e.to_string())?;
        return Ok(output.to_string());
    }

    let args = vec![
        input.to_string(),
        "-rotate".to_string(),
        degrees.to_string(),
        output.to_string(),
    ];

    run_command("magick", &args)?;
    Ok(output.to_string())
}

pub fn trim_video_helper(
    input: &str,
    output: &str,
    options: &VideoOptions,
) -> Result<String, String> {
    let mut args = vec!["-i".to_string(), input.to_string()];
    
    if let Some(ref start) = options.start_time {
        args.push("-ss".to_string());
        args.push(start.clone());
    }
    
    if let Some(ref end) = options.end_time {
        args.push("-to".to_string());
        args.push(end.clone());
    }
    
    args.push("-c".to_string());
    args.push("copy".to_string());
    args.push("-y".to_string());
    args.push(output.to_string());
    
    run_command("ffmpeg", &args)?;
    Ok(output.to_string())
}

#[allow(dead_code)]
pub fn extract_audio_helper(
    input: &str,
    output: &str,
    format: &str,
) -> Result<String, String> {
    let codec = match format {
        "mp3" => "libmp3lame",
        "aac" | "m4a" => "aac",
        "wav" => "pcm_s16le",
        "flac" => "flac",
        "ogg" => "libvorbis",
        _ => "copy",
    };
    
    let args = vec![
        "-i".to_string(),
        input.to_string(),
        "-vn".to_string(),
        "-c:a".to_string(),
        codec.to_string(),
        "-y".to_string(),
        output.to_string(),
    ];
    
    run_command("ffmpeg", &args)?;
    Ok(output.to_string())
}

#[allow(dead_code)]
pub fn compress_video_helper(
    input: &str,
    output: &str,
    options: &VideoOptions,
) -> Result<String, String> {
    let crf = options.crf.unwrap_or(28);
    let use_gpu = options.use_gpu.unwrap_or(false);
    
    let mut args = vec![
        "-i".to_string(),
        input.to_string(),
    ];
    
    // Check for GPU encoding
    let gpu_encoder = if use_gpu {
        let gpu_info = detect_gpu_encoders();
        get_gpu_encoder_for_format("mp4", &gpu_info, options.gpu_encoder.as_deref())
    } else {
        None
    };
    
    if let Some(ref encoder) = gpu_encoder {
        args.push("-c:v".to_string());
        args.push(encoder.clone());
        
        // Set quality based on encoder type
        match encoder.as_str() {
            "h264_nvenc" | "hevc_nvenc" | "av1_nvenc" => {
                args.push("-preset".to_string());
                args.push("p4".to_string());
                args.push("-rc".to_string());
                args.push("vbr".to_string());
                args.push("-cq".to_string());
                args.push(crf.to_string());
            }
            "h264_amf" | "hevc_amf" | "av1_amf" => {
                args.push("-quality".to_string());
                args.push("balanced".to_string());
                args.push("-rc".to_string());
                args.push("vbr_latency".to_string());
                args.push("-qp_i".to_string());
                args.push(crf.to_string());
                args.push("-qp_p".to_string());
                args.push(crf.to_string());
            }
            "h264_qsv" | "hevc_qsv" | "av1_qsv" => {
                args.push("-preset".to_string());
                args.push("medium".to_string());
                args.push("-global_quality".to_string());
                args.push(crf.to_string());
            }
            _ => {
                // Fallback - use bitrate if available
                if let Some(ref bitrate) = options.bitrate {
                    args.push("-b:v".to_string());
                    args.push(bitrate.clone());
                }
            }
        }
    } else {
        // Software encoding
        args.push("-c:v".to_string());
        args.push("libx264".to_string());
        args.push("-crf".to_string());
        args.push(crf.to_string());
        args.push("-preset".to_string());
        args.push("medium".to_string());
    }
    
    if let Some(ref bitrate) = options.audio_bitrate {
        args.push("-b:a".to_string());
        args.push(bitrate.clone());
    }
    
    args.push("-y".to_string());
    args.push(output.to_string());
    
    run_command("ffmpeg", &args)?;
    Ok(output.to_string())
}

#[cfg(test)]
mod document_routing_tests {
    use super::*;

    // Regression test for a routing bug where "docx" was listed in
    // `pandoc_formats`, so every DOCX conversion silently depended on a
    // separately-installed system `pandoc` instead of the bundled
    // `engines::office` (LibreOffice) engine every other Office format
    // uses. This assertion holds regardless of which engine tier is
    // actually available in the environment running the test: either the
    // bundled/system Office engine reports its own structured error, or
    // (if truly unavailable) `OFFICE_ENGINE_NOT_AVAILABLE` - never a raw
    // "Failed to execute pandoc" string.
    #[test]
    fn docx_to_pdf_never_falls_back_to_pandoc() {
        let job_dir = std::env::temp_dir().join(format!(
            "localconvert_docx_routing_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&job_dir).unwrap();

        let options = ConversionOptions::default();
        let result = convert_document(
            "input.docx",
            job_dir.join("output.pdf").to_str().unwrap(),
            "pdf",
            &options,
            None,
            &job_dir,
        );

        let _ = std::fs::remove_dir_all(&job_dir);

        let err = result.unwrap_err();
        assert!(
            !err.to_lowercase().contains("pandoc"),
            "DOCX -> PDF must route through the bundled Office engine, not system pandoc: {}",
            err
        );
    }
}
