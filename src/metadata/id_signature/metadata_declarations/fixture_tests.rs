//! Every public signature computed from a real KLIB's metadata, compared with the signatures its
//! serialized IR declares.
//!
//! `tests/fixtures/klib_signatures/signatures.klib` is kotlinc-native 2.4.20's library for the two
//! Kotlin files beside it, built in a directory named `/tmp/klib_signatures` (so no host path
//! leaks into the archive) with:
//!
//! ```text
//! kotlinc-native -p library -Xmulti-platform -Xexpect-actual-classes \
//!     -XXLanguage:+CompanionBlocksAndExtensions -Xfragments=common,native \
//!     -Xfragment-sources=common:Signatures.kt,native:NativeSignatures.kt \
//!     -Xfragment-refines=native:common -o signatures Signatures.kt NativeSignatures.kt
//! ```

use std::collections::HashSet;
use std::path::Path;

use super::super::{KlibAccessorIdSignature, KlibPublicIdSignature};
use super::*;
use crate::klib::KlibArchive;
use crate::libraries::TypeKind;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrMember};
use crate::metadata::klib_ir::{read_declaration_trees, KlibIrSignature};
use crate::metadata::semantic::{parse_package_fragment_checked, KotlinPackage};
use crate::types::Visibility;

#[derive(Debug, Default, PartialEq, Eq)]
struct Signatures {
    public: HashSet<KlibPublicIdSignature>,
    accessors: HashSet<KlibAccessorIdSignature>,
}

fn fixture() -> KlibArchive {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/klib_signatures/signatures.klib");
    KlibArchive::open(&path).expect("the signature fixture KLIB opens")
}

fn serialized(archive: &KlibArchive) -> Signatures {
    fn walk(arena: &KlibIrArena, member: &KlibIrMember, out: &mut Signatures) {
        let mut record = |signature: &KlibIrSignature| match signature {
            KlibIrSignature::Public(signature) => {
                assert!(out.public.insert(signature.clone()), "{signature:?} twice");
            }
            KlibIrSignature::Accessor(signature) => {
                assert!(
                    out.accessors.insert(signature.clone()),
                    "{signature:?} twice"
                );
            }
            KlibIrSignature::FileLocal { .. } => {}
        };
        match member {
            KlibIrMember::Function(function) => {
                record(&arena.function(*function).base.symbol.signature)
            }
            KlibIrMember::Property(property) => {
                record(&property.base.symbol.signature);
                for accessor in [property.getter, property.setter].into_iter().flatten() {
                    record(&arena.function(accessor).base.symbol.signature);
                }
            }
            KlibIrMember::Class(class) => {
                let class = arena.class(*class);
                record(&class.base.symbol.signature);
                for member in &class.members {
                    walk(arena, member, out);
                }
            }
            KlibIrMember::EnumEntry(entry) => record(&entry.base.symbol.signature),
            KlibIrMember::TypeAlias(alias) => record(&alias.base.symbol.signature),
            _ => {}
        }
    }
    let trees = read_declaration_trees(archive).expect("the fixture's IR decodes");
    let mut signatures = Signatures::default();
    for tree in trees.trees() {
        walk(&tree.arena, &tree.declaration, &mut signatures);
    }
    signatures
}

fn computed(archive: &KlibArchive) -> Signatures {
    let mut out = Signatures::default();
    for fragment in archive.package_fragments() {
        let bytes = archive.read(&fragment.entry).expect("fragment reads");
        let package = parse_package_fragment_checked(&bytes).expect("fragment decodes");
        let segments: Vec<String> = fragment
            .package_fqname
            .split('.')
            .map(str::to_owned)
            .collect();
        sign_package(&segments, &package, &mut out);
    }
    out
}

