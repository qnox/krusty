//! krusty's own static linker: relocatable ELF objects in, a static ELF executable out.
//!
//! Zero-toolchain cross-compilation — the property the whole native track is built on — needs the
//! final link done by krusty. Go has its own linker for exactly this reason. What is needed here is
//! bounded, and deliberately much less than a general linker: a *static* executable at a fixed
//! address, from a handful of objects (the program's, plus the runtime prebuilt with krusty), with
//! no shared libraries, no PIC, no TLS, no GOT or PLT. Symbols are resolved, sections are laid into
//! two loadable segments, relocations are applied, and an ELF header is written by hand.
//!
//! Inputs are parsed with the `object` crate; the executable is written directly, because an ELF64
//! header and two program headers are 176 bytes whose layout is fixed by the ABI, and owning that is
//! simpler than driving a general writer.

mod elf;
#[cfg(test)]
mod fixture;

pub use elf::runtime_symbols;

use super::prebuilt;
use super::target::{Arch, NativeTarget};

/// Why a link failed. Each names what a user (or the emitter's author) needs to act on.
#[derive(Debug)]
pub enum ProgramLinkError {
    /// krusty was built without a prebuilt runtime for this architecture (no C cross-compiler at
    /// build time), so there is nothing to link against.
    NoRuntime(Arch),
    /// An input object could not be parsed as an ELF relocatable, or is malformed.
    Parse(String),
    /// An input object is a well-formed relocatable for a different machine, word size or byte
    /// order than the target's.
    ForeignObject(String),
    /// A symbol was referenced and nothing defines it.
    UndefinedSymbol(String),
    /// Two objects define the same global.
    DuplicateSymbol(String),
    /// A relocation kind the linker does not implement for this architecture.
    UnsupportedRelocation { arch: Arch, r_type: u32 },
    /// A relocation's value does not fit its field: the image is laid out wrong for this code model.
    RelocationOutOfRange(String),
    /// A section or symbol shape the linker does not model.
    Unsupported(String),
}

impl std::fmt::Display for ProgramLinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRuntime(arch) => write!(
                formatter,
                "krusty was built without the native runtime for {arch:?} (no C cross-compiler was \
                 available when krusty was built; install clang and rebuild krusty)"
            ),
            Self::Parse(what) => write!(formatter, "cannot read object: {what}"),
            Self::ForeignObject(what) => write!(formatter, "wrong kind of object: {what}"),
            Self::UndefinedSymbol(name) => write!(formatter, "undefined symbol `{name}`"),
            Self::DuplicateSymbol(name) => write!(formatter, "symbol `{name}` is defined twice"),
            Self::UnsupportedRelocation { arch, r_type } => write!(
                formatter,
                "relocation type {r_type} is not implemented for {arch:?}"
            ),
            Self::RelocationOutOfRange(what) => {
                write!(formatter, "relocation out of range: {what}")
            }
            Self::Unsupported(what) => write!(formatter, "{what}"),
        }
    }
}

impl std::error::Error for ProgramLinkError {}

/// Link a program's objects with the prebuilt runtime for `target` into a static executable.
///
/// The returned bytes are a complete ELF file; the caller decides where to write it.
pub fn link_program(
    program_objects: &[&[u8]],
    target: NativeTarget,
) -> Result<Vec<u8>, ProgramLinkError> {
    let runtime =
        prebuilt::runtime_objects(target.arch).ok_or(ProgramLinkError::NoRuntime(target.arch))?;
    let mut inputs: Vec<&[u8]> = program_objects.to_vec();
    inputs.extend(runtime.iter().map(|(_, bytes)| *bytes));
    elf::link_static(&inputs, target)
}

