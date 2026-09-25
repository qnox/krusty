//! The architecture and operating system a native build targets.
//!
//! This exists because cross-compilation is a requirement, not a feature: krusty takes Go's
//! property — every target buildable from any host, with no per-target toolchain — as
//! non-negotiable. Making the target an explicit value rather than "whatever this machine is" is
//! what keeps the host from leaking into the build by default.

pub use super::target_contract::{Arch, Os};

impl Arch {
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

impl Os {
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

    /// Every target the runtime has a syscall and entry-point implementation for: the build
    /// script's list, which is the one it prebuilds the runtime for.
    pub const ALL: &'static [Self] = &{
        let supported = super::target_contract::SUPPORTED;
        let mut all =
            [Self::new(supported[0].0, supported[0].1); super::target_contract::SUPPORTED.len()];
        let mut index = 0;
        while index < supported.len() {
            all[index] = Self::new(supported[index].0, supported[index].1);
            index += 1;
        }
        all
    };

    /// The host, when krusty targets it. `None` is not a failure — it means this machine is not
    /// itself a supported target, which does not stop it from building for ones that are.
    pub fn host() -> Option<Self> {
        Some(Self::new(Arch::host()?, Os::host()?))
    }

    /// The triple passed to the C compiler. The `gnu` component names the ABI, not a libc: the
    /// emitted program is freestanding and links against no C library.
    pub fn triple(self) -> String {
        super::target_contract::triple(self.arch, self.os)
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
        assert_eq!(
            error,
            "unknown native target `linux-s390x`; supported: linux-x86_64, linux-aarch64, \
             linux-riscv64"
        );
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
