//! Byte equality between the klib krusty writes and the one the reference compiler writes.
//!
//! The standard the JVM lane already holds itself to, applied to Kotlin's other library format: not
//! "a klib the reader accepts" but the same bytes kotlinc produces, because a dependent resolves
//! against a klib's `unique_name`, a linker walks its fragments, and a fingerprint over its bytes
//! decides whether a downstream module recompiles.
//!
//! The comparison is a per-entry ledger and it is two-sided: `divergent` and `extra` are asserted
//! empty, `missing` is asserted exactly, and `identical` is asserted as the full list — so an entry
//! that starts or stops matching fails this test rather than drifting past it.
//!
//! Run with `KRUSTY_KLIB_BYTEDIFF_REPORT=1` for one `KLIBDIFF` line per entry plus a summary.
//!
//! What the writer is given here are the FIXTURE's declarations, not bytes copied from the reference
//! artifact: the interning orders below are the serializer's rules, discovered from reference bytes
//! and now reproduced. Two of those rules are worth naming, because getting either wrong changes
//! every id downstream:
//!
//!   * A declaration's RETURN type is interned before its value parameters. `boxOf(value: T): Box<T>`
//!     therefore interns `T`, then `Box<T>`, and `map`'s `Box<R>` lands before its
//!     `Function1<T, R>`.
//!   * A class fragment interns its package name first and a package fragment interns the first
//!     top-level declaration's name first — the walk starts where the fragment's content does.

use crate::common;
use krusty::klib::fragment::declarations::*;
use krusty::klib::fragment::*;
use krusty::klib::write::{module_header, KlibBuilder, KlibPackageFragments};

/// One package, one class with a type parameter, a constructor, a defaulted member and a property,
/// plus a top-level function. Enough that the reference splits the package into a class fragment and
/// a package fragment, and writes a root-package fragment beside them.
const LIB: &str = "package lib\n\
                   \n\
                   class Box<T>(val value: T) {\n\
                   \x20   fun get(): T = value\n\
                   \x20   fun describe(prefix: String = \"box\"): String = \"$prefix:$value\"\n\
                   }\n\
                   \n\
                   fun <T> boxOf(value: T): Box<T> = Box(value)\n";

/// The `lib` package's CLASS fragment: `class Box<T>(val value: T)` with its two members.
fn class_fragment() -> Vec<u8> {
    let mut strings = StringTable::default();
    let mut names = QualifiedNameTable::default();
    let mut types = TypeTable::default();

    let lib = names
        .intern_package(&mut strings, "lib")
        .expect("a named package");
    let box_name = names.intern(QualifiedName {
        parent: Some(lib),
        short_name: strings.intern("Box"),
        kind: QualifiedNameKind::Class,
    });
    let element = strings.intern("T");
    let kotlin = names
        .intern_package(&mut strings, "kotlin")
        .expect("a named package");
    let any = names.intern(QualifiedName {
        parent: Some(kotlin),
        short_name: strings.intern("Any"),
        kind: QualifiedNameKind::Class,
    });
    // The supertype is interned first, then the constructor's parameter type, then each member's.
    let any_id = types.intern(FragmentType::Class {
        qualified_name: any,
        arguments: Vec::new(),
        nullable: false,
    });
    let element_id = types.intern(FragmentType::ClassTypeParameter {
        id: 0,
        nullable: false,
    });
    let value = strings.intern("value");
    let file = strings.intern("Lib.kt");
    let describe = strings.intern("describe");
    let string_name = names.intern(QualifiedName {
        parent: Some(kotlin),
        short_name: strings.intern("String"),
        kind: QualifiedNameKind::Class,
    });
    let string_id = types.intern(FragmentType::Class {
        qualified_name: string_name,
        arguments: Vec::new(),
        nullable: false,
    });
    let prefix = strings.intern("prefix");
    let get = strings.intern("get");

    let class = ClassMeta {
        flags: None,
        fq_name: box_name,
        supertype_ids: vec![any_id],
        type_parameters: vec![TypeParameterMeta {
            id: 0,
            name: element,
            reified: false,
            variance: None,
            upper_bound_ids: Vec::new(),
        }],
        constructors: vec![ConstructorMeta {
            flags: None,
            value_parameters: vec![ValueParameterMeta {
                flags: None,
                name: value,
                type_id: element_id,
            }],
        }],
        functions: vec![
            FunctionMeta {
                flags: None,
                name: describe,
                type_parameters: Vec::new(),
                // `DECLARES_DEFAULT_VALUE` — `prefix: String = "box"`.
                value_parameters: vec![ValueParameterMeta {
                    flags: Some(2),
                    name: prefix,
                    type_id: string_id,
                }],
                return_type_id: string_id,
                receiver_type_id: None,
                file: Some(file),
            },
            FunctionMeta {
                flags: None,
                name: get,
                type_parameters: Vec::new(),
                value_parameters: Vec::new(),
                return_type_id: element_id,
                receiver_type_id: None,
                file: Some(file),
            },
        ],
        properties: vec![PropertyMeta {
            flags: None,
            name: value,
            return_type_id: element_id,
            file: Some(file),
        }],
        file: Some(file),
    };

    let mut builder = FragmentBuilder::new("lib");
    builder
        .string_table(strings.encode())
        .qualified_name_table(names.encode())
        .class(encode_class_with_table(&class, &types), 1);
    builder.encode()
}

