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

    /// The triple passed to the C compiler. See [`super::target_contract::clang_triple`].
    pub fn triple(self) -> String {
        super::target_contract::clang_triple(self.arch, self.os)
    }

    /// The triple Cranelift parses. See [`super::target_contract::isa_triple`].
    pub fn isa_triple(self) -> String {
        super::target_contract::isa_triple(self.arch, self.os)
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
             linux-riscv64, ios-simulator-aarch64"
        );
    }

    #[test]
    fn the_ios_simulator_names_itself_and_its_two_triples() {
        let target = "ios-simulator-aarch64"
            .parse::<NativeTarget>()
            .expect("the simulator is a supported target");
        assert_eq!(target, NativeTarget::new(Arch::Aarch64, Os::IosSimulator));
        assert_eq!(target.triple(), "aarch64-apple-ios15.0-simulator");
        assert_eq!(target.isa_triple(), "aarch64-apple-ios15.0.0-sim");
        assert!(!target.os.static_elf());
    }

    #[test]
    fn every_supported_architecture_has_a_distinct_elf_machine() {
        // `e_machine` names an instruction set, not an operating system or ABI, so two supported
        // targets that share an architecture share it too. What must hold is that no two
        // ARCHITECTURES do: a shared number would let a test pass while the wrong code was produced.
        let mut architectures: Vec<Arch> =
            NativeTarget::ALL.iter().map(|target| target.arch).collect();
        architectures.sort();
        architectures.dedup();
        let machines = architectures
            .iter()
            .map(|arch| arch.elf_machine())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(machines.len(), architectures.len());
    }
}
