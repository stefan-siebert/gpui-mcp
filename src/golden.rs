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
//! A size mismatch is reported on its own, because nothing was compared and
//! the causes are few enough to name: the window was not the size the script
//! pinned, or the display has a different scale factor than the one the
//! golden was taken on — a golden is in device pixels.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Name of the tool that compares the window against a stored image.
/// Server-local, like the guide and the replay: the golden files live beside
/// the script, not inside the app.
pub const TOOL_NAME: &str = "expect_screenshot";

/// How far a single channel may move before a pixel counts as different.
pub const DEFAULT_CHANNEL_TOLERANCE: u8 = 8;

/// The fraction of pixels allowed to differ before the comparison fails.
pub const DEFAULT_PIXEL_TOLERANCE: f64 = 0.001;

/// Params for the server-local `expect_screenshot` tool.
///
/// Only ever read: a recorder stores a step's arguments as the JSON they
/// arrived as, so nothing serialises this.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GoldenExpectation {
    /// The golden file. Written on the first run, compared afterwards.
    ///
    /// Relative to the working directory when called as a tool. In a script it
    /// is relative to the script file — the recorder writes it that way and
    /// replay resolves it that way — so "the goldens live beside the script"
    /// stays true from whichever directory the replay is started.
    pub path: String,
    /// Compare only this element, as `take_screenshot` would crop it.
    #[serde(default)]
    pub element_id: Option<String>,
    #[serde(default)]
    pub window_id: Option<String>,
    /// See the module docs. Default [`DEFAULT_CHANNEL_TOLERANCE`].
    #[serde(default)]
    pub channel_tolerance: Option<u8>,
    /// See the module docs. Default [`DEFAULT_PIXEL_TOLERANCE`].
    #[serde(default)]
    pub pixel_tolerance: Option<f64>,
}

/// What a comparison found.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub matched: bool,
    /// True when pixels were actually compared. False when the golden was
    /// written or rewritten instead, and false on a size mismatch: the
    /// differing counts are zero then because nothing was looked at, not
    /// because nothing differed.
    pub compared: bool,
    /// True when the golden did not exist and has just been written. The step
    /// passes — there was nothing to compare against — and says so, because a
    /// run that silently creates its own expectations proves nothing.
    pub created: bool,
    pub golden: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// The size of this run's image, in device pixels.
    pub width: u32,
    pub height: u32,
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
        .map(|value| switch_is_on(&value))
        .unwrap_or(false)
}

/// `1` and its spellings mean on. Everything else means off — including
/// `false`, which a CI file exporting a YAML boolean produces as a string. The
/// one switch that makes every golden pass must not be turned on by a value
/// that says no.
fn switch_is_on(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Compare freshly taken PNG bytes against the golden at `path`.
///
/// `scale_factor` is the device scale the screenshot was rendered at, when
/// the app reported one; it only matters for naming the cause of a size
/// mismatch.
pub fn compare(
    expectation: &GoldenExpectation,
    actual_png: &[u8],
    scale_factor: Option<f32>,
) -> anyhow::Result<Comparison> {
    let golden_path = PathBuf::from(&expectation.path);
    let channel_tolerance = expectation
        .channel_tolerance
        .unwrap_or(DEFAULT_CHANNEL_TOLERANCE);
    let pixel_tolerance = expectation
        .pixel_tolerance
        .unwrap_or(DEFAULT_PIXEL_TOLERANCE)
        .clamp(0.0, 1.0);

    // The header is enough to know the size, and to know these bytes are a
    // PNG before they are stored as one. Decoding the pixels waits until
    // there is a golden to compare them with.
    let (width, height) =
        png_dimensions(actual_png).map_err(|e| anyhow::anyhow!("the new screenshot: {e}"))?;
    let total_pixels = u64::from(width) * u64::from(height);

    let created = !golden_path.exists();
    if created || updating_goldens() {
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
            compared: false,
            created,
            golden: golden_path.display().to_string(),
            actual: None,
            width,
            height,
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

    if golden.dimensions() != (width, height) {
        let actual_path = write_actual(&golden_path, actual_png)?;
        let detail = size_mismatch_detail(
            golden.dimensions(),
            (width, height),
            scale_factor,
            expectation.element_id.is_some(),
        );

        return Ok(Comparison {
            matched: false,
            compared: false,
            created: false,
            golden: golden_path.display().to_string(),
            actual: Some(actual_path),
            width,
            height,
            differing_pixels: 0,
            total_pixels,
            differing_fraction: 0.0,
            pixel_tolerance,
            channel_tolerance,
            detail: Some(detail),
        });
    }

    let actual = decode(actual_png).map_err(|e| anyhow::anyhow!("the new screenshot: {e}"))?;
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
        compared: true,
        created: false,
        golden: golden_path.display().to_string(),
        actual: actual_path,
        width,
        height,
        differing_pixels: differing,
        total_pixels,
        differing_fraction: fraction,
        pixel_tolerance,
        channel_tolerance,
        detail,
    })
}

