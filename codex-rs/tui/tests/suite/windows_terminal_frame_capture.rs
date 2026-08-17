//! Presented-frame capture and analysis for the interactive Windows Terminal regression.
//!
//! The observer samples only the `TermControl` client rectangle. Analysis derives the background
//! color from each frame and checks that the completed bottom UI remains present; it does not
//! depend on a terminal theme, font, DPI, or pixel-perfect golden image.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;

const MIN_CAPTURED_FRAMES: usize = 3;
const MIN_REFERENCE_DENSITY: f64 = 0.002;
const MIN_REFERENCE_OCCUPIED_ROWS: f64 = 0.15;
const MIN_REFERENCE_BOTTOM_OCCUPIED_ROWS: usize = 3;
const MIN_BOTTOM_OCCUPIED_ROW_RATIO: f64 = 0.25;
const MIN_PRESENTED_FOREGROUND_DENSITY: f64 = 0.00005;
const BOTTOM_REGION_PERCENT: u32 = 15;
const FOREGROUND_DISTANCE_SQUARED: i32 = 30 * 30;

pub(super) const WINDOWS_TERMINAL_FRAME_OBSERVER: &str = r#"
param(
    [Parameter(Mandatory = $true)]
    [long] $WindowHandle,

    [Parameter(Mandatory = $true)]
    [string] $CaptureDirectory,

    [Parameter(Mandatory = $true)]
    [string] $ReadyPath,

    [Parameter(Mandatory = $true)]
    [string] $StopPath,

    [Parameter(Mandatory = $true)]
    [string] $CompletePath,

    [Parameter(Mandatory = $true)]
    [string] $FailurePath,

    [int] $MaxFrames = 1500
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class CodexTerminalDpi {
    [DllImport("user32.dll")]
    public static extern uint GetDpiForWindow(IntPtr windowHandle);
}
'@

function Find-TerminalControl {
    param(
        [System.Windows.Automation.AutomationElement] $Window
    )

    $classCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty,
        'TermControl'
    )
    $terminal = $Window.FindFirst(
        [System.Windows.Automation.TreeScope]::Descendants,
        $classCondition
    )
    if ($null -eq $terminal) {
        throw 'Windows Terminal did not expose a TermControl automation element.'
    }
    return $terminal
}

