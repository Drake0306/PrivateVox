// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The same binary is the engine worker when started with `--engine-worker` (design §18.1).
    if std::env::args().nth(1).as_deref() == Some("--engine-worker") {
        std::process::exit(privatevox_engine::worker::run_worker());
    }
    privatevox_lib::run();
}
