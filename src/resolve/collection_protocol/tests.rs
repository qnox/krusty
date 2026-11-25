use super::*;
use crate::compilation_target::CompilationTarget;
use crate::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

/// Both Kotlin/Wasm stdlibs declare the one `Iterable.iterator`, `Iterator.hasNext`,
/// `Iterator.next` and `Collection.contains` the handoff freezes, each as its own interface's
/// member.
#[test]
fn each_wasm_stdlib_declares_the_collection_members_a_list_reads() {
    for target in [CompilationTarget::WasmJs, CompilationTarget::WasmWasi] {
        let path =
            crate::toolchain::kotlin_stdlib_klib_or_reason(target).unwrap_or_else(|reason| {
                panic!("the exact target stdlib must be provisioned: {reason}")
            });
        let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
            path,
            KlibStdlibSemantics::Wasm,
        )])
        .unwrap();
        let protocol = selected(&libraries).expect("the collection interfaces declare the members");
        for (identity, owner, name) in [
            (protocol.iterator, wk::iterable(), "iterator"),
            (protocol.has_next, wk::iterator(), "hasNext"),
            (protocol.next, wk::iterator(), "next"),
            (protocol.contains, wk::collection(), "contains"),
        ] {
            let callable = libraries
                .external_callable(identity)
                .expect("the selected member is a provider callable")
                .callable;
            assert_eq!(callable.name, name, "{target:?}");
            assert_eq!(
                callable.declaration_owner,
                Some(crate::types::SemanticCallableOwner::Classifier(owner)),
                "{target:?}"
            );
        }
    }
}
