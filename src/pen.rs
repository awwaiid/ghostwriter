use anyhow::Result;
use evdev::{Device, EventType, InputEvent};
use log::{info, debug};
use std::thread::sleep;
use std::time::Duration;
use svg2polylines;

use crate::device::DeviceModel;

// Output dimensions remain the same for both devices
const VIRTUAL_WIDTH: u32 = 768;
const VIRTUAL_HEIGHT: u32 = 1024;

pub struct Pen {
    device: Option<Device>,
    device_model: DeviceModel,
}

impl Pen {
    pub fn new(no_draw: bool) -> Self {
        let device_model = DeviceModel::detect();
        info!("Pen using device model: {}", device_model.name());

        let pen_input_device = match device_model {
            DeviceModel::Remarkable2 => "/dev/input/event1",
            DeviceModel::RemarkablePaperPro => "/dev/input/event2",
            DeviceModel::Unknown => "/dev/input/event1", // Default to RM2
        };

        let device = if no_draw {
            None
        } else {
            Some(Device::open(pen_input_device).unwrap())
        };

        Self {
            device,
            device_model,
        }
    }

    pub fn draw_line_virtual(&mut self, p1: (i32, i32), p2: (i32, i32)) -> Result<()> {
        self.draw_line(self.virtual_to_input(p1), self.virtual_to_input(p2))
    }

    pub fn draw_line(&mut self, (x1, y1): (i32, i32), (x2, y2): (i32, i32)) -> Result<()> {
        // debug!("Drawing from ({}, {}) to ({}, {})", x1, y1, x2, y2);

        // We know this is a straight line
        // So figure out the length
        // Then divide it into enough steps to only go 10 units or so
        // Start at x1, y1
        // And then for each step add the right amount to x and y

        let length = ((x2 as f32 - x1 as f32).powf(2.0) + (y2 as f32 - y1 as f32).powf(2.0)).sqrt();
        // 5.0 is the maximum distance between points
        // If this is too small
        // let steps = (length / 5.0).ceil() as i32;
        let steps = length.ceil() as i32;
        let dx = (x2 - x1) / steps;
        let dy = (y2 - y1) / steps;
        debug!(
            "Drawing from ({}, {}) to ({}, {}) in {} steps",
            x1, y1, x2, y2, steps
        );

        // self.pen_up()?;
        // self.goto_xy((x1, y1))?;
        // self.pen_down()?;

        for i in 0..steps {
            let x = x1 + dx * i;
            let y = y1 + dy * i;
            self.goto_xy((x, y))?;
            if i % 200 == 0 {
                sleep(Duration::from_millis(1));
            }
            // trace!("Drawing to point at ({}, {})", x, y);
        }
            // sleep(Duration::from_millis(50));

        // self.pen_up()?;

        Ok(())
    }

    pub fn draw_bitmap(&mut self, bitmap: &Vec<Vec<bool>>) -> Result<()> {
        let mut is_pen_down = false;
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &pixel) in row.iter().enumerate() {
                if pixel {
                    if !is_pen_down {
                        self.goto_xy_virtual((x as i32, y as i32))?;
                        self.pen_down()?;
                        is_pen_down = true;
                        sleep(Duration::from_millis(1));
                    }
                    self.goto_xy_virtual((x as i32, y as i32))?;
                    self.goto_xy_virtual((x as i32 + 1, y as i32))?;
                } else if is_pen_down {
                    self.pen_up()?;
                    is_pen_down = false;
                    sleep(Duration::from_millis(1));
                }
            }
            self.pen_up()?;
            is_pen_down = false;
            sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    pub fn draw_svg(&mut self, svg: &str) -> Result<()> {
        // self.pen_up()?;
        let polylines = svg2polylines::parse(svg, 0.01, true).unwrap();
        debug!("Found {} polylines", polylines.len());
        for polyline in polylines {
            debug!("- {:?}", polyline);
            let previous_point = polyline[0];
            let mut previous_x = previous_point.x as i32;
            let mut previous_y = previous_point.y as i32;

            self.pen_up()?;
                    sleep(Duration::from_millis(10));
            self.goto_xy_virtual((previous_x, previous_y))?;
            self.pen_down()?;
                    sleep(Duration::from_millis(50));

            for point in polyline {
                debug!("Point: ({}, {})", point.x, point.y);
                let point_x = point.x as i32;
                let point_y = point.y as i32;
                debug!("Drawing from ({}, {}) to ({}, {})", previous_x, previous_y, point_x, point_y);
                if (previous_x != point_x) || (previous_y != point_y) {
                    self.draw_line_virtual((previous_x, previous_y), (point_x, point_y))?;
                } else {
                    debug!("Skipping because there is no line");
                }
                previous_x = point_x;
                previous_y = point_y;
            }

            self.pen_up()?;
        }
        Ok(())
    }

