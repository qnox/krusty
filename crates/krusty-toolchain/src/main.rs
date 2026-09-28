//! `krusty-toolchain`, the Kotlin Toolchain command line.
//!
//! This executable is not the compiler. `krusty` remains the kotlinc drop-in; `krusty-toolchain build`
//! drives that compiler over a `module.yaml` / `project.yaml` project. The name is the toolchain, not
//! another "kotlin": `krusty` is already the Kotlin compiler.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(krusty_toolchain::run(&args));
}
