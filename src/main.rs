use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use image::GrayImage;
use serde_json::json;
use std::fs::File;
use std::io::Write;
use std::io::{Read, Seek};
use std::{thread, time};

use clap::Parser;


const REMARKABLE_WIDTH: u32 = 1404;
const REMARKABLE_HEIGHT: u32 = 1872;


#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    no_submit: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let screenshot_data = take_screenshot()?;

    // Save the PNG image to a file
    let png_filename = "tmp/screenshot.png";
    let mut png_file = File::create(png_filename)?;
    png_file.write_all(&screenshot_data)?;
    println!("PNG image saved to {}", png_filename);

    let base64_image = general_purpose::STANDARD.encode(&screenshot_data);

    // Save the base64 encoded image to a file
    let base64_filename = "tmp/screenshot_base64.txt";
    let mut base64_file = File::create(base64_filename)?;
    base64_file.write_all(base64_image.as_bytes())?;
    println!("Base64 encoded image saved to {}", base64_filename);

    // Example: Draw a simple line
    // let points = vec![(100, 100), (200, 200), (300, 300)];
    // draw_on_screen()?;

    if args.no_submit {
        println!("Image not submitted to OpenAI due to --no-submit flag");
        return Ok(());
    }

    let api_key = std::env::var("OPENAI_API_KEY")?;
    let body = json!({
        "model": "gpt-4o-mini",
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "strict": true,
                "name": "svg_response",
                "schema": {
                    "type": "object",
                    "properties": {
                        "input_description": {
                            "type": "string",
                            "description": "Description of input, including interpretation of what is being asked and coordinate positions of interesting features"
                        },
                        "output_description": {
                            "type": "string",
                            "description": "Description of response, both in general and specifics about how the response can be represented in an SVG overlayed on the screen. Include specifics such as position in coordinates of response objects"
                        },
                        "svg": {
                            "type": "string",
                            "description": "An SVG in correct SVG format which will be drawn on top of the existing screen elements"
                        }
                    },
                    "required": [
                        "input_description",
                        "output_description",
                        "svg"
                    ],
                    "additionalProperties": false
                }
            }
        },
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "You are a helpful assistant. You live inside of a remarkable2 notepad, which has a 1404x1872 sized screen. Your input is the current content of the screen. Look at this content, interpret it, and respond to the content. The content will contain both handwritten notes and diagrams. Respond in the form of a JSON document which will explain the input, the output, and provide an actual svg, which we will draw onto the same screen, on top of the existing content."
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:image/png;base64,{}", base64_image)
                        }
                    }
                ]
            }
        ],
        "max_tokens": 3000
    });

    let response = ureq::post("https://api.openai.com/v1/chat/completions")
        .set("Authorization", &format!("Bearer {}", api_key))
        .set("Content-Type", "application/json")
        .send_json(&body);

    match response {
        Ok(response) => {
            let json: serde_json::Value = response.into_json()?;
            println!("API Response: {}", json);

            let raw_output = json["choices"][0]["message"]["content"].as_str().unwrap();
            let json_output = serde_json::from_str::<serde_json::Value>(raw_output)?;
            let input_description = json_output["input_description"].as_str().unwrap();
            let output_description = json_output["output_description"].as_str().unwrap();
            let svg_data = json_output["svg"].as_str().unwrap();

            println!("Input Description: {}", input_description);
            println!("Output Description: {}", output_description);
            println!("SVG Data: {}", svg_data);

            
        }
        Err(ureq::Error::Status(code, response)) => {
            println!("HTTP Error: {} {}", code, response.status_text());
            if let Ok(json) = response.into_json::<serde_json::Value>() {
                println!("Error details: {}", json);
            } else {
                println!("Failed to parse error response as JSON");
            }
            return Err(anyhow::anyhow!("API request failed"));
        }
        Err(e) => return Err(anyhow::anyhow!("Request failed: {}", e)),
    }
    Ok(())
}

use std::process::Command;

const WIDTH: usize = 1872;
const HEIGHT: usize = 1404;
const BYTES_PER_PIXEL: usize = 2;
const WINDOW_BYTES: usize = WIDTH * HEIGHT * BYTES_PER_PIXEL;
const INPUT_WIDTH: usize = 15725;
const INPUT_HEIGHT: usize = 20966;

fn take_screenshot() -> Result<Vec<u8>> {
    // Find xochitl's process
    let pid = find_xochitl_pid()?;

    // Find framebuffer location in memory
    let skip_bytes = find_framebuffer_address(&pid)?;

    // Read the framebuffer data
    let screenshot_data = read_framebuffer(&pid, skip_bytes)?;

    // Process the image data (transpose, color correction, etc.)
    let processed_data = process_image(screenshot_data)?;

    Ok(processed_data)
}

