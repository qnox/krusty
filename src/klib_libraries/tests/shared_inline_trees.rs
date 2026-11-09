//! Prepared-inline trees of the Native stdlib are shared per archive generation and exact selection.

use std::sync::Arc;

use super::super::shared_trees::SharedTrees;
use super::analysis::native_stdlib;
use crate::klib::KlibArchive;

#[test]
fn prepared_inline_trees_are_shared_only_for_the_same_selection() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let path =
        crate::toolchain::kotlin_stdlib_klib(crate::compilation_target::CompilationTarget::Native)
            .unwrap();
    let archive = KlibArchive::open(&path).expect("the Native stdlib opens");
    assert!(archive.has_inlinable_ir());
    let module = archive
        .manifest()
        .expect("manifest")
        .unique_name()
        .expect("unique_name")
        .to_owned();
    let selection = libraries.inventory.inline_body_selection();
    let full = selection.of_module(&module).clone();
    assert!(full.len() > 1, "the stdlib selects its inline functions");

    let shared = SharedTrees::new(2);
    let first = shared
        .inline_declaration_trees(&archive, &full)
        .expect("decode the full selection");
    let withdrawn = first
        .function_signatures()
        .find_map(|signature| match signature {
            crate::metadata::klib_ir::KlibIrSignature::Public(signature) => Some(signature.clone()),
            _ => None,
        })
        .expect("a prepared inline function of the selection");
    let mut fewer = full.clone();
    assert!(fewer.remove(&withdrawn));
    let reopened = KlibArchive::open(&path).expect("reopen the Native stdlib");
    let again = shared
        .inline_declaration_trees(&reopened, &full)
        .expect("the full selection again");
    assert!(Arc::ptr_eq(&first, &again));

    let narrowed = shared
        .inline_declaration_trees(&reopened, &fewer)
        .expect("decode the narrower selection");
    assert!(!Arc::ptr_eq(&first, &narrowed));
    let withdrawn = crate::metadata::klib_ir::KlibIrSignature::Public(withdrawn);
    assert!(first
        .function_signatures()
        .any(|signature| signature == &withdrawn));
    assert!(narrowed
        .function_signatures()
        .all(|signature| signature != &withdrawn));
    assert_eq!(
        first.function_signatures().count(),
        narrowed.function_signatures().count() + 1
    );

    let full_again = shared
        .inline_declaration_trees(&reopened, &full)
        .expect("the full selection stays resident beside the narrower one");
    assert!(Arc::ptr_eq(&first, &full_again));
}
