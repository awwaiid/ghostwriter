use anyhow::{Context, Result};
use dotenv;
use image::GrayImage;
use log::{debug, info};
use resvg::render;
use resvg::tiny_skia::Pixmap;
use resvg::usvg;
use resvg::usvg::{fontdb, Options, Tree};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use crate::device::DeviceModel;
use crate::embedded_assets::get_uinput_module_data;

pub type OptionMap = HashMap<String, String>;

pub fn svg_to_bitmap(svg_data: &str, width: u32, height: u32) -> Result<Vec<Vec<bool>>> {
    let mut opt = Options::default();
    let mut fontdb = fontdb::Database::new();
    fontdb.load_system_fonts();

    opt.fontdb = Arc::new(fontdb);

    let tree = match Tree::from_str(svg_data, &opt) {
        Ok(tree) => tree,
        Err(e) => {
            info!("Error parsing SVG: {}. Using fallback SVG.", e);
            let fallback_svg = format!(
                r#"<svg width='{width}' height='{height}' xmlns='http://www.w3.org/2000/svg'><text x='100' y='900' font-family='Noto Sans' font-size='24'>ERROR!</text></svg>"#
            );
            Tree::from_str(&fallback_svg, &opt)?
        }
    };

    let mut pixmap = Pixmap::new(width, height).unwrap();
    // Scale transform so the SVG fills the requested bitmap size (not just its intrinsic size)
    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let transform = usvg::Transform::from_scale(scale_x, scale_y);
    render(&tree, transform, &mut pixmap.as_mut());

    let bitmap = pixmap
        .pixels()
        .chunks(width as usize)
        .map(|row| row.iter().map(|p| p.alpha() > 128).collect())
        .collect();

    Ok(bitmap)
}

/// Same as svg_to_bitmap but with configurable alpha threshold.
pub fn svg_to_bitmap_threshold(svg_data: &str, width: u32, height: u32, threshold: u8) -> Result<Vec<Vec<bool>>> {
    let mut opt = Options::default();
    let mut fontdb = fontdb::Database::new();
    fontdb.load_system_fonts();
    opt.fontdb = Arc::new(fontdb);

    let tree = match Tree::from_str(svg_data, &opt) {
        Ok(tree) => tree,
        Err(e) => {
            info!("Error parsing SVG: {}. Using fallback SVG.", e);
            let fallback_svg = format!(
                r#"<svg width='{width}' height='{height}' xmlns='http://www.w3.org/2000/svg'><text x='100' y='900' font-family='Noto Sans' font-size='24'>ERROR!</text></svg>"#
            );
            Tree::from_str(&fallback_svg, &opt)?
        }
    };

    let mut pixmap = Pixmap::new(width, height).unwrap();
    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let transform = usvg::Transform::from_scale(scale_x, scale_y);
    render(&tree, transform, &mut pixmap.as_mut());

    let bitmap = pixmap
        .pixels()
        .chunks(width as usize)
        .map(|row| row.iter().map(|p| p.alpha() > threshold).collect())
        .collect();

    Ok(bitmap)
}

/// Same as svg_to_bitmap but returns alpha values (0-255) instead of boolean.
pub fn svg_to_alpha_bitmap(svg_data: &str, width: u32, height: u32) -> Result<Vec<Vec<u8>>> {
    let mut opt = Options::default();
    let mut fontdb = fontdb::Database::new();
    fontdb.load_system_fonts();
    opt.fontdb = Arc::new(fontdb);

    let tree = match Tree::from_str(svg_data, &opt) {
        Ok(tree) => tree,
        Err(e) => {
            info!("Error parsing SVG: {}. Using fallback SVG.", e);
            let fallback_svg = format!(
                r#"<svg width='{width}' height='{height}' xmlns='http://www.w3.org/2000/svg'><text x='100' y='900' font-family='Noto Sans' font-size='24'>ERROR!</text></svg>"#
            );
            Tree::from_str(&fallback_svg, &opt)?
        }
    };

    let mut pixmap = Pixmap::new(width, height).unwrap();
    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let transform = usvg::Transform::from_scale(scale_x, scale_y);
    render(&tree, transform, &mut pixmap.as_mut());

    let alpha_bitmap = pixmap
        .pixels()
        .chunks(width as usize)
        .map(|row| row.iter().map(|p| p.alpha()).collect())
        .collect();

    Ok(alpha_bitmap)
}

