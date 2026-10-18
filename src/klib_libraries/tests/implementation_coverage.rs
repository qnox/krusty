//! Every target-stdlib callable which is `external` or has a decoded bodyless IR declaration has
//! a compiler operation, or remains in the exact-identity implementation backlog.
//!
//! Eligibility is declaration/body evidence from the selected KLIB, never an annotation or source
//! spelling. Backlog keys retain the provider module and complete serialized declaration identity;
//! the paths embedded in those identities are output only. Lists live under
//! `tests/klib_uncovered_implementations/<target>/<version>.txt` and may only shrink. A release
//! with no list of its own reads its predecessor's ([`crate::toolchain::release_list`]). The
//! one-off `KRUSTY_SHRINK_UNCOVERED_IMPLEMENTATIONS=1` maintenance mode creates a missing list or
//! removes entries which became implemented; it never admits a newly uncovered declaration to an
//! existing list.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use super::super::{KlibLibraries, KlibLibraryInput};
use super::implementation_inventory::{
    fixture_property, identity, implementation_inventory, inventory, libraries, property_identity,
    wasm_intrinsic_tree, DecodedBodyState, ImplementationFact,
};
use crate::compilation_target::CompilationTarget;
use crate::libraries::{CallablePlacement, KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::{KlibNamePath, KlibPublicIdSignature, MetadataAccessor};

fn push_sized(output: &mut String, value: &str) {
    let _ = write!(output, "{}:{value}", value.len());
}

fn push_path(output: &mut String, path: &KlibNamePath) {
    let _ = write!(output, "{}[", path.segments().len());
    for (index, segment) in path.segments().iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        push_sized(output, segment);
    }
    output.push(']');
}

fn push_public_signature(output: &mut String, signature: &KlibPublicIdSignature) {
    output.push_str("package=");
    push_path(output, signature.package());
    output.push_str(" declaration=");
    push_path(output, signature.declaration());
    output.push_str(" member=");
    match signature.member_id() {
        Some(member) => {
            let _ = write!(output, "{member}");
        }
        None => output.push('-'),
    }
    let _ = write!(output, " mask={}", signature.mask());
}

/// A lossless text form of the provider-owned exact identity used as the committed ratchet key.
/// Length-prefixed module/path segments make separators inside Kotlin names unambiguous.
fn exact_identity(identity: &KlibDependencyCallable) -> String {
    let mut output = String::from("module=");
    push_sized(&mut output, &identity.module);
    match &identity.declaration {
        KlibDeclarationSignature::Public(signature) => {
            output.push_str(" public ");
            push_public_signature(&mut output, signature);
        }
        KlibDeclarationSignature::Accessor(signature) => {
            output.push_str(" accessor property{");
            push_public_signature(&mut output, signature.property());
            output.push_str("} name=");
            push_sized(&mut output, signature.name());
            let _ = write!(
                output,
                " member={} mask={}",
                signature.member_id(),
                signature.mask()
            );
        }
    }
    output
}

fn needs_implementation(fact: ImplementationFact) -> bool {
    fact.external || fact.body == DecodedBodyState::Bodyless
}

/// Every exact eligible declaration for which the normalized provider publishes no operation.
fn uncovered_implementations(libraries: &KlibLibraries) -> BTreeSet<String> {
    implementation_inventory(libraries)
        .into_iter()
        .filter(|(_, fact)| needs_implementation(*fact) && !fact.has_operation)
        .map(|(identity, _)| exact_identity(&identity))
        .collect()
}

/// The target's lists, as the reference version reads and records them.
fn release_list(target: &str) -> crate::toolchain::ReleaseList {
    crate::toolchain::release_list(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/klib_uncovered_implementations")
            .join(target),
    )
}

fn target_is_required(target: &str) -> bool {
    match std::env::var("KRUSTY_REQUIRE_KLIB_IMPLEMENTATION_TARGETS") {
        Ok(required) => required.split(',').any(|entry| entry == target),
        Err(_) => std::env::var_os("KRUSTY_REQUIRE_KLIB").is_some(),
    }
}

