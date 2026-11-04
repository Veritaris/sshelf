//! Writes the macOS icon set for the `.app` bundle: the app icon (the same
//! shape as the window, tray and `.exe` icons, `src/icon_shape.rs`) at every
//! size an `.icns` holds, with Apple's margin around the body. The release
//! workflow turns the folder into `sshelf.icns` with `iconutil`.
//!
//! ```text
//! cargo run -p sshelf --example iconset -- sshelf.iconset
//! iconutil -c icns sshelf.iconset -o sshelf.icns
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

#[path = "../src/icon_shape.rs"]
mod icon_shape;

/// The point sizes of an icon set; each at @1x and @2x.
const POINTS: [u32; 5] = [16, 32, 128, 256, 512];
/// The icon body's share of the canvas (Apple's grid: 824 of 1024).
const BODY: f32 = 824.0 / 1024.0;

fn main() -> ExitCode {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: iconset <output folder, e.g. sshelf.iconset>");
        return ExitCode::FAILURE;
    };
    if let Err(e) = write_set(&dir) {
        eprintln!("iconset: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn write_set(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for points in POINTS {
        for scale in [1, 2] {
            let size = points * scale;
            let name = if scale == 1 {
                format!("icon_{points}x{points}.png")
            } else {
                format!("icon_{points}x{points}@2x.png")
            };
            let path = dir.join(name);
            let file =
                std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            png(std::io::BufWriter::new(file), &padded(size), size)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// The icon body centred on a transparent `size` × `size` canvas.
fn padded(size: u32) -> Vec<u8> {
    let body = ((size as f32 * BODY).round() as u32).max(1);
    let pixels = icon_shape::draw(body, icon_shape::COLORED, None);
    let margin = (size - body) / 2;
    let mut canvas = vec![0u8; (size * size * 4) as usize];
    for y in 0..body {
        let from = (y * body * 4) as usize;
        let to = (((y + margin) * size + margin) * 4) as usize;
        canvas[to..to + (body * 4) as usize]
            .copy_from_slice(&pixels[from..from + (body * 4) as usize]);
    }
    canvas
}

fn png(out: impl std::io::Write, rgba: &[u8], size: u32) -> Result<(), png::EncodingError> {
    let mut encoder = png::Encoder::new(out, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()
}
