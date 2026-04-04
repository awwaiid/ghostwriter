use anyhow::Result;
use evdev::EventType as EvdevEventType;
use evdev::{Device, InputEvent};
use log::{debug, info, trace};

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::device::DeviceModel;

#[derive(Debug, Clone)]
pub enum TriggerCorner {
    UpperRight,
    UpperLeft,
    LowerRight,
    LowerLeft,
}

impl TriggerCorner {
    pub fn from_string(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "ur" | "upper-right" => Ok(TriggerCorner::UpperRight),
            "ul" | "upper-left" => Ok(TriggerCorner::UpperLeft),
            "lr" | "lower-right" => Ok(TriggerCorner::LowerRight),
            "ll" | "lower-left" => Ok(TriggerCorner::LowerLeft),
            _ => Err(anyhow::anyhow!(
                "Invalid trigger corner: {}. Use UR, UL, LR, LL, upper-right, upper-left, lower-right, or lower-left",
                s
            )),
        }
    }
}

// Output dimensions remain the same for both devices
const VIRTUAL_WIDTH: u16 = 768;
const VIRTUAL_HEIGHT: u16 = 1024;

// Event codes
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_TOUCH_MAJOR: u16 = 48;
const ABS_MT_TOUCH_MINOR: u16 = 49;
const ABS_MT_ORIENTATION: u16 = 52;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
// const ABS_MT_TOOL_TYPE: u16 = 55;
const ABS_MT_TRACKING_ID: u16 = 57;
const ABS_MT_PRESSURE: u16 = 58;

pub struct Touch {
    device: Option<Device>,
    device_model: DeviceModel,
    trigger_corner: TriggerCorner,
}

impl Touch {
    pub fn new(no_touch: bool, trigger_corner: TriggerCorner) -> Self {
        let device_model = DeviceModel::detect();
        info!("Touch using device model: {}", device_model.name());

        let device_path = match device_model {
            DeviceModel::Remarkable2 => "/dev/input/event2",
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperProMove => "/dev/input/event3",
            DeviceModel::Unknown => "/dev/input/event2", // Default to RM2
        };

        let device = if no_touch { None } else { Some(Device::open(device_path).unwrap()) };

        Self {
            device,
            device_model,
            trigger_corner,
        }
    }

    pub fn wait_for_trigger(&mut self) -> Result<()> {
        let mut position_x = 0;
        let mut position_y = 0;

        // Require multiple taps in the trigger zone to avoid accidental activation.
        const REQUIRED_TAPS: u8 = 3;
        const MAX_INTERVAL_BETWEEN_TAPS: Duration = Duration::from_millis(300);

        let mut tap_count: u8 = 0;
        let mut last_tap_at: Option<Instant> = None;
        loop {
            // Reset if too much time has passed since the last qualifying tap.
            if let Some(last) = last_tap_at {
                if last.elapsed() > MAX_INTERVAL_BETWEEN_TAPS {
                    tap_count = 0;
                    last_tap_at = None;
                }
            }

            // Store events in a temporary vector to avoid borrowing issues
            let mut events_to_process = Vec::new();
            if let Some(device) = &mut self.device {
                for event in device.fetch_events()? {
                    events_to_process.push(event);
                }
            }

            // Process the events after releasing the mutable borrow
            for event in events_to_process {
                if event.code() == ABS_MT_POSITION_X {
                    position_x = event.value();
                }
                if event.code() == ABS_MT_POSITION_Y {
                    position_y = event.value();
                }
                if event.code() == ABS_MT_TRACKING_ID && event.value() == -1 {
                    let (x, y) = self.input_to_virtual((position_x, position_y));
                    debug!("Touch release detected at ({}, {}) normalized ({}, {})", position_x, position_y, x, y);
                    if self.is_in_trigger_zone(x, y) {
                        let now = Instant::now();
                        if let Some(last) = last_tap_at {
                            if now.duration_since(last) <= MAX_INTERVAL_BETWEEN_TAPS {
                                tap_count = tap_count.saturating_add(1);
                            } else {
                                tap_count = 1;
                            }
                        } else {
                            tap_count = 1;
                        }
                        last_tap_at = Some(now);
                        debug!("Touch release in target zone (tap {}/{})", tap_count, REQUIRED_TAPS);

                        if tap_count >= REQUIRED_TAPS {
                            debug!("Trigger detected (triple-tap)");
                            return Ok(());
                        }
                    }
                    // Any release outside the trigger zone breaks the sequence.
                    else {
                        tap_count = 0;
                        last_tap_at = None;
                    }
                }
            }
        }
    }

