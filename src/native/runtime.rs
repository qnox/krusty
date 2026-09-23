//! The runtime every native program links against: freestanding C under `runtime/`, compiled once
//! per target when krusty itself is built (`build.rs`) and carried inside the compiler (the
//! prebuilt table the linker draws on). Its two headers are exposed here, so a test can compile a C
//! program against the runtime's own declarations and link it through krusty's linker.
//!
//! **It is freestanding: it does not use a C library.** That is not minimalism for its own sake —
//! it is what makes cross-compilation a property rather than a feature. Go's defining build
//! property is that `GOOS=linux GOARCH=arm64 go build` works on any machine with nothing installed,
//! because nothing in a Go binary needs a target C toolchain. A runtime that called `printf` would
//! need a target libc, its headers and a target linker for every architecture — the per-target
//! toolchain problem Go exists to avoid. Talking to the kernel directly removes it:
//! `clang --target=<triple>` compiles any registered architecture with no sysroot at krusty's build
//! time, and krusty's own linker joins the result with a program's objects, so one host produces
//! binaries for all of them and a user's build touches no C toolchain at all.
//!
//! Only the compiler-provided freestanding headers are used (`stdint.h`, `stddef.h`, `stdbool.h`),
//! which C11 §4 guarantees exist without a hosted implementation.
//!
//! The runtime is four translation units over two headers:
//!
//! * [`SYS_HEADER`] (`krusty_sys.h`) is the kernel interface — the syscall shim per architecture
//!   and the page-mapping primitives over it. It is the whole of the target-specific surface, a
//!   header of `static inline` functions so every file reaches the kernel the same way.
//! * [`HEADER`] (`krusty_rt.h`) is the contract generated code is written against.
//! * `krusty_rt.c` is the values: the built-in types, boxing, strings, collections, exceptions and
//!   `kotlin.io`. It allocates only through the collector.
//! * `krusty_fp.c` renders a floating-point value as Kotlin does — the shortest decimal that reads
//!   back as the same value — and computes the floating remainder.
//! * `krusty_gc.c` is the heap: allocator and a mark-sweep collector with conservative roots and a
//!   precisely traced heap. It knows nothing about any particular type; every object tells it,
//!   through its `KType` descriptor, which of its fields are references.
//! * `krusty_start.c` is `_start`, which with no C library the runtime must supply itself.
//!
//! **The descriptor is also the class.** `KType` names its superclass and carries the vtable, so
//! `is` walks the `super` chain and a method call is `obj->type->vtable[slot]` — the header stays
//! one word and the collector's contract stays `header->type`.
//!
//! `tests/native_runtime_e2e.rs` links C drivers against the runtime and runs them.

/// The kernel interface, shared by every translation unit of the runtime.
///
/// One syscall shim per supported architecture. Everything above it is portable C, which is why
/// adding an architecture is a matter of adding a register convention and four numbers rather
/// than porting a runtime.
pub const SYS_HEADER: &str = include_str!("runtime/krusty_sys.h");

/// The header a C program that links against the runtime includes.
pub const HEADER: &str = include_str!("runtime/krusty_rt.h");

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The two headers are complete on their own: a C program that includes nothing but them
    /// compiles for every target the runtime supports. A test elsewhere writes exactly these
    /// strings to disk and compiles against them, so a header that leaned on a file not exposed
    /// here would fail there, far from its cause.
    #[test]
    fn the_exposed_headers_compile_for_every_target() {
        let clang = Command::new("clang")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !clang {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI must compile the runtime headers, but it has no clang"
            );
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "krusty-runtime-headers-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).expect("create a scratch directory");
        std::fs::write(directory.join("krusty_sys.h"), SYS_HEADER).expect("write krusty_sys.h");
        std::fs::write(directory.join("krusty_rt.h"), HEADER).expect("write krusty_rt.h");
        let program = directory.join("program.c");
        std::fs::write(
            &program,
            "#include \"krusty_rt.h\"\n#include \"krusty_sys.h\"\n\
             void kt_program_entry(void) { kt_sys_write(1, \"OK\\n\", 3); }\n",
        )
        .expect("write the program");
        for triple in [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "riscv64-unknown-linux-gnu",
        ] {
            let output = Command::new("clang")
                .arg(format!("--target={triple}"))
                .args([
                    "-std=c11",
                    "-ffreestanding",
                    "-nostdlib",
                    "-fsyntax-only",
                    "-Wall",
                    "-Werror",
                ])
                .arg("-I")
                .arg(&directory)
                .arg(&program)
                .output()
                .expect("run clang");
            assert!(
                output.status.success(),
                "{triple}: a program including only the exposed headers must compile:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(&directory);
    }
}
