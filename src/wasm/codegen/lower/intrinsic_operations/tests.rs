use super::*;
use crate::backend::Backend;
use crate::libraries::CompilerIntrinsic;

#[test]
fn the_exact_provider_operation_is_the_shared_string_read_capability() {
    assert_eq!(
        member_operation(CompilerIntrinsic::StringGet),
        Some(IrIntrinsic::StringGet)
    );
    for operation in [
        CompilerIntrinsic::StringLength,
        CompilerIntrinsic::StringPlus,
        CompilerIntrinsic::ArrayGet,
    ] {
        assert_eq!(member_operation(operation), None);
    }
    for target in crate::wasm::WasmTarget::ALL {
        assert!(crate::wasm::WasmBackend::new(target)
            .realizes_dependency_intrinsic(CompilerIntrinsic::StringGet));
    }
}

#[test]
fn a_string_read_rejects_changed_parameter_result_default_or_extension_roles() {
    assert_eq!(string_get_frame(&[Ty::Int], Ty::Char, &[], None), Ok(()));
    for (params, ret, defaults, extension) in [
        (Vec::new(), Ty::Char, Vec::new(), None),
        (vec![Ty::Long], Ty::Char, Vec::new(), None),
        (vec![Ty::Int, Ty::Int], Ty::Char, Vec::new(), None),
        (vec![Ty::Int], Ty::Int, Vec::new(), None),
        (vec![Ty::Int], Ty::Char, vec![0], None),
        (vec![Ty::Int], Ty::Char, Vec::new(), Some(0)),
    ] {
        assert_eq!(
            string_get_frame(&params, ret, &defaults, extension),
            Err("an authenticated string read with a mismatched call frame".into())
        );
    }
}