    pub fn touch_start(&mut self, xy: (i32, i32)) -> Result<()> {
        let (x, y) = self.virtual_to_input(xy);
        if let Some(device) = &mut self.device {
            trace!("touch_start at ({}, {})", x, y);
            // sleep(Duration::from_millis(100));
            device.send_events(&[
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 1),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, x),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, y),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_PRESSURE, 100),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MAJOR, 17),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MINOR, 17),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_ORIENTATION, 4),
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
            sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    pub fn touch_stop(&mut self) -> Result<()> {
        if let Some(device) = &mut self.device {
            trace!("touch_stop");
            device.send_events(&[
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, -1),
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
            sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    pub fn goto_xy(&mut self, xy: (i32, i32)) -> Result<()> {
        let (x, y) = self.virtual_to_input(xy);
        if let Some(device) = &mut self.device {
            device.send_events(&[
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 1),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, x),
                InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, y),
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    pub fn tap_middle_bottom(&mut self) -> Result<()> {
        self.touch_start((384, 1023)).unwrap(); // middle bottom
        sleep(Duration::from_millis(100));
        self.touch_stop().unwrap();
        // sleep(Duration::from_millis(10));
        // sleep(Duration::from_millis(100));
        Ok(())
    }

    fn is_in_trigger_zone(&self, x: i32, y: i32) -> bool {
        const CORNER_SIZE: i32 = 68; // Size of the trigger zone (68x68 pixels)

        match self.trigger_corner {
            TriggerCorner::UpperRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::UpperLeft => x < CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::LowerRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
            TriggerCorner::LowerLeft => x < CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
        }
    }

    fn screen_width(&self) -> u32 {
        match self.device_model {
            DeviceModel::Remarkable2 => 1404,
            DeviceModel::RemarkablePaperPro => 2065,
            DeviceModel::RemarkablePaperProMove => 954,
            DeviceModel::Unknown => 1404, // Default to RM2
        }
    }

    fn screen_height(&self) -> u32 {
        match self.device_model {
            DeviceModel::Remarkable2 => 1872,
            DeviceModel::RemarkablePaperPro => 2833,
            DeviceModel::RemarkablePaperProMove => 1696,
            DeviceModel::Unknown => 1872, // Default to RM2
        }
    }

    fn virtual_to_input(&self, (x, y): (i32, i32)) -> (i32, i32) {
        // Swap and normalize the coordinates
        let x_normalized = x as f32 / VIRTUAL_WIDTH as f32;
        let y_normalized = y as f32 / VIRTUAL_HEIGHT as f32;

        match self.device_model {
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperProMove => {
                let x_input = (x_normalized * self.screen_width() as f32) as i32;
                let y_input = (y_normalized * self.screen_height() as f32) as i32;
                (x_input, y_input)
            }
            _ => {
                // RM2 coordinate transformation
                let x_input = (x_normalized * self.screen_width() as f32) as i32;
                let y_input = ((1.0 - y_normalized) * self.screen_height() as f32) as i32;
                (x_input, y_input)
            }
        }
    }

    fn input_to_virtual(&self, (x, y): (i32, i32)) -> (i32, i32) {
        // Swap and normalize the coordinates
        let x_normalized = x as f32 / self.screen_width() as f32;
        let y_normalized = y as f32 / self.screen_height() as f32;

        match self.device_model {
            DeviceModel::RemarkablePaperPro | DeviceModel::RemarkablePaperProMove => {
                let x_input = (x_normalized * VIRTUAL_WIDTH as f32) as i32;
                let y_input = (y_normalized * VIRTUAL_HEIGHT as f32) as i32;
                (x_input, y_input)
            }
            _ => {
                // RM2 coordinate transformation
                let x_input = (x_normalized * VIRTUAL_WIDTH as f32) as i32;
                let y_input = ((1.0 - y_normalized) * VIRTUAL_HEIGHT as f32) as i32;
                (x_input, y_input)
            }
        }
    }
}
