//! The native runtime, compiled once when krusty was built and carried inside the compiler.
//!
//! `build.rs` compiles `src/native/runtime/*.c` for every supported target and generates the table
//! included here: one relocatable object per runtime source per architecture. krusty's linker links
//! a program's objects against these; nothing is compiled from C at a user's build. See the module
//! comment in `build.rs` for why this is Go's arrangement and what it costs.

use super::NativeTarget;

include!(concat!(env!("OUT_DIR"), "/prebuilt_runtime.rs"));

/// Whether krusty was built with any prebuilt runtime at all (a C cross-compiler was present at
/// build time). `false` means every native target is unavailable, and says why.
pub fn prebuilt_available() -> bool {
    AVAILABLE
}

/// The prebuilt runtime objects for `target`, or `None` if krusty was built without a C compiler
/// able to target it. The table is keyed by the whole target, architecture and operating system,
/// because the objects make one operating system's system calls for one instruction set.
pub fn runtime_objects(target: NativeTarget) -> Option<&'static [(&'static str, &'static [u8])]> {
    PREBUILT
        .iter()
        .find(|(candidate, _)| *candidate == target)
        .map(|(_, objects)| *objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().expect("two bytes"))
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    fn u64_at(bytes: &[u8], at: usize) -> usize {
        u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes")) as usize
    }

    /// The global and weak symbols an ELF64 relocatable defines, and the ones it leaves undefined.
    fn symbols(bytes: &[u8]) -> (Vec<String>, Vec<String>) {
        let section_headers = u64_at(bytes, 0x28);
        let header_size = u16_at(bytes, 0x3A) as usize;
        let section_count = u16_at(bytes, 0x3C) as usize;
        let section = |index: usize| section_headers + index * header_size;
        let (mut defined, mut undefined) = (Vec::new(), Vec::new());
        for index in 0..section_count {
            let header = section(index);
            if u32_at(bytes, header + 4) != 2 {
                continue; // not SHT_SYMTAB
            }
            let (table, size) = (u64_at(bytes, header + 0x18), u64_at(bytes, header + 0x20));
            let strings = u64_at(bytes, section(u32_at(bytes, header + 0x28) as usize) + 0x18);
            for entry in (table..table + size).step_by(24) {
                let name_start = strings + u32_at(bytes, entry) as usize;
                let name_end = name_start
                    + bytes[name_start..]
                        .iter()
                        .position(|&byte| byte == 0)
                        .expect("a terminated symbol name");
                let name = String::from_utf8_lossy(&bytes[name_start..name_end]).into_owned();
                let binding = bytes[entry + 4] >> 4;
                if name.is_empty() || !matches!(binding, 1 | 2) {
                    continue; // local symbols answer nothing outside their object
                }
                if u16_at(bytes, entry + 6) == 0 {
                    undefined.push(name);
                } else {
                    defined.push(name);
                }
            }
        }
        (defined, undefined)
    }

    #[test]
    fn a_prebuilt_runtime_is_every_source_as_a_closed_set_of_objects_for_its_arch() {
        assert!(
            AVAILABLE || std::env::var_os("CI").is_none(),
            "CI must build the native runtime, but krusty was built without a C compiler for it"
        );
        // Some runtime being there is not enough: a target whose objects failed to build is left out
        // of the table with only a build warning. CI has the compiler for every target, so there
        // the table is every supported target, in the supported order.
        if std::env::var_os("CI").is_some() {
            let built: Vec<NativeTarget> = PREBUILT.iter().map(|(target, _)| *target).collect();
            assert_eq!(
                built,
                NativeTarget::ALL,
                "CI must prebuild the runtime for every supported target"
            );
        }
        for (target, objects) in PREBUILT {
            let arch = target.arch;
            let names: Vec<&str> = objects.iter().map(|(name, _)| *name).collect();
            let expected: Vec<String> = SOURCES
                .iter()
                .map(|source| {
                    std::path::Path::new(source)
                        .with_extension("o")
                        .display()
                        .to_string()
                })
                .collect();
            assert_eq!(names, expected, "{target}: one object per runtime source");
            let mut defined = HashSet::new();
            let mut undefined = Vec::new();
            for (name, bytes) in objects.iter() {
                assert_eq!(&bytes[..4], b"\x7fELF", "{target}/{name} is not ELF");
                assert_eq!(
                    (bytes[4], bytes[5]),
                    (2, 1),
                    "{target}/{name} is not 64-bit little-endian"
                );
                assert_eq!(u16_at(bytes, 16), 1, "{target}/{name} is not a relocatable");
                assert_eq!(
                    u16_at(bytes, 18),
                    arch.elf_machine(),
                    "{target}/{name} is code for another machine"
                );
                let (defines, needs) = symbols(bytes);
                defined.extend(defines);
                undefined.extend(needs.into_iter().map(|symbol| (*name, symbol)));
            }
            // The program supplies its entry; everything else the runtime calls, the runtime defines.
            // A toolchain that slipped in a helper of its own (`__stack_chk_fail`, say) fails here
            // rather than in a user's link.
            for (object, symbol) in undefined {
                assert!(
                    defined.contains(&symbol) || symbol == "kt_program_entry",
                    "{target}/{object} needs `{symbol}`, which no runtime object defines"
                );
            }
        }
    }
}
