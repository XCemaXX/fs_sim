use std::iter;

use fs_api::driver::FsEntry;

fn make_registry() -> Vec<FsEntry> {
    iter::empty()
        .chain(fat::registrations())
        .chain(minix::registrations())
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    if let Err(e) = fs_gui::run(make_registry()) {
        eprintln!("fs_sim: cannot open a window: {e}");
        eprintln!("Check that a display is available (X11 or Wayland).");
        eprintln!("Under WSL, try: WAYLAND_DISPLAY= cargo run");
        std::process::exit(1);
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    fs_gui::run_web(make_registry());
}
