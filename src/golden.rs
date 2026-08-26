//! Golden screenshots: does the window still look the way it looked?
//!
//! This lives in the server rather than the app for the same reason recording
//! does — the golden files sit next to the script that references them, and
//! both are artefacts of a test run, not of the application.
//!
//! ## What "matches" means here
//!
//! Two images match when they are the same size and few enough pixels differ
//! by enough. Both halves of that need a number, and neither number is a
//! perceptual metric in the CIE sense — calling it one would over-claim:
//!
//! - [`GoldenExpectation::channel_tolerance`] is how far one channel may move
//!   before a pixel counts as different at all. Text rendering, subpixel
//!   positioning and GPU filtering shift edge pixels by a few levels between
//!   runs on the same machine, and a comparison that called those a failure
//!   would fail every time and teach everyone to ignore it.
//! - [`GoldenExpectation::pixel_tolerance`] is the fraction of pixels allowed
//!   to differ. A blinking cursor is a handful of pixels; a changed layout is
//!   not.
//!
//! A size mismatch is reported on its own, because it has one cause worth
//! naming: the window was not the size the script pinned.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How far a single channel may move before a pixel counts as different.
pub const DEFAULT_CHANNEL_TOLERANCE: u8 = 8;

/// The fraction of pixels allowed to differ before the comparison fails.
pub const DEFAULT_PIXEL_TOLERANCE: f64 = 0.001;

/// Params for the server-local `expect_screenshot` tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoldenExpectation {
    /// The golden file. Written on the first run, compared afterwards.
    pub path: String,
    /// Compare only this element, as `take_screenshot` would crop it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    /// See the module docs. Default [`DEFAULT_CHANNEL_TOLERANCE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_tolerance: Option<u8>,
    /// See the module docs. Default [`DEFAULT_PIXEL_TOLERANCE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_tolerance: Option<f64>,
}

/// What a comparison found.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub matched: bool,
    /// True when the golden did not exist and has just been written. The step
    /// passes — there was nothing to compare against — and says so, because a
    /// run that silently creates its own expectations proves nothing.
    pub created: bool,
    pub golden: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    pub differing_pixels: u64,
    pub total_pixels: u64,
    pub differing_fraction: f64,
    pub pixel_tolerance: f64,
    pub channel_tolerance: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Whether the run may rewrite goldens instead of failing against them.
///
/// A separate switch rather than a parameter of the step: updating is a thing
/// a person decides for a whole run after looking at what changed, not
/// something a script should be able to ask for on its own. A script that
/// could update its own golden would never fail.
pub fn updating_goldens() -> bool {
    std::env::var("GPUI_MCP_UPDATE_GOLDENS")
        .map(|value| !value.trim().is_empty() && value.trim() != "0")
        .unwrap_or(false)
}

