//! The facts about each supported native target that both the build script and the compiler need.
//!
//! `build.rs` compiles the runtime once per target and files each object set under the whole
//! target, operating system and architecture; the compiler names targets, checks produced
//! binaries, and picks the prebuilt objects to link. Both must agree on which targets exist and on
//! each one's triple and ELF machine number. Kept in two places, a target added or changed on one
//! side would still build, so they are kept here once. The architecture's other ELF facts, such as
//! the page size its executables are laid out for, sit beside the machine number so that each fact
//! about a target's ELF files has one source. This module depends on nothing but `core`: the build
//! script includes it by `#[path]`, before the crate it belongs to exists.

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
    /// The iOS simulator. The image is an arm64 dylib that imports libSystem; it is not a static
    /// ELF executable and it is not signed here.
    IosSimulator,
}

/// Every target the runtime has an implementation for, in the order the build script builds them
/// and the compiler lists them.
pub const SUPPORTED: [(Arch, Os); 4] = [
    (Arch::X86_64, Os::Linux),
    (Arch::Aarch64, Os::Linux),
    (Arch::Riscv64, Os::Linux),
    (Arch::Aarch64, Os::IosSimulator),
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

    /// The largest page size a Linux kernel for this architecture can run with: what a static
    /// executable's loadable segments are aligned to, and how far apart they must start.
    ///
    /// The kernel maps each `PT_LOAD` segment in whole pages of whatever size it was built with,
    /// so two segments that share a page at that size overlap, and the later mapping replaces the
    /// earlier one's permissions or bytes. An executable laid out for 4 KiB pages therefore breaks
    /// on an AArch64 kernel built for 16 or 64 KiB pages, which distributions ship; 64 KiB is the
    /// largest AArch64 Linux supports and what `ld.lld` and GNU ld use there. x86_64 Linux maps
    /// user memory in 4 KiB pages only, and so does RISC-V Linux (its larger sizes are huge pages,
    /// which never back an executable's segments), which is again what both linkers use.
    pub const fn max_page_size(self) -> u64 {
        match self {
            Self::X86_64 => 0x1000,
            Self::Aarch64 => 0x1_0000,
            Self::Riscv64 => 0x1000,
        }
    }

    /// The flags a Linux runtime object for this architecture is compiled with, beyond the ones
    /// every target shares.
    ///
    /// `-fno-pic` and a small code model keep the objects free of GOT/PLT machinery: the output is
    /// a static executable at a fixed address, so absolute 32-bit relocations are fine and
    /// simplest. RISC-V's clang does not take `-mcmodel=small` (its equivalent is the default
    /// `medlow`), and `-mno-relax` keeps linker-relaxation relocations out of its objects: krusty's
    /// own linker applies relocations and relaxes nothing. The iOS simulator does not use these
    /// flags; its runtime is position-independent and calls libSystem.
    pub const fn runtime_cflags(self) -> &'static [&'static str] {
        match self {
            Self::X86_64 => &["-fno-pic", "-mcmodel=small", "-fcf-protection=none"],
            Self::Aarch64 => &["-fno-pic", "-mcmodel=small"],
            Self::Riscv64 => &["-fno-pic", "-mno-relax"],
        }
    }
}

impl Os {
    /// The operating-system half of [`crate::native::NativeTarget::name`]. For Linux this is also
    /// the operating-system token of the C compiler's triple.
    pub const fn triple_name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::IosSimulator => "ios-simulator",
        }
    }

    /// A static ELF executable. The iOS simulator is a Mach-O dylib instead.
    pub const fn static_elf(self) -> bool {
        matches!(self, Self::Linux)
    }
}

/// The triple passed to the C compiler. Linux's `gnu` component names the ABI, not a libc: the
/// runtime is freestanding. The simulator triple is clang's Darwin spelling and needs no SDK.
pub fn clang_triple(arch: Arch, os: Os) -> String {
    match os {
        Os::Linux => format!("{}-unknown-linux-gnu", arch.triple_name()),
        Os::IosSimulator => format!("{}-apple-ios15.0-simulator", arch.triple_name()),
    }
}

/// The triple Cranelift's target lexicon parses. It matches [`clang_triple`] on Linux. The
/// simulator's does not: lexicon spells the environment `sim` and the deployment `major.minor.patch`.
pub fn isa_triple(arch: Arch, os: Os) -> String {
    match os {
        Os::Linux => clang_triple(arch, os),
        Os::IosSimulator => format!("{}-apple-ios15.0.0-sim", arch.triple_name()),
    }
}