/// Whether this build of krusty can link native programs for `target` at all.
pub fn can_link(target: NativeTarget) -> bool {
    prebuilt::runtime_objects(target.arch).is_some()
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::fixture::{linux, Image, Obj};
    use super::*;

    /// Whether a test over the real prebuilt runtime can run. Only a build without a C compiler
    /// has no runtime, and CI must not be one: there, a missing runtime fails the test instead.
    fn runtime_for(arch: Arch) -> bool {
        if can_link(linux(arch)) {
            return true;
        }
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must build the native runtime for {arch:?}, but krusty was built without it"
        );
        false
    }

    /// Linking with NO program objects reads every prebuilt runtime object, resolves what it can,
    /// and names the one symbol a program is expected to supply.
    #[test]
    fn the_runtime_alone_is_missing_only_the_program_entry() {
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            if !runtime_for(arch) {
                continue;
            }
            match link_program(&[], linux(arch)) {
                Err(ProgramLinkError::UndefinedSymbol(name)) => {
                    assert_eq!(name, "kt_program_entry", "{arch:?}");
                }
                other => {
                    panic!("{arch:?}: expected the program entry to be missing, got {other:?}")
                }
            }
        }
    }

    /// The smallest program — a `kt_program_entry` that returns at once — links against the real
    /// runtime on every architecture, into an executable for that machine whose entry is inside
    /// its code.
    #[test]
    fn a_program_links_against_the_runtime_on_every_architecture() {
        for (arch, ret) in [
            (Arch::X86_64, vec![0xc3]),
            (Arch::Aarch64, 0xd65f_03c0u32.to_le_bytes().to_vec()),
            (Arch::Riscv64, 0x0000_8067u32.to_le_bytes().to_vec()),
        ] {
            if !runtime_for(arch) {
                continue;
            }
            let mut program = Obj::new(arch);
            let text = program.section(".text", SectionKind::Text, &ret, 4);
            program.define("kt_program_entry", text, 0);
            let bytes = link_program(&[&program.bytes()], linux(arch))
                .unwrap_or_else(|error| panic!("{arch:?}: {error}"));
            let image = Image(&bytes);
            assert_eq!(image.machine(), arch.elf_machine(), "{arch:?}");
            assert!(
                (0x40_0000..image.data()).contains(&image.entry()),
                "{arch:?}: entry {:#x} is outside the executable segment",
                image.entry()
            );
        }
    }

    /// A program linked against the real runtime RUNS on an x86_64 Linux host: `_start` calls
    /// `kt_program_entry`, which returns, and the runtime exits with status 0.
    ///
    /// The entry reads a pointer from `.data` (`R_X86_64_PC32`) to a value in `.rodata`
    /// (`R_X86_64_64`) and returns only if the value is the 42 it should be; otherwise it exits
    /// with the value it found, so a mislinked program cannot pass by returning.
    #[test]
    fn a_program_linked_against_the_runtime_runs_on_this_host() {
        if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !runtime_for(Arch::X86_64) {
            return;
        }
        let mut program = Obj::new(Arch::X86_64);
        #[rustfmt::skip]
        let code = [
            0x48, 0x8b, 0x05, 0, 0, 0, 0,   // 0:  mov rax, [rip + pointer]
            0x8b, 0x38,                     // 7:  mov edi, [rax]
            0x83, 0xff, 0x2a,               // 9:  cmp edi, 42
            0x75, 0x01,                     // 12: jne 15
            0xc3,                           // 14: ret
            0xb8, 0x3c, 0, 0, 0,            // 15: mov eax, 60 (exit)
            0x0f, 0x05,                     // 20: syscall
        ];
        let text = program.section(".text", SectionKind::Text, &code, 16);
        let rodata = program.section(".rodata", SectionKind::ReadOnlyData, &[42, 0, 0, 0], 4);
        let data = program.section(".data", SectionKind::Data, &[0; 8], 8);
        program.define("kt_program_entry", text, 0);
        let answer = program.local("answer", rodata, 0);
        let pointer = program.local("pointer", data, 0);
        program.reloc(text, 3, pointer, -4, 2); // R_X86_64_PC32
        program.reloc(data, 0, answer, 0, 1); // R_X86_64_64
        let bytes = link_program(&[&program.bytes()], linux(Arch::X86_64))
            .unwrap_or_else(|error| panic!("{error}"));

        let path = std::env::temp_dir().join(format!("krusty-linker-smoke-{}", std::process::id()));
        std::fs::write(&path, &bytes).expect("write the executable");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("make the executable executable");
        }
        // Another test thread forking while the file was open for writing leaves it "busy" for a
        // moment (ETXTBSY); that is the host, not the executable.
        let mut attempts = 0;
        let status = loop {
            match std::process::Command::new(&path).status() {
                Err(error) if error.raw_os_error() == Some(26) && attempts < 50 => {
                    attempts += 1;
                    std::thread::yield_now();
                }
                other => break other,
            }
        };
        let _ = std::fs::remove_file(&path);
        let status = status.expect("run the linked executable");
        assert_eq!(status.code(), Some(0), "{status}");
    }

    /// The names the runtime defines are read out of its objects: its entry point and helpers are
    /// among them, and the symbol a PROGRAM supplies is not.
    #[test]
    fn the_runtime_symbols_are_what_the_runtime_defines() {
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            if !runtime_for(arch) {
                assert!(runtime_symbols(arch).expect("no runtime").is_empty());
                continue;
            }
            let names = runtime_symbols(arch).unwrap_or_else(|error| panic!("{arch:?}: {error}"));
            assert!(names.contains("_start"), "{arch:?}: {names:?}");
            assert!(names.contains("kt_program_returned"), "{arch:?}: {names:?}");
            assert!(!names.contains("kt_program_entry"), "{arch:?}: {names:?}");
        }
    }

    /// A build with no runtime for the target says what to do about it, since the remedy is to
    /// rebuild krusty rather than to change the program.
    #[test]
    fn a_missing_runtime_names_its_remedy() {
        let said = ProgramLinkError::NoRuntime(Arch::Riscv64).to_string();
        assert!(said.contains("Riscv64"), "{said}");
        assert!(said.contains("clang"), "{said}");
    }
}