fn check_target(target: CompilationTarget, list: &str) {
    let Some(stdlib) = crate::toolchain::kotlin_stdlib_klib(target) else {
        assert!(
            !target_is_required(list),
            "{target:?} is required by KRUSTY_REQUIRE_KLIB_IMPLEMENTATION_TARGETS but its stdlib KLIB is not provisioned"
        );
        return;
    };
    let semantics = match target {
        CompilationTarget::Native => super::super::KlibStdlibSemantics::Native,
        CompilationTarget::WasmJs | CompilationTarget::WasmWasi => {
            super::super::KlibStdlibSemantics::Wasm
        }
        CompilationTarget::Js | CompilationTarget::Jvm => {
            panic!("{target:?} is not a Native/Wasm implementation-coverage target")
        }
    };
    let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(stdlib, semantics)])
        .unwrap_or_else(|error| panic!("{error}"));
    let uncovered = uncovered_implementations(&libraries);
    let path = release_list(list).read;
    let shrink = std::env::var_os("KRUSTY_SHRINK_UNCOVERED_IMPLEMENTATIONS").is_some();
    let listed = read_list(&path);
    let Some(listed) = listed else {
        if shrink {
            write_list(list, &uncovered);
            return;
        }
        panic!(
            "no committed exact-identity backlog for {target:?} at {}; create it once with \
             KRUSTY_SHRINK_UNCOVERED_IMPLEMENTATIONS=1",
            path.display()
        );
    };
    let added: Vec<_> = uncovered.difference(&listed).collect();
    assert!(
        added.is_empty(),
        "{target:?} stdlib external/bodyless declarations with no compiler operation, not in {}:\n{}",
        path.display(),
        added
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let stale: Vec<_> = listed.difference(&uncovered).collect();
    if stale.is_empty() {
        return;
    }
    if shrink {
        write_list(list, &uncovered);
        return;
    }
    panic!(
        "{} is stale: these exact declarations are implemented or no longer eligible; remove them, \
         or rerun with KRUSTY_SHRINK_UNCOVERED_IMPLEMENTATIONS=1:\n{}",
        path.display(),
        stale
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn read_list(path: &std::path::Path) -> Option<BTreeSet<String>> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.lines().map(str::to_string).collect())
}

