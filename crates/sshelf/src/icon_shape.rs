//! The app icon's shape at any size: a rounded Magenta square with three
//! dark shelf bars, and an optional status dot. Shared by `src/icon.rs` (the
//! window and tray icons) and `build.rs` (the `.exe` icon), so it uses no
//! crates; its colours are the palette's, checked by a test in `icon.rs`.

/// Magenta (`Palette::accent`).
pub const ACCENT: [u8; 3] = [198, 120, 221];
/// Black (`Palette::bg_dark`).
pub const BG_DARK: [u8; 3] = [33, 37, 43];

/// The icon's colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// The rounded square.
    pub body: [u8; 3],
    /// The shelf bars and the ring around the status dot; `None` cuts them
    /// out (see-through), as a monochrome icon has them.
    pub cut: Option<[u8; 3]>,
}

/// The app icon's own colours.
pub const COLORED: Look = Look {
    body: ACCENT,
    cut: Some(BG_DARK),
};

/// The shape is designed on a 32-unit grid.
const GRID: f32 = 32.0;
/// Samples per pixel along each axis (smooth edges at large sizes).
const SAMPLES: u32 = 4;

/// RGBA pixels (straight alpha), row by row, `size` × `size`, in `look`'s
/// colours, with `dot`'s colour in the status dot (none when `None`).
pub fn draw(size: u32, look: Look, dot: Option<[u8; 3]>) -> Vec<u8> {
    let scale = size as f32 / GRID;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // Premultiplied sums over the samples.
            let mut sum = [0.0f32; 4];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let offset = |i: u32| (i as f32 + 0.5) / SAMPLES as f32;
                    let gx = (x as f32 + offset(sx)) / scale;
                    let gy = (y as f32 + offset(sy)) / scale;
                    if let Some(color) = sample(gx, gy, look, dot) {
                        for (s, c) in sum.iter_mut().zip(color) {
                            *s += f32::from(c);
                        }
                        sum[3] += 255.0;
                    }
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let alpha = sum[3] / n;
            let straight = |c: f32| {
                if alpha > 0.0 {
                    (c / n * 255.0 / alpha).round() as u8
                } else {
                    0
                }
            };
            pixels.extend_from_slice(&[
                straight(sum[0]),
                straight(sum[1]),
                straight(sum[2]),
                alpha.round() as u8,
            ]);
        }
    }
    pixels
}

/// The colour at a point of the 32-unit grid, or `None` where it is
/// see-through (outside the shape, or a cut-out).
fn sample(x: f32, y: f32, look: Look, dot: Option<[u8; 3]>) -> Option<[u8; 3]> {
    if let Some(dot) = dot {
        let d = ((x - 25.0).powi(2) + (y - 25.0).powi(2)).sqrt();
        if d <= 5.0 {
            return Some(dot);
        }
        // A ring cuts the dot out of the square.
        if d <= 6.5 {
            return look.cut;
        }
    }
    // Rounded square, 1 unit margin, 6 unit corner radius.
    if !inside_rounded(x, y, 1.0, GRID - 1.0, 6.0) {
        return None;
    }
    let bar = (8.0..24.0).contains(&x)
        && [8.5, 14.5, 20.5]
            .iter()
            .any(|top| (*top..top + 3.0).contains(&y));
    if bar { look.cut } else { Some(look.body) }
}

fn inside_rounded(x: f32, y: f32, min: f32, max: f32, radius: f32) -> bool {
    if !(min..max).contains(&x) || !(min..max).contains(&y) {
        return false;
    }
    let cx = x.clamp(min + radius, max - radius);
    let cy = y.clamp(min + radius, max - radius);
    (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
}
