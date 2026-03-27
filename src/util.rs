use anyhow::Result;
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
    render(&tree, usvg::Transform::default(), &mut pixmap.as_mut());

    let bitmap = pixmap
        .pixels()
        .chunks(width as usize)
        .map(|row| row.iter().map(|p| p.alpha() > 128).collect())
        .collect();

    Ok(bitmap)
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

pub fn option_or_env(options: &OptionMap, key: &str, env_key: &str) -> String {
    let option = options.get(key);
    if let Some(value) = option {
        value.to_string()
    } else {
        std::env::var(env_key).unwrap().to_string()
    }
}

pub fn option_or_env_fallback(options: &OptionMap, key: &str, env_key: &str, fallback: &str) -> String {
    let option = options.get(key);
    if let Some(value) = option {
        value.to_string()
    } else {
        std::env::var(env_key).unwrap_or_else(|_| fallback.to_string())
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

    if device_model == DeviceModel::RemarkableMove {
        // Check if uinput module is loaded by looking at the lsmod output
        let output = std::process::Command::new("lsmod").output().expect("Failed to execute lsmod");
        let output_str = std::str::from_utf8(&output.stdout).unwrap();
        if output_str.contains("uinput") {
            debug!("uinput module already loaded");
            return Ok(());
        }
        // Try loading system module via modprobe
        info!("Move: attempting to load system uinput module");
        let output = std::process::Command::new("modprobe").arg("uinput").output();
        if let Ok(result) = output {
            if result.status.success() {
                info!("System uinput module loaded via modprobe");
                return Ok(());
            }
        }
        // Fall back to finding and loading the module directly
        let find_output = std::process::Command::new("find")
            .args(&["/usr/lib/modules", "-name", "uinput.ko"])
            .output();
        if let Ok(result) = find_output {
            let module_path = std::str::from_utf8(&result.stdout).unwrap().trim().to_string();
            if !module_path.is_empty() {
                info!("Loading uinput from: {}", module_path);
                let _ = std::process::Command::new("insmod").arg(&module_path).output();
                return Ok(());
            }
        }
        info!("Could not load uinput module for Move - keyboard input may not work");
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
