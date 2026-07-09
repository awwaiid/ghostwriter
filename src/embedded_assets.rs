use rust_embed::Embed;

#[derive(Embed)]
#[folder = "prompts/"]
pub struct AssetPrompts;

#[derive(Embed)]
#[folder = "fonts/"]
pub struct AssetFonts;

#[derive(Embed)]
#[folder = "utils/"]
#[include = "rmpp/uinput-*"]
pub struct AssetUtils;

// Function to provide access to the uinput module data
pub fn get_uinput_module_data(version: &str) -> Option<Vec<u8>> {
    let target_module_filename = format!("rmpp/uinput-{}.ko", version);
    AssetUtils::get(target_module_filename.as_str()).map(|asset| asset.data.to_vec())
}

pub fn load_config(filename: &str) -> String {
    log::debug!("Loading config from {}", filename);

    if std::path::Path::new(filename).exists() {
        std::fs::read_to_string(filename).unwrap()
    } else {
        std::str::from_utf8(AssetPrompts::get(filename).unwrap().data.as_ref()).unwrap().to_string()
    }
}

pub fn load_font_asset(filename: &str) -> String {
    log::debug!("Loading font asset {}", filename);

    if std::path::Path::new(filename).exists() {
        std::fs::read_to_string(filename).unwrap()
    } else {
        std::str::from_utf8(AssetFonts::get(filename).unwrap().data.as_ref()).unwrap().to_string()
    }
}