/// Name what a size mismatch can mean, in the order worth checking.
///
/// The image is in device pixels and a viewport in logical ones, so the same
/// pinned window on a display with a different scale factor is a different
/// image — and that case is recognisable, because both sides grow by the same
/// factor. A window that was not pinned is the other cause, and a cropped
/// element that changed its own size the third.
fn size_mismatch_detail(
    golden: (u32, u32),
    actual: (u32, u32),
    scale_factor: Option<f32>,
    cropped: bool,
) -> String {
    let mut detail = format!(
        "The golden is {}x{} and this run is {}x{}{}. Nothing was compared: a different size is \
         a different layout.",
        golden.0,
        golden.1,
        actual.0,
        actual.1,
        scale_factor
            .map(|scale| format!(" at a device scale of {scale}"))
            .unwrap_or_default()
    );

    let ratio_x = f64::from(actual.0) / f64::from(golden.0.max(1));
    let ratio_y = f64::from(actual.1) / f64::from(golden.1.max(1));
    let same_factor = (ratio_x - ratio_y).abs() < 0.01 && (ratio_x - 1.0).abs() > 0.01;

    if same_factor {
        detail.push_str(&format!(
            " Both sides differ by the same factor ({ratio_x:.2}), which is what a different \
             display scale looks like: a golden is in device pixels, so it only matches on a \
             display with the scale factor it was taken at. If the display is the same, the \
             window was not pinned — give the script a viewport, or call set_viewport before \
             the first step."
        ));
    } else if cropped {
        detail.push_str(
            " With element_id set, either the element itself changed size, or the window was \
             not pinned — give the script a viewport, or call set_viewport before the first \
             step.",
        );
    } else {
        detail.push_str(
            " This is what an unpinned window looks like — give the script a viewport, or call \
             set_viewport before the first step.",
        );
    }
    detail
}

/// The size a PNG declares, read from its header alone.
fn png_dimensions(bytes: &[u8]) -> anyhow::Result<(u32, u32)> {
    let reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    Ok(reader.into_dimensions()?)
}

fn decode(bytes: &[u8]) -> anyhow::Result<image::RgbaImage> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?;
    Ok(decoded.into_rgba8())
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
    crate::script::ensure_parent_dir(path)?;
    std::fs::write(path, bytes)
        .map_err(|e| anyhow::anyhow!("Cannot write {}: {e}", path.display()))?;
    Ok(())
}

/// Where a failing image goes: beside the golden, as `<name>.actual.png`.
pub fn actual_path(golden: &Path) -> PathBuf {
    let mut name = golden.file_stem().unwrap_or_default().to_os_string();
    name.push(".actual.png");
    golden.with_file_name(name)
}

