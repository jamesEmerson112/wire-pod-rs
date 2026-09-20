//! Translation of `pkg/scripting/display.go`.

// taken from https://github.com/fforchino/vector-go-sdk

use image::{DynamicImage, GenericImageView};

pub fn convert_pixes_to_16_bit_rgb(
    r: u32,
    g: u32,
    b: u32,
    _a: u32,
    opacity_percentage: u16,
) -> u16 {
    let (mut red, mut green, mut blue) = ((r / 257) as u16, (g / 8193) as u16, (b / 257) as u16);

    // Go's uint16 multiply wraps where Rust's would panic in a debug build.
    red = red.wrapping_mul(opacity_percentage) / 100;
    green = green.wrapping_mul(opacity_percentage) / 100;
    blue = blue.wrapping_mul(opacity_percentage) / 100;

    //The format appears to be: 000bbbbbrrrrrggg

    let br: u16 = (blue & 0xF8) << 5; // 5 bits for blue  [8..12]
    let rr: u16 = red & 0xF8; // 5 bits for red   [3..7]
    let gr: u16 = green; // 3 bits for green [0..2]

    br | rr | gr
}

pub fn convert_pixels_to_raw_bitmap(image: &DynamicImage, opacity_percentage: i64) -> Vec<u16> {
    let (img_height, img_width) = (image.height(), image.width());
    let mut bitmap = vec![0u16; (img_width * img_height) as usize];

    for y in 0..img_height {
        for x in 0..img_width {
            let px = image.get_pixel(x, y);
            // Go's `color.Color.RGBA()` is 16 bit and alpha-premultiplied; this
            // widens the 8-bit channels the same way but does not premultiply,
            // so a partly transparent source pixel differs.
            let (r, g, b, a) = (
                u32::from(px[0]) * 257,
                u32::from(px[1]) * 257,
                u32::from(px[2]) * 257,
                u32::from(px[3]) * 257,
            );
            bitmap[(y * img_width + x) as usize] =
                convert_pixes_to_16_bit_rgb(r, g, b, a, opacity_percentage as u16);
        }
    }
    bitmap
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::*;

    #[test]
    fn the_three_primaries_land_in_their_own_bit_fields() {
        assert_eq!(convert_pixes_to_16_bit_rgb(65535, 0, 0, 65535, 100), 0x00F8);
        assert_eq!(convert_pixes_to_16_bit_rgb(0, 65535, 0, 65535, 100), 0x0007);
        assert_eq!(convert_pixes_to_16_bit_rgb(0, 0, 65535, 65535, 100), 0x1F00);
    }

    #[test]
    fn a_bitmap_is_row_major_and_scaled_by_the_opacity() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, Rgba([0, 0, 255, 255]));
        let image = DynamicImage::ImageRgba8(img);
        assert_eq!(
            convert_pixels_to_raw_bitmap(&image, 100),
            vec![0x00F8, 0x1F00]
        );
        assert_eq!(
            convert_pixels_to_raw_bitmap(&image, 50),
            vec![0x0078, 0x0F00]
        );
    }
}