$manifest = $null
try {
    [System.IO.Directory]::CreateDirectory($CaptureDirectory) | Out-Null
    $windowPointer = [IntPtr]::new($WindowHandle)
    $window = [System.Windows.Automation.AutomationElement]::FromHandle($windowPointer)
    if ($null -eq $window) {
        throw "No UI Automation window exists for handle $WindowHandle."
    }
    $terminal = Find-TerminalControl -Window $window

    $windowPatternObject = $null
    if (-not $window.TryGetCurrentPattern(
        [System.Windows.Automation.WindowPattern]::Pattern,
        [ref] $windowPatternObject
    )) {
        throw 'Windows Terminal window does not expose WindowPattern.'
    }
    $windowPattern = [System.Windows.Automation.WindowPattern] $windowPatternObject
    $dpi = [CodexTerminalDpi]::GetDpiForWindow($windowPointer)
    $manifestPath = Join-Path $CaptureDirectory 'frames.csv'
    $manifest = [System.IO.StreamWriter]::new($manifestPath, $false)
    $manifest.WriteLine('index,elapsed_ms,x,y,width,height,dpi')

    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $frameIndex = 0
    while (-not (Test-Path -LiteralPath $StopPath) -and $frameIndex -lt $MaxFrames) {
        $visualState = $windowPattern.Current.WindowVisualState
        if ($visualState -eq [System.Windows.Automation.WindowVisualState]::Minimized) {
            Start-Sleep -Milliseconds 5
            continue
        }

        try {
            if ($terminal.Current.IsOffscreen) {
                Start-Sleep -Milliseconds 5
                continue
            }
            $bounds = $terminal.Current.BoundingRectangle
            $x = [int] [Math]::Floor($bounds.X)
            $y = [int] [Math]::Floor($bounds.Y)
            $width = [int] [Math]::Ceiling($bounds.Width)
            $height = [int] [Math]::Ceiling($bounds.Height)
            if ($width -lt 100 -or $height -lt 100) {
                Start-Sleep -Milliseconds 5
                continue
            }

            $bitmap = [System.Drawing.Bitmap]::new(
                $width,
                $height,
                [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
            )
            try {
                $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
                try {
                    $graphics.CopyFromScreen(
                        $x,
                        $y,
                        0,
                        0,
                        [System.Drawing.Size]::new($width, $height),
                        [System.Drawing.CopyPixelOperation]::SourceCopy
                    )
                } finally {
                    $graphics.Dispose()
                }

                $elapsedMs = [long] $timer.Elapsed.TotalMilliseconds
                $fileName = 'frame-{0:D5}-{1:D8}.png' -f $frameIndex, $elapsedMs
                $framePath = Join-Path $CaptureDirectory $fileName
                $bitmap.Save($framePath, [System.Drawing.Imaging.ImageFormat]::Png)
                $manifest.WriteLine(
                    '{0},{1},{2},{3},{4},{5},{6}',
                    $frameIndex,
                    $elapsedMs,
                    $x,
                    $y,
                    $width,
                    $height,
                    $dpi
                )
                $manifest.Flush()
                $frameIndex++
                if (-not (Test-Path -LiteralPath $ReadyPath)) {
                    [System.IO.File]::WriteAllText($ReadyPath, [string] $frameIndex)
                }
            } finally {
                $bitmap.Dispose()
            }
        } catch {
            # Window animations can invalidate one bounds sample. A later frame remains useful.
            $terminal = Find-TerminalControl -Window $window
        }
        Start-Sleep -Milliseconds 5
    }

    if ($frameIndex -eq 0) {
        throw 'The observer did not capture a visible TermControl frame.'
    }
    [System.IO.File]::WriteAllText($CompletePath, [string] $frameIndex)
    exit 0
} catch {
    [System.IO.File]::WriteAllText($FailurePath, $_.Exception.ToString())
    exit 1
} finally {
    if ($null -ne $manifest) {
        $manifest.Dispose()
    }
}
"#;

#[derive(Debug)]
struct FrameMetrics {
    path: PathBuf,
    elapsed_ms: u64,
    width: u32,
    height: u32,
    background: [u8; 3],
    foreground_density: f64,
    occupied_row_ratio: f64,
    longest_blank_row_ratio: f64,
    bottom_occupied_rows: usize,
}

#[derive(Debug)]
pub(super) struct PresentedFrameSummary {
    pub(super) frame_count: usize,
    pub(super) average_cadence_ms: f64,
    pub(super) minimum_bottom_occupied_row_ratio: f64,
}

pub(super) fn analyze_presented_frames(capture_dir: &Path) -> Result<PresentedFrameSummary> {
    let frame_paths = captured_frame_paths(capture_dir)?;
    if frame_paths.len() < MIN_CAPTURED_FRAMES {
        bail!(
            "captured only {} presented frames; expected at least {MIN_CAPTURED_FRAMES}; artifacts: {}",
            frame_paths.len(),
            capture_dir.display()
        );
    }

    let metrics = frame_paths
        .iter()
        .map(|path| measure_frame(path))
        .collect::<Result<Vec<_>>>()?;
    let before = metrics.first().context("missing before frame")?;
    let after = metrics.last().context("missing settled after frame")?;
    let reference_density = before.foreground_density.min(after.foreground_density);
    let reference_occupied_rows = before.occupied_row_ratio.min(after.occupied_row_ratio);
    let reference_bottom_occupied_rows =
        before.bottom_occupied_rows.min(after.bottom_occupied_rows);
    if reference_density < MIN_REFERENCE_DENSITY
        || reference_occupied_rows < MIN_REFERENCE_OCCUPIED_ROWS
        || reference_bottom_occupied_rows < MIN_REFERENCE_BOTTOM_OCCUPIED_ROWS
    {
        write_metrics_report(capture_dir, &metrics, None)?;
        bail!(
            "before/after captures do not contain a sufficiently occupied terminal frame; artifacts: {}",
            capture_dir.display()
        );
    }

    let bottom_occupied_rows_floor =
        reference_bottom_occupied_rows as f64 * MIN_BOTTOM_OCCUPIED_ROW_RATIO;
    // Fully blank captures occur while Windows Terminal is minimized between resize cycles. Any
    // nonblank frame without the completed composer/status area exposes an in-progress replay.
    let violating_index = metrics.iter().position(|frame| {
        frame.foreground_density >= MIN_PRESENTED_FOREGROUND_DENSITY
            && frame.bottom_occupied_rows as f64 <= bottom_occupied_rows_floor
    });
    write_metrics_report(capture_dir, &metrics, violating_index)?;

    if let Some(index) = violating_index {
        retain_failure_frames(capture_dir, &metrics, index)?;
        let frame = &metrics[index];
        bail!(
            "presented frame {} lost the completed bottom UI during transcript replay: density \
             {:.4}, occupied rows {:.4}, blank run {:.4}, bottom occupied rows {}; artifacts: {}",
            frame.path.display(),
            frame.foreground_density,
            frame.occupied_row_ratio,
            frame.longest_blank_row_ratio,
            frame.bottom_occupied_rows,
            capture_dir.display()
        );
    }

    let average_cadence_ms = metrics
        .last()
        .zip(metrics.first())
        .map(|(last, first)| {
            last.elapsed_ms.saturating_sub(first.elapsed_ms) as f64
                / (metrics.len().saturating_sub(1)) as f64
        })
        .unwrap_or_default();
    Ok(PresentedFrameSummary {
        frame_count: metrics.len(),
        average_cadence_ms,
        minimum_bottom_occupied_row_ratio: metrics
            .iter()
            .filter(|frame| frame.foreground_density >= MIN_PRESENTED_FOREGROUND_DENSITY)
            .map(|frame| frame.bottom_occupied_rows as f64 / reference_bottom_occupied_rows as f64)
            .fold(f64::INFINITY, f64::min),
    })
}

fn captured_frame_paths(capture_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(capture_dir)
        .with_context(|| format!("read frame capture directory {}", capture_dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|extension| extension.to_str()) == Some("png")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.starts_with("frame-"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn measure_frame(path: &Path) -> Result<FrameMetrics> {
    let image = image::open(path)
        .with_context(|| format!("decode presented frame {}", path.display()))?
        .into_rgb8();
    let width = image.width();
    let height = image.height();
    if width < 100 || height < 100 {
        bail!("presented frame is too small: {}", path.display());
    }

    let mut color_buckets = [0_u32; 4096];
    for pixel in image.pixels() {
        let index = usize::from(pixel[0] >> 4) << 8
            | usize::from(pixel[1] >> 4) << 4
            | usize::from(pixel[2] >> 4);
        color_buckets[index] += 1;
    }
    let background_bucket = color_buckets
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| *count)
        .map(|(index, _)| index)
        .context("frame contains no pixels")?;
    let background = [
        (((background_bucket >> 8) & 0x0f) as u8) * 16 + 7,
        (((background_bucket >> 4) & 0x0f) as u8) * 16 + 7,
        ((background_bucket & 0x0f) as u8) * 16 + 7,
    ];

    let horizontal_inset = 2_u32;
    let usable_width = width.saturating_sub(horizontal_inset * 2);
    let minimum_row_foreground = (usable_width as usize / 250).max(3);
    let mut foreground_pixels = 0_usize;
    let mut occupied_rows = 0_usize;
    let bottom_start =
        height.saturating_sub(height.saturating_mul(BOTTOM_REGION_PERCENT).div_ceil(100));
    let mut bottom_occupied_rows = 0_usize;
    let mut current_blank_rows = 0_usize;
    let mut longest_blank_rows = 0_usize;
    for y in 0..height {
        let row_foreground = (horizontal_inset..width - horizontal_inset)
            .filter(|x| {
                let pixel = image.get_pixel(*x, y);
                let red = i32::from(pixel[0]) - i32::from(background[0]);
                let green = i32::from(pixel[1]) - i32::from(background[1]);
                let blue = i32::from(pixel[2]) - i32::from(background[2]);
                red * red + green * green + blue * blue > FOREGROUND_DISTANCE_SQUARED
            })
            .count();
        foreground_pixels += row_foreground;
        if row_foreground >= minimum_row_foreground {
            occupied_rows += 1;
            if y >= bottom_start {
                bottom_occupied_rows += 1;
            }
            current_blank_rows = 0;
        } else {
            current_blank_rows += 1;
            longest_blank_rows = longest_blank_rows.max(current_blank_rows);
        }
    }

    let elapsed_ms = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.split('-').nth(2))
        .and_then(|elapsed| elapsed.parse::<u64>().ok())
        .with_context(|| {
            format!(
                "frame name does not contain elapsed time: {}",
                path.display()
            )
        })?;
    Ok(FrameMetrics {
        path: path.to_path_buf(),
        elapsed_ms,
        width,
        height,
        background,
        foreground_density: foreground_pixels as f64 / f64::from(usable_width * height),
        occupied_row_ratio: occupied_rows as f64 / f64::from(height),
        longest_blank_row_ratio: longest_blank_rows as f64 / f64::from(height),
        bottom_occupied_rows,
    })
}

fn write_metrics_report(
    capture_dir: &Path,
    metrics: &[FrameMetrics],
    violating_index: Option<usize>,
) -> Result<()> {
    let mut report = String::from(
        "file\telapsed_ms\tsize\tbackground\tdensity\toccupied_rows\tblank_run\tbottom_occupied_rows\tviolating\n",
    );
    for (index, frame) in metrics.iter().enumerate() {
        report.push_str(&format!(
            "{}\t{}\t{}x{}\t#{:02x}{:02x}{:02x}\t{:.6}\t{:.6}\t{:.6}\t{}\t{}\n",
            frame
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<unknown>"),
            frame.elapsed_ms,
            frame.width,
            frame.height,
            frame.background[0],
            frame.background[1],
            frame.background[2],
            frame.foreground_density,
            frame.occupied_row_ratio,
            frame.longest_blank_row_ratio,
            frame.bottom_occupied_rows,
            violating_index == Some(index),
        ));
    }
    fs::write(capture_dir.join("analysis.tsv"), report)
        .with_context(|| format!("write frame analysis in {}", capture_dir.display()))
}

fn retain_failure_frames(
    capture_dir: &Path,
    metrics: &[FrameMetrics],
    violating_index: usize,
) -> Result<()> {
    let diagnostic_frames = [
        ("before.png", 0),
        ("violating.png", violating_index),
        (
            "following.png",
            (violating_index + 1).min(metrics.len() - 1),
        ),
        ("after.png", metrics.len() - 1),
    ];
    for (name, index) in diagnostic_frames {
        fs::copy(&metrics[index].path, capture_dir.join(name)).with_context(|| {
            format!(
                "retain diagnostic frame {} as {name}",
                metrics[index].path.display()
            )
        })?;
    }
    for frame in metrics {
        fs::remove_file(&frame.path)
            .with_context(|| format!("remove non-diagnostic frame {}", frame.path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;
    use image::RgbImage;

    #[test]
    fn stable_occupied_frames_pass() -> Result<()> {
        let capture_dir = tempfile::tempdir()?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 0,
            /*elapsed_ms*/ 0,
            1.0,
        )?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 1,
            /*elapsed_ms*/ 15,
            1.0,
        )?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 2,
            /*elapsed_ms*/ 30,
            1.0,
        )?;

        let summary = analyze_presented_frames(capture_dir.path())?;

        assert_eq!(summary.frame_count, 3);
        assert_eq!(summary.average_cadence_ms, 15.0);
        Ok(())
    }

    #[test]
    fn single_partial_replay_frame_retains_only_bounded_diagnostics() -> Result<()> {
        let capture_dir = tempfile::tempdir()?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 0,
            /*elapsed_ms*/ 0,
            1.0,
        )?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 1,
            /*elapsed_ms*/ 15,
            0.3,
        )?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 2,
            /*elapsed_ms*/ 30,
            1.0,
        )?;

        let error = analyze_presented_frames(capture_dir.path()).expect_err("partial replay");

        assert!(
            error
                .to_string()
                .contains("lost the completed bottom UI during transcript replay")
        );
        for name in ["before.png", "violating.png", "following.png", "after.png"] {
            assert!(capture_dir.path().join(name).is_file(), "missing {name}");
        }
        assert!(captured_frame_paths(capture_dir.path())?.is_empty());
        Ok(())
    }

    #[test]
    fn fully_blank_minimize_frame_is_ignored() -> Result<()> {
        let capture_dir = tempfile::tempdir()?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 0,
            /*elapsed_ms*/ 0,
            1.0,
        )?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 1,
            /*elapsed_ms*/ 15,
            0.0,
        )?;
        write_synthetic_frame(
            capture_dir.path(),
            /*index*/ 2,
            /*elapsed_ms*/ 30,
            1.0,
        )?;

        analyze_presented_frames(capture_dir.path())?;
        Ok(())
    }

    fn write_synthetic_frame(
        capture_dir: &Path,
        index: usize,
        elapsed_ms: u64,
        occupied_fraction: f64,
    ) -> Result<()> {
        let width = 200;
        let height = 120;
        let occupied_height = (f64::from(height) * occupied_fraction) as u32;
        let mut image = RgbImage::from_pixel(width, height, Rgb([8, 8, 8]));
        for top in (4..occupied_height.saturating_sub(2)).step_by(8) {
            for y in top..(top + 2).min(height) {
                for x in 8..120 {
                    image.put_pixel(x, y, Rgb([220, 220, 220]));
                }
            }
        }
        let path = capture_dir.join(format!("frame-{index:05}-{elapsed_ms:08}.png"));
        image
            .save(&path)
            .with_context(|| format!("write synthetic frame {}", path.display()))
    }
}
