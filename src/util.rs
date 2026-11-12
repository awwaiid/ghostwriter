use anyhow::{Context, Result};
use image::GrayImage;
use log::{debug, info, warn};
use resvg::tiny_skia::Pixmap;
use resvg::usvg::{self, fontdb, Options, Tree};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use crate::device::{firmware_version, DeviceModel};
use crate::embedded_assets::get_uinput_module_data;

pub type OptionMap = HashMap<String, String>;

/// Parse an SVG, with the system fonts available for any text in it.
pub fn parse_svg(svg_data: &str) -> Result<Tree> {
    let mut fontdb = fontdb::Database::new();
    fontdb.load_system_fonts();
    let options = Options {
        fontdb: Arc::new(fontdb),
        ..Options::default()
    };
    Ok(Tree::from_str(svg_data, &options)?)
}

/// Render an SVG scaled to fill a `width` x `height` pixmap.
/// An SVG that fails to parse is replaced by a small "ERROR!" marker.
fn render_svg(svg_data: &str, width: u32, height: u32) -> Result<Pixmap> {
    let tree = match parse_svg(svg_data) {
        Ok(tree) => tree,
        Err(e) => {
            info!("Error parsing SVG: {}. Using fallback SVG.", e);
            let fallback_svg = format!(
                r#"<svg width='{width}' height='{height}' xmlns='http://www.w3.org/2000/svg'><text x='100' y='900' font-family='Noto Sans' font-size='24'>ERROR!</text></svg>"#
            );
            parse_svg(&fallback_svg)?
        }
    };

    render_tree(&tree, width, height)
}

/// Render a parsed SVG scaled to fill a `width` x `height` pixmap.
fn render_tree(tree: &Tree, width: u32, height: u32) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(width, height).with_context(|| format!("Invalid bitmap size {width}x{height}"))?;
    // Scale so the SVG fills the requested bitmap size (not just its intrinsic size)
    let svg_size = tree.size();
    let transform = usvg::Transform::from_scale(width as f32 / svg_size.width(), height as f32 / svg_size.height());
    resvg::render(tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

/// Render an SVG to rows of alpha values (0-255).
pub fn svg_to_alpha_bitmap(svg_data: &str, width: u32, height: u32) -> Result<Vec<Vec<u8>>> {
    Ok(alpha_rows(&render_svg(svg_data, width, height)?))
}

/// Render an SVG that has been parsed to rows of alpha values (0-255), scaled to fill `width` x `height`.
pub fn tree_to_alpha_bitmap(tree: &Tree, width: u32, height: u32) -> Result<Vec<Vec<u8>>> {
    Ok(alpha_rows(&render_tree(tree, width, height)?))
}

fn alpha_rows(pixmap: &Pixmap) -> Vec<Vec<u8>> {
    pixmap
        .pixels()
        .chunks(pixmap.width() as usize)
        .map(|row| row.iter().map(|p| p.alpha()).collect())
        .collect()
}

/// Render an SVG to rows of on/off pixels; a pixel is on when its alpha is above `threshold`.
pub fn svg_to_bitmap_threshold(svg_data: &str, width: u32, height: u32, threshold: u8) -> Result<Vec<Vec<bool>>> {
    let alpha_bitmap = svg_to_alpha_bitmap(svg_data, width, height)?;
    Ok(alpha_bitmap
        .into_iter()
        .map(|row| row.into_iter().map(|alpha| alpha > threshold).collect())
        .collect())
}

/// Render an SVG to rows of on/off pixels.
pub fn svg_to_bitmap(svg_data: &str, width: u32, height: u32) -> Result<Vec<Vec<bool>>> {
    svg_to_bitmap_threshold(svg_data, width, height, 128)
}

pub fn write_bitmap_to_file(bitmap: &[Vec<bool>], filename: &str) -> Result<()> {
    let width = bitmap.first().map_or(0, |row| row.len());
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

/// Make sure the kernel's uinput device is available, since the virtual keyboard needs it.
///
/// Recent firmware has uinput built in or shipped as a loadable module. Older Paper Pro
/// firmware has neither, so modules built for those versions are bundled in the binary.
pub fn setup_uinput() -> Result<()> {
    let device_model = DeviceModel::detect();
    info!("Device model detected: {}", device_model.name());

    if device_model == DeviceModel::Remarkable2 {
        info!("Device is Remarkable2, skipping uinput module check and installation");
        return Ok(());
    }

    if Path::new("/dev/uinput").exists() {
        info!("/dev/uinput exists, skipping module loading");
        return Ok(());
    }

    if command_output("lsmod", &[]).is_some_and(|modules| modules.contains("uinput")) {
        debug!("uinput module already loaded");
        return Ok(());
    }

    // The firmware's own module, if it ships one
    if command_output("modprobe", &["uinput"]).is_some() {
        info!("Loaded the system uinput module with modprobe");
        return Ok(());
    }
    if let Some(module_path) = command_output("find", &["/usr/lib/modules", "-name", "uinput.ko"]).and_then(|found| found.lines().next().map(str::to_string)) {
        if command_output("insmod", &[&module_path]).is_some() {
            info!("Loaded the system uinput module from {}", module_path);
            return Ok(());
        }
    }

    if device_model == DeviceModel::RemarkablePaperPro {
        if let Some((major, minor)) = firmware_version() {
            let version = format!("{major}.{minor}");
            if let Some(module_data) = get_uinput_module_data(&version) {
                info!("Installing the bundled uinput module for firmware {}", version);
                std::fs::write("/tmp/uinput.ko", module_data)?;
                let output = Command::new("insmod").arg("/tmp/uinput.ko").output()?;
                info!("insmod output: {}", String::from_utf8_lossy(&output.stderr));
                return Ok(());
            }
            warn!("No bundled uinput module for firmware {}", version);
        }
    }

    warn!("uinput is not available; typing text with the virtual keyboard will not work");
    Ok(())
}

/// Run a command and return its stdout, or None if it could not be run or failed.
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
