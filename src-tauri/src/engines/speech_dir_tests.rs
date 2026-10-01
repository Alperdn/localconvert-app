//! Engine directory states end to end (manifest -> load -> report -> capability),
//! using copies of the real staged engine so they exercise the real code path.
//! Skips (returns early) when no engine is staged on this machine.

use super::engine_id::EngineId;
use super::resolver;
use super::speech::{report_from_load, SpeechEngine, SpeechEngineStatus, WhisperCppEngine};
use super::speech_error::{SpeechError, SpeechErrorCode};
use super::speech_manifest;
use crate::capabilities::{speech_capability_state, CapabilityState};
use std::path::PathBuf;

fn staged() -> Option<PathBuf> {
    let d = resolver::bundled_engine_dir(EngineId::Speech)?;
    d.join("manifest.json").is_file().then_some(d)
}

/// Copy of the staged engine (manifest + bin/, model optional).
fn copy_engine(with_model: bool) -> Option<PathBuf> {
    let src = staged()?;
    let dst = std::env::temp_dir().join(format!("meb_speech_copy_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dst.join("bin")).ok()?;
    std::fs::create_dir_all(dst.join("models")).ok()?;
    std::fs::copy(src.join("manifest.json"), dst.join("manifest.json")).ok()?;
    for e in std::fs::read_dir(src.join("bin")).ok()?.flatten() {
        std::fs::copy(e.path(), dst.join("bin").join(e.file_name())).ok()?;
    }
    if with_model {
        let m = speech_manifest::load_manifest(&src).ok()?;
        let rel = m.model.file_relative_path;
        std::fs::copy(src.join(&rel), dst.join(&rel)).ok()?;
    }
    Some(dst)
}

#[test]
fn complete_copy_loads_and_declares_turkish_and_timestamps() {
    let Some(dir) = copy_engine(true) else { return };
    let engine = WhisperCppEngine::load_from_dir(&dir).expect("valid engine copy must load");
    let info = engine.info();
    assert!(info.languages.contains(&"tr".to_string()));
    assert!(info.supports_timestamps);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_model_reports_model_missing_and_maps_to_bilesen_eksik() {
    let Some(dir) = copy_engine(false) else { return };
    let err = WhisperCppEngine::load_from_dir(&dir).err().expect("must not load without the model");
    assert_eq!(err.code, SpeechErrorCode::SpeechModelMissing);
    let report = report_from_load(Err(err));
    assert_eq!(report.status, SpeechEngineStatus::ModelMissing);
    assert_eq!(speech_capability_state(report.status), CapabilityState::EngineMissing);
    assert!(report.message.contains("model"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_engine_dir_reports_engine_missing() {
    let nowhere = std::env::temp_dir().join(format!("meb_speech_absent_{}", uuid::Uuid::new_v4()));
    let err = WhisperCppEngine::load_from_dir(&nowhere).err().unwrap();
    assert_eq!(err.code, SpeechErrorCode::SpeechEngineMissing);
    let report = report_from_load(Err(err));
    assert_eq!(report.status, SpeechEngineStatus::EngineMissing);
    assert!(report.engine.is_none() && report.model.is_none() && report.languages.is_empty());
}

#[test]
fn a_tampered_binary_is_invalid_not_available() {
    let Some(dir) = copy_engine(true) else { return };
    let dll = dir.join("bin").join("whisper.dll");
    let mut bytes = std::fs::read(&dll).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&dll, bytes).unwrap();
    let err = WhisperCppEngine::load_from_dir(&dir).err().expect("tampered engine must not load");
    assert_eq!(err.code, SpeechErrorCode::SpeechEngineInvalid);
    assert_eq!(report_from_load(Err(err)).status, SpeechEngineStatus::EngineInvalid);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_tampered_model_is_invalid_not_available() {
    let Some(dir) = copy_engine(true) else { return };
    let m = speech_manifest::load_manifest(&dir).unwrap();
    let model = dir.join(&m.model.file_relative_path);
    // flip one byte in the middle of the (190 MB) model
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&model).unwrap();
        f.seek(SeekFrom::Start(90_000_000)).unwrap();
        f.write_all(&[0xAB]).unwrap();
    }
    let err = WhisperCppEngine::load_from_dir(&dir).err().expect("tampered model must not load");
    assert_eq!(err.code, SpeechErrorCode::SpeechEngineInvalid);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cpu_unsupported_maps_to_henuz_desteklenmiyor_not_missing() {
    let report = report_from_load(Err(SpeechError::new(SpeechErrorCode::SpeechCpuUnsupported)));
    assert_eq!(report.status, SpeechEngineStatus::CpuUnsupported);
    assert_eq!(speech_capability_state(report.status), CapabilityState::NotImplemented);
    assert!(report.message.contains("desteklenmiyor"));
}

#[test]
fn report_messages_are_turkish_and_never_expose_paths() {
    for code in [
        SpeechErrorCode::SpeechEngineMissing,
        SpeechErrorCode::SpeechModelMissing,
        SpeechErrorCode::SpeechEngineInvalid,
        SpeechErrorCode::SpeechCpuUnsupported,
        SpeechErrorCode::SpeechRuntimeMissing,
    ] {
        let r = report_from_load(Err(SpeechError::new(code).with_detail("C:\\secret\\whisper-cli.exe")));
        assert!(!r.message.contains('\\') && !r.message.to_lowercase().contains(".exe"), "{:?}", code);
    }
}

#[test]
fn missing_app_local_runtime_dll_reports_runtime_missing_and_bilesen_eksik() {
    let Some(dir) = copy_engine(true) else { return };
    std::fs::remove_file(dir.join("bin").join("vcomp140.dll")).unwrap();
    let err = WhisperCppEngine::load_from_dir(&dir).err().expect("must not load without its runtime");
    assert_eq!(err.code, SpeechErrorCode::SpeechRuntimeMissing);
    let report = report_from_load(Err(err));
    assert_eq!(report.status, SpeechEngineStatus::RuntimeMissing);
    assert_eq!(speech_capability_state(report.status), CapabilityState::EngineMissing);
    // never tells the user to go and install anything from Microsoft
    let m = report.message.to_lowercase();
    assert!(!m.contains("visual c++") && !m.contains("redistributable") && m.contains("yeniden yükleyin"), "{}", report.message);
    let _ = std::fs::remove_dir_all(dir);
}

/// The Windows loader must prefer the app-local runtime over any copy in System32.
/// Proof by poisoning: with a garbage app-local `vcruntime140.dll` the engine cannot
/// start; that is only possible if it is loading the app-local file, not a system one.
#[test]
fn engine_loads_its_app_local_runtime_not_the_system_copy() {
    let Some(dir) = copy_engine(false) else { return };
    let exe = dir.join("bin").join("whisper-cli.exe");
    let good = std::process::Command::new(&exe).arg("--version").output().expect("spawn");
    assert!(good.status.success(), "engine must start with its app-local runtime");
    std::fs::write(dir.join("bin").join("vcruntime140.dll"), vec![0u8; 4096]).unwrap();
    let bad = std::process::Command::new(&exe).arg("--version").output().expect("spawn");
    assert!(!bad.status.success(), "a poisoned app-local runtime must break the engine (proves it is the one being loaded)");
    let _ = std::fs::remove_dir_all(dir);
}
