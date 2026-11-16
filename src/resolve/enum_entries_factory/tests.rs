use super::*;
use crate::compilation_target::CompilationTarget;
use crate::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

/// Both Kotlin/Wasm stdlibs declare the one `enumEntries(Array<E>)` the enum realization calls:
/// a top-level function of `kotlin.enums` with one type parameter, taking an `Array` of it and
/// answering `EnumEntries` of it.
#[test]
fn each_wasm_stdlib_declares_the_enum_entries_factory() {
    for target in [CompilationTarget::WasmJs, CompilationTarget::WasmWasi] {
        let path = crate::toolchain::kotlin_stdlib_klib(target)
            .expect("the exact target stdlib must be provisioned");
        let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
            path,
            KlibStdlibSemantics::Wasm,
        )])
        .unwrap();
        let factory = selected(&libraries).expect("`kotlin.enums` declares `enumEntries(Array)`");
        let declaration = libraries
            .external_callable(factory.function)
            .expect("the selected factory is a provider callable")
            .callable;
        assert_eq!(declaration.name, "enumEntries", "{target:?}");
        assert_eq!(
            declaration.declaration_owner,
            Some(crate::types::SemanticCallableOwner::Package(type_name(
                "kotlin/enums"
            ))),
            "{target:?}"
        );
        assert_eq!(
            declaration.ret.non_null().obj_internal(),
            Some(type_name("kotlin/enums/EnumEntries")),
            "{target:?}"
        );
    }
}
