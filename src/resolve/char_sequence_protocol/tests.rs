use super::*;
use crate::compilation_target::CompilationTarget;
use crate::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

/// Both Kotlin/Wasm stdlibs declare the one `CharSequence.length` property and `get(Int)`
/// function the handoff freezes, as the members `CharSequence` itself declares.
#[test]
fn each_wasm_stdlib_declares_the_char_sequence_members_an_append_reads() {
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
        let protocol = selected(&libraries).expect("`CharSequence` declares `length` and `get`");
        let length = libraries
            .external_property(protocol.length)
            .expect("the selected `length` is a provider property");
        assert_eq!(length.name, "length");
        let get = libraries
            .external_callable(protocol.get)
            .expect("the selected `get` is a provider callable")
            .callable;
        assert_eq!(get.name, "get");
        for owner in [
            libraries
                .external_callable(length.getter)
                .expect("the getter is a provider callable")
                .callable
                .declaration_owner,
            get.declaration_owner,
        ] {
            assert_eq!(
                owner,
                Some(crate::types::SemanticCallableOwner::Classifier(
                    char_sequence()
                )),
                "{target:?}"
            );
        }
    }
}
