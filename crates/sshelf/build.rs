//! Windows: embeds the app icon and the version information that Explorer
//! shows in the file's Properties (description, product, version,
//! copyright). The icon is drawn by `src/icon_shape.rs`, the same shape as
//! the window and tray icons, at every size Windows asks for, and written
//! as an `.ico` into `OUT_DIR`.

use std::path::PathBuf;
use std::{env, fs};

#[path = "src/icon_shape.rs"]
mod icon_shape;

/// The sizes Explorer, the taskbar and Alt+Tab use at 100–200 % scaling.
const SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon_shape.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = out.join("sshelf.ico");
    fs::write(&icon, ico(&SIZES)).expect("writing the icon into OUT_DIR");

    let mut resource = winresource::WindowsResource::new();
    // FileVersion and ProductVersion come from the package version.
    resource
        .set_icon(icon.to_str().expect("OUT_DIR is valid UTF-8"))
        .set_language(0x0409)
        .set("FileDescription", "sshelf")
        .set("ProductName", "sshelf")
        .set("InternalName", "sshelf")
        .set("OriginalFilename", "sshelf.exe")
        .set("Comments", env!("CARGO_PKG_DESCRIPTION"))
        .set(
            "LegalCopyright",
            "Copyright \u{a9} 2026 Georgii Imeshkenov. MIT License.",
        );
    resource
        .compile()
        .expect("embedding the icon and version information");
}

/// An `.ico` file with one 32-bit image per size.
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&size| dib(size, &icon_shape::draw(size, icon_shape::COLORED, None)))
        .collect();
    let mut out = Vec::new();
    // ICONDIR: reserved, type 1 (icon), count.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&size, image) in sizes.iter().zip(&images) {
        // ICONDIRENTRY: 0 stands for 256.
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// One icon image: a BITMAPINFOHEADER (double height: colour + mask), the
/// BGRA rows bottom-up, then an empty AND mask (the alpha channel decides).
fn dib(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes()); // header size
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&(size as i32 * 2).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(size * size * 4).to_le_bytes());
    out.extend_from_slice(&[0; 16]); // resolution, palette
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    let mask_row = size.div_ceil(32) * 4;
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}
