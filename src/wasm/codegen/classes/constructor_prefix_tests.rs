use super::check_supported;
use crate::ir::{IrClass, IrConstructorCapture, IrCtorParameterProvenance};
use crate::types::type_name;

fn forwarded_classes() -> Vec<IrClass> {
    let ir = crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../tests/box_regressions/nestedAnonymousConstructionOrder.kt"),
        "NestedAnonymousConstructionOrder",
    );
    let classes = ir
        .classes
        .into_iter()
        .filter(|class| class.is_local_class)
        .collect::<Vec<_>>();
    assert_eq!(classes.len(), 2, "both anonymous constructor declarations");
    for (base, prefix) in [("Leaf", 0), ("Container", 1)] {
        let matching = classes
            .iter()
            .filter(|class| {
                class
                    .supertypes
                    .iter()
                    .any(|ty| ty.kotlin_class_internal() == Some(type_name(base)))
            })
            .collect::<Vec<_>>();
        let [class] = matching.as_slice() else {
            panic!("one anonymous subclass of {base}")
        };
        assert_eq!(class.constructor_prefix_count, prefix);
        assert_eq!(class.ctor_args.len(), prefix as usize);
    }
    classes
}

#[test]
fn forwarded_super_constructor_operands_are_ordinary_values() {
    for class in forwarded_classes() {
        assert_eq!(check_supported(&class), Ok(()));
    }
}

#[test]
fn capture_storage_and_unproven_prefix_roles_still_decline() {
    let original = forwarded_classes()
        .into_iter()
        .find(|class| class.constructor_prefix_count == 1)
        .expect("authenticated forwarded class");
    let expected = Err(format!(
        "a class capturing its surroundings (`{}`)",
        original.fq_name()
    ));
    for provenance in [
        IrCtorParameterProvenance::Capture,
        IrCtorParameterProvenance::EnclosingInstance,
        IrCtorParameterProvenance::Continuation,
        IrCtorParameterProvenance::ContinuationDispatchReceiver,
        IrCtorParameterProvenance::ScriptReceiver { ordinal: 0 },
    ] {
        let mut class = original.clone();
        class.ctor_args[0].provenance = provenance;
        assert_eq!(check_supported(&class), expected, "{provenance:?}");
    }
    let mut missing_role = original.clone();
    missing_role.ctor_args[0].anonymous_super_forward = None;
    assert_eq!(check_supported(&missing_role), expected);
    for ordinal in [0, 2] {
        let mut wrong_ordinal = original.clone();
        wrong_ordinal.ctor_args[0].anonymous_super_forward = Some(ordinal);
        assert_eq!(check_supported(&wrong_ordinal), expected);
    }
    let mut captured = original.clone();
    captured.ctor_args[0].capture = Some(IrConstructorCapture {
        source_name: "captured".into(),
        receiver: None,
        crossinline_parameter: None,
    });
    assert_eq!(check_supported(&captured), expected);
    let mut stored = original.clone();
    stored.ctor_args[0].is_field = true;
    stored.ctor_args[0].field_index = Some(0);
    assert_eq!(check_supported(&stored), expected);
    let mut field_slot = original.clone();
    field_slot.ctor_args[0].field_index = Some(0);
    assert_eq!(check_supported(&field_slot), expected);
    let mut inner = original.clone();
    inner.is_inner_class = true;
    inner.ctor_args[0].provenance = IrCtorParameterProvenance::EnclosingInstance;
    assert_eq!(check_supported(&inner), Ok(()));
    let mut missing_parameter = original;
    missing_parameter.ctor_args.clear();
    assert_eq!(check_supported(&missing_parameter), expected);
}
