use anyhow::Result;
use image::GrayImage;
use log::debug;
use std::fs::File;
use std::io::Write;
use std::io::{Read, Seek};
use std::process;

use base64::{engine::general_purpose, Engine as _};
use image::ImageEncoder;

const WIDTH: usize = 1872;
const HEIGHT: usize = 1404;
const BYTES_PER_PIXEL: usize = 2;
const WINDOW_BYTES: usize = WIDTH * HEIGHT * BYTES_PER_PIXEL;

const REMARKABLE_WIDTH: u32 = 1404;
const REMARKABLE_HEIGHT: u32 = 1872;

const OUTPUT_WIDTH: u32 = 768;
const OUTPUT_HEIGHT: u32 = 1024;

pub struct Screenshot {
    data: Vec<u8>,
}

impl Screenshot {
    pub fn new() -> Result<Screenshot> {
        let screenshot_data = Self::take_screenshot()?;
        Ok(Screenshot {
            data: screenshot_data,
        })
    }

    fn take_screenshot() -> Result<Vec<u8>> {
        // Find xochitl's process
        let pid = Self::find_xochitl_pid()?;

        // Find framebuffer location in memory
        let skip_bytes = Self::find_framebuffer_address(&pid)?;

        // Read the framebuffer data
        let screenshot_data = Self::read_framebuffer(&pid, skip_bytes)?;

        // Process the image data (transpose, color correction, etc.)
        let processed_data = Self::process_image(screenshot_data)?;

        Ok(processed_data)
    }

    fn find_xochitl_pid() -> Result<String> {
        let output = process::Command::new("pidof").arg("xochitl").output()?;
        let pids = String::from_utf8(output.stdout)?;
        for pid in pids.split_whitespace() {
            let has_fb = process::Command::new("grep")
                .args(&["-C1", "/dev/fb0", &format!("/proc/{}/maps", pid)])
                .output()?;
            if !has_fb.stdout.is_empty() {
                return Ok(pid.to_string());
            }
        }
        anyhow::bail!("No xochitl process with /dev/fb0 found")
    }

    fn find_framebuffer_address(pid: &str) -> Result<u64> {
        let output = process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "grep -C1 '/dev/fb0' /proc/{}/maps | tail -n1 | sed 's/-.*$//'",
                pid
            ))
            .output()?;
        let address_hex = String::from_utf8(output.stdout)?.trim().to_string();
        let address = u64::from_str_radix(&address_hex, 16)?;
        Ok(address + 7)
    }

    fn read_framebuffer(pid: &str, skip_bytes: u64) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; WINDOW_BYTES];
        let mut file = std::fs::File::open(format!("/proc/{}/mem", pid))?;
        file.seek(std::io::SeekFrom::Start(skip_bytes))?;
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn process_image(data: Vec<u8>) -> Result<Vec<u8>> {
        // Encode the raw data to PNG
        let png_data = Self::encode_png(&data)?;

        // Resize the PNG to OUTPUT_WIDTH x OUTPUT_HEIGHT
        let img = image::load_from_memory(&png_data)?;
        let resized_img = img.resize(
            OUTPUT_WIDTH,
            OUTPUT_HEIGHT,
            image::imageops::FilterType::Lanczos3,
        );

        // Encode the resized image back to PNG
        let mut resized_png_data = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut resized_png_data);
        encoder.write_image(
            resized_img.as_luma8().unwrap().as_raw(),
            OUTPUT_WIDTH,
            OUTPUT_HEIGHT,
            image::ExtendedColorType::L8,
        )?;

        Ok(resized_png_data)
    }

    fn encode_png(raw_data: &[u8]) -> Result<Vec<u8>> {
        let raw_u8: Vec<u8> = raw_data
            .chunks_exact(2)
            .map(|chunk| u8::from_le_bytes([chunk[1]]))
            .collect();

        let mut processed = vec![0u8; (REMARKABLE_WIDTH * REMARKABLE_HEIGHT) as usize];

        for y in 0..REMARKABLE_HEIGHT {
            for x in 0..REMARKABLE_WIDTH {
                let src_idx =
                    (REMARKABLE_HEIGHT - 1 - y) + (REMARKABLE_WIDTH - 1 - x) * REMARKABLE_HEIGHT;
                let dst_idx = y * REMARKABLE_WIDTH + x;
                processed[dst_idx as usize] = Self::apply_curves(raw_u8[src_idx as usize]);
            }
        }

        let img = GrayImage::from_raw(REMARKABLE_WIDTH as u32, REMARKABLE_HEIGHT as u32, processed)
            .ok_or_else(|| anyhow::anyhow!("Failed to create image from raw data"))?;

        let mut png_data = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
        encoder.write_image(
            img.as_raw(),
            REMARKABLE_WIDTH as u32,
            REMARKABLE_HEIGHT as u32,
            image::ExtendedColorType::L8,
        )?;

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
        let mut png_file = File::create(filename)?;
        png_file.write_all(&self.data)?;
        debug!("PNG image saved to {}", filename);
        Ok(())
    }

    pub fn base64(&self) -> Result<String> {
        let base64_image = general_purpose::STANDARD.encode(&self.data);
        Ok(base64_image)
    }
}
