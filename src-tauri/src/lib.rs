mod capabilities;
mod commands;
mod converter;
mod engines;
mod native;
mod pdf_text_editor;
mod reconstruction;
mod security;
mod tools;
mod types;

use tauri::Manager;

// Phase 1 - Secure Desktop Foundation:
//
// Plugins intentionally NOT registered here, and why:
//
//   tauri_plugin_shell    - Never imported by the frontend (verified: no
//                            `@tauri-apps/plugin-shell` usage anywhere in
//                            src/). All external tool execution already goes
//                            through Rust `std::process::Command` with argument
//                            arrays. Registering this plugin would only grant
//                            the webview a capability it doesn't use and that
//                            we explicitly do not want it to have.
//   tauri_plugin_updater  - Implements automatic update checking against a
//                            GitHub release endpoint. This app must not make
//                            network requests during normal use and must
//                            never silently download/install/relaunch. Removed
//                            entirely rather than left registered-but-unused,
//                            so there is no code path that can call it.
//   tauri_plugin_process   - Was only used for `relaunch()` after an auto
//                            update. No longer needed with the updater gone.
//   tauri_plugin_opener    - Not imported by the frontend. Any future
//                            "open external link" need should go through a
//                            dedicated, validated Rust command instead (see
//                            `commands::open_file_location` / `open_folder`,
//                            which already use direct OS calls, not this
//                            plugin).
//   tauri_plugin_notification - Also grep-verified unused: nothing in
//                            src/ calls `@tauri-apps/plugin-notification`.
//                            Completion feedback is implemented via a Web
//                            Audio API chime (`utils/sounds.ts`), not OS
//                            notifications. Registering an unused plugin
//                            just to have it "in case" is exactly the kind
//                            of standing, unused capability this phase is
//                            about removing - if OS notifications become a
//                            real feature later, re-add the plugin then,
//                            deliberately, alongside the code that uses it.
//
// Kept: dialog (native file pickers), fs (used narrowly by the PDF editor,
// scoped down - see security::fs_scope).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            // Initialize app data directory
            let app_data_dir = app.path().app_data_dir().expect("Failed to get app data dir");
            std::fs::create_dir_all(&app_data_dir).ok();
            
            // Create binaries directory for conversion tools
            let binaries_dir = app_data_dir.join("binaries");
            std::fs::create_dir_all(&binaries_dir).ok();
            
            // Set app handle for progress events
            converter::set_app_handle(app.handle().clone());

            // Best-effort cleanup of any job temp directories left behind by
            // a previous run that crashed or was force-killed before its
            // Drop handler could run (see security::temp::JobTempDir).
            security::temp::cleanup_stale_job_dirs();
            
            Ok(())
        })
        // Phase 1 - Secure Desktop Foundation:
        //
        // Only commands the frontend actually calls are registered here
        // (grep-verified against every `invoke(...)` call site in `src/`).
        // Registering a command that nothing calls is unnecessary IPC
        // attack surface - any future XSS bug in the webview would be
        // able to call it directly, regardless of whether any UI exposes
        // it. The commands below that are NOT registered still exist as
        // ordinary functions in `commands.rs` (nothing was deleted) -
        // they're either superseded by the pure-Rust lopdf-based PDF text
        // editing that IS registered (`apply_pdf_text_edits`,
        // `get_pdf_info` before `get_pdf_text_blocks`/
        // `edit_pdf_text_lopdf` existed), or scaffolding for a
        // conversion-tools panel (PDF merge/split/compress/rotate/
        // watermark, image resize/compress/crop/rotate, audio/video
        // trim-adjacent operations, archive extract/create) that hasn't
        // been wired into any UI yet. Re-register any of them the moment
        // a real UI calls them - and wire `security::path_validation`
        // into it first (see the commands that already do, e.g.
        // `merge_pdfs`, `split_pdf`, `compress_pdf`, for the pattern).
        .invoke_handler(tauri::generate_handler![
            commands::check_tools,
            commands::detect_gpu,
            commands::convert_file,
            commands::cancel_conversion,
            commands::get_file_info,
            commands::trim_video,
            commands::get_default_output_dir,
            commands::get_image_preview,
            commands::open_file_location,
            commands::get_file_size_estimate,
            commands::get_video_duration,
            commands::get_video_thumbnail,
            commands::get_video_metadata,
            commands::get_hardware_encoders,
            commands::register_context_menu,
            commands::unregister_context_menu,
            commands::get_startup_files,
            commands::get_pdf_form_fields,
            commands::fill_pdf_form_fields,
            // Pure Rust PDF text editing (lopdf - MIT licensed)
            commands::get_pdf_text_blocks,
            commands::edit_pdf_text_lopdf,
            // Phase 1 - Secure Desktop Foundation
            commands::authorize_fs_path,
            commands::system_status,
            // V1 dependency architecture - backend-computed capability
            // model (see capabilities.rs)
            capabilities::get_capabilities,
            // Step 4 - Office Engine self-check (structured status only,
            // never raw filesystem paths - see engines::office_manifest).
            engines::office_manifest::office_engine_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
