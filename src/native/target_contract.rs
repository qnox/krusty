//! The facts about each supported native target that both the build script and the compiler need.
//!
//! `build.rs` compiles the runtime once per target and files each object set under that target's
//! architecture; the compiler names targets, checks produced binaries, and picks the prebuilt
//! objects to link. Both must agree on which targets exist and on each one's triple and ELF machine
//! number. Kept in two places, a target added or changed on one side would still build, so they are
//! kept here once. This module depends on nothing but `core`: the build script includes it by
//! `#[path]`, before the crate it belongs to exists.

/// A supported instruction set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
    Riscv64,
}

/// A supported operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Os {
    Linux,
}

/// Every target the runtime has a syscall and entry-point implementation for, in the order the
/// build script builds them and the compiler lists them.
pub const SUPPORTED: [(Arch, Os); 3] = [
    (Arch::X86_64, Os::Linux),
    (Arch::Aarch64, Os::Linux),
    (Arch::Riscv64, Os::Linux),
];

impl Arch {
    /// The spelling in an LLVM target triple.
    pub const fn triple_name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Riscv64 => "riscv64",
        }
    }

    /// The `EM_*` machine number an ELF header carries, so an object or a produced binary can be
    /// checked against the target it was built for rather than assumed correct.
    pub const fn elf_machine(self) -> u16 {
        match self {
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
            Self::Riscv64 => 243,
        }
    }

    /// The flags this architecture's runtime objects are compiled with, beyond the ones every
    /// target shares.
    ///
    /// `-fno-pic` and a small code model keep the objects free of GOT/PLT machinery: the output is
    /// a static executable at a fixed address, so absolute 32-bit relocations are fine and
    /// simplest. RISC-V's clang does not take `-mcmodel=small` (its equivalent is the default
    /// `medlow`), and `-mno-relax` keeps linker-relaxation relocations out of its objects: krusty's
    /// own linker applies relocations and relaxes nothing.
    pub const fn runtime_cflags(self) -> &'static [&'static str] {
        match self {
            Self::X86_64 => &["-mcmodel=small", "-fcf-protection=none"],
            Self::Aarch64 => &["-mcmodel=small"],
            Self::Riscv64 => &["-mno-relax"],
        }
    }
}

impl Os {
    /// The spelling in an LLVM target triple.
    pub const fn triple_name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
        }
    }
}

/// The triple passed to the C compiler for `arch` on `os`. The `gnu` component names the ABI, not a
/// libc: the emitted program is freestanding and links against no C library.
pub fn triple(arch: Arch, os: Os) -> String {
    format!("{}-unknown-{}-gnu", arch.triple_name(), os.triple_name())
}