fn find_xochitl_pid() -> Result<String> {
    let output = Command::new("pidof").arg("xochitl").output()?;
    let pids = String::from_utf8(output.stdout)?;
    for pid in pids.split_whitespace() {
        let has_fb = Command::new("grep")
            .args(&["-C1", "/dev/fb0", &format!("/proc/{}/maps", pid)])
            .output()?;
        if !has_fb.stdout.is_empty() {
            return Ok(pid.to_string());
        }
    }
    anyhow::bail!("No xochitl process with /dev/fb0 found")
}

fn find_framebuffer_address(pid: &str) -> Result<u64> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "grep -C1 '/dev/fb0' /proc/{}/maps | tail -n1 | sed 's/-.*$//'",
            pid
        ))
        .output()?;
    let address_hex = String::from_utf8(output.stdout)?.trim().to_string();
    let address = u64::from_str_radix(&address_hex, 16)?;
    Ok(address + 7)
}

fn read_framebuffer(pid: &str, skip_bytes: u64) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; WINDOW_BYTES];
    let mut file = std::fs::File::open(format!("/proc/{}/mem", pid))?;
    file.seek(std::io::SeekFrom::Start(skip_bytes))?;
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

fn process_image(data: Vec<u8>) -> Result<Vec<u8>> {
    // Implement image processing here (transpose, color correction, etc.)
    // For now, we'll just encode the raw data to PNG
    encode_png(&data)
}

use image;

fn encode_png(raw_data: &[u8]) -> Result<Vec<u8>> {
    let raw_u8: Vec<u8> = raw_data.chunks_exact(2)
        .map(|chunk| u8::from_le_bytes([chunk[1]]))
        .collect();

    let mut processed = vec![0u8; (REMARKABLE_WIDTH * REMARKABLE_HEIGHT) as usize];

    for y in 0..REMARKABLE_HEIGHT {
        for x in 0..REMARKABLE_WIDTH {
            // let src_idx = y * REMARKABLE_WIDTH + x;
            // let dst_idx = x * REMARKABLE_HEIGHT + (REMARKABLE_HEIGHT - 1 - y);
            let src_idx = (REMARKABLE_HEIGHT - 1 - y) + (REMARKABLE_WIDTH - 1 - x) * REMARKABLE_HEIGHT;
            let dst_idx = y * REMARKABLE_WIDTH + x;
            processed[dst_idx as usize] = apply_curves(raw_u8[src_idx as usize]);
        }
    }

    let img = GrayImage::from_raw(REMARKABLE_WIDTH as u32, REMARKABLE_HEIGHT as u32, processed)
        .ok_or_else(|| anyhow::anyhow!("Failed to create image from raw data"))?;

    let mut png_data = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
    encoder.encode(
        img.as_raw(),
        REMARKABLE_WIDTH as u32,
        REMARKABLE_HEIGHT as u32,
        image::ColorType::L8
    )?;

    Ok(png_data)
}

fn apply_curves(value: u8) -> u8 {
    let normalized = value as f32 / 255.0;
    let adjusted = if normalized < 0.045 {
        0.0
    } else if normalized < 0.06 {
        (normalized - 0.045) / (0.06 - 0.045)
    } else {
        1.0
    };
    (adjusted * 255.0) as u8
}

use evdev::{Device, InputEvent, EventType, InputEventKind};

use std::os::unix::io::AsRawFd;


fn draw_line(device: &mut Device, (x1, y1): (i32, i32), (x2, y2): (i32, i32)) -> Result<()> {
    // println!("Drawing from ({}, {}) to ({}, {})", x1, y1, x2, y2);

    // We know this is a straight line
    // So figure out the length
    // Then divide it into enough steps to only go 10 units or so
    // Start at x1, y1
    // And then for each step add the right amount to x and y

    let length = ((x2 as f32 - x1 as f32).powf(2.0) + (y2 as f32 - y1 as f32).powf(2.0)).sqrt();
    // 5.0 is the maximum distance between points
    // If this is too small 
    let steps = (length / 5.0).ceil() as i32;
    let dx = (x2 - x1) / steps;
    let dy = (y2 - y1) / steps;
    // println!("Drawing from ({}, {}) to ({}, {}) in {} steps", x1, y1, x2, y2, steps);

    device.send_events(&[
        InputEvent::new(EventType::ABSOLUTE, 0, x1),     // ABS_X
        InputEvent::new(EventType::ABSOLUTE, 1, y1),     // ABS_Y

        InputEvent::new(EventType::KEY, 320, 1),         // BTN_TOOL_PEN
        InputEvent::new(EventType::KEY, 330, 1),         // BTN_TOUCH
        InputEvent::new(EventType::ABSOLUTE, 24, 2630),  // ABS_PRESSURE (max pressure)
        InputEvent::new(EventType::ABSOLUTE, 25, 0),  // ABS_DISTANCE
        InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
    ])?;

    for i in 0..steps {
        let x = x1 + dx * i;
        let y = y1 + dy * i;
        // println!("Drawing to point at ({}, {})", x, y);
        device.send_events(&[
            InputEvent::new(EventType::ABSOLUTE, 0, x),     // ABS_X
            InputEvent::new(EventType::ABSOLUTE, 1, y),     // ABS_Y
            InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
        ])?;
    }

    device.send_events(&[
        InputEvent::new(EventType::ABSOLUTE, 24, 0),  // ABS_PRESSURE (max pressure)
        InputEvent::new(EventType::KEY, 330, 0),         // BTN_TOUCH
        InputEvent::new(EventType::KEY, 320, 0),         // BTN_TOOL_PEN
        InputEvent::new(EventType::ABSOLUTE, 25, 100),  // ABS_DISTANCE
        InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
    ])?;

    Ok(())
}

