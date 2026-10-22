//! Compiles the settings window (ui/settings.slint).

fn main() {
    slint_build::compile("ui/settings.slint").expect("compiling the settings window");
}
