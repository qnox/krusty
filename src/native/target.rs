//! The architecture and operating system a native build targets.
//!
//! This exists because cross-compilation is a requirement, not a feature: krusty takes Go's
//! property — every target buildable from any host, with no per-target toolchain — as
//! non-negotiable. Making the target an explicit value rather than "whatever this machine is" is
//! what keeps the host from leaking into the build by default.

/// A supported instruction set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
    Riscv64,
}

impl Arch {
    /// The spelling in an LLVM target triple.
    pub fn triple_name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Riscv64 => "riscv64",
        }
    }

    /// The `EM_*` machine number an ELF header carries, so a produced binary can be checked
    /// against the target it was asked for rather than assumed correct.
    pub fn elf_machine(self) -> u16 {
        match self {
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
            Self::Riscv64 => 243,
        }
    }

    /// The architecture this process is running on, when it is one krusty targets.
    pub fn host() -> Option<Self> {
        Some(match std::env::consts::ARCH {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            "riscv64" => Self::Riscv64,
            _ => return None,
        })
    }
}

/// A supported operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Os {
    Linux,
}

impl Os {
    pub fn triple_name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
        }
    }

    pub fn host() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NativeTarget {
    pub arch: Arch,
    pub os: Os,
}

impl NativeTarget {
    pub const fn new(arch: Arch, os: Os) -> Self {
        Self { arch, os }
    }

    /// Every target the runtime has a syscall and entry-point implementation for.
    pub const ALL: &'static [Self] = &[
        Self::new(Arch::X86_64, Os::Linux),
        Self::new(Arch::Aarch64, Os::Linux),
        Self::new(Arch::Riscv64, Os::Linux),
    ];

    /// The host, when krusty targets it. `None` is not a failure — it means this machine is not
    /// itself a supported target, which does not stop it from building for ones that are.
    pub fn host() -> Option<Self> {
        Some(Self::new(Arch::host()?, Os::host()?))
    }

    /// The triple passed to the C compiler. The `gnu` component names the ABI, not a libc: the
    /// emitted program is freestanding and links against no C library.
    pub fn triple(self) -> String {
        format!(
            "{}-unknown-{}-gnu",
            self.arch.triple_name(),
            self.os.triple_name()
        )
    }

    /// The conventional short name, as a user would write it.
    pub fn name(self) -> String {
        format!("{}-{}", self.os.triple_name(), self.arch.triple_name())
    }
}

impl std::fmt::Display for NativeTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.name())
    }
}

impl std::str::FromStr for NativeTarget {
    type Err = String;

    /// Parses `<os>-<arch>` (`linux-aarch64`), the spelling [`NativeTarget::name`] produces.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        NativeTarget::ALL
            .iter()
            .copied()
            .find(|target| target.name() == text)
            .ok_or_else(|| {
                let supported = NativeTarget::ALL
                    .iter()
                    .map(|target| target.name())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown native target `{text}`; supported: {supported}")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_round_trips_through_its_name() {
        for target in NativeTarget::ALL {
            assert_eq!(
                target.name().parse::<NativeTarget>().expect("round trip"),
                *target
            );
        }
    }

    #[test]
    fn an_unknown_target_lists_the_supported_ones() {
        let error = "linux-s390x".parse::<NativeTarget>().expect_err("unknown");
        assert!(error.contains("linux-x86_64"), "{error}");
        assert!(error.contains("linux-riscv64"), "{error}");
    }

    #[test]
    fn every_supported_target_has_a_distinct_elf_machine() {
        let machines = NativeTarget::ALL
            .iter()
            .map(|target| target.arch.elf_machine())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            machines.len(),
            NativeTarget::ALL.len(),
            "a shared machine number would let a test pass while the wrong binary was produced"
        );
    }
}
