//! KLIB container behavior over repository-owned semantic bytes.
//!
//! No installed Kotlin distribution participates: the module header and root fragment are fixed
//! bytes copied from Kotlin's checked-in `unpackedExampleKlib` fixture.

mod repository_owned {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use krusty::klib::{KlibArchive, KlibError};

    const MODULE_HEADER: &[u8] = b"\x0a\x15<unpackedExampleKlib>\x3a\x00";
    const ROOT_FRAGMENT: &[u8] = b"\x0a\x1d\x0a\x04main\x0a\x06kotlin\x0a\x04Unit\x0a\x07main.kt\x12\x0c\x0a\x02\x10\x01\x0a\x06\x08\x00\x10\x02\x18\x00\x1a\x1c\x1a\x07\x10\x00\x38\x00\xe0\x0a\x03\xf2\x01\x04\x0a\x02\x30\x01\xd8\x0a\xff\xff\xff\xff\xff\xff\xff\xff\xff\x01\xe0\x0a\x00\xea\x0a\x00";

    fn scratch(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory =
            std::env::temp_dir().join(format!("krusty-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the fixture directory");
        directory
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create the fixture archive");
        let mut archive = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            archive
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .expect("start the fixture entry");
            archive
                .write_all(contents)
                .expect("write the fixture entry");
        }
        archive.finish().expect("finish the fixture archive");
    }

    /// The shape a distribution ships, written here so the three failures below are told apart from
    /// a success rather than only from each other.
    #[test]
    fn a_zip_klib_reads_its_manifest_fragments_and_ir() {
        let directory = scratch("klib-zip-valid");
        let path = directory.join("fixture.klib");
        write_zip(
            &path,
            &[
                (
                    "default/manifest",
                    b"unique_name=fixture\nbuiltins_platform=JS\n",
                ),
                ("default/linkdata/module", MODULE_HEADER),
                ("default/linkdata/root_package/00_.knm", ROOT_FRAGMENT),
                ("default/ir/irDeclarations.knd", b"ir"),
            ],
        );

        let archive = KlibArchive::open(&path).expect("the fixture opens as a klib");
        let manifest = archive.manifest().expect("the manifest reads");
        assert_eq!(manifest.unique_name(), Some("fixture"));
        assert_eq!(manifest.builtins_platform(), Some("JS"));
        assert_eq!(
            archive.module_header().expect("the module header reads"),
            MODULE_HEADER
        );
        assert_eq!(
            archive
                .package_fragments()
                .iter()
                .map(|fragment| fragment.package_fqname.clone())
                .collect::<Vec<_>>(),
            [""],
            "the repository fixture declares its root package"
        );
        assert_eq!(
            archive
                .read("default/linkdata/root_package/00_.knm")
                .expect("read semantic root fragment"),
            ROOT_FRAGMENT
        );
        assert_eq!(archive.ir_entries(), vec!["default/ir/irDeclarations.knd"]);
    }

    /// A file that is not a zip at all. Something is there and cannot be read, which is not the
    /// same answer as nothing being there — and both produced `None` before.
    #[test]
    fn a_file_that_does_not_parse_as_a_zip_is_malformed() {
        let directory = scratch("klib-zip-invalid");
        let path = directory.join("fixture.klib");
        std::fs::write(
            &path,
            b"PK\x03\x04 and then nothing that follows the format",
        )
        .expect("write the fixture");
        assert!(matches!(
            KlibArchive::open(&path),
            Err(KlibError::InvalidZip { .. })
        ));
    }

    /// A selected `.klib` with no `default/` tree is malformed rather than an empty library.
    #[test]
    fn a_zip_without_a_default_tree_is_not_a_klib() {
        let directory = scratch("klib-zip-not-a-klib");
        let path = directory.join("fixture.klib");
        write_zip(
            &path,
            &[("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n")],
        );
        assert!(matches!(
            KlibArchive::open(&path),
            Err(KlibError::MissingDefaultTree { .. })
        ));
    }

    /// An entry the central directory LISTS and whose payload does not inflate. The archive
    /// advertises a fragment it cannot hand back, which is corruption; skipping it would publish a
    /// library with silently fewer declarations than it claims.
    #[test]
    fn an_entry_that_cannot_be_inflated_is_reported_not_skipped() {
        let directory = scratch("klib-zip-bad-entry");
        let path = directory.join("fixture.klib");
        write_zip(
            &path,
            &[
                ("default/manifest", b"unique_name=fixture\n"),
                (
                    "default/linkdata/package_fixture.core/01_core.knm",
                    b"a payload long enough that corrupting it cannot still inflate",
                ),
            ],
        );
        // Corrupt the deflate stream in place, leaving the central directory — and so the entry
        // listing — intact. The archive still says it has the fragment.
        let mut bytes = std::fs::read(&path).expect("read the fixture back");
        let start = bytes.len() / 3;
        for byte in &mut bytes[start..start + 24] {
            *byte ^= 0xff;
        }
        std::fs::write(&path, &bytes).expect("write the corrupted fixture");

        let archive = KlibArchive::open(&path).expect("the archive still opens");
        let entry = "default/linkdata/package_fixture.core/01_core.knm";
        assert!(
            archive.entries().iter().any(|listed| listed == entry),
            "the archive still lists the fragment: {:?}",
            archive.entries()
        );
        assert!(
            archive.read(entry).is_err(),
            "and reading it is a failure, not an empty fragment"
        );
    }
}
