//! Draws Mail's app icon: a charcoal tile with an envelope, in the
//! window's own colours. No unread dot, so it never looks like new mail.
//!
//!     cargo run -p mail --example render_icon -- packaging/macos/icon-1024.png
//!
//! Shapes are signed distance functions, so every edge is anti-aliased
//! without a vector graphics library.

use image::{Rgba, RgbaImage};

const SIZE: u32 = 1024;
const TILE: [u8; 3] = [0x1e, 0x21, 0x24];
const PAPER: [u8; 3] = [0xec, 0xee, 0xf0];

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "packaging/macos/icon-1024.png".into());
    let mut icon = RgbaImage::new(SIZE, SIZE);
    for (x, y, pixel) in icon.enumerate_pixels_mut() {
        // Sample at the pixel centre.
        let p = (x as f32 + 0.5, y as f32 + 0.5);
        *pixel = shade(p);
    }
    icon.save(&path).expect("could not write the icon");
    println!("{path}");
}

/// The colour at one point, painting back to front.
fn shade(p: (f32, f32)) -> Rgba<u8> {
    // macOS icons sit on a 824px rounded tile with 100px of margin.
    let tile = coverage(rounded_rect(p, (512.0, 512.0), (412.0, 412.0), 185.0));
    if tile == 0.0 {
        return Rgba([0, 0, 0, 0]);
    }
    let mut colour = TILE.map(f32::from);

    // The envelope, centred on the tile: an outlined body and the flap's V.
    let stroke = 34.0;
    let body = rounded_rect(p, (512.0, 512.0), (250.0, 180.0), 42.0);
    let outline = coverage(body.abs() - stroke / 2.0);
    let flap_left = segment(p, (288.0, 362.0), (512.0, 527.0));
    let flap_right = segment(p, (736.0, 362.0), (512.0, 527.0));
    // Clipped at the outline's outer edge, so the two overlap without a seam.
    let flap = coverage(flap_left.min(flap_right) - stroke / 2.0) * coverage(body - stroke / 2.0);
    blend(&mut colour, PAPER, outline.max(flap));

    let [r, g, b] = colour.map(|channel| channel.round() as u8);
    Rgba([r, g, b, (tile * 255.0).round() as u8])
}

/// How much of a pixel lies inside a shape, from its signed distance.
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn blend(colour: &mut [f32; 3], over: [u8; 3], amount: f32) {
    for (channel, over) in colour.iter_mut().zip(over) {
        *channel += (f32::from(over) - *channel) * amount;
    }
}

fn rounded_rect(p: (f32, f32), centre: (f32, f32), half: (f32, f32), radius: f32) -> f32 {
    let qx = (p.0 - centre.0).abs() - half.0 + radius;
    let qy = (p.1 - centre.1).abs() - half.1 + radius;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

fn segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (pa, ba) = ((p.0 - a.0, p.1 - a.1), (b.0 - a.0, b.1 - a.1));
    let t = ((pa.0 * ba.0 + pa.1 * ba.1) / (ba.0 * ba.0 + ba.1 * ba.1)).clamp(0.0, 1.0);
    ((pa.0 - ba.0 * t).powi(2) + (pa.1 - ba.1 * t).powi(2)).sqrt()
}
