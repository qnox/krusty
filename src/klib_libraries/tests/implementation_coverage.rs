//! Every target-stdlib callable which is `external` or has a decoded bodyless IR declaration has
//! a compiler operation, or remains in the exact-identity implementation backlog.
//!
//! Eligibility is declaration/body evidence from the selected KLIB, never an annotation or source
//! spelling. Backlog keys retain the provider module and complete serialized declaration identity;
//! the paths embedded in those identities are output only. Lists live under
//! `tests/klib_uncovered_implementations/<target>/<version>.txt` and may only shrink. The one-off
//! `KRUSTY_SHRINK_UNCOVERED_IMPLEMENTATIONS=1` maintenance mode creates a missing list or removes
//! entries which became implemented; it never admits a newly uncovered declaration to an existing
//! list.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use super::super::{KlibLibraries, KlibLibraryInput};
use super::implementation_inventory::{
    identity, implementation_inventory, DecodedBodyState, ImplementationFact,
};
use crate::compilation_target::CompilationTarget;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::{KlibNamePath, KlibPublicIdSignature};

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

fn list_path(target: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/klib_uncovered_implementations")
        .join(target)
        .join(format!("{}.txt", crate::kotlin_version::target()))
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
    let path = list_path(list);
    let shrink = std::env::var_os("KRUSTY_SHRINK_UNCOVERED_IMPLEMENTATIONS").is_some();
    let listed: Option<BTreeSet<String>> = std::fs::read_to_string(&path)
        .ok()
        .map(|text| text.lines().map(str::to_string).collect());
    let Some(listed) = listed else {
        if shrink {
            write_list(&path, &uncovered);
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
        write_list(&path, &uncovered);
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

fn write_list(path: &std::path::Path, uncovered: &BTreeSet<String>) {
    std::fs::create_dir_all(path.parent().expect("the list has a directory"))
        .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
    let mut text = String::new();
    for line in uncovered {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(path, text).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
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
