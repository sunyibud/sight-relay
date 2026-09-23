use image::{DynamicImage, ImageBuffer, Rgba};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use thiserror::Error;

pub type RgbaImage = ImageBuffer<Rgba<u8>, Vec<u8>>;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Roi {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("invalid ROI: {0}")]
    InvalidRoi(String),
    #[error("image encoding failed: {0}")]
    Encode(#[from] image::ImageError),
    #[error("screen capture failed: {0}")]
    Screen(String),
}

impl Roi {
    pub fn full() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        }
    }

    pub fn to_pixels(
        self,
        display_width: u32,
        display_height: u32,
    ) -> Result<PixelRect, CaptureError> {
        let values = [self.x, self.y, self.width, self.height];
        if values
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0)
            || self.x >= 1.0
            || self.y >= 1.0
            || self.width <= 0.0
            || self.height <= 0.0
            || self.x + self.width > 1.0
            || self.y + self.height > 1.0
        {
            return Err(CaptureError::InvalidRoi(format!("{self:?}")));
        }
        let x = (self.x * display_width as f32).floor() as u32;
        let y = (self.y * display_height as f32).floor() as u32;
        let right = ((self.x + self.width).min(1.0) * display_width as f32).floor() as u32;
        let bottom = ((self.y + self.height).min(1.0) * display_height as f32).floor() as u32;
        let width = right.saturating_sub(x).max(1);
        let height = bottom.saturating_sub(y).max(1);
        Ok(PixelRect {
            x,
            y,
            width,
            height,
        })
    }
}

#[derive(Debug, Clone)]
pub struct EncodedImage {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

pub fn encode_jpeg(image: &RgbaImage, quality: u8) -> Result<EncodedImage, CaptureError> {
    let quality = quality.clamp(1, 100);
    let mut bytes = Cursor::new(Vec::new());
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder.encode_image(&DynamicImage::ImageRgba8(image.clone()))?;
    let bytes = bytes.into_inner();
    let mut digest = Sha256::new();
    digest.update(&bytes);
    Ok(EncodedImage {
        width: image.width(),
        height: image.height(),
        sha256: format!("{:x}", digest.finalize()),
        bytes,
    })
}

/// Captures a display on macOS using XCap. The caller is responsible for granting
/// Screen Recording permission to the signed application.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn capture_display(display_id: u32, roi: Roi) -> Result<EncodedImage, CaptureError> {
    #[cfg(target_os = "macos")]
    {
        let _ = display_id; // kept for config/Windows compatibility; macOS uses the main display
        // xcap's CGWindowListCreateImage(OptionAll) can return the desktop layer
        // (wallpaper) when the app is hidden in the menu bar.  Use Apple's
        // screencapture utility for the actual visible, composited screen.  The
        // `-m` flag means the main display; this is intentional because the
        // capture client targets the display the user is currently looking at.
        let path = std::env::temp_dir().join(format!(
            "sight-relay-screen-{}-{}.png",
            std::process::id(),
            std::thread::current().name().unwrap_or("capture")
        ));
        let output = std::process::Command::new("/usr/sbin/screencapture")
            .args(["-x", "-m", "-t", "png"])
            .arg(&path)
            .output()
            .map_err(|e| CaptureError::Screen(format!("启动 screencapture 失败: {e}")))?;
        if !output.status.success() {
            let _ = std::fs::remove_file(&path);
            return Err(CaptureError::Screen(format!(
                "screencapture 失败: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| CaptureError::Screen(format!("读取屏幕截图失败: {e}")))?;
        let _ = std::fs::remove_file(&path);
        let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|e| CaptureError::Screen(format!("解析屏幕截图失败: {e}")))?
            .to_rgba8();
        let rect = roi.to_pixels(image.width(), image.height())?;
        let cropped =
            image::imageops::crop_imm(&image, rect.x, rect.y, rect.width, rect.height).to_image();
        encode_jpeg(&cropped, 85)
    }

    #[cfg(target_os = "windows")]
    {
        let monitors = xcap::Monitor::all().map_err(|e| CaptureError::Screen(e.to_string()))?;
        let monitor = monitors
            .into_iter()
            .find(|m| m.id().ok() == Some(display_id))
            .ok_or_else(|| CaptureError::Screen(format!("display {display_id} not found")))?;
        let image = monitor
            .capture_image()
            .map_err(|e| CaptureError::Screen(e.to_string()))?;
        let rect = roi.to_pixels(image.width(), image.height())?;
        let cropped =
            image::imageops::crop_imm(&image, rect.x, rect.y, rect.width, rect.height).to_image();
        encode_jpeg(&cropped, 85)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn capture_display(_display_id: u32, _roi: Roi) -> Result<EncodedImage, CaptureError> {
    Err(CaptureError::Screen(
        "macOS capture is only available on macOS".into(),
    ))
}

pub fn list_displays() -> Result<Vec<(u32, String, u32, u32)>, CaptureError> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        xcap::Monitor::all()
            .map_err(|e| CaptureError::Screen(e.to_string()))
            .map(|monitors| {
                monitors
                    .into_iter()
                    .filter_map(|m| {
                        Some((
                            m.id().ok()?,
                            m.name().ok()?,
                            m.width().ok()?,
                            m.height().ok()?,
                        ))
                    })
                    .collect()
            })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Err(CaptureError::Screen(
        "macOS capture is only available on macOS".into(),
    ))
}

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_seconds: u64,
}
impl RetryPolicy {
    pub fn delay_seconds(self, attempt: u32) -> u64 {
        self.base_delay_seconds
            .saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)))
            .min(300)
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    #[test]
    fn retry_delay_is_exponential_and_capped() {
        let p = RetryPolicy {
            max_attempts: 6,
            base_delay_seconds: 1,
        };
        assert_eq!(p.delay_seconds(1), 1);
        assert_eq!(p.delay_seconds(3), 4);
        assert_eq!(p.delay_seconds(99), 300);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingUpload {
    pub id: String,
    pub device_id: String,
    pub server_url: String,
    pub image_path: String,
    pub attempts: u32,
    pub next_attempt_unix: i64,
}

pub mod queue {
    use super::*;
    use std::{fs, path::Path};
    pub fn enqueue(dir: &Path, item: &PendingUpload) -> Result<(), std::io::Error> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", item.id));
        fs::write(path, serde_json::to_vec_pretty(item).unwrap())
    }
    pub fn load_ready(dir: &Path, now: i64) -> Result<Vec<PendingUpload>, std::io::Error> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for e in fs::read_dir(dir)? {
            let p = e?.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Ok(b) = fs::read(&p)
                && let Ok(i) = serde_json::from_slice::<PendingUpload>(&b)
                && i.next_attempt_unix <= now
            {
                out.push(i)
            }
        }
        Ok(out)
    }
    pub fn remove(dir: &Path, id: &str) -> Result<(), std::io::Error> {
        let p = dir.join(format!("{id}.json"));
        if p.exists() {
            fs::remove_file(p)
        } else {
            Ok(())
        }
    }
}
