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
//!
//! That is the Linux image. The iOS simulator target is an unsigned arm64 `MH_DYLIB`: its objects
//! are position-independent Mach-O, and the link imports `write`, `mmap`, `munmap` and `exit` from
//! `/usr/lib/libSystem.B.dylib`. No Apple SDK is read. The load commands dyld needs — the libSystem
//! dependency, the simulator build version, and classic bind and rebase info — are written by hand
//! the same way the ELF header is.

mod abi;
mod elf;
#[cfg(test)]
mod fixture;
mod macho;
mod relocate;

/// Every global symbol the prebuilt runtime for `target` defines. Empty when this build has no
/// runtime for it. Names are the link names: a Mach-O leading underscore is not part of one, so a
/// program symbol and the runtime symbol it calls compare equal.
pub fn runtime_symbols(target: NativeTarget) -> Result<HashSet<String>, ProgramLinkError> {
    if target.os.static_elf() {
        elf::runtime_symbols(target)
    } else {
        macho::runtime_symbols(target)
    }
}

use std::collections::HashSet;

use super::prebuilt;
use super::target::{Arch, NativeTarget};

/// Why a link failed. Each names what a user (or the emitter's author) needs to act on.
#[derive(Debug, PartialEq, Eq)]
pub enum ProgramLinkError {
    /// krusty was built without a prebuilt runtime for this target (no C cross-compiler at
    /// build time), so there is nothing to link against.
    NoRuntime(NativeTarget),
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
    /// An input's ELF flags or build attributes name an ABI the target does not use, or one the
    /// other inputs' cannot be combined with.
    IncompatibleAbi {
        input: String,
        mismatch: AbiMismatch,
    },
    /// An input (named as `input`) relocates `section` with a relocation table other than
    /// `SHT_RELA`, whose addends the linker does not read.
    UnsupportedRelocationTable {
        input: String,
        section: String,
        table: RelocationTable,
    },
}

/// How an input's stated ABI conflicts with the target's or another input's. Only RISC-V objects
/// state one; see `abi.rs` for what the psABI requires and why each check is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiMismatch {
    /// `EF_RISCV_FLOAT_ABI` is not the target's.
    RiscvFloatAbi {
        found: RiscvFloatAbi,
        expected: RiscvFloatAbi,
    },
    /// `EF_RISCV_RVE`: the RV64E base ISA and its `lp64e` ABI.
    RiscvRve,
    /// `EF_RISCV_RV64ILP32`: 32-bit pointers on RV64.
    RiscvRv64Ilp32,
    /// `e_flags` bits the psABI reserves or leaves to vendors.
    RiscvUnknownFlags(u32),
    /// `Tag_RISCV_stack_align` is not the target ABI's.
    RiscvStackAlign { found: u64, expected: u64 },
    /// `Tag_RISCV_arch` names an instruction set that is not RV64I.
    RiscvArch(String),
    /// `Tag_RISCV_unaligned_access` carries a value the psABI does not define.
    RiscvUnknownUnalignedAccess(u64),
    /// Deprecated `Tag_RISCV_priv_spec*` attributes name different versions.
    RiscvPrivSpec {
        found: RiscvPrivSpec,
        other_input: String,
        other: RiscvPrivSpec,
    },
    /// `Tag_RISCV_atomic_abi` is A6C where another input's is A7, or the other way round.
    RiscvAtomicAbi {
        found: RiscvAtomicAbi,
        other_input: String,
        other: RiscvAtomicAbi,
    },
    /// `Tag_RISCV_atomic_abi` carries a value the psABI does not define.
    RiscvUnknownAtomicAbi(u64),
    /// `Tag_RISCV_x3_reg_usage` disagrees with another input's use of `x3`/`gp`.
    RiscvX3Usage {
        found: u64,
        other_input: String,
        other: u64,
    },
    /// An unrecognized RISC-V attribute whose tag makes it mandatory.
    RiscvUnknownMandatoryAttribute(u64),
}

/// The deprecated privileged-specification version encoded by tags 8, 10 and 12.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiscvPrivSpec {
    pub major: u64,
    pub minor: u64,
    pub revision: u64,
}

/// A RISC-V floating-point calling convention, as `EF_RISCV_FLOAT_ABI` states it for RV64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiscvFloatAbi {
    Soft,
    Single,
    Double,
    Quad,
}

