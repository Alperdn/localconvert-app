# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Summary

LocalConvert is a privacy-focused, open-source (MIT licensed) desktop file converter. Built with **Tauri v2** (Rust backend) + **React 19** (TypeScript frontend). Cross-platform: Windows, macOS, and Linux. All file conversions run locally via external CLI tools (FFmpeg, ImageMagick, LibreOffice, Pandoc, Ghostscript, Tesseract, 7-Zip). Supports 100+ formats across video, audio, image, document, PDF, archive, and more.

Backend-specific guidance, including the Tauri v1→v2 migration rules and the auto-updater removal rationale, lives in `src-tauri/CLAUDE.md` (loads only when working under `src-tauri/`).

## Architecture

### IPC Flow

Frontend communicates with Rust via Tauri's `invoke()` / event system:
- **Frontend → Backend:** `invoke<T>("command_name", { params })` from `@tauri-apps/api/core` (async)
- **Backend → Frontend:** `app.emit("event_name", payload)` (used for conversion progress updates)

### Conversion Routing

File extension → category → external tool:
- Video/Audio → FFmpeg
- Image → ImageMagick
- Document → LibreOffice or Pandoc
- PDF operations → Ghostscript (compression, merge, split) or lopdf (text editing)
- Archives → 7-Zip
- OCR → Tesseract

### Platform-Specific Features

- **Windows context menu integration** — Gate behind `#[cfg(target_os = "windows")]`. Do NOT compile on macOS/Linux.
- **Windows registry access (`winreg` crate)** — Gate behind `#[cfg(target_os = "windows")]`.
- **Ghostscript binary name** — `gswin64c.exe` on Windows, `gs` on macOS/Linux.
- **LibreOffice binary name** — `soffice.exe` on Windows, `soffice` on macOS/Linux.
- **GPU detection** — NVENC works on all platforms, QSV on Windows/Linux, VCE/VCN on Windows/Linux. Gate macOS GPU detection appropriately (VideoToolbox for Apple Silicon).

## Conventions

- TypeScript strict mode is enabled (`noUnusedLocals`, `noUnusedParameters`)
- Path alias: `@/*` maps to `src/*`
- Toast notifications (react-hot-toast) for user-facing feedback

## License

MIT — Open source. A separate closed-source Pro version may be offered in the future with additional features.