/// The `lib` package's PACKAGE fragment: the top-level `boxOf`.
fn package_fragment() -> Vec<u8> {
    let mut strings = StringTable::default();
    let mut names = QualifiedNameTable::default();
    let mut types = TypeTable::default();

    // A package fragment's walk starts at its first top-level declaration, so `boxOf` interns before
    // the package's own name.
    let box_of = strings.intern("boxOf");
    let lib = names
        .intern_package(&mut strings, "lib")
        .expect("a named package");
    let box_name = names.intern(QualifiedName {
        parent: Some(lib),
        short_name: strings.intern("Box"),
        kind: QualifiedNameKind::Class,
    });
    let element = strings.intern("T");
    let element_id = types.intern(FragmentType::NamedTypeParameter {
        name: element,
        nullable: false,
    });
    let box_of_t = types.intern(FragmentType::Class {
        qualified_name: box_name,
        arguments: vec![TypeArgument {
            projection: None,
            type_id: element_id,
        }],
        nullable: false,
    });
    let value = strings.intern("value");
    let file = strings.intern("Lib.kt");

    let functions = vec![FunctionMeta {
        flags: None,
        name: box_of,
        type_parameters: vec![TypeParameterMeta {
            id: 0,
            name: element,
            reified: false,
            variance: None,
            upper_bound_ids: Vec::new(),
        }],
        value_parameters: vec![ValueParameterMeta {
            flags: None,
            name: value,
            type_id: element_id,
        }],
        return_type_id: box_of_t,
        receiver_type_id: None,
        file: Some(file),
    }];

    let mut builder = FragmentBuilder::new("lib");
    builder
        .string_table(strings.encode())
        .qualified_name_table(names.encode())
        .package(encode_package(&functions, &[], &types, 0));
    builder.encode()
}

#[test]
fn the_metadata_klib_matches_the_reference_byte_for_byte() {
    let Some(reference) = common::kotlinc_metadata_klib("container", &[("Lib.kt", LIB)]) else {
        return;
    };
    let reference_archive =
        krusty::klib::KlibArchive::open(&reference).expect("the reference wrote a klib");
    let reference_manifest = reference_archive.manifest();

    // The manifest's VALUES are inputs taken from the reference artifact; what is under test is its
    // shape — the key set, their order, the separator, the terminators, and the absence of the
    // timestamp comment `java.util.Properties.store` would put in a distributed artifact.
    let mut builder = KlibBuilder::metadata_only(
        reference_manifest
            .unique_name()
            .expect("the reference names the library"),
        reference_manifest
            .abi_version()
            .expect("the reference states an ABI version"),
        reference_manifest
            .metadata_version()
            .expect("the reference states a metadata version"),
        reference_manifest
            .compiler_version()
            .expect("the reference states a compiler version"),
        reference_manifest
            .get("ir_signature_versions")
            .expect("the reference states its signature versions"),
    );
    builder
        .module_header(module_header("<main>", &[String::new(), "lib".to_string()]))
        // The root package declares nothing and still gets a fragment: a resolver walking a
        // qualifier has to be able to see that the package exists.
        .package(KlibPackageFragments {
            package_fqname: String::new(),
            chunks: vec![empty_fragment("")],
        })
        .package(KlibPackageFragments {
            package_fqname: "lib".to_string(),
            chunks: vec![class_fragment(), package_fragment()],
        });

    let krusty_out = common::scratch_dir()
        .expect("scratch dir")
        .join("krusty-metadata-klib");
    std::fs::remove_dir_all(&krusty_out).ok();
    builder
        .write_directory(&krusty_out)
        .expect("krusty writes its klib");

    let diff = common::diff_klib_directories(&krusty_out, &reference);
    diff.report("container");

    assert!(
        diff.divergent.is_empty(),
        "an entry krusty writes must MATCH, not merely exist: {:?}",
        diff.divergent
    );
    assert!(
        diff.extra.is_empty(),
        "krusty must not invent entries the reference does not write: {:?}",
        diff.extra
    );
    assert!(
        diff.missing.is_empty(),
        "every entry the reference writes must be written: {:?}",
        diff.missing
    );
    assert_eq!(
        diff.identical,
        vec![
            "default/linkdata/module".to_string(),
            "default/linkdata/package_lib/0_lib.knm".to_string(),
            "default/linkdata/package_lib/1_lib.knm".to_string(),
            "default/linkdata/root_package/0_.knm".to_string(),
            "default/manifest".to_string(),
        ],
        "every entry of the metadata klib is byte-identical"
    );
    assert_eq!(diff.reference_entries(), 5);
}

/// The reference compiler's metadata klib carries NO serialized IR, which is what makes it the right
/// first target: a difference is a difference in declarations, not in a body serialization krusty
/// has not started.
#[test]
fn a_metadata_klib_has_no_ir_and_no_platform() {
    let Some(reference) = common::kotlinc_metadata_klib("shape", &[("Lib.kt", LIB)]) else {
        return;
    };
    let archive = krusty::klib::KlibArchive::open(&reference).expect("the reference wrote a klib");
    assert!(
        archive.ir_entries().is_empty(),
        "a metadata klib serializes no bodies: {:?}",
        archive.ir_entries()
    );
    let manifest = archive.manifest();
    assert!(
        manifest.builtins_platform().is_none(),
        "and names no platform: {:?}",
        manifest.builtins_platform()
    );
    assert!(
        manifest.targets().is_empty(),
        "and no targets: {:?}",
        manifest.targets()
    );
}
