//! Writes the background of the macOS `.dmg` window: a light canvas with a
//! Magenta arrow from where sshelf.app sits to where Applications sits, and
//! "Drag sshelf to Applications" under it. 1× and @2x; the release workflow
//! joins them with `tiffutil` and lays the window out with `dmgbuild`
//! (`assets/macos/dmg-settings.py`, whose icon positions match these).
//!
//! ```text
//! cargo run -p sshelf --example dmg_background -- <output folder>
//! ```

use std::path::Path;
use std::process::ExitCode;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

/// The window's content size in points (`dmg-settings.py`'s `window_rect`).
const WIDTH: u32 = 640;
const HEIGHT: u32 = 400;
/// Icon centres (`icon_locations`), the arrow between them, the text line.
const LEFT: f32 = 170.0;
const RIGHT: f32 = 470.0;
const ICON_Y: f32 = 180.0;
const TEXT_Y: f32 = 330.0;

const BACKGROUND: [u8; 3] = [245, 246, 248];
/// One Dark Magenta, as the app icon.
const ACCENT: [u8; 3] = [198, 120, 221];
/// One Dark comment grey.
const TEXT: [u8; 3] = [92, 99, 112];
const TEXT_SIZE: f32 = 15.0;
const CAPTION: &str = "Drag sshelf to Applications";

fn main() -> ExitCode {
    let Some(dir) = std::env::args_os().nth(1) else {
        eprintln!("usage: dmg_background <output folder>");
        return ExitCode::FAILURE;
    };
    let dir = Path::new(&dir);
    let result = std::fs::create_dir_all(dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            write(&dir.join("background.png"), 1)?;
            write(&dir.join("background@2x.png"), 2)
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dmg_background: {e}");
            ExitCode::FAILURE
        }
    }
}

fn write(path: &Path, scale: u32) -> Result<(), String> {
    let (w, h) = (WIDTH * scale, HEIGHT * scale);
    let mut canvas = Canvas::new(w, h, BACKGROUND);
    canvas.arrow(scale as f32);
    canvas.caption(scale as f32)?;
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writer
        .write_image_data(&canvas.pixels)
        .map_err(|e| format!("{}: {e}", path.display()))
}

struct Canvas {
    width: u32,
    height: u32,
    /// RGB, row by row.
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32, fill: [u8; 3]) -> Self {
        let pixels = fill
            .iter()
            .copied()
            .cycle()
            .take((width * height * 3) as usize)
            .collect();
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Mixes `color` into pixel (`x`, `y`) by `coverage` (0..=1).
    fn blend(&mut self, x: u32, y: u32, color: [u8; 3], coverage: f32) {
        if x >= self.width || y >= self.height || coverage <= 0.0 {
            return;
        }
        let i = ((y * self.width + x) * 3) as usize;
        let a = coverage.min(1.0);
        for (c, new) in self.pixels[i..i + 3].iter_mut().zip(color) {
            *c = (f32::from(*c) * (1.0 - a) + f32::from(new) * a).round() as u8;
        }
    }

    /// The arrow between the icons: a rounded shaft and a head, smoothed by
    /// 4 × 4 samples per pixel.
    fn arrow(&mut self, scale: f32) {
        let (from, to) = (LEFT + 84.0, RIGHT - 84.0);
        let half = 3.0;
        let head = 16.0;
        let inside = |x: f32, y: f32| -> bool {
            let dy = (y - ICON_Y).abs();
            // Shaft with round ends, up to the head's base.
            let shaft_end = to - head;
            let cx = x.clamp(from, shaft_end);
            let shaft = (x - cx).powi(2) + dy.powi(2) <= half * half;
            // Head: a triangle pointing right.
            let tip = to;
            let into = tip - x;
            let head = (0.0..=head).contains(&into) && dy <= into * 0.75;
            shaft || head
        };
        let (x0, x1) = ((from - 8.0) * scale, (to + 8.0) * scale);
        let (y0, y1) = ((ICON_Y - 16.0) * scale, (ICON_Y + 16.0) * scale);
        for py in y0 as u32..y1 as u32 {
            for px in x0 as u32..x1 as u32 {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / scale;
                        let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / scale;
                        hits += u32::from(inside(x, y));
                    }
                }
                self.blend(px, py, ACCENT, hits as f32 / 16.0);
            }
        }
    }

    /// The caption, centred under the arrow, in Inter SemiBold.
    fn caption(&mut self, scale: f32) -> Result<(), String> {
        let font = FontRef::try_from_slice(include_bytes!("../assets/fonts/Inter-SemiBold.ttf"))
            .map_err(|e| e.to_string())?;
        let scaled = font.as_scaled(PxScale::from(TEXT_SIZE * scale));
        let ids: Vec<_> = CAPTION.chars().map(|c| font.glyph_id(c)).collect();
        let mut width = 0.0;
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                width += scaled.kern(ids[i - 1], *id);
            }
            width += scaled.h_advance(*id);
        }
        let mut x = (WIDTH as f32 * scale - width) / 2.0;
        let baseline = TEXT_Y * scale;
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                x += scaled.kern(ids[i - 1], *id);
            }
            let glyph = id.with_scale_and_position(scaled.scale(), point(x, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    let px = bounds.min.x as i64 + i64::from(gx);
                    let py = bounds.min.y as i64 + i64::from(gy);
                    if px >= 0 && py >= 0 {
                        self.blend(px as u32, py as u32, TEXT, coverage);
                    }
                });
            }
            x += scaled.h_advance(*id);
        }
        Ok(())
    }
}
