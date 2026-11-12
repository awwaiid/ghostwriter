use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use image::{DynamicImage, GrayImage, ImageEncoder, RgbImage, RgbaImage};
use log::{debug, info};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::process;
use std::sync::Mutex;

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

/// The xochitl process the screen buffer was last found in, and its address there.
/// Finding it takes a quarter of a second, and it stays put for as long as xochitl runs.
static SCREEN_BUFFER: Mutex<Option<(String, u64)>> = Mutex::new(None);

/// Reads single pixels of the live screen, for watching ink arrive without capturing the lot.
pub struct ScreenProbe {
    memory: File,
    address: u64,
    width: u32,
    height: u32,
}

impl ScreenProbe {
    /// Whether the pixel at (x, y), in page pixels, is dark. Off the page, or unreadable, is not dark.
    pub fn is_dark(&self, x: u32, y: u32) -> bool {
        self.brightness(x, y).is_some_and(|brightness| brightness < 128)
    }

    /// Whether the pixel at (x, y) has any ink to speak of on it: it need not be dark,
    /// only clearly not blank paper. The edge of a pen's line counts, and so does a
    /// stroke that came out thinner than expected.
    pub fn is_inked(&self, x: u32, y: u32) -> bool {
        self.brightness(x, y).is_some_and(|brightness| brightness < 200)
    }

    /// How bright the pixel at (x, y) is, from 0 for black to 255 for white. None off the page.
    fn brightness(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let mut pixel = [0u8; 4];
        let offset = (y as u64 * self.width as u64 + x as u64) * 4;
        // Blue, green, red, alpha
        self.memory.read_exact_at(&mut pixel, self.address + offset).ok()?;
        Some((pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 3)
    }
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
            DeviceModel::RemarkablePaperProMove => 2,
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

        let image = self.capture_full_resolution()?;
        let (png, scaled) = Self::scale_and_encode(&image)?;

        if let ScreenshotMode::Real { data, pixels, .. } = &mut self.mode {
            *data = png;
            *pixels = Some(scaled);
        }

        Ok(())
    }

    /// Capture the screen at the device's own resolution, upright.
    pub fn capture_full_resolution(&mut self) -> Result<DynamicImage> {
        debug!("screenshot: finding the screen buffer");
        let (pid, address) = self.locate_screen_buffer()?;

        debug!("screenshot: reading data");
        let framebuffer = self.read_framebuffer(&pid, address)?;

        debug!("screenshot: processing image");
        self.decode_framebuffer(framebuffer)
    }

    /// A probe for reading single pixels of the live screen.
    /// Only for screens stored upright with 4 bytes a pixel, which is the Paper Pro.
    pub fn probe(&mut self) -> Result<ScreenProbe> {
        anyhow::ensure!(
            matches!(self.device_model(), DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure),
            "Reading single pixels is only supported on the Paper Pro"
        );
        let (pid, address) = self.locate_screen_buffer()?;
        Ok(ScreenProbe {
            memory: File::open(format!("/proc/{}/mem", pid))?,
            address,
            width: self.screen_width(),
            height: self.screen_height(),
        })
    }

    /// The xochitl process, and the address of the screen buffer in its memory.
    fn locate_screen_buffer(&self) -> Result<(String, u64)> {
        let mut known = SCREEN_BUFFER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((pid, address)) = known.as_ref() {
            let still_xochitl = std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|name| name.trim() == "xochitl");
            let still_a_screen = File::open(format!("/proc/{pid}/mem")).is_ok_and(|memory| self.holds_opaque_pixels(&memory, *address));
            if still_xochitl && still_a_screen {
                return Ok((pid.clone(), *address));
            }
            debug!("The screen buffer is no longer where it was");
        }

