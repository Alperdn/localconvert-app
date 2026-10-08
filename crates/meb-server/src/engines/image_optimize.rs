//! Image optimization: the same image, same format, fewer bytes.
//!
//! In-process, over `meb_core::image` - the same decode bounds, the same
//! encoders and the same cancellation hooks the conversion pipeline uses.
//! Nothing is spawned, so this runner is registered UNCONDITIONALLY and
//! `image_optimize` is available on every machine.
//!
//! Two promises define the operation, and both are enforced here rather
//! than described in a comment:
//!
//! - **The output is never larger than the input.** Re-encoding an already
//!   well-compressed file usually makes it bigger, and an "optimizer" that
//!   hands back a larger file has failed at the one thing it was asked to
//!   do. When the re-encode does not win, the original bytes are published
//!   unchanged and the job reports a 0% saving (the result carries both
//!   sizes, so the client can say so).
//! - **The level means what it says.** What `lossless`, `balanced` and
//!   `maximum` can honestly mean depends on the format's encoder, so the
//!   accepted pairs come from `spec::image_optimization_levels` - one
//!   definition, which `JobSpec::from_request` validates against and
//!   `GET /capabilities` publishes. A pair this server cannot deliver is
//!   refused before a job exists; it is never silently served as a
//!   different level.
//!
//! Where the numbers come from: JPEG quality 80 is the usual "visually
//! indistinguishable at normal viewing size" point and 60 is the usual
//! "clearly smaller, visibly softer on detail" one. PNG optimization is
//! maximum deflate effort with adaptive per-row filtering, which is
//! lossless by construction. WebP is encoded by the lossless WebP encoder.

use crate::runner::{ConversionRunner, JobControl, RunError, RunRequest};
use crate::spec::{ImageOptimizeSpec, JobKind, JobSpec, OptimizeLevel};
use meb_core::image::{
    self, NativeConvertOptions, NativeImageFormat, PipelineHooks, PipelineStage,
};
use std::path::Path;

/// JPEG quality for the two lossy levels.
const BALANCED_JPEG_QUALITY: u8 = 80;
const MAXIMUM_JPEG_QUALITY: u8 = 60;

/// Deflate effort for a lossless PNG re-encode. 9 selects the encoder's
/// `Best` compression type; the filtering is adaptive either way.
const PNG_MAX_EFFORT: u8 = 9;

pub struct ImageOptimizeRunner;

impl ImageOptimizeRunner {
    /// The job kinds this runner backs.
    pub const KINDS: [JobKind; 1] = [JobKind::ImageOptimize];
}

impl ConversionRunner for ImageOptimizeRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        let spec = match request.spec {
            JobSpec::ImageOptimize(spec) => spec,
            _ => return Err(RunError::wrong_kind("ImageOptimizeRunner")),
        };
        optimize(spec, request.input(), request.output, control)
    }
}

fn optimize(
    spec: &ImageOptimizeSpec,
    input: &Path,
    output: &Path,
    control: &JobControl<'_>,
) -> Result<(), RunError> {
    let Some(options) = encode_options(spec.format, spec.level) else {
        // Unreachable for a validated spec: every pair the matrix accepts
        // has settings here, which `every_accepted_pair_has_encoder_
        // settings` keeps true. Reaching it would mean the two definitions
        // had drifted, which is a server bug and not the user's problem.
        return Err(RunError::Failed {
            code: "INTERNAL_ERROR",
            detail: format!(
                "no encoder settings for {:?} at level {}",
                spec.format,
                spec.level.wire()
            ),
        });
    };

    let is_cancelled = || control.is_cancelled();
    // The pipeline's real stage boundaries, with the last tenth left for
    // the size comparison and the publish below.
    let on_stage = |stage: PipelineStage| {
        control.report_progress(match stage {
            PipelineStage::Decoded => 25,
            PipelineStage::Transformed => 45,
            PipelineStage::Encoded => 70,
            PipelineStage::Written => 90,
        })
    };
    let hooks = PipelineHooks {
        is_cancelled: &is_cancelled,
        on_stage: &on_stage,
    };
    image::convert_file_with_hooks(input, output, spec.format, &options, &hooks).map_err(|e| {
        if e.kind() == image::ImageNativeErrorKind::Cancelled {
            RunError::Cancelled
        } else {
            RunError::Failed {
                code: e.code(),
                detail: format!("{e:?}"),
            }
        }
    })?;

    if !re_encode_won(input, output) {
        // The original is already at least as small as anything this
        // pipeline can produce. Publishing it unchanged is the honest
        // result: the user keeps their file, and the two sizes in the job
        // result say that nothing was saved.
        tracing::info!(
            format = ?spec.format,
            level = spec.level.wire(),
            "optimization did not shrink the file; publishing the original"
        );
        std::fs::copy(input, output).map_err(|e| RunError::Failed {
            code: "IMAGE_ENCODE_FAILED",
            detail: format!("the original could not be published unchanged: {e}"),
        })?;
    }
    control.report_progress(100);
    Ok(())
}