/// Record the target reference version's list: in its own file, unless it is exactly the list the
/// version would inherit.
fn write_list(target: &str, uncovered: &BTreeSet<String>) {
    let lists = release_list(target);
    let path = lists.own;
    if lists
        .inherited
        .and_then(|inherited| read_list(&inherited))
        .as_ref()
        == Some(uncovered)
    {
        if path.exists() {
            std::fs::remove_file(&path)
                .unwrap_or_else(|error| panic!("remove {}: {error}", path.display()));
        }
        return;
    }
    std::fs::create_dir_all(path.parent().expect("the list has a directory"))
        .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
    let mut text = String::new();
    for line in uncovered {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

#[test]
fn backlog_keys_keep_the_exact_provider_module() {
    let declaration = super::function("same", None, Vec::new());
    let first = identity("first", &["fixture"], &[], &declaration);
    let second = identity("second", &["fixture"], &[], &declaration);
    assert_eq!(first.declaration, second.declaration);
    assert_ne!(exact_identity(&first), exact_identity(&second));
    assert!(exact_identity(&first).starts_with("module=5:first public "));
}

#[test]
fn implementation_eligibility_is_external_or_decoded_bodyless() {
    let fact = |external, body| ImplementationFact {
        placement: CallablePlacement::Package(crate::types::TypeName::ROOT),
        body,
        external,
        has_operation: false,
    };
    assert_eq!(
        [
            needs_implementation(fact(false, DecodedBodyState::Absent)),
            needs_implementation(fact(false, DecodedBodyState::Bodyful)),
            needs_implementation(fact(true, DecodedBodyState::Absent)),
            needs_implementation(fact(false, DecodedBodyState::Bodyless)),
            needs_implementation(fact(true, DecodedBodyState::Bodyless)),
        ],
        [false, false, true, true, true]
    );
}

#[test]
fn operation_coverage_does_not_cross_equal_declarations_in_distinct_modules() {
    let declaration = || {
        let mut function = super::function(
            "wasm_i32_reinterpret_f32",
            None,
            vec![super::class("kotlin/Float", false)],
        );
        function.ret = super::class("kotlin/Int", false);
        function.is_external = true;
        function
    };
    let implemented = identity(
        "implemented",
        &["kotlin", "wasm", "internal"],
        &[],
        &declaration(),
    );
    let unimplemented = identity(
        "unimplemented",
        &["kotlin", "wasm", "internal"],
        &[],
        &declaration(),
    );
    let implemented_trees = wasm_intrinsic_tree(&implemented);
    let unimplemented_trees = wasm_intrinsic_tree(&unimplemented);
    let mut declarations = inventory(vec![
        (
            super::segments(&["kotlin", "wasm", "internal"]),
            super::package_of(vec![declaration()]),
            implemented.module.clone(),
        ),
        (
            super::segments(&["kotlin", "wasm", "internal"]),
            super::package_of(vec![declaration()]),
            unimplemented.module.clone(),
        ),
    ]);
    declarations.attach_ir_intrinsics(
        &implemented.module,
        Some(super::super::target_intrinsics::TargetIntrinsicPlatform::Wasm),
        &implemented_trees,
    );
    declarations.attach_ir_intrinsics(&unimplemented.module, None, &unimplemented_trees);
    let libraries = libraries(
        declarations,
        vec![
            (implemented.module.clone(), implemented_trees),
            (unimplemented.module.clone(), unimplemented_trees),
        ],
    );

    assert_eq!(
        uncovered_implementations(&libraries),
        BTreeSet::from([exact_identity(&unimplemented)])
    );
}

#[test]
fn backlog_keys_render_the_complete_public_and_accessor_identities() {
    let function = super::function("same", None, Vec::new());
    let public = identity("mod:ule", &["pack,age"], &[], &function);
    let property = fixture_property("value", true);
    let accessor = property_identity(
        "mod:ule",
        &["pack,age"],
        &[],
        &property,
        MetadataAccessor::Setter,
    );
    let KlibDeclarationSignature::Public(public_signature) = &public.declaration else {
        panic!("the fixture function has a public signature")
    };
    let KlibDeclarationSignature::Accessor(accessor_signature) = &accessor.declaration else {
        panic!("the fixture setter has an accessor signature")
    };
    assert_eq!(
        exact_identity(&public),
        format!(
            "module=7:mod:ule public package=1[8:pack,age] declaration=1[4:same] member={} mask={}",
            public_signature
                .member_id()
                .expect("a function has a member id"),
            public_signature.mask()
        )
    );
    assert_eq!(
        exact_identity(&accessor),
        format!(
            "module=7:mod:ule accessor property{{package=1[8:pack,age] declaration=1[5:value] member={} mask={}}} name=11:<set-value> member={} mask={}",
            accessor_signature
                .property()
                .member_id()
                .expect("a property has a member id"),
            accessor_signature.property().mask(),
            accessor_signature.member_id(),
            accessor_signature.mask()
        )
    );
}

#[test]
fn native_stdlib_implementation_backlog_only_shrinks() {
    check_target(CompilationTarget::Native, "native");
}

#[test]
fn wasm_js_stdlib_implementation_backlog_only_shrinks() {
    check_target(CompilationTarget::WasmJs, "wasm-js");
}

#[test]
fn wasm_wasi_stdlib_implementation_backlog_only_shrinks() {
    check_target(CompilationTarget::WasmWasi, "wasm-wasi");
}