        let pid = Self::find_xochitl_pid()?;
        let address = self.find_framebuffer_address(&pid)?;
        *known = Some((pid.clone(), address));
        Ok((pid, address))
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
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure | DeviceModel::RemarkablePaperProMove => {
                // arm64 devices: the approach from goMarkableStream's pointer_arm64.go
                let start_address = self.get_memory_range(pid)?;
                match self.calculate_frame_pointer(pid, start_address) {
                    Ok(frame_pointer) => Ok(frame_pointer),
                    Err(e) => {
                        // Firmware 3.29+: the screen buffer no longer directly follows the DRI mappings
                        debug!("Walking from the DRI mappings failed ({}), scanning for the screen buffer", e);
                        self.scan_for_screen_buffer(pid, start_address)
                    }
                }
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
        let mut file = File::open(format!("/proc/{}/mem", pid))?;

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

    /// Find the screen buffer by scanning xochitl's memory for an allocation of the right size.
    ///
    /// Since firmware 3.29 other allocations sit between the DRI mappings and the screen
    /// buffer, so the walk in `calculate_frame_pointer` no longer reaches it.
    fn scan_for_screen_buffer(&self, pid: &str, dri_end_address: u64) -> Result<u64> {
        let screen_size = self.screen_width() as u64 * self.screen_height() as u64 * self.bytes_per_pixel() as u64;
        let maps = std::fs::read_to_string(format!("/proc/{}/maps", pid))?;
        let mem = File::open(format!("/proc/{}/mem", pid))?;

        let candidates = Self::screen_buffer_candidates(&maps, screen_size, |address| {
            let mut header = [0u8; 16];
            mem.read_exact_at(&mut header, address).ok().map(|_| header)
        });
        debug!("Screen buffer candidates: {:x?}", candidates);

        // Other allocations can have the same size. The screen is made of opaque pixels,
        // and it is allocated just before the DRI buffers are mapped, which puts it at
        // the nearest address above them.
        let opaque: Vec<u64> = candidates.iter().copied().filter(|&address| self.holds_opaque_pixels(&mem, address)).collect();
        let candidates = if opaque.is_empty() { candidates } else { opaque };
        let nearest_above_dri = candidates.iter().copied().filter(|&address| address >= dri_end_address).min();

        let address = nearest_above_dri
            .or(candidates.first().copied())
            .context("No screen buffer found in xochitl's memory")?;
        debug!("Screen buffer found at {:#x}", address);
        Ok(address)
    }

    /// The addresses of every allocation in a process that could hold a screen of `screen_size` bytes.
    ///
    /// The screen buffer is one large glibc allocation with a mapping of its own, which puts
    /// its chunk header at the start of a page: 8 bytes in is the chunk size, a little more
    /// than the screen size, with the "mmapped" flag set. The kernel merges neighbouring
    /// anonymous mappings, so that page can be anywhere inside a mapping.
    ///
    /// `maps` is the content of /proc/pid/maps, and `read_header` reads 16 bytes of the
    /// process's memory at an address.
    fn screen_buffer_candidates(maps: &str, screen_size: u64, read_header: impl Fn(u64) -> Option<[u8; 16]>) -> Vec<u64> {
        const PAGE_SIZE: usize = 4096;
        const CHUNK_HEADER_SIZE: u64 = 16;
        const IS_MMAPPED: u64 = 0x2;
        // How much bigger than the screen the allocation may be
        const MAX_EXTRA_BYTES: u64 = 65536;

        let chunk_sizes = screen_size + CHUNK_HEADER_SIZE..=screen_size + MAX_EXTRA_BYTES;
        let mut candidates = Vec::new();

        for line in maps.lines() {
            // Each line is: start-end permissions offset device inode [path]
            let mut fields = line.split_whitespace();
            let (Some(range), Some(permissions)) = (fields.next(), fields.next()) else {
                continue;
            };
            let path = fields.nth(3);
            // malloc's memory is anonymous and read-write
            if !permissions.starts_with("rw") || path.is_some_and(|path| path != "[heap]") {
                continue;
            }
            let Some((Ok(start), Ok(end))) = range
                .split_once('-')
                .map(|(start, end)| (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16)))
            else {
                continue;
            };
            if end.saturating_sub(start) < screen_size {
                continue;
            }

            for page in (start..end - screen_size).step_by(PAGE_SIZE) {
                let Some(header) = read_header(page) else {
                    continue;
                };
                let size_field = u64::from_le_bytes([header[8], header[9], header[10], header[11], header[12], header[13], header[14], header[15]]);
                if size_field & IS_MMAPPED != 0 && chunk_sizes.contains(&(size_field & !7)) {
                    candidates.push(page + CHUNK_HEADER_SIZE);
                }
            }
        }

        candidates
    }

