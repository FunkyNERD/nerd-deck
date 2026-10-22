//! Embeds the device firmware when NERD_DECK_FIRMWARE names a merged image
//! (firmware/.pio/build/<env>/firmware-merged.bin, written by
//! firmware/merge_firmware.py). Its board and version are read from the
//! `nerd-deck-firmware:<board>:<version>;` marker the firmware carries
//! (firmware/src/protocol.cpp), so they always describe the image itself.
//! Without the variable the host builds with no firmware and never flashes.
//!
//! Also compiles the settings window (ui/settings.slint).

use std::path::PathBuf;
use std::{env, fs};

/// Must match firmware::BOARD.
const BOARD: &str = "wt32-sc01-plus";
const MARKER: &[u8] = b"nerd-deck-firmware:";

fn main() {
    slint_build::compile("ui/settings.slint").expect("compiling the settings window");

    println!("cargo::rerun-if-env-changed=NERD_DECK_FIRMWARE");

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("firmware.bin");
    let (image, version) = match env::var_os("NERD_DECK_FIRMWARE") {
        Some(path) => {
            let path = PathBuf::from(path);
            println!("cargo::rerun-if-changed={}", path.display());
            let image = fs::read(&path)
                .unwrap_or_else(|e| panic!("NERD_DECK_FIRMWARE: reading {}: {e}", path.display()));
            let (board, version) = identify(&image)
                .unwrap_or_else(|| panic!("{} has no nerd-deck firmware marker", path.display()));
            assert_eq!(
                board,
                BOARD,
                "{} is firmware for another board",
                path.display()
            );
            (image, version)
        }
        None => (Vec::new(), String::new()),
    };
    fs::write(&out, image).unwrap();
    println!("cargo::rustc-env=NERD_DECK_FIRMWARE_VERSION={version}");
}

/// Finds `nerd-deck-firmware:<board>:<version>;` and returns (board, version).
fn identify(image: &[u8]) -> Option<(String, String)> {
    let start = image.windows(MARKER.len()).position(|w| w == MARKER)? + MARKER.len();
    let len = image[start..].iter().take(64).position(|&b| b == b';')?;
    let id = std::str::from_utf8(&image[start..start + len]).ok()?;
    let (board, version) = id.split_once(':')?;
    Some((board.to_owned(), version.to_owned()))
}
