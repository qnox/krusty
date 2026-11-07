use super::*;

#[test]
fn selected_array_size_getters_reach_reference_storage_in_complete_programs() {
    let sources = [
        include_str!("../../../../tests/box_regressions/arrayElementWriteOrder.kt"),
        include_str!("../../../../tests/box_regressions/arrayLiteralSpread.kt"),
        include_str!("../../../../tests/box_regressions/arrayLiteralSpreadMutation.kt"),
    ];
    for target in WasmTarget::ALL {
        for source in sources {
            let (artifacts, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(artifacts.len(), 2);
        }
    }
}