    /// Whether the memory at `address` looks like 32-bit pixels, going by their alpha bytes.
    /// Devices with other pixel formats have nothing to check, so anything passes.
    fn holds_opaque_pixels(&self, mem: &File, address: u64) -> bool {
        const SAMPLES: u64 = 1024;

        if self.bytes_per_pixel() != 4 {
            return true;
        }
        let pixel_count = self.screen_width() as u64 * self.screen_height() as u64;
        (0..SAMPLES).all(|sample| {
            let mut pixel = [0u8; 4];
            let pixel_address = address + (sample * pixel_count / SAMPLES) * 4;
            mem.read_exact_at(&mut pixel, pixel_address).is_ok() && pixel[3] == 0xff
        })
    }

    fn read_framebuffer(&self, pid: &str, skip_bytes: u64) -> Result<Vec<u8>> {
        let window_bytes = self.screen_width() as usize * self.screen_height() as usize * self.bytes_per_pixel();
        let mut buffer = vec![0u8; window_bytes];
        let mut file = File::open(format!("/proc/{}/mem", pid))?;
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
                // 32-bit pixels, stored blue first
                debug!("Decoding {}x{} BGRA framebuffer", width, height);
                let mut pixels = raw_data;
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                Ok(RgbaImage::from_raw(width, height, pixels).ok_or_else(invalid_size)?.into())
            }
            DeviceModel::RemarkablePaperProMove => {
                // 16 bits per pixel with the grey level in the low byte, already in portrait.
                let grey: Vec<u8> = raw_data.as_chunks::<2>().0.iter().map(|pixel| pixel[0]).collect();
                Ok(GrayImage::from_raw(width, height, grey).ok_or_else(invalid_size)?.into())
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
        // Averaging, not picking every other pixel: a pen line is only two pixels wide,
        // and picking can miss it altogether
        let scaled = image.thumbnail_exact(VIRTUAL_WIDTH, VIRTUAL_HEIGHT);

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

#[cfg(test)]
mod tests {
    use super::*;

    // Paper Pro screen: 1632 x 2154 pixels of 4 bytes
    const SCREEN_SIZE: u64 = 1632 * 2154 * 4;

    // From a Paper Pro on firmware 3.29: the screen buffer shares a mapping with a smaller
    // buffer below it, and an unrelated allocation of nearly the same size sits further up.
    const MAPS: &str = "\
aaaaea8c0000-aaaaeb3f8000 rw-p 00000000 00:00 0                          [heap]
ffff8d0e2000-ffff8d28f000 rw-s 00000000 00:06 234                        /dev/dri/card0
ffff8d28f000-ffff8d5ec000 rw-p 00000000 00:00 0
ffff8d6b0000-ffff8e780000 rw-p 00000000 00:00 0
ffff8e780000-ffff8e790000 ---p 00000000 00:00 0
ffff943aa000-ffff95120000 rw-p 00000000 00:00 0
ffff95130000-ffff95930000 rw-p 00000000 00:00 0
ffff96000000-ffff97000000 r--p 00000000 b3:02 1234                       /usr/lib/libQt6Gui.so.6
";

    /// The memory of that process: mmapped chunk headers at the given addresses, zeros elsewhere.
    fn read_header(address: u64) -> Option<[u8; 16]> {
        let chunk_size: u64 = match address {
            0xffff8d28f000 => 0x35cff0, // 8-bit buffer
            0xffff8d6b0000 => 0x35cff0, // another, merged into the next mapping
            0xffff8da0d000 => 0xd72ff0, // the screen
            0xffff943aa000 => 0xd75ff0, // same size class, but not pixels
            _ => 0,
        };
        let mut header = [0u8; 16];
        if chunk_size != 0 {
            header[8..].copy_from_slice(&(chunk_size | 0x2).to_le_bytes());
        }
        Some(header)
    }

    #[test]
    fn finds_screen_sized_allocations_anywhere_in_a_mapping() {
        let candidates = Screenshot::screen_buffer_candidates(MAPS, SCREEN_SIZE, read_header);

        assert_eq!(candidates, vec![0xffff8da0d010, 0xffff943aa010]);
    }

    #[test]
    fn finds_nothing_when_memory_cannot_be_read() {
        let candidates = Screenshot::screen_buffer_candidates(MAPS, SCREEN_SIZE, |_| None);

        assert!(candidates.is_empty());
    }
}
