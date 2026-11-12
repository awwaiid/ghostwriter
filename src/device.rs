use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Every device is normalized to this virtual page size: screenshots are scaled to it
/// before they go to the model, and the coordinates the model draws with are scaled
/// back from it to the device's real pen and touch ranges.
pub const VIRTUAL_WIDTH: u32 = 768;
pub const VIRTUAL_HEIGHT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DeviceModel {
    Remarkable2,
    RemarkablePaperPro,
    RemarkablePaperPure,
    RemarkablePaperProMove,
    Unknown,
}

/// How the pen and touch digitizers are oriented relative to the portrait page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputOrientation {
    /// Digitizer axes match the portrait page (Paper Pro family).
    Portrait,
    /// reMarkable 2: the touch y axis is flipped and the pen digitizer is rotated 90°.
    Rm2,
}

impl DeviceModel {
    pub fn from_string(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "rm2" | "remarkable2" | "remarkable-2" => Ok(DeviceModel::Remarkable2),
            "rmpp" | "remarkable-paper-pro" | "remarkablepaperpro" | "paperpro" | "ferrari" => Ok(DeviceModel::RemarkablePaperPro),
            "rmpure" | "tatsu" | "paperpure" => Ok(DeviceModel::RemarkablePaperPure),
            "rmppm" | "move" | "chiappa" | "paperpromove" => Ok(DeviceModel::RemarkablePaperProMove),
            _ => Err(anyhow::anyhow!("Invalid device model: {}. Use 'rm2', 'rmpp', 'rmpure' or 'move'", s)),
        }
    }

    /// Identify the device from the platform codename in /etc/hwrevision.
    pub fn detect() -> Self {
        let Ok(hwrev) = std::fs::read_to_string("/etc/hwrevision") else {
            return DeviceModel::Unknown;
        };

        if hwrev.contains("ferrari 1.0") {
            DeviceModel::RemarkablePaperPro
        } else if hwrev.contains("reMarkable2 1.0") {
            DeviceModel::Remarkable2
        } else if hwrev.contains("tatsu 1.0") {
            DeviceModel::RemarkablePaperPure
        } else if hwrev.contains("chiappa 1.0") {
            DeviceModel::RemarkablePaperProMove
        } else {
            DeviceModel::Unknown
        }
    }

    pub fn name(&self) -> &str {
        match self {
            DeviceModel::Remarkable2 => "Remarkable2",
            DeviceModel::RemarkablePaperPro => "RemarkablePaperPro",
            DeviceModel::RemarkablePaperPure => "RemarkablePaperPure",
            DeviceModel::RemarkablePaperProMove => "RemarkablePaperProMove",
            DeviceModel::Unknown => "Unknown",
        }
    }

    // The per-device values below treat an unknown device as a reMarkable 2.

    pub fn input_orientation(&self) -> InputOrientation {
        match self {
            DeviceModel::Remarkable2 | DeviceModel::Unknown => InputOrientation::Rm2,
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure | DeviceModel::RemarkablePaperProMove => InputOrientation::Portrait,
        }
    }

    /// Touchscreen input device.
    pub fn touch_device_path(&self) -> &'static str {
        match self {
            DeviceModel::Remarkable2 | DeviceModel::Unknown => "/dev/input/event2",
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure | DeviceModel::RemarkablePaperProMove => "/dev/input/event3",
        }
    }

    /// Pen (marker) input device.
    pub fn pen_device_path(&self) -> &'static str {
        match self {
            DeviceModel::Remarkable2 | DeviceModel::Unknown => "/dev/input/event1",
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperPure | DeviceModel::RemarkablePaperProMove => "/dev/input/event2",
        }
    }

    /// Range of the touchscreen's ABS_MT_POSITION_X / ABS_MT_POSITION_Y values, as (width, height).
    pub fn touch_size(&self) -> (u32, u32) {
        match self {
            DeviceModel::Remarkable2 | DeviceModel::Unknown => (1404, 1872),
            DeviceModel::RemarkablePaperPro => (2065, 2833),
            DeviceModel::RemarkablePaperPure => (1776, 2400),
            DeviceModel::RemarkablePaperProMove => (1248, 2208),
        }
    }

    /// Maximum ABS_X / ABS_Y values of the pen digitizer along the page's (width, height).
    pub fn pen_size(&self) -> (i32, i32) {
        match self {
            DeviceModel::Remarkable2 | DeviceModel::Unknown => (15725, 20966),
            DeviceModel::RemarkablePaperPro => (11180, 15340),
            DeviceModel::RemarkablePaperPure => (9620, 13000),
            DeviceModel::RemarkablePaperProMove => (6760, 11960),
        }
    }

    /// The (width, height) of the page in framebuffer pixels, the right way up.
    pub fn page_size(&self) -> (u32, u32) {
        match self {
            DeviceModel::Remarkable2 | DeviceModel::Unknown => (1404, 1872),
            _ => self.framebuffer_size(),
        }
    }

    /// The (width, height), in framebuffer pixels, of the area the pen digitizer's range covers,
    /// starting from the page's top-left corner.
    ///
    /// Usually that is the whole page. The Paper Pro's framebuffer has been 1632x2154 since
    /// firmware 3.20, but its pen still spans the panel's own 1620x2160. Measured on
    /// firmware 3.29 by drawing crosses and finding them in a capture: without this,
    /// strokes land up to 10 pixels left and 6 pixels low of where they were aimed.
    pub fn pen_area_size(&self) -> (u32, u32) {
        match self {
            DeviceModel::RemarkablePaperPro => (1620, 2160),
            _ => self.page_size(),
        }
    }

    /// Framebuffer (width, height) in pixels, as stored in xochitl's memory.
    /// The reMarkable 2 stores it in landscape before firmware 3.24.
    pub fn framebuffer_size(&self) -> (u32, u32) {
        match self {
            DeviceModel::Remarkable2 | DeviceModel::Unknown => (1872, 1404),
            DeviceModel::RemarkablePaperPro => (1632, 2154),
            DeviceModel::RemarkablePaperPure => (1404, 1872),
            // The panel is 954 pixels wide; rows are padded to 960.
            DeviceModel::RemarkablePaperProMove => (960, 1696),
        }
    }
}

