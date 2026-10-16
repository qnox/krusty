//! The C libraries a Native program imports from, as its interop KLIBs name them.
//!
//! An interop KLIB (`interop=true`) carries the link facts of the C libraries its `.def` file
//! links (`cinterop::link_facts`), captured when it was made. A program built against it links
//! with those facts as its [`ImportLibrary`] list; the C library file itself is never read again.
//! [`NativeKlibInputs`] freezes them beside the declarations from the same open, so program linking
//! cannot reopen a selected KLIB path as a different generation. A KLIB that is not an interop
//! library links no C library.

use super::cinterop::link_facts;
use super::linker::{ImportLibrary, ProgramLinkError};
use super::target::NativeTarget;
use crate::klib_libraries::{
    KlibLibraries, KlibLibrariesOpenError, KlibLibraryInput, OpenedKlibLibraries,
};

/// The complete Native view of one selected set of KLIB inputs.
///
/// Declarations, bodies, and C-library imports all come from the same opened archives. Splitting
/// this value transfers those two views to the frontend and linker without leaving a path that can
/// be reopened as a different archive generation.
pub struct NativeKlibInputs {
    libraries: KlibLibraries,
    imports: Vec<ImportLibrary>,
}

impl NativeKlibInputs {
    /// Open `inputs` once for `target`, refusing malformed declarations or Native link facts.
    pub fn open(
        inputs: &[KlibLibraryInput],
        target: NativeTarget,
    ) -> Result<Self, NativeKlibInputsOpenError> {
        let opened =
            OpenedKlibLibraries::open_native(inputs).map_err(NativeKlibInputsOpenError::Klib)?;
        let imports =
            interop_link_libraries(&opened, target).map_err(NativeKlibInputsOpenError::Link)?;
        let libraries = opened
            .into_libraries()
            .map_err(NativeKlibInputsOpenError::Klib)?;
        Ok(Self { libraries, imports })
    }

    /// Transfer the common semantic provider and frozen Native linker inputs together.
    pub fn into_parts(self) -> (KlibLibraries, Vec<ImportLibrary>) {
        (self.libraries, self.imports)
    }

    /// The common semantic provider decoded from this input snapshot.
    pub fn libraries(&self) -> &KlibLibraries {
        &self.libraries
    }

    /// The frozen libraries the Native link imports from.
    pub fn imports(&self) -> &[ImportLibrary] {
        &self.imports
    }
}

/// Why a selected Native KLIB input set could not be assembled.
#[derive(Debug)]
pub enum NativeKlibInputsOpenError {
    /// The KLIB containers or their semantic channels are invalid.
    Klib(KlibLibrariesOpenError),
    /// A Native interop library's target or captured link facts are invalid.
    Link(ProgramLinkError),
}

