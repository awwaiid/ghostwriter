use image::{GrayImage, Rgb, RgbImage};
use imageproc::contours::find_contours;
use imageproc::geometry::{contour_area, min_area_rect};
use log::trace;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Region {
    pub bounds: (u32, u32, u32, u32), // x, y, width, height
    pub area: u32,
    pub contour_points: Vec<(u32, u32)>,
}

#[derive(Debug, Serialize)]
pub struct SegmentationResult {
    pub regions: Vec<Region>,
    pub image_size: (u32, u32),
}

pub struct ImageAnalyzer {
    min_region_size: f32,
    max_regions: usize,
}

impl ImageAnalyzer {
    pub fn new(min_region_size: f32, max_regions: usize) -> Self {
        Self { min_region_size, max_regions }
    }

    pub fn analyze_image(&self, image_path: &str) -> Result<SegmentationResult, Box<dyn std::error::Error>> {
        trace!("Reading image from: {}", image_path);

        // Read image and convert to grayscale
        let img = image::open(image_path)?.to_rgb8();
        let (width, height) = img.dimensions();
        trace!("Image loaded: {}x{}", width, height);

        // Convert to grayscale
        let gray: GrayImage = image::imageops::grayscale(&img);

        // Simple thresholding
        let binary = gray.clone().into_raw().into_iter().map(|p| if p > 127 { 255 } else { 0 }).collect::<Vec<u8>>();
        let binary = GrayImage::from_raw(width, height, binary).ok_or("Failed to create binary image")?;

        // Find contours
        let contours = find_contours(&binary);
        trace!("Found {} contours", contours.len());

        // Process regions
        let mut regions = Vec::new();
        let min_area = (width * height) as f32 * self.min_region_size;

        for contour in contours {
            let area = contour_area(&contour.points) as f32;

            if area >= min_area {
                // The corners of the fitted rectangle can fall just outside the image, so clamp them
                let bounds = min_area_rect(&contour.points);
                let x_min = bounds.iter().map(|p| p.x).min().unwrap_or(0).max(0) as u32;
                let y_min = bounds.iter().map(|p| p.y).min().unwrap_or(0).max(0) as u32;
                let x_max = bounds.iter().map(|p| p.x).max().unwrap_or(0).max(0) as u32;
                let y_max = bounds.iter().map(|p| p.y).max().unwrap_or(0).max(0) as u32;
                let width = x_max - x_min;
                let height = y_max - y_min;

                let contour_points: Vec<(u32, u32)> = contour.points.iter().map(|p| (p.x as u32, p.y as u32)).collect();

                regions.push(Region {
                    bounds: (x_min, y_min, width, height),
                    area: area as u32,
                    contour_points,
                });
            }
        }

        // Sort by area and limit number of regions
        regions.sort_by(|a, b| b.area.partial_cmp(&a.area).unwrap());
        regions.truncate(self.max_regions);

        trace!("Processed {} significant regions", regions.len());

        Ok(SegmentationResult {
            regions,
            image_size: (width, height),
        })
    }

    pub fn generate_description(&self, result: &SegmentationResult) -> String {
        let mut description = format!(
            "Image size: {}x{}\nDetected {} regions:\n\n",
            result.image_size.0,
            result.image_size.1,
            result.regions.len()
        );

        for (i, region) in result.regions.iter().enumerate() {
            description.push_str(&format!(
                "Region {}:\n\
                 - Position: ({}, {})\n\
                 - Size: {}x{}\n\
                 - Area: {} pixels\n\
                 - Relative position: {:.2}%, {:.2}%\n\n",
                i + 1,
                region.bounds.0,
                region.bounds.1,
                region.bounds.2,
                region.bounds.3,
                region.area,
                (region.bounds.0 as f32 / result.image_size.0 as f32) * 100.0,
                (region.bounds.1 as f32 / result.image_size.1 as f32) * 100.0,
            ));
        }

        description
    }

    // Optional: Add a method to visualize the regions
    pub fn visualize_regions(&self, result: &SegmentationResult) -> Result<RgbImage, Box<dyn std::error::Error>> {
        let mut output = RgbImage::new(result.image_size.0, result.image_size.1);

        // Draw regions in different colors
        for (i, region) in result.regions.iter().enumerate() {
            let color = Rgb([((i * 90) % 255) as u8, ((i * 140) % 255) as u8, ((i * 200) % 255) as u8]);

            // Draw contour
            for point in &region.contour_points {
                if point.0 < output.width() && point.1 < output.height() {
                    output.put_pixel(point.0, point.1, color);
                }
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_stay_within_the_image() {
        // This image has a region whose fitted rectangle pokes past the image's edge
        let analyzer = ImageAnalyzer::new(0.001, 10);
        let result = analyzer.analyze_image("evaluations/tic_tac_toe_1/input.png").unwrap();

        assert!(!result.regions.is_empty());
        for region in &result.regions {
            let (x, y, _width, _height) = region.bounds;
            assert!(x < result.image_size.0 && y < result.image_size.1, "{:?}", region.bounds);
        }
    }
}
