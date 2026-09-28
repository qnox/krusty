//! `krusty-kotlin`, the Kotlin Toolchain command line.
//!
//! This executable is not the compiler. `krusty` remains the kotlinc drop-in; `krusty-kotlin build`
//! drives that compiler over a `module.yaml` / `project.yaml` project. The name follows `krusty-lsp`:
//! upstream would call this `kotlin` beside `kotlinc`, and krusty does not take the `kotlinc` name.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(krusty_kotlin::run(&args));
}
