//! `kotlin`, the Kotlin Toolchain command line.
//!
//! This executable is not the compiler. `krusty` remains the kotlinc drop-in; `kotlin build` drives
//! that compiler over a `module.yaml` / `project.yaml` project.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(krusty_kotlin::run(&args));
}