pub fn write_bitmap_to_file(bitmap: &[Vec<bool>], filename: &str) -> Result<()> {
    let width = bitmap[0].len();
    let height = bitmap.len();
    let mut img = GrayImage::new(width as u32, height as u32);

    for (y, row) in bitmap.iter().enumerate() {
        for (x, &pixel) in row.iter().enumerate() {
            img.put_pixel(x as u32, y as u32, image::Luma([if pixel { 0 } else { 255 }]));
        }
    }

    img.save(filename)?;
    info!("Bitmap saved to {}", filename);
    Ok(())
}

/// The option `key` if it was given, otherwise the environment variable `env_key`.
pub fn option_or_env(options: &OptionMap, key: &str, env_key: &str) -> Result<String> {
    match options.get(key) {
        Some(value) => Ok(value.clone()),
        None => std::env::var(env_key).with_context(|| format!("{env_key} is not set (and no '{key}' option was given)")),
    }
}

pub fn option_or_env_fallback(options: &OptionMap, key: &str, env_key: &str, fallback: &str) -> String {
    match options.get(key) {
        Some(value) => value.clone(),
        None => std::env::var(env_key).unwrap_or_else(|_| fallback.to_string()),
    }
}

pub fn setup_uinput() -> Result<()> {
    debug!("Checking for uinput module");

    // Use DeviceModel to detect the device type
    let device_model = DeviceModel::detect();
    info!("Device model detected: {}", device_model.name());

    if device_model == DeviceModel::Remarkable2 {
        info!("Device is Remarkable2, skipping uinput module check and installation");
        return Ok(());
    }

    // If /dev/uinput already exists, the kernel has uinput built in
    if std::path::Path::new("/dev/uinput").exists() {
        info!("/dev/uinput exists, kernel has uinput built in, skipping module loading");
        return Ok(());
    }

    // Check if uinput module is loaded by looking at the lsmod output
    let output = std::process::Command::new("lsmod").output().expect("Failed to execute lsmod");
    let output_str = std::str::from_utf8(&output.stdout).unwrap();
    if output_str.contains("uinput") {
        debug!("uinput module already loaded");
    } else {
        info!("uinput module not found, installing bundled version");

        let os_info_path = String::from("/etc/os-release");
        if std::path::Path::new(os_info_path.as_str()).exists() {
            dotenv::from_path(os_info_path)?;
        }

        let img_version = std::env::var("IMG_VERSION").unwrap_or_default();

        if img_version.is_empty() {
            return Ok(());
        }

        let short_version = img_version.split('.').take(2).collect::<Vec<&str>>().join(".");

        // let target_module_filename = format!("rmpp/uinput-{short_version}.ko");

        // Use the function from embedded_assets module to get the module data
        let uinput_module_data = get_uinput_module_data(&short_version).unwrap_or_else(|| panic!("Uinput module for version {} not found", short_version));
        let raw_uinput_module_data = uinput_module_data.as_slice();
        let mut uinput_module_file = std::fs::File::create("/tmp/uinput.ko")?;
        uinput_module_file.write_all(raw_uinput_module_data)?;
        uinput_module_file.flush()?;
        drop(uinput_module_file);
        let output = std::process::Command::new("insmod").arg("/tmp/uinput.ko").output()?;
        let output_str = std::str::from_utf8(&output.stderr).unwrap();
        info!("insmod output: {}", output_str);
    }

    Ok(())
}