    // fn draw_dot(device: &mut Device, (x, y): (i32, i32)) -> Result<()> {
    //     // trace!("Drawing at ({}, {})", x, y);
    //     goto_xy(device, (x, y))?;
    //     pen_down(device)?;
    //
    //     // Wiggle a little bit
    //     for n in 0..2 {
    //         goto_xy(device, (x + n, y + n))?;
    //     }
    //
    //     pen_up(device)?;
    //
    //     // sleep for 5ms
    //     thread::sleep(time::Duration::from_millis(1));
    //
    //     Ok(())
    // }

    pub fn pen_down(&mut self) -> Result<()> {
        if let Some(device) = &mut self.device {
            device.send_events(&[
                InputEvent::new(EventType::KEY, 320, 1), // BTN_TOOL_PEN
                InputEvent::new(EventType::KEY, 330, 1), // BTN_TOUCH
                InputEvent::new(EventType::ABSOLUTE, 24, 2630), // ABS_PRESSURE (max pressure)
                InputEvent::new(EventType::ABSOLUTE, 25, 0), // ABS_DISTANCE
                InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    pub fn pen_up(&mut self) -> Result<()> {
        if let Some(device) = &mut self.device {
            device.send_events(&[
                InputEvent::new(EventType::ABSOLUTE, 24, 0), // ABS_PRESSURE
                InputEvent::new(EventType::ABSOLUTE, 25, 100), // ABS_DISTANCE
                InputEvent::new(EventType::KEY, 330, 0),     // BTN_TOUCH
                InputEvent::new(EventType::KEY, 320, 0),     // BTN_TOOL_PEN
                InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    pub fn goto_xy_virtual(&mut self, point: (i32, i32)) -> Result<()> {
        self.goto_xy(self.virtual_to_input(point))
    }

    pub fn goto_xy(&mut self, (x, y): (i32, i32)) -> Result<()> {
        // debug!("Going to ({}, {})", x, y);
        if let Some(device) = &mut self.device {
            device.send_events(&[
                InputEvent::new(EventType::ABSOLUTE, 0, x),        // ABS_X
                InputEvent::new(EventType::ABSOLUTE, 1, y),        // ABS_Y
                InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    pub fn max_x_value(&self) -> i32 {
        match self.device_model {
            DeviceModel::Remarkable2 => 15725,
            DeviceModel::RemarkablePaperPro => 11180,
            DeviceModel::Unknown => 15725, // Default to RM2
        }
    }

    pub fn max_y_value(&self) -> i32 {
        match self.device_model {
            DeviceModel::Remarkable2 => 20966,
            DeviceModel::RemarkablePaperPro => 15340,
            DeviceModel::Unknown => 20966, // Default to RM2
        }
    }

    fn virtual_to_input(&self, (x, y): (i32, i32)) -> (i32, i32) {
        // Swap and normalize the coordinates
        let x_normalized = x as f32 / VIRTUAL_WIDTH as f32;
        let y_normalized = y as f32 / VIRTUAL_HEIGHT as f32;

        match self.device_model {
            DeviceModel::RemarkablePaperPro => {
                let x_input = (x_normalized * (self.max_x_value() as f32)) as i32;
                let y_input = (y_normalized * (self.max_y_value() as f32)) as i32;
                (x_input, y_input)
            }
            _ => {
                let x_input = ((1.0 - y_normalized) * self.max_y_value() as f32) as i32;
                let y_input = (x_normalized * self.max_x_value() as f32) as i32;
                (x_input, y_input)
            }
        }
    }
}