/// A RISC-V mapping of atomic operations to instructions (`Tag_RISCV_atomic_abi`), among the two
/// that cannot be mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiscvAtomicAbi {
    A6C,
    A7,
}

impl std::fmt::Display for RiscvFloatAbi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Soft => "lp64 (soft-float)",
            Self::Single => "lp64f (single-float)",
            Self::Double => "lp64d (double-float)",
            Self::Quad => "lp64q (quad-float)",
        })
    }
}

impl std::fmt::Display for RiscvAtomicAbi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::A6C => "A6C",
            Self::A7 => "A7",
        })
    }
}

impl std::fmt::Display for RiscvPrivSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.revision)
    }
}

fn riscv_x3_usage(value: u64) -> String {
    match value {
        0 => "a fixed register with unknown purpose".to_string(),
        1 => "the global pointer".to_string(),
        2 => "the shadow-stack pointer".to_string(),
        3 => "a temporary register".to_string(),
        4..=1023 => format!("reserved standard platform use {value}"),
        1024..=2047 => format!("nonstandard platform use {value}"),
        _ => format!("undefined use {value}"),
    }
}

/// Read after the input's name: "input 1 is built for …".
impl std::fmt::Display for AbiMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RiscvFloatAbi { found, expected } => write!(
                formatter,
                "is built for the {found} ABI, but the target uses {expected}; objects for \
                 different floating-point ABIs pass arguments in different registers and cannot \
                 be linked"
            ),
            Self::RiscvRve => formatter.write_str(
                "is built for RV64E (EF_RISCV_RVE: 16 integer registers, the lp64e ABI); the \
                 target is RV64I with the lp64d ABI",
            ),
            Self::RiscvRv64Ilp32 => formatter.write_str(
                "is built for the RV64ILP32 ABI (EF_RISCV_RV64ILP32: 32-bit pointers); the \
                 target's lp64d ABI has 64-bit pointers",
            ),
            Self::RiscvUnknownFlags(bits) => write!(
                formatter,
                "sets ELF flags {bits:#x}, which the RISC-V psABI reserves or leaves to vendors; \
                 krusty's linker cannot tell whether the object suits the target"
            ),
            Self::RiscvStackAlign { found, expected } => write!(
                formatter,
                "assumes a {found}-byte-aligned stack (Tag_RISCV_stack_align); the target's ABI \
                 aligns it to {expected} bytes"
            ),
            Self::RiscvArch(isa) => write!(
                formatter,
                "is built for `{isa}` (Tag_RISCV_arch), which is not an RV64I instruction set"
            ),
            Self::RiscvUnknownUnalignedAccess(value) => write!(
                formatter,
                "sets Tag_RISCV_unaligned_access to undefined value {value}; the psABI defines \
                 only 0 (no unaligned accesses) and 1 (may use unaligned accesses)"
            ),
            Self::RiscvPrivSpec {
                found,
                other_input,
                other,
            } => write!(
                formatter,
                "requires privileged specification {found} (Tag_RISCV_priv_spec*), which cannot \
                 be linked with {other_input}'s {other}"
            ),
            Self::RiscvAtomicAbi {
                found,
                other_input,
                other,
            } => write!(
                formatter,
                "maps atomics by the {found} atomic ABI (Tag_RISCV_atomic_abi), which cannot be \
                 linked with {other_input}'s {other}: they order sequentially consistent loads and \
                 stores with different fences"
            ),
            Self::RiscvUnknownAtomicAbi(value) => write!(
                formatter,
                "sets Tag_RISCV_atomic_abi to undefined value {value}; the psABI defines only 0 \
                 (UNKNOWN), 1 (A6C), 2 (A6S) and 3 (A7)"
            ),
            Self::RiscvX3Usage {
                found,
                other_input,
                other,
            } => write!(
                formatter,
                "uses x3/gp as {} (Tag_RISCV_x3_reg_usage), which cannot be linked with \
                 {other_input}, which uses it as {}",
                riscv_x3_usage(*found),
                riscv_x3_usage(*other)
            ),
            Self::RiscvUnknownMandatoryAttribute(tag) => write!(
                formatter,
                "uses unrecognized mandatory RISC-V attribute tag {tag}; the psABI requires an \
                 error instead of ignoring tags whose value modulo 128 is below 64"
            ),
        }
    }
}