impl std::fmt::Display for NativeKlibInputsOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Klib(error) => error.fmt(formatter),
            Self::Link(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NativeKlibInputsOpenError {}

/// Read C-library facts from the same selected archives that own semantic publication.
fn interop_link_libraries(
    opened: &OpenedKlibLibraries<'_>,
    target: NativeTarget,
) -> Result<Vec<ImportLibrary>, ProgramLinkError> {
    let mut libraries = Vec::new();
    for (path, archive) in opened.archives() {
        let what = path.display().to_string();
        let unreadable =
            |error: crate::klib::KlibError| ProgramLinkError::InvalidLibrary(error.to_string());
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
    use crate::klib_libraries::KlibStdlibSemantics;
    use crate::native::linker::{Export, ExportKind};
    use crate::native::target::{Arch, Os};
    use std::path::PathBuf;

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

    fn open_dependencies(
        paths: Vec<PathBuf>,
        target: NativeTarget,
    ) -> Result<NativeKlibInputs, NativeKlibInputsOpenError> {
        let inputs = paths
            .into_iter()
            .map(KlibLibraryInput::dependency)
            .collect::<Vec<_>>();
        NativeKlibInputs::open(&inputs, target)
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
        let inputs = open_dependencies(vec![plain, first, headers_only, second], LINUX_X64)
            .expect("the inputs are assembled");
        assert_eq!(
            inputs.imports(),
            vec![
                library("libfirst.so.1", "first_call", None),
                library("libsecond.so", "second_call", Some("V_2")),
            ]
            .as_slice()
        );
    }

    #[test]
    fn an_interop_klib_for_another_target_or_with_malformed_facts_is_refused() {
        let scratch = Scratch::new("refused");
        let arm = scratch.klib("arm", &interop("linux_arm64"), Some(b"library libarm.so\n"));
        assert_eq!(
            open_dependencies(vec![arm.clone()], LINUX_X64)
                .err()
                .expect("the wrong target is refused")
                .to_string(),
            ProgramLinkError::InvalidLibrary(format!(
                "the interop library {} was made for linux_arm64, not linux_x64",
                arm.display()
            ))
            .to_string()
        );
        let malformed = scratch.klib(
            "malformed",
            &interop("linux_x64"),
            Some(b"library libm.so\nsymbol sqrt\n"),
        );
        assert_eq!(
            open_dependencies(vec![malformed.clone()], LINUX_X64)
                .err()
                .expect("malformed facts are refused")
                .to_string(),
            ProgramLinkError::InvalidLibrary(format!(
                "the link facts of {} have a malformed line 2: \"symbol sqrt\"",
                malformed.display()
            ))
            .to_string()
        );
        let riscv = NativeTarget {
            arch: Arch::Riscv64,
            os: Os::Linux,
        };
        assert_eq!(
            open_dependencies(vec![malformed.clone()], riscv)
                .err()
                .expect("a target without interop is refused")
                .to_string(),
            ProgramLinkError::InvalidLibrary(format!(
                "the interop library {} cannot be linked for {riscv}, which Kotlin/Native has no \
                 interop libraries for",
                malformed.display()
            ))
            .to_string()
        );
    }

    #[test]
    fn replacing_an_unpacked_klib_cannot_mismatch_semantics_and_link_facts() {
        let scratch = Scratch::new("replacement");
        let directory = scratch.0.join("selected");
        let mut first_writer = KlibWriter::native_test_fixture("selected");
        for (key, value) in interop("linux_x64") {
            first_writer.set_property(key, value);
        }
        first_writer.add_file(
            &link_facts::entry("linux_x64"),
            b"library libfirst.so\nfunction selected_first\n".to_vec(),
        );
        first_writer
            .write_directory(&directory)
            .expect("write the first unpacked generation");

        let first_inputs = [KlibLibraryInput::platform_stdlib(
            directory.clone(),
            KlibStdlibSemantics::Native,
        )];
        let first = NativeKlibInputs::open(&first_inputs, LINUX_X64)
            .expect("assemble the first selected generation");
        let first_snapshot = first
            .libraries()
            .dependency_body_snapshot()
            .expect("the platform input owns a snapshot")
            .clone();

        std::fs::remove_dir_all(&directory).expect("remove the first unpacked generation");
        let mut second_writer = KlibWriter::native_test_fixture("selected");
        for (key, value) in interop("linux_x64") {
            second_writer.set_property(key, value);
        }
        second_writer.add_file(
            &link_facts::entry("linux_x64"),
            b"library libsecond.so\nfunction selected_second\n".to_vec(),
        );
        second_writer
            .write_directory(&directory)
            .expect("replace the unpacked generation at the selected path");

        assert_eq!(first.imports()[0].soname(), "libfirst.so");
        assert!(first.imports()[0].export("selected_first").is_some());
        assert!(first.imports()[0].export("selected_second").is_none());

        let second_inputs = [KlibLibraryInput::platform_stdlib(
            directory,
            KlibStdlibSemantics::Native,
        )];
        let second = NativeKlibInputs::open(&second_inputs, LINUX_X64)
            .expect("assemble the replacement generation");
        assert_ne!(
            &first_snapshot,
            second
                .libraries()
                .dependency_body_snapshot()
                .expect("the replacement platform input owns a snapshot")
        );
        assert_eq!(second.imports()[0].soname(), "libsecond.so");
        assert!(second.imports()[0].export("selected_second").is_some());
        assert!(second.imports()[0].export("selected_first").is_none());
    }
}
