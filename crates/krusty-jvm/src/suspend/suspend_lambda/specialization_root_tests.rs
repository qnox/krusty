use super::specialization_root;
use krusty::ir::{IrFile, IrSpecializedFunction};

fn copy(source: u32, parent: Option<u32>) -> IrSpecializedFunction {
    IrSpecializedFunction {
        source,
        caller_declaration: krusty::fir::DeclarationId::from_raw(0),
        caller: Some(krusty::ir::IrEnclosure::File),
        caller_is_default: false,
        caller_source_name: "box".to_string(),
        inline_callee: krusty::ir::IrInlineCallee::Module(krusty::fir::CallableId::from_raw(0)),
        inline_callee_source_name: "next".to_string(),
        parent,
        consumed_inline_capture: false,
    }
}

#[test]
fn a_nested_copy_belongs_to_the_outermost_copy_of_its_expansion() {
    let mut ir = IrFile::default();
    // Function 7 is a declaration. 10 and 20 are two expansions' copies of its suspend lambda, and
    // 11 and 21 are the helpers each copied for its own expansion.
    ir.specialized_functions.insert(10, copy(7, None));
    ir.specialized_functions.insert(11, copy(8, Some(10)));
    ir.specialized_functions.insert(20, copy(7, None));
    ir.specialized_functions.insert(21, copy(8, Some(20)));
    ir.specialized_functions.insert(22, copy(9, Some(21)));

    assert_eq!(specialization_root(&ir, 7), None);
    assert_eq!(specialization_root(&ir, 8), None);
    assert_eq!(specialization_root(&ir, 10), Some(10));
    assert_eq!(specialization_root(&ir, 11), Some(10));
    assert_eq!(specialization_root(&ir, 21), Some(20));
    assert_eq!(specialization_root(&ir, 22), Some(20));
}