/// Put the failing image beside the golden.
///
/// A failure that only reports a percentage cannot be acted on. The two files
/// side by side can be opened, diffed, or dropped into a review.
fn write_actual(golden: &Path, bytes: &[u8]) -> anyhow::Result<String> {
    let path = actual_path(golden);
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
        std::fs::remove_file(actual_path(path)).ok();
    }

    /// The first run has nothing to compare against. It must say so rather
    /// than report a pass: a run that quietly writes its own expectations
    /// proves nothing about the next one.
    #[test]
    fn the_first_run_writes_the_golden_and_says_so() {
        let path = temp("first");
        clean(&path);

        let bytes = png(4, 4, [10, 20, 30, 255]);
        let result = compare(&expectation(&path), &bytes, None).unwrap();

        assert!(result.matched);
        assert!(result.created);
        assert!(!result.compared);
        assert_eq!((result.width, result.height), (4, 4));
        assert!(result
            .detail
            .as_deref()
            .unwrap()
            .contains("nothing to compare"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "stored byte for byte");

        clean(&path);
    }

    /// Whatever the app hands over is stored as the golden, so it had better
    /// be an image — a run that stored garbage would fail every later run
    /// with a message about the golden, not about the screenshot.
    #[test]
    fn bytes_that_are_not_a_png_are_refused_before_being_stored() {
        let path = temp("garbage");
        clean(&path);

        let error = compare(&expectation(&path), b"not a png", None).expect_err("refused");
        assert!(error.to_string().contains("new screenshot"), "{error}");
        assert!(!path.exists(), "nothing was written");
    }

    /// Edge pixels move by a level or two between runs on the same machine.
    /// A comparison that called that a failure would fail every time, and a
    /// check that always fails is a check nobody reads.
    #[test]
    fn a_shift_within_the_channel_tolerance_still_matches() {
        let path = temp("shift");
        std::fs::write(&path, png(4, 4, [100, 100, 100, 255])).unwrap();

        let result = compare(&expectation(&path), &png(4, 4, [104, 96, 100, 255]), None).unwrap();

        assert!(result.matched, "{:?}", result.detail);
        assert!(result.compared);
        assert_eq!(result.differing_pixels, 0);

        clean(&path);
    }

    #[test]
    fn a_real_difference_fails_and_leaves_the_actual_image_behind() {
        let path = temp("differs");
        std::fs::write(&path, png(4, 4, [0, 0, 0, 255])).unwrap();

        let result = compare(&expectation(&path), &png(4, 4, [255, 255, 255, 255]), None).unwrap();

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

        let strict = compare(&expectation(&path), &bytes, None).unwrap();
        assert!(!strict.matched, "one pixel of sixteen is over the default");
        assert_eq!(strict.differing_pixels, 1);

        let lenient = compare(
            &GoldenExpectation {
                pixel_tolerance: Some(0.1),
                ..expectation(&path)
            },
            &bytes,
            None,
        )
        .unwrap();
        assert!(lenient.matched, "{:?}", lenient.detail);

        clean(&path);
    }

    /// A different size is reported as exactly that, with nothing counted as
    /// differing — "100% of pixels differ" would be a claim about a comparison
    /// that never happened.
    #[test]
    fn a_different_size_says_the_window_was_not_pinned() {
        let path = temp("size");
        std::fs::write(&path, png(4, 4, [0, 0, 0, 255])).unwrap();

        let result = compare(&expectation(&path), &png(8, 4, [0, 0, 0, 255]), None).unwrap();

        assert!(!result.matched);
        assert!(!result.compared);
        assert_eq!(result.differing_pixels, 0);
        assert_eq!(result.differing_fraction, 0.0);
        let detail = result.detail.as_deref().unwrap();
        assert!(detail.contains("4x4"), "{detail}");
        assert!(detail.contains("8x4"), "{detail}");
        assert!(detail.contains("viewport"), "{detail}");
        assert!(!detail.contains("display scale"), "{detail}");

        clean(&path);
    }

    /// The same window on a 200% display is twice the image. Telling that
    /// user to pin the window would send them looking in the wrong place.
    #[test]
    fn a_uniformly_scaled_size_names_the_display_scale() {
        let path = temp("scale");
        std::fs::write(&path, png(4, 4, [0, 0, 0, 255])).unwrap();

        let result = compare(&expectation(&path), &png(8, 8, [0, 0, 0, 255]), Some(2.0)).unwrap();

        assert!(!result.matched);
        let detail = result.detail.as_deref().unwrap();
        assert!(detail.contains("2.00"), "{detail}");
        assert!(detail.contains("display scale"), "{detail}");
        assert!(detail.contains("device scale of 2"), "{detail}");

        clean(&path);
    }

    /// A cropped element has a third way to change size: itself.
    #[test]
    fn a_cropped_mismatch_mentions_the_element() {
        let path = temp("cropped");
        std::fs::write(&path, png(4, 4, [0, 0, 0, 255])).unwrap();

        let result = compare(
            &GoldenExpectation {
                element_id: Some("sidebar".into()),
                ..expectation(&path)
            },
            &png(6, 4, [0, 0, 0, 255]),
            None,
        )
        .unwrap();

        let detail = result.detail.as_deref().unwrap();
        assert!(detail.contains("element itself"), "{detail}");

        clean(&path);
    }

    /// `GPUI_MCP_UPDATE_GOLDENS=false` in a CI file must not switch on the one
    /// thing that makes every golden pass.
    #[test]
    fn only_a_yes_turns_updating_on() {
        for on in ["1", "true", "TRUE", "yes", "on", " 1 "] {
            assert!(switch_is_on(on), "{on:?}");
        }
        for off in ["", "0", "false", "False", "no", "off", "2", "update"] {
            assert!(!switch_is_on(off), "{off:?}");
        }
    }

    #[test]
    fn the_actual_image_sits_beside_the_golden() {
        assert_eq!(
            actual_path(Path::new("tests/golden/sidebar.png")),
            Path::new("tests/golden/sidebar.actual.png")
        );
    }
}