/// The encoder settings for one format at one level, or `None` for a pair
/// this server does not offer (see `spec::image_optimization_levels`).
fn encode_options(
    format: NativeImageFormat,
    level: OptimizeLevel,
) -> Option<NativeConvertOptions> {
    let quality = |q: u8| {
        Some(NativeConvertOptions {
            quality: Some(q),
            ..Default::default()
        })
    };
    match (format, level) {
        (NativeImageFormat::Jpeg, OptimizeLevel::Balanced) => quality(BALANCED_JPEG_QUALITY),
        (NativeImageFormat::Jpeg, OptimizeLevel::Maximum) => quality(MAXIMUM_JPEG_QUALITY),
        (NativeImageFormat::Png, OptimizeLevel::Lossless) => Some(NativeConvertOptions {
            png_compression_level: Some(PNG_MAX_EFFORT),
            ..Default::default()
        }),
        // The lossless WebP encoder takes no settings; quality would be
        // ignored, so none is passed.
        (NativeImageFormat::WebP, OptimizeLevel::Lossless) => {
            Some(NativeConvertOptions::default())
        }
        _ => None,
    }
}

/// Whether the re-encoded file is actually smaller than the original.
/// An unreadable size on either side counts as "no", so the original is
/// kept whenever the comparison cannot be made.
fn re_encode_won(input: &Path, output: &Path) -> bool {
    let size_of = |path: &Path| std::fs::metadata(path).ok().map(|m| m.len());
    match (size_of(input), size_of(output)) {
        (Some(original), Some(produced)) => produced > 0 && produced < original,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{image_optimization_levels, CreateJobRequest};
    use meb_core::format::SourceFormat;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    struct Workspace {
        dir: PathBuf,
    }

    impl Workspace {
        fn new() -> Workspace {
            let dir = std::env::temp_dir()
                .join(format!("meb_optimize_{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            Workspace { dir }
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A photograph-like image: smooth gradients plus noise, so that lossy
    /// encoding has something to actually discard. A flat colour field
    /// would compress to nothing at every quality and prove nothing.
    fn photo(width: u32, height: u32) -> ::image::DynamicImage {
        use ::image::{Rgb, RgbImage};
        let mut buffer: RgbImage = RgbImage::new(width, height);
        for (x, y, pixel) in buffer.enumerate_pixels_mut() {
            // A cheap deterministic hash, for noise without a dependency.
            let noise = ((x * 7919 + y * 104_729) % 97) as u8;
            *pixel = Rgb([
                (x * 255 / width.max(1)) as u8 ^ noise,
                (y * 255 / height.max(1)) as u8,
                noise.wrapping_mul(3),
            ]);
        }
        ::image::DynamicImage::ImageRgb8(buffer)
    }

    fn write_image(path: &Path, format: NativeImageFormat, quality: Option<u8>) {
        let img = photo(160, 120);
        let bytes = {
            let mut out = Vec::new();
            let mut cursor = std::io::Cursor::new(&mut out);
            match format {
                NativeImageFormat::Jpeg => {
                    let q = quality.unwrap_or(95);
                    let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(
                        &mut cursor,
                        q,
                    );
                    img.write_with_encoder(encoder).unwrap();
                }
                NativeImageFormat::Png => {
                    // Written with the FAST encoder settings, so that the
                    // maximum-effort re-encode has room to win.
                    let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
                        &mut cursor,
                        ::image::codecs::png::CompressionType::Fast,
                        ::image::codecs::png::FilterType::NoFilter,
                    );
                    img.write_with_encoder(encoder).unwrap();
                }
                NativeImageFormat::WebP => {
                    let encoder = ::image::codecs::webp::WebPEncoder::new_lossless(&mut cursor);
                    img.write_with_encoder(encoder).unwrap();
                }
                other => panic!("no fixture writer for {other:?}"),
            }
            out
        };
        std::fs::write(path, bytes).unwrap();
    }

    fn spec_for(format: NativeImageFormat, level: OptimizeLevel) -> JobSpec {
        let request = CreateJobRequest {
            kind: JobKind::ImageOptimize.wire().to_string(),
            file_id: Some("f".to_string()),
            file_ids: None,
            output_format: None,
            options: Some(serde_json::json!({ "level": level.wire() })),
        };
        JobSpec::from_request(
            JobKind::ImageOptimize,
            &request,
            &[SourceFormat::Image(format)],
        )
        .unwrap_or_else(|e| panic!("{format:?}/{} was refused: {}", level.wire(), e.code))
    }

    /// Runs one optimization and returns (original size, produced size).
    fn run_optimize(
        dir: &Path,
        format: NativeImageFormat,
        level: OptimizeLevel,
        source_quality: Option<u8>,
    ) -> (u64, u64, JobSpec) {
        let input = dir.join(format!("source.{}", format.canonical_extension()));
        write_image(&input, format, source_quality);
        let output = dir.join(format!("result.{}", format.canonical_extension()));
        let spec = spec_for(format, level);

        let inputs = vec![input.clone()];
        let request = RunRequest {
            inputs: &inputs,
            output: &output,
            spec: &spec,
        };
        let cancel = AtomicBool::new(false);
        let progress = |_: u8| {};
        let control = JobControl {
            cancel: &cancel,
            progress: &progress,
        };
        ImageOptimizeRunner
            .run(&request, &control)
            .unwrap_or_else(|e| panic!("{format:?}/{} failed: {e:?}", level.wire()));

        let original = std::fs::metadata(&input).unwrap().len();
        let produced = std::fs::metadata(&output).unwrap().len();
        (original, produced, spec)
    }

    #[test]
    fn every_accepted_pair_has_encoder_settings() {
        // The matrix in `spec` and the settings here are two halves of one
        // decision. If a format gains a level there and not here, the job
        // would be accepted and then fail with an internal error.
        for format in [
            NativeImageFormat::Jpeg,
            NativeImageFormat::Png,
            NativeImageFormat::WebP,
            NativeImageFormat::Bmp,
            NativeImageFormat::Gif,
            NativeImageFormat::Tiff,
        ] {
            for level in OptimizeLevel::ALL {
                let accepted = image_optimization_levels(format).contains(&level);
                assert_eq!(
                    encode_options(format, level).is_some(),
                    accepted,
                    "{format:?} at {} is accepted={accepted} but settings disagree",
                    level.wire()
                );
            }
        }
    }

    #[test]
    fn the_lossy_levels_differ_and_the_more_aggressive_one_is_smaller() {
        let workspace = Workspace::new();
        // A high-quality JPEG in, so both levels have room to shrink it.
        let (original, balanced, _) = run_optimize(
            &workspace.dir,
            NativeImageFormat::Jpeg,
            OptimizeLevel::Balanced,
            Some(95),
        );
        let (_, maximum, _) = run_optimize(
            &workspace.dir,
            NativeImageFormat::Jpeg,
            OptimizeLevel::Maximum,
            Some(95),
        );
        assert!(
            balanced < original,
            "balanced did not shrink the file: {balanced} vs {original}"
        );
        assert!(
            maximum < balanced,
            "maximum is not smaller than balanced: {maximum} vs {balanced}"
        );
    }

    #[test]
    fn a_lossless_level_shrinks_the_file_without_changing_a_pixel() {
        let workspace = Workspace::new();
        let input = workspace.dir.join("source.png");
        write_image(&input, NativeImageFormat::Png, None);
        let (original, produced, spec) = run_optimize(
            &workspace.dir,
            NativeImageFormat::Png,
            OptimizeLevel::Lossless,
            None,
        );
        assert!(
            produced < original,
            "the lossless re-encode did not shrink the file: {produced} vs {original}"
        );
        // Lossless means exactly that: the decoded pixels are identical.
        let before = ::image::open(workspace.dir.join("source.png")).unwrap();
        let after = ::image::open(workspace.dir.join("result.png")).unwrap();
        assert_eq!(before.to_rgba8(), after.to_rgba8(), "pixels changed");
        // And it is still a PNG, which is what the job promised.
        assert!(spec.validate_output(&workspace.dir.join("result.png")).is_ok());
    }

    #[test]
    fn webp_is_optimized_losslessly_and_stays_a_webp() {
        let workspace = Workspace::new();
        let (_, _, spec) = run_optimize(
            &workspace.dir,
            NativeImageFormat::WebP,
            OptimizeLevel::Lossless,
            None,
        );
        let output = workspace.dir.join("result.webp");
        assert!(spec.validate_output(&output).is_ok());
        let before = ::image::open(workspace.dir.join("source.webp")).unwrap();
        let after = ::image::open(&output).unwrap();
        assert_eq!(before.to_rgba8(), after.to_rgba8(), "pixels changed");
    }

    #[test]
    fn a_file_that_cannot_be_shrunk_comes_back_unchanged_rather_than_larger() {
        // The case that makes a naive optimizer embarrassing: an input that
        // is already smaller than anything the pipeline can produce. Here
        // it is a JPEG written at quality 30 and optimized at `balanced`
        // (quality 80), which can only grow it.
        let workspace = Workspace::new();
        let input = workspace.dir.join("source.jpg");
        write_image(&input, NativeImageFormat::Jpeg, Some(30));
        let before = std::fs::read(&input).unwrap();

        let (original, produced, spec) = run_optimize(
            &workspace.dir,
            NativeImageFormat::Jpeg,
            OptimizeLevel::Balanced,
            Some(30),
        );
        assert_eq!(
            produced, original,
            "the result is not the original: {produced} vs {original}"
        );
        let after = std::fs::read(workspace.dir.join("result.jpg")).unwrap();
        assert_eq!(after, before, "the published bytes are not the original");
        assert!(spec.validate_output(&workspace.dir.join("result.jpg")).is_ok());
    }

    #[test]
    fn the_output_is_never_larger_than_the_input_at_any_level() {
        // The promise, checked across every pair the API accepts, for a
        // source that is already compressed.
        let workspace = Workspace::new();
        for format in [
            NativeImageFormat::Jpeg,
            NativeImageFormat::Png,
            NativeImageFormat::WebP,
        ] {
            for level in image_optimization_levels(format) {
                let (original, produced, _) =
                    run_optimize(&workspace.dir, format, level, Some(40));
                assert!(
                    produced <= original,
                    "{format:?} at {} grew the file: {produced} vs {original}",
                    level.wire()
                );
                assert!(produced > 0);
            }
        }
    }

    #[test]
    fn a_corrupt_image_fails_with_the_shared_pipeline_code() {
        let workspace = Workspace::new();
        let input = workspace.dir.join("source.png");
        std::fs::write(&input, b"\x89PNG\r\n\x1a\nnot really a png").unwrap();
        let output = workspace.dir.join("result.png");
        let spec = spec_for(NativeImageFormat::Png, OptimizeLevel::Lossless);
        let inputs = vec![input];
        let request = RunRequest {
            inputs: &inputs,
            output: &output,
            spec: &spec,
        };
        let cancel = AtomicBool::new(false);
        let progress = |_: u8| {};
        let control = JobControl {
            cancel: &cancel,
            progress: &progress,
        };
        match ImageOptimizeRunner.run(&request, &control) {
            // The same code the conversion pipeline reports for the same
            // input, so one message covers both.
            Err(RunError::Failed { code, .. }) => assert_eq!(code, "IMAGE_DECODE_FAILED"),
            other => panic!("expected a decode failure, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_job_writes_no_output() {
        let workspace = Workspace::new();
        let input = workspace.dir.join("source.png");
        write_image(&input, NativeImageFormat::Png, None);
        let output = workspace.dir.join("result.png");
        let spec = spec_for(NativeImageFormat::Png, OptimizeLevel::Lossless);
        let inputs = vec![input];
        let request = RunRequest {
            inputs: &inputs,
            output: &output,
            spec: &spec,
        };
        let cancel = AtomicBool::new(true);
        let progress = |_: u8| {};
        let control = JobControl {
            cancel: &cancel,
            progress: &progress,
        };
        assert!(matches!(
            ImageOptimizeRunner.run(&request, &control),
            Err(RunError::Cancelled)
        ));
        assert!(!output.exists(), "a cancelled optimization wrote an output");
    }

    #[test]
    fn a_kind_this_runner_does_not_back_is_a_wiring_mistake() {
        let workspace = Workspace::new();
        let input = workspace.dir.join("source.png");
        write_image(&input, NativeImageFormat::Png, None);
        let output = workspace.dir.join("result.png");
        let request = CreateJobRequest {
            kind: JobKind::ImageConvert.wire().to_string(),
            file_id: Some("f".to_string()),
            file_ids: None,
            output_format: Some("jpg".to_string()),
            options: None,
        };
        let foreign = JobSpec::from_request(
            JobKind::ImageConvert,
            &request,
            &[SourceFormat::Image(NativeImageFormat::Png)],
        )
        .unwrap();
        let inputs = vec![input];
        let run_request = RunRequest {
            inputs: &inputs,
            output: &output,
            spec: &foreign,
        };
        let cancel = AtomicBool::new(false);
        let progress = |_: u8| {};
        let control = JobControl {
            cancel: &cancel,
            progress: &progress,
        };
        assert!(matches!(
            ImageOptimizeRunner.run(&run_request, &control),
            Err(RunError::Failed { code: "INTERNAL_ERROR", .. })
        ));
    }
}
