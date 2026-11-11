use anyhow::{Context, Result};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "prompts/"]
pub struct AssetPrompts;

#[derive(Embed)]
#[folder = "utils/"]
#[include = "rmpp/uinput-*"]
pub struct AssetUtils;

// Function to provide access to the uinput module data
pub fn get_uinput_module_data(version: &str) -> Option<Vec<u8>> {
    let target_module_filename = format!("rmpp/uinput-{}.ko", version);
    AssetUtils::get(target_module_filename.as_str()).map(|asset| asset.data.to_vec())
}

/// Load a prompt or tool definition: a local file of that name if there is one,
/// otherwise the copy bundled into the binary from `prompts/`.
pub fn load_config(filename: &str) -> Result<String> {
    log::debug!("Loading config from {}", filename);

    if std::path::Path::new(filename).exists() {
        std::fs::read_to_string(filename).with_context(|| format!("Failed to read {filename}"))
    } else {
        let asset = AssetPrompts::get(filename).with_context(|| format!("No file or bundled prompt named '{filename}'"))?;
        Ok(String::from_utf8(asset.data.into_owned())?)
    }
}
