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

/// A prompt or tool definition as it should read for a display that ghostwriter can
/// draw on in colour. The bundled ones are written for a black and white display; where
/// they say so in these words they are changed, and anything else is left as it is.
pub fn for_colour_display(text: &str) -> String {
    text.replace(
        "The display can only show black and white.",
        "The display shows colour. Draw in black as a rule. Where a colour helps, a shape's stroke or a text's fill can be one of these instead, by name: gray, blue, red, green, yellow, cyan, magenta. Do not use white.",
    )
    .replace("which can only display grayscale", "which displays colour")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_prompts_can_be_told_about_colour() {
        let tool = load_config("tool_draw_svg.json").unwrap();
        let coloured = for_colour_display(&tool);
        assert!(tool.contains("black and white") && !coloured.contains("black and white"));
        assert!(coloured.contains("magenta"));
        // Still the JSON it was
        assert!(serde_json::from_str::<serde_json::Value>(&coloured).is_ok());

        let prompt = load_config("general.json").unwrap();
        assert!(prompt.contains("grayscale") && !for_colour_display(&prompt).contains("grayscale"));
    }
}
