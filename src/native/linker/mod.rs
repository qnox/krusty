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

use super::prebuilt;
use super::target::{Arch, NativeTarget};

/// Why a link failed. Each names what a user (or the emitter's author) needs to act on.
#[derive(Debug)]
pub enum ProgramLinkError {
    /// krusty was built without a prebuilt runtime for this architecture (no C cross-compiler at
    /// build time), so there is nothing to link against.
    NoRuntime(Arch),
    /// An input object could not be parsed as an ELF relocatable.
    Parse(String),
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
    use super::*;

    /// Every architecture whose runtime was prebuilt can be linked for, and no other.
    ///
    /// The two answers come from the same table, and a disagreement between them would be a build
    /// that promises a target it cannot produce.
    #[test]
    fn linkability_follows_the_prebuilt_runtime() {
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            let target = NativeTarget {
                arch,
                os: super::super::target::Os::Linux,
            };
            assert_eq!(
                can_link(target),
                prebuilt::runtime_objects(arch).is_some(),
                "{arch:?}"
            );
        }
    }

    /// Linking with NO program objects reads every prebuilt runtime object, resolves what it can,
    /// and names the one symbol a program is expected to supply.
    ///
    /// The runtime is a complete input on its own apart from that entry point, so this exercises
    /// the parse and the symbol table over all four real objects on each architecture — which is
    /// what makes it a test of the linker rather than of a fixture. A link that answered anything
    /// else would be failing to read an object, or inventing a definition for a symbol nothing
    /// defines.
    #[test]
    fn the_runtime_alone_is_missing_only_the_program_entry() {
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            let target = NativeTarget {
                arch,
                os: super::super::target::Os::Linux,
            };
            if !can_link(target) {
                continue;
            }
            match link_program(&[], target) {
                Err(ProgramLinkError::UndefinedSymbol(name)) => {
                    assert_eq!(name, "kt_program_entry", "{arch:?}");
                }
                other => {
                    panic!("{arch:?}: expected the program entry to be missing, got {other:?}")
                }
            }
        }
    }

    /// An input that is not an object is a PARSE error rather than a panic: krusty's own emitter
    /// produces these, and a malformed one has to name itself.
    #[test]
    fn a_malformed_object_is_an_error_not_a_panic() {
        let target = NativeTarget {
            arch: Arch::X86_64,
            os: super::super::target::Os::Linux,
        };
        if !can_link(target) {
            return;
        }
        let garbage = b"not an object file at all";
        assert!(matches!(
            link_program(&[garbage], target),
            Err(ProgramLinkError::Parse(_))
        ));
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