/// A relocation table the linker refuses, because its addends are not stored beside each entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocationTable {
    /// `SHT_REL`: each addend is the value already in the field the entry patches.
    Rel,
    /// `SHT_CREL`: entries in a compact encoding, with addends present or implicit.
    Crel,
}

impl std::fmt::Display for RelocationTable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Rel => "SHT_REL relocations, whose addends are implicit in the bytes they patch",
            Self::Crel => "SHT_CREL (compact) relocations",
        })
    }
}

impl std::fmt::Display for ProgramLinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRuntime(target) => write!(
                formatter,
                "krusty was built without the native runtime for {target} (no C cross-compiler was \
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
            Self::IncompatibleAbi { input, mismatch } => write!(formatter, "{input} {mismatch}"),
            Self::UnsupportedRelocationTable {
                input,
                section,
                table,
            } => write!(
                formatter,
                "{input} relocates `{section}` with {table}; krusty's linker reads only SHT_RELA \
                 relocations, which the x86_64, AArch64 and RISC-V psABIs specify"
            ),
        }
    }
}

impl std::error::Error for ProgramLinkError {}

/// Link a program's objects with the prebuilt runtime for `target`.
///
/// A Linux target produces a static ELF executable. The iOS simulator produces an unsigned Mach-O
/// dylib. The caller decides where to write the bytes.
pub fn link_program(
    program_objects: &[&[u8]],
    target: NativeTarget,
) -> Result<Vec<u8>, ProgramLinkError> {
    let runtime = prebuilt::runtime_objects(target).ok_or(ProgramLinkError::NoRuntime(target))?;
    let mut inputs: Vec<&[u8]> = program_objects.to_vec();
    inputs.extend(runtime.iter().map(|(_, bytes)| *bytes));
    if target.os.static_elf() {
        elf::link_static(&inputs, target)
    } else {
        macho::link_dylib(&inputs, target)
    }
}

