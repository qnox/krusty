use super::{compile, WasmTarget};

#[test]
fn the_whole_named_companion_program_emits_both_wasm_targets() {
    let source = include_str!("../../../../tests/box_regressions/companionNamedOnNextLine.kt");
    for target in WasmTarget::ALL {
        let (outputs, diagnostics) = compile(target, source);
        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(outputs.len(), 2);
    }
}

#[test]
fn constructing_a_holder_directly_or_through_a_subclass_or_an_object_emits() {
    for source in [
        "class Holder { companion object {} }\nfun box(): String { Holder(); return \"OK\" }\n",
        "open class Holder { companion object {} }\nclass Child : Holder()\nfun box(): String { Child(); return \"OK\" }\n",
        "open class Holder { companion object {} }\nobject Child : Holder() { fun value() = \"OK\" }\nfun box(): String = Child.value()\n",
    ] {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }
}
