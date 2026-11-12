use super::*;

fn getter(source: Ty) -> IrCheckedOperation {
    IrCheckedOperation::ExternalPropertyRead {
        target: krusty::fir::ExternalPropertyId::from_raw(7),
        dispatch: IrPropertyDispatch::Ordinary,
        receiver: Some(23),
        arguments: Vec::new(),
        parameters: Vec::new(),
        result: Ty::Int,
        source_receiver: Some(source),
    }
}

#[test]
fn size_frames_keep_generic_and_projected_reference_array_storage() {
    let formal = Ty::ty_param(
        "array-element",
        Ty::nullable(Ty::obj_name(krusty::types::wk::any())),
    );
    for element in [Ty::String, Ty::Int, formal, Ty::out_projection(formal)] {
        let array = Ty::obj_args_name(krusty::types::wk::array(), &[element]);
        assert_eq!(array_size_receiver(&getter(array)), Ok((23, array)));
    }
}

#[test]
fn size_frames_keep_primitive_array_storage() {
    for element in [
        Ty::Boolean,
        Ty::Byte,
        Ty::Short,
        Ty::Char,
        Ty::Int,
        Ty::Long,
        Ty::Float,
        Ty::Double,
    ] {
        let array = Ty::array(element);
        assert!(!array.is_reference_array());
        assert_eq!(array_size_receiver(&getter(array)), Ok((23, array)));
    }
}

#[test]
fn size_frames_reject_missing_or_extra_operands_and_non_array_receivers() {
    let valid = getter(Ty::array(Ty::String));
    let mut malformed = Vec::new();
    for replacement in [None, Some(Ty::Int), Some(Ty::String)] {
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
                    owner: krusty::types::wk::array(),
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
