use anyhow::Result;
use image::GrayImage;
use log::{debug, info};
use std::fs::File;
use std::io::Write;
use std::io::{Read, Seek};
use std::process;

use base64::{engine::general_purpose, Engine as _};
use image::{GenericImageView, ImageEncoder};

use crate::device::DeviceModel;
use crate::simulation::{ScreenshotSimulator, SimulationConfig};

const VIRTUAL_WIDTH: u32 = 768;
const VIRTUAL_HEIGHT: u32 = 1024;

pub enum ScreenshotMode {
    Real { data: Vec<u8>, device_model: DeviceModel },
    Simulated { simulator: ScreenshotSimulator },
}

pub struct Screenshot {
    mode: ScreenshotMode,
}

impl Screenshot {
    pub fn new() -> Result<Screenshot> {
        let device_model = DeviceModel::detect();
        info!("Screen detected device: {}", device_model.name());
        Ok(Screenshot {
            mode: ScreenshotMode::Real { data: vec![], device_model },
        })
    }

    pub fn new_simulated(simulation_config: SimulationConfig) -> Result<Screenshot> {
        let simulator = ScreenshotSimulator::new(simulation_config)?;
        info!("Screen using simulation mode");
        Ok(Screenshot {
            mode: ScreenshotMode::Simulated { simulator },
        })
    }

    fn screen_width(&self) -> u32 {
        let device_model = match &self.mode {
            ScreenshotMode::Real { device_model, .. } => device_model,
            ScreenshotMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::Remarkable2 => 1872,
            DeviceModel::RemarkablePaperPro => 1632,
            DeviceModel::RemarkablePaperPure => 1404,
            DeviceModel::Unknown => 1872, // Default to RM2
        }
    }

    fn screen_height(&self) -> u32 {
        let device_model = match &self.mode {
            ScreenshotMode::Real { device_model, .. } => device_model,
            ScreenshotMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::Remarkable2 => 1404,
            DeviceModel::RemarkablePaperPro => 2154,
            DeviceModel::RemarkablePaperPure => 1872,
            DeviceModel::Unknown => 1404, // Default to RM2
        }
    }

    pub fn bytes_per_pixel(&self) -> usize {
        let device_model = match &self.mode {
            ScreenshotMode::Real { device_model, .. } => device_model,
            ScreenshotMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::Remarkable2 => Self::detect_rm2_bytes_per_pixel(),
            DeviceModel::RemarkablePaperPro => 4,
            DeviceModel::RemarkablePaperPure => 4,
            DeviceModel::Unknown => 2, // Default to RM2
        }
    }

