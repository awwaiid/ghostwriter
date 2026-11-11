use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use image::{DynamicImage, GrayImage, ImageEncoder, RgbImage, RgbaImage};
use log::{debug, info};
use std::io::{Read, Seek, SeekFrom};
use std::process;

use crate::device::{firmware_version, DeviceModel, VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::simulation::{ScreenshotSimulator, SimulationConfig};

pub enum ScreenshotMode {
    Real {
        /// The screenshot as a PNG, scaled to the virtual page size
        data: Vec<u8>,
        /// The same screenshot decoded, for reading individual pixels
        pixels: Option<RgbImage>,
        device_model: DeviceModel,
    },
    Simulated {
        simulator: ScreenshotSimulator,
    },
}

pub struct Screenshot {
    mode: ScreenshotMode,
}

impl Screenshot {
    pub fn new() -> Result<Screenshot> {
        let device_model = DeviceModel::detect();
        info!("Screen detected device: {}", device_model.name());
        Ok(Screenshot {
            mode: ScreenshotMode::Real {
                data: vec![],
                pixels: None,
                device_model,
            },
        })
    }

    pub fn new_simulated(simulation_config: SimulationConfig) -> Result<Screenshot> {
        let simulator = ScreenshotSimulator::new(simulation_config)?;
        info!("Screen using simulation mode");
        Ok(Screenshot {
            mode: ScreenshotMode::Simulated { simulator },
        })
    }

    fn device_model(&self) -> DeviceModel {
        match &self.mode {
            ScreenshotMode::Real { device_model, .. } => *device_model,
            ScreenshotMode::Simulated { .. } => DeviceModel::Unknown,
        }
    }

    fn screen_width(&self) -> u32 {
        self.device_model().framebuffer_size().0
    }

    fn screen_height(&self) -> u32 {
        self.device_model().framebuffer_size().1
    }

    pub fn bytes_per_pixel(&self) -> usize {
        match self.device_model() {
            DeviceModel::Remarkable2 => {
                if Self::rm2_has_bgra_framebuffer() {
                    4
                } else {
                    2
                }
            }
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure => 4,
            DeviceModel::Unknown => 2, // Default to RM2
        }
    }

    // Firmware 3.24+ changed the RM2 framebuffer from 16-bit (2 bpp) to 32-bit BGRA (4 bpp).
    fn rm2_has_bgra_framebuffer() -> bool {
        let version = firmware_version();
        debug!("RM2 firmware version: {:?}", version);
        version.is_some_and(|version| version >= (3, 24))
    }

    // Memory offset within the post-fb0 mapping where the current framebuffer data starts.
    // Reference: goMarkableStream internal/remarkable/detect.go
    fn rm2_pointer_offset() -> u64 {
        if Self::rm2_has_bgra_framebuffer() {
            2629632
        } else {
            0
        }
    }

    pub fn take_screenshot(&mut self) -> Result<()> {
        if let ScreenshotMode::Simulated { simulator } = &mut self.mode {
            // In simulation mode, just advance to next image
            simulator.advance_to_next_image();
            debug!("Simulated screenshot taken (advanced to next test image)");
            return Ok(());
        }

        debug!("screenshot: finding pid");
        let pid = Self::find_xochitl_pid()?;

        debug!("screenshot: finding address");
        let skip_bytes = self.find_framebuffer_address(&pid)?;

        debug!("screenshot: reading data");
        let framebuffer = self.read_framebuffer(&pid, skip_bytes)?;

        debug!("screenshot: processing image");
        let image = self.decode_framebuffer(framebuffer)?;
        let (png, scaled) = Self::scale_and_encode(&image)?;

        if let ScreenshotMode::Real { data, pixels, .. } = &mut self.mode {
            *data = png;
            *pixels = Some(scaled);
        }

        Ok(())
    }

    fn find_xochitl_pid() -> Result<String> {
        let output = process::Command::new("pidof").arg("xochitl").output()?;
        let pids = String::from_utf8(output.stdout)?;
        match pids.split_whitespace().next() {
            Some(pid) => Ok(pid.to_string()),
            None => anyhow::bail!("No xochitl process found"),
        }
    }

    fn find_framebuffer_address(&self, pid: &str) -> Result<u64> {
        match self.device_model() {
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure => {
                // arm64 devices: the approach from goMarkableStream's pointer_arm64.go
                let start_address = self.get_memory_range(pid)?;
                self.calculate_frame_pointer(pid, start_address)
            }
            DeviceModel::Remarkable2 | DeviceModel::Unknown => {
                // RM2: find the mapping after /dev/fb0 in /proc/pid/maps, then apply firmware offset.
                // Reference: goMarkableStream internal/remarkable/pointer.go
                let output = process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("grep -A1 '/dev/fb0' /proc/{}/maps | tail -n1 | sed 's/-.*$//'", pid))
                    .output()?;
                let address_hex = String::from_utf8(output.stdout)?.trim().to_string();
                let address = u64::from_str_radix(&address_hex, 16)?;
                let pointer_offset = Self::rm2_pointer_offset();
                debug!(
                    "RM2 framebuffer: base={:#x}, pointer_offset={}, total={:#x}",
                    address,
                    pointer_offset,
                    address + pointer_offset + 8
                );
                Ok(address + pointer_offset + 8)
            }
        }
    }

    // The end of xochitl's last /dev/dri/card0 mapping, based on goMarkableStream/pointer_arm64.go
    fn get_memory_range(&self, pid: &str) -> Result<u64> {
        let maps_file_path = format!("/proc/{}/maps", pid);
        debug!("screenshot: reading memory range from {}", maps_file_path);
        let maps_content = std::fs::read_to_string(&maps_file_path)?;

        let memory_range = maps_content
            .lines()
            .rfind(|line| line.contains("/dev/dri/card0"))
            .context("No mapping found for /dev/dri/card0")?;
        debug!("Memory range: {}", memory_range);

        let (_start, end) = memory_range
            .split_whitespace()
            .next()
            .and_then(|range| range.split_once('-'))
            .context("Invalid memory range format")?;
        Ok(u64::from_str_radix(end, 16)?)
    }

    // Walk the allocations after the DRI mapping until one is big enough to hold the screen,
    // based on goMarkableStream/pointer_arm64.go
    fn calculate_frame_pointer(&self, pid: &str, start_address: u64) -> Result<u64> {
        let mut file = std::fs::File::open(format!("/proc/{}/mem", pid))?;

        let screen_size_bytes = self.screen_width() as u64 * self.screen_height() as u64 * self.bytes_per_pixel() as u64;

        let mut offset: u64 = 0;
        let mut length: u64 = 2;

        while length < screen_size_bytes {
            offset += length - 2;

            file.seek(SeekFrom::Start(start_address + offset + 8))?;
            let mut header = [0u8; 8];
            file.read_exact(&mut header)?;
            debug!("  ... header: {:?}", header);

            length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as u64;
            debug!("  ... length: {}", length);
            if length < 2 {
                anyhow::bail!("Invalid header length");
            }
        }

        Ok(start_address + offset)
    }

    fn read_framebuffer(&self, pid: &str, skip_bytes: u64) -> Result<Vec<u8>> {
        let window_bytes = self.screen_width() as usize * self.screen_height() as usize * self.bytes_per_pixel();
        let mut buffer = vec![0u8; window_bytes];
        let mut file = std::fs::File::open(format!("/proc/{}/mem", pid))?;
        file.seek(SeekFrom::Start(skip_bytes))?;
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    /// Turn raw framebuffer bytes into an upright, full-resolution image.
    fn decode_framebuffer(&self, raw_data: Vec<u8>) -> Result<DynamicImage> {
        let width = self.screen_width();
        let height = self.screen_height();
        let invalid_size = || anyhow::anyhow!("Failed to create image from raw data");

        match self.device_model() {
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure => {
                // 32-bit RGBA
                debug!("Decoding {}x{} RGBA framebuffer", width, height);
                Ok(RgbaImage::from_raw(width, height, raw_data).ok_or_else(invalid_size)?.into())
            }
            DeviceModel::Remarkable2 | DeviceModel::Unknown => {
                if self.bytes_per_pixel() == 4 {
                    // Firmware 3.24+: data is stored portrait (1404×1872), 32-bit BGRA.
                    // Values are already full 8-bit range — don't apply the old aggressive curve.
                    // Convert with Rec. 601 luma rather than taking one channel: coloured ink (e.g. blue)
                    // is near-white in its own channel and would vanish from the screenshot.
                    let grey: Vec<u8> = raw_data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|bgra| ((bgra[2] as u32 * 299 + bgra[1] as u32 * 587 + bgra[0] as u32 * 114) / 1000) as u8)
                        .collect();
                    Ok(GrayImage::from_raw(1404, 1872, grey).ok_or_else(invalid_size)?.into())
                } else {
                    // Pre-3.24: data is stored landscape (1872×1404), 16-bit RGB565. Take high byte.
                    let grey: Vec<u8> = raw_data.as_chunks::<2>().0.iter().map(|pixel| Self::apply_curves(pixel[1])).collect();
                    let landscape = GrayImage::from_raw(width, height, grey).ok_or_else(invalid_size)?;
                    let rotated = image::imageops::rotate270(&landscape);
                    Ok(image::imageops::flip_horizontal(&rotated).into())
                }
            }
        }
    }

    /// Scale an image to the virtual page size. Returns it as a PNG and as decoded pixels.
    fn scale_and_encode(image: &DynamicImage) -> Result<(Vec<u8>, RgbImage)> {
        debug!("Resizing image to {}x{}", VIRTUAL_WIDTH, VIRTUAL_HEIGHT);
        let scaled = image.resize_exact(VIRTUAL_WIDTH, VIRTUAL_HEIGHT, image::imageops::FilterType::Nearest);

        debug!("Encoding resized image");
        let mut png_data = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png_data).write_image(scaled.as_bytes(), VIRTUAL_WIDTH, VIRTUAL_HEIGHT, scaled.color().into())?;

        Ok((png_data, scaled.to_rgb8()))
    }

    fn apply_curves(value: u8) -> u8 {
        let normalized = value as f32 / 255.0;
        let adjusted = if normalized < 0.045 {
            0.0
        } else if normalized < 0.06 {
            (normalized - 0.045) / (0.06 - 0.045)
        } else {
            1.0
        };
        (adjusted * 255.0) as u8
    }

    pub fn save_image(&self, filename: &str) -> Result<()> {
        match &self.mode {
            ScreenshotMode::Simulated { simulator } => {
                simulator.save_image(filename)?;
                debug!("Simulated PNG image saved to {}", filename);
            }
            ScreenshotMode::Real { data, .. } => {
                std::fs::write(filename, data)?;
                debug!("PNG image saved to {}", filename);
            }
        }
        Ok(())
    }

    pub fn base64(&self) -> Result<String> {
        match &self.mode {
            ScreenshotMode::Simulated { simulator } => simulator.get_base64_image(),
            ScreenshotMode::Real { data, .. } => Ok(general_purpose::STANDARD.encode(data)),
        }
    }

    /// Return the (r, g, b) pixel value at virtual coordinate (vx, vy) in the 768×1024 space.
    /// Returns None if no screenshot has been taken or the coordinate is off the page.
    pub fn get_pixel(&self, vx: u32, vy: u32) -> Option<(u8, u8, u8)> {
        match &self.mode {
            ScreenshotMode::Real { pixels, .. } => {
                let pixel = pixels.as_ref()?.get_pixel_checked(vx, vy)?;
                Some((pixel[0], pixel[1], pixel[2]))
            }
            ScreenshotMode::Simulated { simulator } => simulator.get_pixel(vx, vy),
        }
    }
}