/// Compare freshly taken PNG bytes against the golden at `path`.
pub fn compare(expectation: &GoldenExpectation, actual_png: &[u8]) -> anyhow::Result<Comparison> {
    let golden_path = PathBuf::from(&expectation.path);
    let channel_tolerance = expectation
        .channel_tolerance
        .unwrap_or(DEFAULT_CHANNEL_TOLERANCE);
    let pixel_tolerance = expectation
        .pixel_tolerance
        .unwrap_or(DEFAULT_PIXEL_TOLERANCE)
        .clamp(0.0, 1.0);

    let actual = decode(actual_png).map_err(|e| anyhow::anyhow!("the new screenshot: {e}"))?;
    let total_pixels = u64::from(actual.width()) * u64::from(actual.height());

    if !golden_path.exists() || updating_goldens() {
        let created = !golden_path.exists();
        write_png(&golden_path, actual_png)?;
        let detail = if created {
            format!(
                "Wrote {} — there was nothing to compare against yet. Look at it before \
                 trusting the next run.",
                golden_path.display()
            )
        } else {
            format!(
                "GPUI_MCP_UPDATE_GOLDENS is set, so {} was rewritten rather than compared.",
                golden_path.display()
            )
        };

        return Ok(Comparison {
            matched: true,
            created,
            golden: golden_path.display().to_string(),
            actual: None,
            differing_pixels: 0,
            total_pixels,
            differing_fraction: 0.0,
            pixel_tolerance,
            channel_tolerance,
            detail: Some(detail),
        });
    }

    let golden_bytes = std::fs::read(&golden_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {e}", golden_path.display()))?;
    let golden =
        decode(&golden_bytes).map_err(|e| anyhow::anyhow!("{}: {e}", golden_path.display()))?;

    if golden.dimensions() != actual.dimensions() {
        let actual_path = write_actual(&golden_path, actual_png)?;
        let detail = format!(
            "The golden is {}x{} and this run is {}x{}. Nothing was compared: a different size \
             is a different layout. This is what an unpinned window looks like — give the \
             script a viewport, or call set_viewport before the first step.",
            golden.width(),
            golden.height(),
            actual.width(),
            actual.height()
        );

        return Ok(Comparison {
            matched: false,
            created: false,
            golden: golden_path.display().to_string(),
            actual: Some(actual_path),
            differing_pixels: total_pixels,
            total_pixels,
            differing_fraction: 1.0,
            pixel_tolerance,
            channel_tolerance,
            detail: Some(detail),
        });
    }

    let differing = differing_pixels(&golden, &actual, channel_tolerance);
    let fraction = if total_pixels == 0 {
        0.0
    } else {
        differing as f64 / total_pixels as f64
    };
    let matched = fraction <= pixel_tolerance;

    let actual_path = if matched {
        None
    } else {
        Some(write_actual(&golden_path, actual_png)?)
    };

    let detail = actual_path.as_ref().map(|actual_path| {
        format!(
            "{differing} of {total_pixels} pixels differ by more than {channel_tolerance} per \
             channel ({:.4}% against a {:.4}% allowance). This run was written to {actual_path} \
             — open both. If the change is intended, re-run with GPUI_MCP_UPDATE_GOLDENS=1.",
            fraction * 100.0,
            pixel_tolerance * 100.0,
        )
    });

    Ok(Comparison {
        matched,
        created: false,
        golden: golden_path.display().to_string(),
        actual: actual_path,
        differing_pixels: differing,
        total_pixels,
        differing_fraction: fraction,
        pixel_tolerance,
        channel_tolerance,
        detail,
    })
}

fn decode(bytes: &[u8]) -> anyhow::Result<image::RgbaImage> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?;
    Ok(decoded.to_rgba8())
}

/// Count the pixels that moved further than the tolerance on any channel.
///
/// Alpha counts too. A screenshot with a transparent region that became
/// opaque looks identical over a white page and is not the same image.
fn differing_pixels(golden: &image::RgbaImage, actual: &image::RgbaImage, tolerance: u8) -> u64 {
    golden
        .pixels()
        .zip(actual.pixels())
        .filter(|(left, right)| {
            left.0
                .iter()
                .zip(right.0.iter())
                .any(|(a, b)| a.abs_diff(*b) > tolerance)
        })
        .count() as u64
}

fn write_png(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, bytes)
        .map_err(|e| anyhow::anyhow!("Cannot write {}: {e}", path.display()))?;
    Ok(())
}