fn sign_package(package_segments: &[String], package: &KotlinPackage, out: &mut Signatures) {
    let top = MetadataContainer {
        package: package_segments,
        classes: &[],
        native_interop_library: false,
    };
    for function in &package.functions {
        if function.visibility != Visibility::Private {
            out.public
                .insert(package_function_signature(top, function).unwrap());
        }
    }
    for property in &package.properties {
        if property.visibility == Visibility::Private {
            continue;
        }
        out.public
            .insert(package_property_signature(top, property).unwrap());
        out.accessors.insert(
            package_property_accessor_signature(top, property, MetadataAccessor::Getter).unwrap(),
        );
        if property.is_var {
            out.accessors.insert(
                package_property_accessor_signature(top, property, MetadataAccessor::Setter)
                    .unwrap(),
            );
        }
    }
    let prefix = format!("{}/", package_segments.join("/"));
    for (qualified, class) in &package.classes {
        if class.visibility == Visibility::Private {
            continue;
        }
        let local = qualified
            .strip_prefix(&prefix)
            .expect("a class of its package");
        let names: Vec<&str> = local.split('.').collect();
        let chain: Vec<MetadataClass<'_>> = names
            .iter()
            .enumerate()
            .map(|(depth, name)| {
                let key = format!("{prefix}{}", names[..=depth].join("."));
                MetadataClass::of(name, &package.classes[&key])
            })
            .collect();
        let (own, outer) = chain.split_last().expect("a class has a name");
        let around = MetadataContainer {
            package: package_segments,
            classes: outer,
            native_interop_library: false,
        };
        out.public.insert(metadata_class_signature(around, *own));
        let inside = MetadataContainer {
            classes: &chain,
            ..around
        };
        for member in &class.members {
            if member.visibility == Visibility::Private {
                continue;
            }
            out.public.insert(member_signature(inside, member).unwrap());
            if member.is_property {
                out.accessors.insert(
                    member_property_accessor_signature(inside, member, MetadataAccessor::Getter)
                        .unwrap(),
                );
                if member.is_var {
                    out.accessors.insert(
                        member_property_accessor_signature(
                            inside,
                            member,
                            MetadataAccessor::Setter,
                        )
                        .unwrap(),
                    );
                }
            }
        }
        for constructor in &class.constructors {
            out.public
                .insert(constructor_signature(inside, constructor).unwrap());
        }
        if class.kind == TypeKind::Enum {
            for entry in &class.enum_entries {
                out.public
                    .insert(metadata_enum_entry_signature(inside, entry));
            }
            let implicit = enum_class_member_signatures(inside, class.has_enum_entries).unwrap();
            out.public.extend([implicit.values, implicit.value_of]);
            out.public.extend(implicit.entries);
            out.accessors.extend(implicit.entries_getter);
        }
    }
}

fn paths(signatures: &HashSet<KlibPublicIdSignature>) -> Vec<String> {
    let mut paths: Vec<String> = signatures
        .iter()
        .map(|signature| signature.declaration().segments().join("."))
        .collect();
    paths.sort();
    paths
}

#[test]
fn metadata_signs_every_declaration_exactly_as_the_serialized_ir() {
    let archive = fixture();
    let serialized = serialized(&archive);
    let computed = computed(&archive);
    assert_eq!(
        paths(
            &serialized
                .public
                .difference(&computed.public)
                .cloned()
                .collect()
        ),
        Vec::<String>::new(),
        "declared by IR but not computed from metadata"
    );
    assert_eq!(
        paths(
            &computed
                .public
                .difference(&serialized.public)
                .cloned()
                .collect()
        ),
        Vec::<String>::new(),
        "computed from metadata but not declared by IR"
    );
    assert_eq!(serialized, computed);
    assert_eq!(serialized.public.len(), 40);
    assert_eq!(serialized.accessors.len(), 14);

    // Renaming every type parameter leaves the member ids alone; only the paths differ.
    let member_id = |path: &str| {
        serialized
            .public
            .iter()
            .find(|signature| signature.declaration().segments().join(".") == path)
            .and_then(KlibPublicIdSignature::member_id)
            .unwrap_or_else(|| panic!("{path} is declared"))
    };
    assert_eq!(member_id("Outer.shadow"), member_id("Renamed.shadow"));
    assert_eq!(member_id("Outer.keep"), member_id("Renamed.keep"));
    assert_ne!(member_id("Outer.shadow"), member_id("Outer.keep"));
}

/// Companion-block members and companion extensions are the declarations metadata marks static;
/// the exact comparison above holds only because their signatures carry `#static`.
#[test]
fn metadata_marks_companion_block_members_and_companion_extensions_static() {
    let archive = fixture();
    let mut statics = Vec::new();
    for fragment in archive.package_fragments() {
        let bytes = archive.read(&fragment.entry).expect("fragment reads");
        let package = parse_package_fragment_checked(&bytes).expect("fragment decodes");
        statics.extend(
            package
                .functions
                .iter()
                .filter(|function| function.is_static)
                .map(|function| function.name.clone()),
        );
        statics.extend(
            package
                .properties
                .iter()
                .filter(|property| property.is_static)
                .map(|property| property.name.clone()),
        );
        for (qualified, class) in &package.classes {
            statics.extend(
                class
                    .members
                    .iter()
                    .filter(|member| member.is_static)
                    .map(|member| format!("{qualified}.{}", member.name)),
            );
        }
    }
    statics.sort();
    assert_eq!(
        statics,
        [
            "capacity",
            "fixture/signatures/Registry.create",
            "fixture/signatures/Registry.label",
            "fixture/signatures/Registry.size",
            "named",
        ]
    );
}