fn draw_dot(device: &mut Device, (x, y): (i32, i32)) -> Result<()> {
    // println!("Drawing at ({}, {})", x, y);
    device.send_events(&[

        InputEvent::new(EventType::ABSOLUTE, 0, x),     // ABS_X
        InputEvent::new(EventType::ABSOLUTE, 1, y),     // ABS_Y
        InputEvent::new(EventType::KEY, 320, 1),         // BTN_TOOL_PEN
        InputEvent::new(EventType::KEY, 330, 1),         // BTN_TOUCH
        InputEvent::new(EventType::ABSOLUTE, 24, 2630),  // ABS_PRESSURE
        InputEvent::new(EventType::ABSOLUTE, 25, 0),  // ABS_DISTANCE
        InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
    ])?;

        for n in 0..10 {
         device.send_events(&[
        
        InputEvent::new(EventType::ABSOLUTE, 0, x+n),     // ABS_X
        InputEvent::new(EventType::ABSOLUTE, 1, y+n),     // ABS_Y
        InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
        ])?;
        }

        device.send_events(&[
        InputEvent::new(EventType::ABSOLUTE, 24, 0),  // ABS_PRESSURE
        InputEvent::new(EventType::ABSOLUTE, 25, 0),  // ABS_DISTANCE
        InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
        
        InputEvent::new(EventType::KEY, 330, 0),         // BTN_TOUCH
        InputEvent::new(EventType::KEY, 320, 1),         // BTN_TOOL_PEN
        InputEvent::new(EventType::SYNCHRONIZATION, 0, 0), // SYN_REPORT
        ])?;
    
    Ok(())
}


fn screen_to_input((x, y): (i32, i32)) -> (i32, i32) {
    // Swap and normalize the coordinates
    let x_normalized = x as f32 / REMARKABLE_WIDTH as f32;
    let y_normalized = y as f32 / REMARKABLE_HEIGHT as f32;

    let x_input = ((1.0 - y_normalized) * INPUT_HEIGHT as f32) as i32;
    let y_input = (x_normalized * INPUT_WIDTH as f32) as i32;
    (x_input, y_input)
}


fn draw_on_screen() -> Result<()> {
    let mut device = Device::open("/dev/input/event1")?; // Pen input device

    // draw_line(device, (10035, 6173), (12000, 7173))?;
    // draw_line(device, (17396, 1530), (14401,9494))?;
    // draw_line(&mut device, screen_to_input((200, 200)), screen_to_input((800,500)))?;
    // draw_line(&mut device, screen_to_input((200, 500)), screen_to_input((800,200)))?;

    // println!("Line 1");
    // draw_line(&mut device, screen_to_input((200, 1000)), screen_to_input((400,1000)))?;
    // println!("Line 2");
    // draw_line(&mut device, screen_to_input((400,1000)), screen_to_input((400,1200)))?;
    // println!("Line 3");
    // draw_line(&mut device, screen_to_input((400,1200)), screen_to_input((200,1200)))?;
    // println!("Line 4");
    // draw_line(&mut device, screen_to_input((200,1200)), screen_to_input((200,1000)))?;

    // for x in 0..100 {
    //     draw_line(&mut device, screen_to_input((200+x, 1000)), screen_to_input((200+x,1200)))?;
    // }

    for x in 0..100 {
        draw_dot(&mut device, screen_to_input((1000+x, 1000)))?;
    }
    // draw_dot(&mut device, screen_to_input((1000, 1000)))?;

    Ok(())
}