/// Put the failing image beside the golden, as `<name>.actual.png`.
///
/// A failure that only reports a percentage cannot be acted on. The two files
/// side by side can be opened, diffed, or dropped into a review.
fn write_actual(golden: &Path, bytes: &[u8]) -> anyhow::Result<String> {
    let mut name = golden.file_stem().unwrap_or_default().to_os_string();
    name.push(".actual.png");
    let path = golden.with_file_name(name);
    write_png(&path, bytes)?;
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, fill: [u8; 4]) -> Vec<u8> {
        encode(&image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba(fill),
        ))
    }

    fn encode(image: &image::RgbaImage) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode");
        bytes.into_inner()
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gpui-mcp-golden-{}-{}.png",
            std::process::id(),
            name
        ))
    }

    fn expectation(path: &Path) -> GoldenExpectation {
        GoldenExpectation {
            path: path.display().to_string(),
            ..Default::default()
        }
    }

    fn clean(path: &Path) {
        std::fs::remove_file(path).ok();
        let mut name = path.file_stem().unwrap_or_default().to_os_string();
        name.push(".actual.png");
        std::fs::remove_file(path.with_file_name(name)).ok();
    }

    /// The first run has nothing to compare against. It must say so rather
    /// than report a pass: a run that quietly writes its own expectations
    /// proves nothing about the next one.
    #[test]
    fn the_first_run_writes_the_golden_and_says_so() {
        let path = temp("first");
        clean(&path);

        let result = compare(&expectation(&path), &png(4, 4, [10, 20, 30, 255])).unwrap();

        assert!(result.matched);
        assert!(result.created);
        assert!(result
            .detail
            .as_deref()
            .unwrap()
            .contains("nothing to compare"));
        assert!(path.exists());

        clean(&path);
    }

    /// Edge pixels move by a level or two between runs on the same machine.
    /// A comparison that called that a failure would fail every time, and a
    /// check that always fails is a check nobody reads.
    #[test]
    fn a_shift_within_the_channel_tolerance_still_matches() {
        let path = temp("shift");
        std::fs::write(&path, png(4, 4, [100, 100, 100, 255])).unwrap();

        let result = compare(&expectation(&path), &png(4, 4, [104, 96, 100, 255])).unwrap();

        assert!(result.matched, "{:?}", result.detail);
        assert_eq!(result.differing_pixels, 0);

        clean(&path);
    }

    #[test]
    fn a_real_difference_fails_and_leaves_the_actual_image_behind() {
        let path = temp("differs");
        std::fs::write(&path, png(4, 4, [0, 0, 0, 255])).unwrap();

        let result = compare(&expectation(&path), &png(4, 4, [255, 255, 255, 255])).unwrap();

        assert!(!result.matched);
        assert_eq!(result.differing_pixels, 16);
        assert_eq!(result.differing_fraction, 1.0);

        let actual = result.actual.as_deref().expect("the failing image");
        assert!(Path::new(actual).exists(), "{actual}");
        assert!(actual.ends_with(".actual.png"), "{actual}");

        clean(&path);
    }

    /// One pixel of sixteen is 6.25%, well over the default allowance — and
    /// the same pixel in a real window is far under it. The fraction is what
    /// makes one tolerance mean the same thing at every size.
    #[test]
    fn the_allowance_is_a_fraction_not_a_count() {
        let path = temp("fraction");
        std::fs::write(&path, png(4, 4, [0, 0, 0, 255])).unwrap();

        let mut changed = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        changed.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        let bytes = encode(&changed);

        let strict = compare(&expectation(&path), &bytes).unwrap();
        assert!(!strict.matched, "one pixel of sixteen is over the default");
        assert_eq!(strict.differing_pixels, 1);

        let lenient = compare(
            &GoldenExpectation {
                pixel_tolerance: Some(0.1),
                ..expectation(&path)
            },
            &bytes,
        )
        .unwrap();
        assert!(lenient.matched, "{:?}", lenient.detail);

        clean(&path);
    }

    /// A different size has one cause worth naming, and comparing pixels
    /// across it would be meaningless anyway.
    #[test]
    fn a_different_size_says_the_window_was_not_pinned() {
        let path = temp("size");
        std::fs::write(&path, png(4, 4, [0, 0, 0, 255])).unwrap();

        let result = compare(&expectation(&path), &png(8, 4, [0, 0, 0, 255])).unwrap();

        assert!(!result.matched);
        let detail = result.detail.as_deref().unwrap();
        assert!(detail.contains("4x4"), "{detail}");
        assert!(detail.contains("8x4"), "{detail}");
        assert!(detail.contains("viewport"), "{detail}");

        clean(&path);
    }
}