/// Firmware (major, minor) version from IMG_VERSION in /etc/os-release, if it can be read.
pub fn firmware_version() -> Option<(u32, u32)> {
    parse_firmware_version(&std::fs::read_to_string("/etc/os-release").ok()?)
}

fn parse_firmware_version(os_release: &str) -> Option<(u32, u32)> {
    let value = os_release.lines().find_map(|line| line.strip_prefix("IMG_VERSION="))?;
    let mut parts = value.trim_matches('"').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_firmware_version_from_os_release() {
        let os_release = "ID=codex\nNAME=\"Codex Linux\"\nIMG_VERSION=\"3.29.1.0\"\n";
        assert_eq!(parse_firmware_version(os_release), Some((3, 29)));
        assert_eq!(parse_firmware_version("ID=ubuntu\n"), None);
        assert_eq!(parse_firmware_version("IMG_VERSION=\"garbage\"\n"), None);
    }

    #[test]
    fn from_string_accepts_each_device() {
        assert_eq!(DeviceModel::from_string("rm2").unwrap(), DeviceModel::Remarkable2);
        assert_eq!(DeviceModel::from_string("RMPP").unwrap(), DeviceModel::RemarkablePaperPro);
        assert_eq!(DeviceModel::from_string("tatsu").unwrap(), DeviceModel::RemarkablePaperPure);
        assert_eq!(DeviceModel::from_string("move").unwrap(), DeviceModel::RemarkablePaperProMove);
        assert!(DeviceModel::from_string("kindle").is_err());
    }
}
