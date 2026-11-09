use super::*;

fn getter(source: Ty) -> IrCheckedOperation {
    IrCheckedOperation::ExternalPropertyRead {
        target: crate::fir::ExternalPropertyId::from_raw(7),
        dispatch: IrPropertyDispatch::Ordinary,
        receiver: Some(23),
        arguments: Vec::new(),
        parameters: Vec::new(),
        result: Ty::Int,
        source_receiver: Some(source),
    }
}

#[test]
fn size_frames_keep_reference_arrays_and_int_array_storage() {
    let formal = Ty::ty_param(
        "array-element",
        Ty::nullable(Ty::obj_name(crate::types::wk::any())),
    );
    let mut admitted = Vec::new();
    for element in [Ty::String, Ty::Int, formal, Ty::out_projection(formal)] {
        admitted.push(Ty::obj_args_name(crate::types::wk::array(), &[element]));
    }
    admitted.push(Ty::array(Ty::Int));
    admitted.push(Ty::nullable(Ty::array(Ty::Int)));
    for array in admitted {
        assert_eq!(
            array_size_receiver(&getter(array)),
            Ok((23, array.non_null()))
        );
    }
}

#[test]
fn size_frames_reject_missing_or_extra_operands_and_other_primitive_storage() {
    let valid = getter(Ty::array(Ty::String));
    let mut malformed = Vec::new();
    for replacement in [
        None,
        Some(Ty::Int),
        Some(Ty::array(Ty::Long)),
        Some(Ty::array(Ty::Byte)),
    ] {
        let mut operation = valid.clone();
        let IrCheckedOperation::ExternalPropertyRead {
            source_receiver, ..
        } = &mut operation
        else {
            unreachable!()
        };
        *source_receiver = replacement;
        malformed.push(operation);
    }
    for field in 0..5 {
        let mut operation = valid.clone();
        let IrCheckedOperation::ExternalPropertyRead {
            receiver,
            dispatch,
            arguments,
            parameters,
            result,
            ..
        } = &mut operation
        else {
            unreachable!()
        };
        match field {
            0 => *receiver = None,
            1 => {
                *dispatch = IrPropertyDispatch::Super {
                    owner: crate::types::wk::array(),
                    interface: false,
                }
            }
            2 => arguments.push(24),
            3 => parameters.push(Ty::Int),
            4 => *result = Ty::Long,
            _ => unreachable!(),
        }
        malformed.push(operation);
    }
    for operation in malformed {
        assert_eq!(
            array_size_receiver(&operation),
            Err("an authenticated array size read with a mismatched getter frame".to_owned())
        );
    }
}