/// Whether this build of krusty can link native programs for `target` at all.
pub fn can_link(target: NativeTarget) -> bool {
    prebuilt::runtime_objects(target).is_some()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use object::SectionKind;

    use super::fixture::{linux, Image, Obj};
    use super::*;

    /// Whether a test over the real prebuilt runtime can run for `target`. Only a build without a
    /// C compiler has no runtime, and CI must not be one: there, a missing runtime fails the test
    /// instead.
    fn runtime_for(target: NativeTarget) -> bool {
        if can_link(target) {
            return true;
        }
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must build the native runtime for {target}, but krusty was built without it"
        );
        false
    }

    /// A `kt_program_entry` that returns at once, in `arch`'s instructions.
    fn returning_entry(arch: Arch) -> Obj {
        let ret = match arch {
            Arch::X86_64 => vec![0xc3],
            Arch::Aarch64 => 0xd65f_03c0u32.to_le_bytes().to_vec(),
            Arch::Riscv64 => 0x0000_8067u32.to_le_bytes().to_vec(),
        };
        let mut program = Obj::new(arch);
        let text = program.section(".text", SectionKind::Text, &ret, 4);
        program.define("kt_program_entry", text, 0);
        program
    }

    /// Linking with NO program objects reads every prebuilt runtime object, resolves what it can,
    /// and names the one symbol a program is expected to supply.
    #[test]
    fn the_runtime_alone_is_missing_only_the_program_entry() {
        for target in NativeTarget::ALL.iter().copied() {
            if !runtime_for(target) {
                continue;
            }
            if !target.os.static_elf() {
                // The simulator dylib has no `_start`, so the runtime alone is a complete image.
                // Its load commands are checked by `the_ios_simulator_runtime_is_an_unsigned_dylib`.
                link_program(&[], target)
                    .unwrap_or_else(|error| panic!("{target}: the runtime alone links: {error}"));
                continue;
            }
            match link_program(&[], target) {
                Err(ProgramLinkError::UndefinedSymbol(name)) => {
                    assert_eq!(name, "kt_program_entry", "{target}");
                }
                other => {
                    panic!("{target}: expected the program entry to be missing, got {other:?}")
                }
            }
        }
    }

    /// The smallest program — a `kt_program_entry` that returns at once — links against the real
    /// runtime on every target, into an executable for that machine whose entry is inside its
    /// code and whose `e_flags` are what its objects merge to.
    #[test]
    fn a_program_links_against_the_runtime_on_every_target() {
        for target in NativeTarget::ALL.iter().copied() {
            if !target.os.static_elf() || !runtime_for(target) {
                continue;
            }
            let program = returning_entry(target.arch);
            let bytes = link_program(&[&program.bytes()], target)
                .unwrap_or_else(|error| panic!("{target}: {error}"));
            let image = Image(&bytes);
            assert_eq!(image.machine(), target.arch.elf_machine(), "{target}");
            // The program says only lp64d; the runtime's clang objects add compressed code.
            let flags = match target.arch {
                Arch::X86_64 | Arch::Aarch64 => 0,
                Arch::Riscv64 => 0x5, // EF_RISCV_RVC | EF_RISCV_FLOAT_ABI_DOUBLE
            };
            assert_eq!(image.flags(), flags, "{target}");
            assert!(
                (0x40_0000..image.data()).contains(&image.entry()),
                "{target}: entry {:#x} is outside the executable segment",
                image.entry()
            );
        }
    }

    /// A program whose `kt_program_entry` reads a pointer from `.data` to a value in `.rodata`
    /// and returns only if the value is 42; otherwise it exits with the value it found, so a
    /// mislinked program cannot pass by returning. Every architecture reaches `.data` by
    /// PC-relative code and `.rodata` by an absolute 64-bit word, so both kinds of reference must
    /// be right. Each instruction word is what `llvm-mc` encodes for the instruction shown.
    fn answer_program(arch: Arch) -> Obj {
        let mut program = Obj::new(arch);
        let text = match arch {
            #[rustfmt::skip]
            Arch::X86_64 => program.section(".text", SectionKind::Text, &[
                0x48, 0x8b, 0x05, 0, 0, 0, 0,   // 0:  mov rax, [rip + pointer]
                0x8b, 0x38,                     // 7:  mov edi, [rax]
                0x83, 0xff, 0x2a,               // 9:  cmp edi, 42
                0x75, 0x01,                     // 12: jne 15
                0xc3,                           // 14: ret
                0xb8, 0x3c, 0, 0, 0,            // 15: mov eax, 60 (exit)
                0x0f, 0x05,                     // 20: syscall
            ], 16),
            Arch::Aarch64 => program.text_words(&[
                0x9000_0000, // 0:  adrp x0, pointer
                0xf940_0000, // 4:  ldr x0, [x0, :lo12:pointer]
                0xb940_0000, // 8:  ldr w0, [x0]
                0x7100_a81f, // 12: cmp w0, #42
                0x5400_0041, // 16: b.ne 24
                0xd65f_03c0, // 20: ret
                0xd280_0ba8, // 24: mov x8, #93 (exit)
                0xd400_0001, // 28: svc #0
            ]),
            Arch::Riscv64 => program.text_words(&[
                0x0000_0517, // 0:  auipc a0, %pcrel_hi(pointer)
                0x0005_3503, // 4:  ld a0, %pcrel_lo(0b)(a0)
                0x0005_2503, // 8:  lw a0, 0(a0)
                0x02a0_0293, // 12: li t0, 42
                0x0055_1463, // 16: bne a0, t0, 24
                0x0000_8067, // 20: ret
                0x05d0_0893, // 24: li a7, 93 (exit)
                0x0000_0073, // 28: ecall
            ]),
        };
        let rodata = program.section(".rodata", SectionKind::ReadOnlyData, &[42, 0, 0, 0], 4);
        let data = program.section(".data", SectionKind::Data, &[0; 8], 8);
        program.define("kt_program_entry", text, 0);
        let answer = program.local("answer", rodata, 0);
        let pointer = program.local("pointer", data, 0);
        match arch {
            Arch::X86_64 => {
                program.reloc(text, 3, pointer, -4, 2); // R_X86_64_PC32
                program.reloc(data, 0, answer, 0, 1); // R_X86_64_64
            }
            Arch::Aarch64 => {
                program.reloc(text, 0, pointer, 0, 275); // R_AARCH64_ADR_PREL_PG_HI21
                program.reloc(text, 4, pointer, 0, 286); // R_AARCH64_LDST64_ABS_LO12_NC
                program.reloc(data, 0, answer, 0, 257); // R_AARCH64_ABS64
            }
            Arch::Riscv64 => {
                let hi = program.local(".Lpcrel_hi0", text, 0);
                program.reloc(text, 0, pointer, 0, 23); // R_RISCV_PCREL_HI20
                program.reloc(text, 4, hi, 0, 24); // R_RISCV_PCREL_LO12_I
                program.reloc(data, 0, answer, 0, 2); // R_RISCV_64
            }
        }
        program
    }

    /// How this host can run an executable for `target`: directly when it IS the target, else
    /// through a QEMU user-mode emulator (`qemu-aarch64`, `qemu-riscv64`, `qemu-x86_64`) found on
    /// `PATH` when the host runs the target's operating system. `None` when it cannot.
    fn runner(target: NativeTarget) -> Option<Option<PathBuf>> {
        let host = NativeTarget::host()?;
        if host == target {
            return Some(None);
        }
        if host.os != target.os {
            return None;
        }
        let emulator = format!("qemu-{}", target.arch.triple_name());
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|directory| directory.join(&emulator))
            .find(|candidate| candidate.is_file())
            .map(Some)
    }

    /// Run `executable`, directly or under `emulator`, and return its exit status.
    fn run(executable: &Path, emulator: Option<&Path>) -> std::process::ExitStatus {
        let (program, args): (OsString, Vec<OsString>) = match emulator {
            None => (executable.into(), Vec::new()),
            Some(emulator) => (emulator.into(), vec![executable.into()]),
        };
        // Another test thread forking while the file was open for writing leaves it "busy" for a
        // moment (ETXTBSY); that is the host, not the executable.
        let mut attempts = 0;
        loop {
            match std::process::Command::new(&program).args(&args).status() {
                Err(error) if error.raw_os_error() == Some(26) && attempts < 50 => {
                    attempts += 1;
                    std::thread::yield_now();
                }
                other => break other.expect("run the linked executable"),
            }
        }
    }

    /// A program linked against the real runtime RUNS, for every target this host can run: its
    /// own natively, and any other through a QEMU user-mode emulator on `PATH`. `_start` calls
    /// `kt_program_entry`, which returns, and the runtime exits with status 0. A target the host
    /// cannot run is still linked, so a link failure shows on every host.
    #[test]
    fn a_program_linked_against_the_runtime_runs_where_this_host_can_run_it() {
        for target in NativeTarget::ALL.iter().copied() {
            if !target.os.static_elf() || !runtime_for(target) {
                continue;
            }
            let program = answer_program(target.arch);
            let bytes = link_program(&[&program.bytes()], target)
                .unwrap_or_else(|error| panic!("{target}: {error}"));
            let Some(emulator) = runner(target) else {
                continue;
            };
            let path = std::env::temp_dir().join(format!(
                "krusty-linker-smoke-{target}-{}",
                std::process::id()
            ));
            std::fs::write(&path, &bytes).expect("write the executable");
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("make the executable executable");
            }
            let status = run(&path, emulator.as_deref());
            let _ = std::fs::remove_file(&path);
            assert_eq!(status.code(), Some(0), "{target}: {status}");
        }
    }

    /// The names the runtime defines are read out of its objects: its entry point and helpers are
    /// among them, and the symbol a PROGRAM supplies is not.
    #[test]
    fn the_runtime_symbols_are_what_the_runtime_defines() {
        for target in NativeTarget::ALL.iter().copied() {
            if !runtime_for(target) {
                assert!(runtime_symbols(target).expect("no runtime").is_empty());
                continue;
            }
            let names = runtime_symbols(target).unwrap_or_else(|error| panic!("{target}: {error}"));
            assert!(!names.contains("kt_program_entry"), "{target}: {names:?}");
            if target.os.static_elf() {
                assert!(names.contains("_start"), "{target}: {names:?}");
                assert!(names.contains("kt_program_returned"), "{target}: {names:?}");
            } else {
                assert!(!names.contains("_start"), "{target}: {names:?}");
                assert!(names.contains("kt_runtime_init"), "{target}: {names:?}");
            }
        }
    }

    /// The simulator image is an unsigned arm64 dylib. The runtime alone is enough: there is no
    /// `_start` to satisfy, and the only undefined symbols are the four libSystem imports. The
    /// gate reads the load commands; it does not execute the dylib.
    #[test]
    fn the_ios_simulator_runtime_is_an_unsigned_dylib() {
        use object::{Object, ObjectSection, ObjectSymbol};

        let target = "ios-simulator-aarch64"
            .parse::<NativeTarget>()
            .expect("the simulator is a supported target");
        if !runtime_for(target) {
            return;
        }
        let bytes = link_program(&[], target)
            .unwrap_or_else(|error| panic!("{target}: the runtime alone links: {error}"));
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(&bytes[..4], &[0xcf, 0xfa, 0xed, 0xfe], "MH_MAGIC_64");
        assert_eq!(u32_at(4), 0x0100_000c, "CPU_TYPE_ARM64");
        assert_eq!(u32_at(12), 6, "MH_DYLIB");
        assert_eq!(
            u32_at(24),
            0x4 | 0x80 | 0x10_0000,
            "MH_DYLDLINK|MH_TWOLEVEL|MH_NO_REEXPORTED_DYLIBS"
        );
        let ncmds = u32_at(16);
        let mut at = 32usize;
        let mut load_dylib = None;
        let mut build_version = None;
        let mut signed = false;
        let mut bitcode = false;
        for _ in 0..ncmds {
            let cmd = u32_at(at);
            let size = u32_at(at + 4) as usize;
            assert!(
                size >= 8 && at + size <= bytes.len(),
                "load command {cmd:#x} at {at}"
            );
            if cmd == 0x1d {
                signed = true;
            }
            if cmd == 0xc {
                let name_at = at + u32_at(at + 8) as usize;
                let end = bytes[name_at..]
                    .iter()
                    .position(|&byte| byte == 0)
                    .expect("a terminated dylib path");
                load_dylib =
                    Some(String::from_utf8_lossy(&bytes[name_at..name_at + end]).into_owned());
            }
            if cmd == 0x32 {
                build_version = Some((u32_at(at + 8), u32_at(at + 12)));
            }
            if cmd == 0x19 {
                let nsects = u32_at(at + 64) as usize;
                let mut section = at + 72;
                for _ in 0..nsects {
                    let name_end = bytes[section..section + 16]
                        .iter()
                        .position(|&byte| byte == 0)
                        .unwrap_or(16);
                    let name =
                        std::str::from_utf8(&bytes[section..section + name_end]).unwrap_or("");
                    if name == "__LLVM" {
                        bitcode = true;
                    }
                    section += 80;
                }
            }
            at += size;
        }
        assert_eq!(load_dylib.as_deref(), Some("/usr/lib/libSystem.B.dylib"));
        assert_eq!(
            build_version,
            Some((7, 15 << 16)),
            "PLATFORM_IOSSIMULATOR, 15.0.0"
        );
        assert!(!signed, "the dylib is unsigned");
        assert!(!bitcode, "the dylib contains no bitcode");
        let file = object::File::parse(bytes.as_slice()).expect("the dylib parses");
        let mut undefined = Vec::new();
        for symbol in file.symbols() {
            if symbol.is_undefined() {
                let name = symbol.name().expect("an undefined symbol has a name");
                undefined.push(name.strip_prefix('_').unwrap_or(name).to_string());
            }
        }
        undefined.sort();
        undefined.dedup();
        assert_eq!(
            undefined,
            ["exit", "mmap", "munmap", "write"],
            "libSystem is the whole import surface"
        );
        for section in file.sections() {
            let name = section.name().unwrap_or("");
            assert_ne!(name, "__LLVM");
        }
    }

    /// A build with no runtime for the target says what to do about it, since the remedy is to
    /// rebuild krusty rather than to change the program.
    #[test]
    fn a_missing_runtime_names_its_remedy() {
        assert_eq!(
            ProgramLinkError::NoRuntime(linux(Arch::Riscv64)).to_string(),
            "krusty was built without the native runtime for linux-riscv64 (no C cross-compiler was \
             available when krusty was built; install clang and rebuild krusty)"
        );
    }
}
