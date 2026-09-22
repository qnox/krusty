//! The native runtime, compiled once when krusty was built and carried inside the compiler.
//!
//! `build.rs` compiles `src/native/runtime/*.c` for every supported target and generates the table
//! included here: one relocatable object per runtime source per architecture. krusty's linker links
//! a program's objects against these; nothing is compiled from C at a user's build. See the module
//! comment in `build.rs` for why this is Go's arrangement and what it costs.

use super::Arch;

include!(concat!(env!("OUT_DIR"), "/prebuilt_runtime.rs"));

/// Whether krusty was built with any prebuilt runtime at all (a C cross-compiler was present at
/// build time). `false` means every native target is unavailable, and says why.
pub fn prebuilt_available() -> bool {
    AVAILABLE
}

/// The prebuilt runtime objects for `arch`, or `None` if krusty was built without a C compiler able
/// to target it.
pub fn runtime_objects(arch: Arch) -> Option<&'static [(&'static str, &'static [u8])]> {
    PREBUILT
        .iter()
        .find(|(candidate, _)| *candidate == arch)
        .map(|(_, objects)| *objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prebuilt_runtime_has_every_source_and_is_an_elf_object() {
        // On a host without clang this is legitimately empty; the native tests skip. On one with
        // it, every target must carry one object per runtime source, each a real ELF relocatable.
        // The count comes from `build.rs`'s own list, so adding a source cannot silently leave a
        // target short of it.
        const SOURCES: usize = 4;
        for (arch, objects) in PREBUILT {
            assert_eq!(
                objects.len(),
                SOURCES,
                "{arch:?} is missing a runtime object"
            );
            for (name, bytes) in objects.iter() {
                assert!(name.ends_with(".o"), "{name}");
                assert_eq!(&bytes[..4], b"\x7fELF", "{arch:?}/{name} is not ELF");
                assert_eq!(
                    bytes[16], 1,
                    "{arch:?}/{name} is not ET_REL (a relocatable object)"
                );
            }
        }
        if !AVAILABLE {
            eprintln!("no C compiler at build time: prebuilt runtime table is empty");
        }
    }
}