    // Returns (major, minor) firmware version from /etc/os-release IMG_VERSION field.
    fn detect_rm2_firmware_version() -> (u32, u32) {
        if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
            for line in content.lines() {
                if line.starts_with("IMG_VERSION=") {
                    let value = line.trim_start_matches("IMG_VERSION=").trim_matches('"');
                    let parts: Vec<&str> = value.split('.').collect();
                    if parts.len() >= 2 {
                        let major = parts[0].parse::<u32>().unwrap_or(0);
                        let minor = parts[1].parse::<u32>().unwrap_or(0);
                        debug!("RM2 firmware version: {}.{}", major, minor);
                        return (major, minor);
                    }
                }
            }
        }
        (0, 0)
    }

    // Firmware 3.24+ changed RM2 framebuffer from 16-bit (2 bpp) to 32-bit BGRA (4 bpp).
    fn detect_rm2_bytes_per_pixel() -> usize {
        let (major, minor) = Self::detect_rm2_firmware_version();
        if major > 3 || (major == 3 && minor >= 24) {
            4
        } else {
            2
        }
    }

    // Memory offset within the post-fb0 mapping where the current framebuffer data starts.
    // Reference: goMarkableStream internal/remarkable/detect.go
    fn detect_rm2_pointer_offset() -> u64 {
        let (major, minor) = Self::detect_rm2_firmware_version();
        if major > 3 || (major == 3 && minor >= 24) {
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

        // For real mode, handle separately to avoid borrowing issues
        // Find xochitl's process
        debug!("screenshot: finding pid");
        let pid = Self::find_xochitl_pid()?;

        // Find framebuffer location in memory
        debug!("screenshot: finding address");
        let skip_bytes = self.find_framebuffer_address(&pid)?;

        // Read the framebuffer data
        debug!("screenshot: reading data");
        let screenshot_data = self.read_framebuffer(&pid, skip_bytes)?;

        // Process the image data (transpose, color correction, etc.)
        debug!("screenshot: processing image");
        let processed_data = self.process_image(screenshot_data)?;

        // Update the data
        if let ScreenshotMode::Real { data, .. } = &mut self.mode {
            *data = processed_data;
        }

        Ok(())
    }

    fn find_xochitl_pid() -> Result<String> {
        let output = process::Command::new("pidof").arg("xochitl").output()?;
        let pids = String::from_utf8(output.stdout)?;
        if let Some(pid) = pids.split_whitespace().next() {
            return Ok(pid.to_string());
            // let has_fb = process::Command::new("grep")
            //     .args(&["-C1", "/dev/fb0", &format!("/proc/{}/maps", pid)])
            //     .output()?;
            // if !has_fb.stdout.is_empty() {
            //     return Ok(pid.to_string());
            // }
        }
        anyhow::bail!("No xochitl process found")
    }

    fn find_framebuffer_address(&self, pid: &str) -> Result<u64> {
        let device_model = match &self.mode {
            ScreenshotMode::Real { device_model, .. } => device_model,
            ScreenshotMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure => {
                // For RMPP (arm64), we need to use the approach from pointer_arm64.go
                let start_address = self.get_memory_range(pid)?;
                let frame_pointer = self.calculate_frame_pointer(pid, start_address)?;
                Ok(frame_pointer)
            }
            _ => {
                // RM2: find the mapping after /dev/fb0 in /proc/pid/maps, then apply firmware offset.
                // Reference: goMarkableStream internal/remarkable/pointer.go
                let output = process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("grep -A1 '/dev/fb0' /proc/{}/maps | tail -n1 | sed 's/-.*$//'", pid))
                    .output()?;
                let address_hex = String::from_utf8(output.stdout)?.trim().to_string();
                let address = u64::from_str_radix(&address_hex, 16)?;
                let pointer_offset = Self::detect_rm2_pointer_offset();
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

    // Get memory range for RMPP based on goMarkableStream/pointer_arm64.go
    fn get_memory_range(&self, pid: &str) -> Result<u64> {
        let maps_file_path = format!("/proc/{}/maps", pid);
        debug!("screenshot: reading memory range from {}", maps_file_path);
        let maps_content = std::fs::read_to_string(&maps_file_path)?;

        let mut memory_range = String::new();
        debug!("Scanning for '/dev/dri/card0' in memory");
        for line in maps_content.lines() {
            if line.contains("/dev/dri/card0") {
                memory_range = line.to_string();
                debug!("Found memory range: {}", memory_range);
            }
        }

        if memory_range.is_empty() {
            anyhow::bail!("No mapping found for /dev/dri/card0");
        }

        debug!("Final memory range: {}", memory_range);
        let fields: Vec<&str> = memory_range.split_whitespace().collect();
        let range_field = fields[0];
        let start_end: Vec<&str> = range_field.split('-').collect();

        if start_end.len() != 2 {
            anyhow::bail!("Invalid memory range format");
        }

        let end = u64::from_str_radix(start_end[1], 16)?;
        debug!("range_field: {}\nstart_end: {}\nend: {}", range_field, start_end[1], end);
        Ok(end)
    }

    // Calculate frame pointer for RMPP based on goMarkableStream/pointer_arm64.go
    fn calculate_frame_pointer(&self, pid: &str, start_address: u64) -> Result<u64> {
        let mem_file_path = format!("/proc/{}/mem", pid);
        let mut file = std::fs::File::open(mem_file_path)?;

        let screen_size_bytes = self.screen_width() as u64 * self.screen_height() as u64 * self.bytes_per_pixel() as u64;

        let mut offset: u64 = 0;
        let mut length: u64 = 2;

        while length < screen_size_bytes {
            // debug!("looping while {} < {}", length, screen_size_bytes);
            offset += length - 2;

            // debug!("  ... trying {}", start_address + offset + 8);
            file.seek(std::io::SeekFrom::Start(start_address + offset + 8))?;
            let mut header = [0u8; 8];
            file.read_exact(&mut header)?;
            debug!("  ... header: {:?}", header);

            length = (header[0] as u64) | ((header[1] as u64) << 8) | ((header[2] as u64) << 16) | ((header[3] as u64) << 24);
            debug!("  ... length: {}", length);
            if length < 2 {
                anyhow::bail!("Invalid header length");
            }
        }

        Ok(start_address + offset)
    }

    fn read_framebuffer(&self, pid: &str, skip_bytes: u64) -> Result<Vec<u8>> {
        // println!("taking screenshot \n assumed dimensions {} w x {} h", self.screen_width(), self.screen_height());
        let window_bytes = self.screen_width() as usize * self.screen_height() as usize * self.bytes_per_pixel();
        let mut buffer = vec![0u8; window_bytes];
        let mut file = std::fs::File::open(format!("/proc/{}/mem", pid))?;
        file.seek(std::io::SeekFrom::Start(skip_bytes))?;
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn process_image(&self, data: Vec<u8>) -> Result<Vec<u8>> {
        // Encode the raw data to PNG
        debug!("Encoding raw image data to PNG");
        let png_data = self.encode_png(&data)?;

        // Resize the PNG to VIRTUAL_WIDTH x VIRTUAL_HEIGHT
        debug!("Resizing image to {}x{}", VIRTUAL_WIDTH, VIRTUAL_HEIGHT);
        let img = image::load_from_memory(&png_data)?;
        let resized_img = img.resize_exact(VIRTUAL_WIDTH, VIRTUAL_HEIGHT, image::imageops::FilterType::Nearest);

        // Encode the resized image back to PNG
        debug!("Re-encoding resized image");
        let mut resized_png_data = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut resized_png_data);

        // Handle different color types based on device
        let device_model = match &self.mode {
            ScreenshotMode::Real { device_model, .. } => device_model,
            ScreenshotMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure => {
                encoder.write_image(
                    resized_img.as_rgba8().unwrap().as_raw(),
                    VIRTUAL_WIDTH,
                    VIRTUAL_HEIGHT,
                    image::ExtendedColorType::Rgba8,
                )?;
            }
            _ => {
                encoder.write_image(
                    resized_img.as_luma8().unwrap().as_raw(),
                    VIRTUAL_WIDTH,
                    VIRTUAL_HEIGHT,
                    image::ExtendedColorType::L8,
                )?;
            }
        }

        Ok(resized_png_data)
    }

    fn encode_png(&self, raw_data: &[u8]) -> Result<Vec<u8>> {
        let device_model = match &self.mode {
            ScreenshotMode::Real { device_model, .. } => device_model,
            ScreenshotMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure => {
                // RMPP uses 32-bit RGBA format
                self.encode_png_rmpp(raw_data)
            }
            _ => {
                // RM2: 16-bit pre-3.24, 32-bit RGB32 on 3.24+
                self.encode_png_rm2(raw_data)
            }
        }
    }
    fn encode_png_rm2(&self, raw_data: &[u8]) -> Result<Vec<u8>> {
        let bpp = self.bytes_per_pixel();
        let mut png_data = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_data);

        if bpp == 4 {
            // Firmware 3.24+: data is stored portrait (1404×1872), 32-bit BGRA.
            // Values are already full 8-bit range — don't apply the old aggressive curve.
            // Convert with Rec. 601 luma rather than taking one channel: coloured ink (e.g. blue)
            // is near-white in its own channel and would vanish from the screenshot.
            let processed: Vec<u8> = raw_data
                .chunks_exact(4)
                .map(|chunk| ((chunk[2] as u32 * 299 + chunk[1] as u32 * 587 + chunk[0] as u32 * 114) / 1000) as u8)
                .collect();
            let img = GrayImage::from_raw(1404, 1872, processed).ok_or_else(|| anyhow::anyhow!("Failed to create image from raw data"))?;
            encoder.write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::L8)?;
        } else {
            // Pre-3.24: data is stored landscape (1872×1404), 16-bit RGB565. Take high byte.
            let processed: Vec<u8> = raw_data.chunks_exact(2).map(|chunk| Self::apply_curves(chunk[1])).collect();
            let img = GrayImage::from_raw(self.screen_width(), self.screen_height(), processed)
                .ok_or_else(|| anyhow::anyhow!("Failed to create image from raw data"))?;
            let rotated_img = image::imageops::rotate270(&img);
            let final_image = image::imageops::flip_horizontal(&rotated_img);
            encoder.write_image(final_image.as_raw(), final_image.width(), final_image.height(), image::ExtendedColorType::L8)?;
        }

        Ok(png_data)
    }

    fn encode_png_rmpp(&self, raw_data: &[u8]) -> Result<Vec<u8>> {
        let width = self.screen_width();
        let height = self.screen_height();
        let mut png_data = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
        debug!("Encoding {}x{} image", width, height);
        encoder.write_image(raw_data, width, height, image::ExtendedColorType::Rgba8)?;
        Ok(png_data)
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
                Ok(())
            }
            ScreenshotMode::Real { data, .. } => {
                let mut png_file = File::create(filename)?;
                png_file.write_all(data)?;
                debug!("PNG image saved to {}", filename);
                Ok(())
            }
        }
    }

    pub fn base64(&self) -> Result<String> {
        match &self.mode {
            ScreenshotMode::Simulated { simulator } => simulator.get_base64_image(),
            ScreenshotMode::Real { data, .. } => {
                let base64_image = general_purpose::STANDARD.encode(data);
                Ok(base64_image)
            }
        }
    }

    /// Return the (r, g, b) pixel value at virtual coordinate (vx, vy) in the 768×1024 space.
    /// Decodes the stored PNG on each call. Returns None if no screenshot data available.
    pub fn get_pixel(&self, vx: u32, vy: u32) -> Option<(u8, u8, u8)> {
        let data = match &self.mode {
            ScreenshotMode::Real { data, .. } if !data.is_empty() => data,
            ScreenshotMode::Simulated { simulator } => {
                return simulator.get_pixel(vx, vy);
            }
            _ => return None,
        };
        let img = image::load_from_memory(data).ok()?;
        let pixel = img.get_pixel(vx, vy);
        Some((pixel[0], pixel[1], pixel[2]))
    }
}
