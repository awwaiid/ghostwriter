use crate::touch::TriggerCorner;
use anyhow::Result;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    // Direct mapping to CLI args - no arbitrary grouping
    pub engine: Option<String>,
    pub engine_base_url: Option<String>,
    pub engine_api_key: Option<String>,
    pub model: String,
    pub prompt: String,
    pub no_submit: bool,
    pub no_draw: bool,
    pub no_svg: bool,
    pub no_keyboard: bool,
    pub no_draw_progress: bool,
    pub input_png: Option<String>,
    pub output_file: Option<String>,
    pub model_output_file: Option<String>,
    pub save_screenshot: Option<String>,
    pub save_bitmap: Option<String>,
    pub no_loop: bool,
    pub no_trigger: bool,
    pub apply_segmentation: bool,
    pub web_search: bool,
    pub thinking: bool,
    pub thinking_tokens: u32,
    pub log_level: String,
    pub trigger_corner: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            engine: None,
            engine_base_url: None,
            engine_api_key: None,
            model: "claude-sonnet-4-0".to_string(),
            prompt: "general.json".to_string(),
            no_submit: false,
            no_draw: false,
            no_svg: false,
            no_keyboard: false,
            no_draw_progress: false,
            input_png: None,
            output_file: None,
            model_output_file: None,
            save_screenshot: None,
            save_bitmap: None,
            no_loop: false,
            no_trigger: false,
            apply_segmentation: false,
            web_search: false,
            thinking: false,
            thinking_tokens: 5000,
            log_level: "info".to_string(),
            trigger_corner: "UR".to_string(),
        }
    }
}

impl Config {
    /// Load configuration using figment (file -> env -> CLI precedence)
    pub fn load<T: Serialize>(args: &T) -> Result<Self> {
        let config: Self = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::file(Self::config_path()?))
            .merge(Env::prefixed("GHOSTWRITER_"))
            .merge(Serialized::globals(args))
            .extract()
            .map_err(|e| anyhow::anyhow!("Configuration error: {}", e))?;

        config.validate()?;
        Ok(config)
    }

    /// Save current configuration to TOML file
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        log::info!("Saving config to {:?}", config_path);
        let content = toml::to_string_pretty(self).map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;

        std::fs::write(&config_path, content).map_err(|e| anyhow::anyhow!("Failed to write config file {:?}: {}", config_path, e))?;

        Ok(())
    }

    /// Get the config file path: ~/.ghostwriter.toml
    pub fn config_path() -> Result<std::path::PathBuf> {
        let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME environment variable not set"))?;
        Ok(std::path::Path::new(&home).join(".ghostwriter.toml"))
    }

    /// Validate the configuration and return any errors
    pub fn validate(&self) -> Result<()> {
        // Validate trigger corner
        TriggerCorner::from_string(&self.trigger_corner)?;

        // Validate log level
        match self.log_level.as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => {}
            _ => return Err(anyhow::anyhow!("Invalid log level: {}", self.log_level)),
        }

        // Validate thinking tokens
        if self.thinking_tokens == 0 {
            return Err(anyhow::anyhow!("thinking_tokens must be greater than 0"));
        }

        Ok(())
    }
}
