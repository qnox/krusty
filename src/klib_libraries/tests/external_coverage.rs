//! Every `external` function of each target's stdlib KLIB has a compiler operation.
//!
//! An `external` declaration has no IR body: the target supplies it. Its operation is attached by
//! the shared builtin rules, by exact declaration, never from the target's own intrinsic
//! annotation. A declaration the rules do not name yet is listed in
//! `tests/klib_uncovered_externals/<target>/<version>.txt`. The list may only shrink: a newly
//! uncovered declaration fails, and so does a listed one that is now covered or no longer exists.
//! With `KRUSTY_SHRINK_UNCOVERED_EXTERNALS` set, the test rewrites a list without the entries it
//! no longer needs; it never adds one to an existing list.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::super::classifier_records::classifier_record;
use super::super::lookup::published_function;
use super::super::KlibLibraries;
use crate::compilation_target::CompilationTarget;
use crate::libraries::{CallablePlacement, FunctionInfo, MemberRealization};
use crate::metadata::semantic::{KotlinFunction, KotlinType};

/// Whether the published `function` carries an operation a backend realizes in place of a body.
fn has_operation(function: &FunctionInfo) -> bool {
    function.callable.compiler_intrinsic.is_some()
        || function.callable.semantic_role.is_some()
        || matches!(
            function.callable.member_realization,
            MemberRealization::Intrinsic(_) | MemberRealization::RangeConstruction { .. }
        )
}

fn render_type(ty: &KotlinType) -> String {
    match ty {
        KotlinType::Class {
            internal,
            args,
            nullable,
            ..
        } => {
            let mut rendered = internal.replace('/', ".");
            if !args.is_empty() {
                let args: Vec<_> = args.iter().map(render_type).collect();
                rendered = format!("{rendered}<{}>", args.join(", "));
            }
            if *nullable {
                rendered.push('?');
            }
            rendered
        }
        KotlinType::Param { name, nullable, .. } => {
            format!("{name}{}", if *nullable { "?" } else { "" })
        }
        KotlinType::InProjection(inner) => format!("in {}", render_type(inner)),
        KotlinType::OutProjection(inner) => format!("out {}", render_type(inner)),
        KotlinType::Star => "*".to_string(),
    }
}

/// `<container>: fun [Receiver.]name(Params): Ret`, one line per declaration in the committed
/// list. The container is `package p`, `class C` or `companion of C`.
fn render(container: &str, function: &KotlinFunction) -> String {
    let receiver = function
        .receiver
        .as_ref()
        .map(|receiver| format!("{}.", render_type(receiver)))
        .unwrap_or_default();
    let params: Vec<_> = function.params.iter().map(render_type).collect();
    format!(
        "{container}: fun {receiver}{}({}): {}",
        function.name,
        params.join(", "),
        render_type(&function.ret)
    )
}

/// A declaration owner as Kotlin source spells it, for the committed list.
fn dotted(name: crate::types::TypeName) -> String {
    name.render().replace('/', ".")
}

/// Every `external` function `libraries` publishes that has no compiler operation.
fn uncovered_externals(libraries: &KlibLibraries) -> BTreeSet<String> {
    let inventory = &libraries.inventory;
    let identities = &libraries.identities;
    let mut uncovered = BTreeSet::new();
    for (package, signed) in inventory.all_functions() {
        if !signed.declaration.is_external {
            continue;
        }
        let published = published_function(
            identities,
            signed,
            CallablePlacement::Package(package),
            &Default::default(),
        );
        if !has_operation(&published) {
            uncovered.insert(render(
                &format!("package {}", dotted(package)),
                &signed.declaration,
            ));
        }
    }
    for (identity, signed) in inventory.all_classifiers() {
        let externals: Vec<_> = signed
            .functions
            .iter()
            .chain(&signed.associated_functions)
            .filter(|function| function.declaration.is_external)
            .collect();
        if externals.is_empty() {
            continue;
        }
        let record = classifier_record(inventory, identities, identity)
            .expect("an inventory classifier has a record");
        let owner = dotted(identity);
        for function in signed
            .functions
            .iter()
            .filter(|function| function.declaration.is_external)
        {
            // The record publishes a name's members in declaration order.
            let index = signed
                .functions
                .iter()
                .filter(|other| other.declaration.name == function.declaration.name)
                .position(|other| std::ptr::eq(other, function))
                .expect("the member is among its own name's members");
            let published =
                &record.declared_callables[&function.declaration.name].functions()[index];
            if !has_operation(published) {
                uncovered.insert(render(&format!("class {owner}"), &function.declaration));
            }
        }
        // A `companion { … }` block member is published with no builtin operation.
        for function in signed
            .associated_functions
            .iter()
            .filter(|function| function.declaration.is_external)
        {
            uncovered.insert(render(
                &format!("companion of {owner}"),
                &function.declaration,
            ));
        }
    }
    for (classifier, extension) in inventory.all_companion_functions() {
        if extension.signed.declaration.is_external {
            let owner = format!("companion of {}", dotted(classifier));
            uncovered.insert(render(&owner, &extension.signed.declaration));
        }
    }
    uncovered
}

fn list_path(target: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/klib_uncovered_externals")
        .join(target)
        .join(format!("{}.txt", crate::kotlin_version::target()))
}

fn check_target(target: CompilationTarget, list: &str) {
    let Some(stdlib) = crate::toolchain::kotlin_stdlib_klib(target) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "KRUSTY_REQUIRE_KLIB is set but no {target:?} stdlib KLIB is provisioned"
        );
        return;
    };
    let libraries = KlibLibraries::open(&[stdlib]).unwrap_or_else(|error| panic!("{error}"));
    let uncovered = uncovered_externals(&libraries);
    let path = list_path(list);
    let shrink = std::env::var_os("KRUSTY_SHRINK_UNCOVERED_EXTERNALS").is_some();
    let listed: Option<BTreeSet<String>> = std::fs::read_to_string(&path)
        .ok()
        .map(|text| text.lines().map(str::to_string).collect());
    let Some(listed) = listed else {
        if shrink {
            write_list(&path, &uncovered);
            return;
        }
        panic!(
            "no committed list of uncovered {target:?} externals at {}; \
             write it with KRUSTY_SHRINK_UNCOVERED_EXTERNALS=1",
            path.display()
        );
    };
    let added: Vec<_> = uncovered.difference(&listed).collect();
    assert!(
        added.is_empty(),
        "{target:?} stdlib externals with no compiler operation, not in {}:\n{}",
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
        "{} is stale: these entries are covered or no longer declared; remove them, or rerun \
         with KRUSTY_SHRINK_UNCOVERED_EXTERNALS=1:\n{}",
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
fn native_stdlib_externals_have_compiler_operations() {
    check_target(CompilationTarget::Native, "native");
}

#[test]
fn js_stdlib_externals_have_compiler_operations() {
    check_target(CompilationTarget::Js, "js");
}

#[test]
fn wasm_js_stdlib_externals_have_compiler_operations() {
    check_target(CompilationTarget::WasmJs, "wasm-js");
}

#[test]
fn wasm_wasi_stdlib_externals_have_compiler_operations() {
    check_target(CompilationTarget::WasmWasi, "wasm-wasi");
}
