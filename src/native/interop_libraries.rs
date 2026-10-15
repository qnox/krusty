//! The C libraries a Native program imports from, as its interop KLIBs name them.
//!
//! An interop KLIB (`interop=true`) carries the link facts of the C libraries its `.def` file
//! links (`cinterop::link_facts`), captured when it was made. A program built against it links
//! with those facts as its [`ImportLibrary`] list; the C library file itself is never read again.
//! A KLIB that is not an interop library links no C library.

use std::path::PathBuf;

use super::cinterop::link_facts;
use super::linker::{ImportLibrary, ProgramLinkError};
use super::target::NativeTarget;
use crate::klib::KlibArchive;

/// The libraries a program linked for `target` against `klibs` imports from, in KLIB order and,
/// within a KLIB, in the order its `.def` file names them.
///
/// An interop KLIB not made for `target`, and one whose facts are malformed, is refused.
pub fn interop_link_libraries(
    klibs: &[PathBuf],
    target: NativeTarget,
) -> Result<Vec<ImportLibrary>, ProgramLinkError> {
    let mut libraries = Vec::new();
    for path in klibs {
        let what = path.display().to_string();
        let unreadable =
            |error: crate::klib::KlibError| ProgramLinkError::InvalidLibrary(error.to_string());
        let archive = KlibArchive::open(path).map_err(unreadable)?;
        let manifest = archive.manifest().map_err(unreadable)?;
        if manifest.get("interop") != Some("true") {
            continue;
        }
        let Some(konan_target) = target.konan_name() else {
            return Err(ProgramLinkError::InvalidLibrary(format!(
                "the interop library {what} cannot be linked for {target}, which Kotlin/Native \
                 has no interop libraries for"
            )));
        };
        if !manifest.targets().contains(&konan_target) {
            return Err(ProgramLinkError::InvalidLibrary(format!(
                "the interop library {what} was made for {}, not {konan_target}",
                manifest.targets().join(", ")
            )));
        }
        let entry = link_facts::entry(konan_target);
        if archive.entries().binary_search(&entry).is_err() {
            continue;
        }
        let facts = archive.read(&entry).map_err(unreadable)?;
        libraries.extend(link_facts::decode(&facts, &what)?);
    }
    Ok(libraries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::klib::write::KlibWriter;
    use crate::native::linker::{Export, ExportKind};
    use crate::native::target::{Arch, Os};

    const LINUX_X64: NativeTarget = NativeTarget {
        arch: Arch::X86_64,
        os: Os::Linux,
    };

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-interop-libraries-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create the scratch directory");
            Self(path)
        }

        /// A metadata-only KLIB named `name` with the manifest `properties` and the `facts`
        /// entry for linux_x64.
        fn klib(&self, name: &str, properties: &[(&str, &str)], facts: Option<&[u8]>) -> PathBuf {
            let mut writer = KlibWriter::native_test_fixture(name);
            for (key, value) in properties {
                writer.set_property(key, value);
            }
            if let Some(facts) = facts {
                writer.add_file(&link_facts::entry("linux_x64"), facts.to_vec());
            }
            let path = self.0.join(format!("{name}.klib"));
            writer.write_archive(&path).expect("write the KLIB");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn interop(target: &'static str) -> [(&'static str, &'static str); 2] {
        [("interop", "true"), ("native_targets", target)]
    }

    #[test]
    fn interop_klibs_contribute_their_facts_in_order_and_others_none() {
        let scratch = Scratch::new("order");
        let plain = scratch.klib(
            "plain",
            &[("native_targets", "linux_x64")],
            Some(b"library libignored.so\n"),
        );
        let first = scratch.klib(
            "first",
            &interop("linux_x64"),
            Some(b"library libfirst.so.1\nfunction first_call\n"),
        );
        let headers_only = scratch.klib("headers", &interop("linux_x64"), None);
        let second = scratch.klib(
            "second",
            &interop("linux_x64 linux_arm64"),
            Some(b"library libsecond.so\nfunction second_call V_2\n"),
        );
        let library = |soname: &str, name: &str, version: Option<&str>| {
            ImportLibrary::new(
                soname.to_string(),
                [(
                    name.to_string(),
                    Export {
                        kind: ExportKind::Function,
                        version: version.map(str::to_string),
                    },
                )],
            )
            .expect("a valid library")
        };
        assert_eq!(
            interop_link_libraries(&[plain, first, headers_only, second], LINUX_X64)
                .expect("the facts are read"),
            vec![
                library("libfirst.so.1", "first_call", None),
                library("libsecond.so", "second_call", Some("V_2")),
            ]
        );
    }

    #[test]
    fn an_interop_klib_for_another_target_or_with_malformed_facts_is_refused() {
        let scratch = Scratch::new("refused");
        let arm = scratch.klib("arm", &interop("linux_arm64"), Some(b"library libarm.so\n"));
        assert_eq!(
            interop_link_libraries(std::slice::from_ref(&arm), LINUX_X64),
            Err(ProgramLinkError::InvalidLibrary(format!(
                "the interop library {} was made for linux_arm64, not linux_x64",
                arm.display()
            )))
        );
        let malformed = scratch.klib(
            "malformed",
            &interop("linux_x64"),
            Some(b"library libm.so\nsymbol sqrt\n"),
        );
        assert_eq!(
            interop_link_libraries(std::slice::from_ref(&malformed), LINUX_X64),
            Err(ProgramLinkError::InvalidLibrary(format!(
                "the link facts of {} have a malformed line 2: \"symbol sqrt\"",
                malformed.display()
            )))
        );
        let riscv = NativeTarget {
            arch: Arch::Riscv64,
            os: Os::Linux,
        };
        assert_eq!(
            interop_link_libraries(std::slice::from_ref(&malformed), riscv),
            Err(ProgramLinkError::InvalidLibrary(format!(
                "the interop library {} cannot be linked for {riscv}, which Kotlin/Native has no \
                 interop libraries for",
                malformed.display()
            )))
        );
    }
}
