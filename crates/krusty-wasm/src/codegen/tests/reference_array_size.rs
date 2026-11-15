use super::*;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

#[test]
fn selected_array_size_getters_reach_reference_storage_in_complete_programs() {
    let sources = [
        include_str!("../../../../../tests/box_regressions/arrayElementWriteOrder.kt"),
        include_str!("../../../../../tests/box_regressions/arrayLiteralSpread.kt"),
        include_str!("../../../../../tests/box_regressions/arrayLiteralSpreadMutation.kt"),
    ];
    for target in WasmTarget::ALL {
        let path = krusty::toolchain::kotlin_stdlib_klib(
            krusty::backend::Backend::compilation_target(&WasmBackend::new(target)),
        )
        .expect("the exact target stdlib must be provisioned");
        for source in sources {
            let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
                path.clone(),
                KlibStdlibSemantics::Wasm,
            )])
            .unwrap();
            let (artifacts, diagnostics) =
                compile_program_with_provider(target, Entry::Box, &[source], Box::new(libraries));
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(artifacts.len(), 2);
        }
    }
}
