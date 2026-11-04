use super::check_supported;
use krusty::fir::ClassCaptureIdentity;
use krusty::ir::{
    ClassId, IrCapturedReceiver, IrClass, IrConstructorCapture, IrCtorParameterProvenance, IrFile,
};
use krusty::types::{type_name, Ty};

fn check_class(class: &IrClass) -> Result<(), super::Unsupported> {
    let mut ir = IrFile::default();
    ir.classes.push(class.clone());
    check_supported(&ir, 0)
}

fn forwarded_classes() -> Vec<IrClass> {
    let ir = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../tests/box_regressions/nestedAnonymousConstructionOrder.kt"),
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
        assert_eq!(check_class(&class), Ok(()));
    }
}

#[test]
fn stored_value_captures_use_the_existing_constructor_field_transport() {
    for (file, source) in [
        (
            "RetainedLexicalValueIdentity",
            include_str!("../../../../../tests/box_regressions/retainedLexicalValueIdentity.kt"),
        ),
        (
            "StoredClassCapturePrefixes",
            include_str!("../../../../../tests/box_regressions/storedClassCapturePrefixes.kt"),
        ),
    ] {
        let ir = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
            source, file,
        );
        let captured = ir
            .classes
            .iter()
            .enumerate()
            .filter(|(_, class)| {
                class
                    .ctor_args
                    .iter()
                    .any(|argument| argument.capture.is_some())
            })
            .map(|(id, _)| id as ClassId)
            .collect::<Vec<_>>();
        assert!(
            !captured.is_empty(),
            "whole source must produce captured classifiers"
        );
        for id in captured {
            assert_eq!(
                check_supported(&ir, id),
                Ok(()),
                "{}",
                ir.classes[id as usize].fq_name()
            );
        }
    }
}

#[test]
fn constructor_only_value_captures_use_parameters_without_capture_fields() {
    let ir = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../tests/box_regressions/constructorOnlyClassCaptures.kt"),
        "ConstructorOnlyClassCaptures",
    );
    let captured = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, class)| class.constructor_prefix_count != 0)
        .collect::<Vec<_>>();
    assert_eq!(
        captured.len(),
        2,
        "superclass and delegate constructor inputs"
    );
    for (id, class) in captured {
        assert_eq!(class.constructor_prefix_count, 2);
        assert_eq!(class.ctor_args.len(), 2);
        assert_eq!(class.pre_super_param_fields, []);
        for argument in &class.ctor_args {
            assert_eq!(argument.provenance, IrCtorParameterProvenance::Capture);
            assert!(argument.capture.is_some());
            assert!(!argument.is_field);
            assert_eq!(argument.field_index, None);
        }
        assert_eq!(check_supported(&ir, id as ClassId), Ok(()));
    }
}

#[test]
fn stored_and_constructor_only_captures_keep_independent_field_and_parameter_slots() {
    let ir = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../tests/box_regressions/mixedConstructorClassCaptures.kt"),
        "MixedConstructorClassCaptures",
    );
    let captured = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, class)| class.constructor_prefix_count != 0)
        .collect::<Vec<_>>();
    let [(id, class)] = captured.as_slice() else {
        panic!("the complete source has one captured classifier");
    };
    assert_eq!(class.constructor_prefix_count, 2);
    assert_eq!(class.ctor_args.len(), 2);
    assert_eq!(class.fields.len(), 1);
    let stored = class
        .ctor_args
        .iter()
        .enumerate()
        .filter(|(_, argument)| argument.is_field)
        .collect::<Vec<_>>();
    let [(parameter, argument)] = stored.as_slice() else {
        panic!("one capture survives into the member body");
    };
    assert_eq!(argument.ty, Ty::String);
    assert_eq!(argument.field_index, Some(0));
    assert_eq!(class.pre_super_param_fields, [(*parameter as u32, 0)]);
    let constructor_only = &class.ctor_args[1 - *parameter];
    assert_eq!(constructor_only.ty, Ty::Int);
    assert!(!constructor_only.is_field);
    assert_eq!(constructor_only.field_index, None);
    assert_eq!(check_supported(&ir, *id as ClassId), Ok(()));
}

#[test]
fn constructor_only_captures_reject_inconsistent_transport_roles() {
    type Mutation = fn(&mut IrFile, ClassId, u32);
    let mutations: &[(&str, Mutation)] = &[
        ("field flag without storage", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].is_field = true;
        }),
        ("field index without storage", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].field_index = Some(0);
        }),
        (
            "constructor-only parameter stored before super",
            |ir, id, p| {
                ir.classes[id as usize].pre_super_param_fields.push((p, 0));
            },
        ),
        ("mixed forward role", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].anonymous_super_forward = Some(1);
        }),
        ("missing capture payload", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].capture = None;
        }),
        ("receiver capture", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize]
                .capture
                .as_mut()
                .unwrap()
                .receiver = Some(IrCapturedReceiver::Lambda(None));
        }),
        ("identity registry mismatch", |ir, id, p| {
            ir.class_capture_identities
                .insert((id, p), ClassCaptureIdentity::Receiver(7));
        }),
        ("receiver identity", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].capture_identity =
                Some(ClassCaptureIdentity::Receiver(7));
            ir.class_capture_identities
                .insert((id, p), ClassCaptureIdentity::Receiver(7));
        }),
        ("shared capture field", |ir, id, _| {
            ir.shared_class_capture_fields.insert((id, 0), Ty::String);
        }),
        ("shared superclass edge", |ir, id, _| {
            ir.shared_super_capture_parameters
                .insert((id, 0), Ty::String);
        }),
        ("shared secondary superclass edge", |ir, id, _| {
            ir.shared_secondary_super_capture_parameters
                .insert((id, 0, 0), Ty::String);
        }),
    ];
    let original = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../tests/box_regressions/constructorOnlyClassCaptures.kt"),
        "ConstructorOnlyClassCaptures",
    );
    let class = original
        .classes
        .iter()
        .position(|class| class.constructor_prefix_count != 0)
        .expect("a real constructor-only capture") as ClassId;
    assert_eq!(check_supported(&original, class), Ok(()));
    let expected = Err(format!(
        "a class capturing its surroundings (`{}`)",
        original.classes[class as usize].fq_name()
    ));
    for (label, mutate) in mutations {
        let mut ir = copy_transport(&original);
        mutate(&mut ir, class, 0);
        assert_eq!(check_supported(&ir, class), expected, "{label}");
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
        assert_eq!(check_class(&class), expected, "{provenance:?}");
    }
    let mut missing_role = original.clone();
    missing_role.ctor_args[0].anonymous_super_forward = None;
    assert_eq!(check_class(&missing_role), expected);
    for ordinal in [0, 2] {
        let mut wrong_ordinal = original.clone();
        wrong_ordinal.ctor_args[0].anonymous_super_forward = Some(ordinal);
        assert_eq!(check_class(&wrong_ordinal), expected);
    }
    let mut captured = original.clone();
    captured.ctor_args[0].capture = Some(IrConstructorCapture {
        source_name: "captured".into(),
        receiver: None,
        crossinline_parameter: None,
    });
    assert_eq!(check_class(&captured), expected);
    let mut stored = original.clone();
    stored.ctor_args[0].is_field = true;
    stored.ctor_args[0].field_index = Some(0);
    assert_eq!(check_class(&stored), expected);
    let mut field_slot = original.clone();
    field_slot.ctor_args[0].field_index = Some(0);
    assert_eq!(check_class(&field_slot), expected);
    let mut inner = original.clone();
    inner.is_inner_class = true;
    inner.ctor_args[0].provenance = IrCtorParameterProvenance::EnclosingInstance;
    assert_eq!(check_class(&inner), Ok(()));
    let mut missing_parameter = original;
    missing_parameter.ctor_args.clear();
    assert_eq!(check_class(&missing_parameter), expected);
}

fn capture_transport() -> (IrFile, ClassId, u32) {
    let ir = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../tests/box_regressions/retainedLexicalValueIdentity.kt"),
        "RetainedLexicalValueIdentity",
    );
    let (class, parameter) = ir
        .classes
        .iter()
        .enumerate()
        .find_map(|(class, declaration)| {
            declaration
                .ctor_args
                .iter()
                .position(|argument| argument.capture.is_some())
                .map(|parameter| (class as ClassId, parameter as u32))
        })
        .expect("the real source has a captured value");
    assert_eq!(check_supported(&ir, class), Ok(()));
    (ir, class, parameter)
}

fn copy_transport(ir: &IrFile) -> IrFile {
    let mut copy = IrFile::default();
    copy.classes.extend(ir.classes.iter().cloned());
    copy.shared_class_capture_fields.extend(
        ir.shared_class_capture_fields
            .iter()
            .map(|(key, value)| (*key, *value)),
    );
    copy.shared_super_capture_parameters.extend(
        ir.shared_super_capture_parameters
            .iter()
            .map(|(key, value)| (*key, *value)),
    );
    copy.shared_secondary_super_capture_parameters.extend(
        ir.shared_secondary_super_capture_parameters
            .iter()
            .map(|(key, value)| (*key, *value)),
    );
    copy.class_capture_identities.extend(
        ir.class_capture_identities
            .iter()
            .map(|(key, value)| (*key, *value)),
    );
    copy
}

#[test]
fn incomplete_or_shared_capture_transport_is_rejected_without_panicking() {
    type Mutation = fn(&mut IrFile, ClassId, u32);
    let mutations: &[(&str, Mutation)] = &[
        ("missing capture payload", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].capture = None
        }),
        ("receiver capture", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize]
                .capture
                .as_mut()
                .unwrap()
                .receiver = Some(IrCapturedReceiver::Lambda(None))
        }),
        ("wrong provenance", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].provenance =
                IrCtorParameterProvenance::Value
        }),
        ("not stored", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].is_field = false
        }),
        ("fieldless", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].field_index = None
        }),
        ("unknown field", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].field_index = Some(u32::MAX)
        }),
        ("wrong storage type", |ir, id, p| {
            let field = ir.classes[id as usize].ctor_args[p as usize]
                .field_index
                .unwrap();
            ir.classes[id as usize].fields[field as usize].ty = Ty::Int;
        }),
        ("mutable storage", |ir, id, p| {
            let field = ir.classes[id as usize].ctor_args[p as usize]
                .field_index
                .unwrap();
            ir.classes[id as usize].fields[field as usize].flags = ir.classes[id as usize].fields
                [field as usize]
                .flags
                .with_is_final(false);
        }),
        ("missing pre-super store", |ir, id, _| {
            ir.classes[id as usize].pre_super_param_fields.clear()
        }),
        ("duplicate pre-super store", |ir, id, p| {
            let field = ir.classes[id as usize].ctor_args[p as usize]
                .field_index
                .unwrap();
            ir.classes[id as usize]
                .pre_super_param_fields
                .push((p, field));
        }),
        ("wrong pre-super source", |ir, id, p| {
            let field = ir.classes[id as usize].ctor_args[p as usize]
                .field_index
                .unwrap();
            ir.classes[id as usize].pre_super_param_fields = vec![(p + 1, field)];
        }),
        ("ambiguous field parameter", |ir, id, p| {
            let duplicate = ir.classes[id as usize].ctor_args[p as usize].clone();
            ir.classes[id as usize].ctor_args.push(duplicate);
        }),
        ("shared capture field", |ir, id, p| {
            let field = ir.classes[id as usize].ctor_args[p as usize]
                .field_index
                .unwrap();
            ir.shared_class_capture_fields
                .insert((id, field), Ty::String);
        }),
        ("shared superclass edge", |ir, id, _| {
            ir.shared_super_capture_parameters
                .insert((id, 0), Ty::String);
        }),
        ("shared secondary superclass edge", |ir, id, _| {
            ir.shared_secondary_super_capture_parameters
                .insert((id, 0, 0), Ty::String);
        }),
        ("mixed forward role", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].anonymous_super_forward = Some(1)
        }),
        ("identity registry mismatch", |ir, id, p| {
            ir.class_capture_identities
                .insert((id, p), ClassCaptureIdentity::Receiver(7));
        }),
        ("receiver identity", |ir, id, p| {
            ir.classes[id as usize].ctor_args[p as usize].capture_identity =
                Some(ClassCaptureIdentity::Receiver(7));
            ir.class_capture_identities
                .insert((id, p), ClassCaptureIdentity::Receiver(7));
        }),
        ("truncated prefix", |ir, id, _| {
            ir.classes[id as usize].constructor_prefix_count = u32::MAX
        }),
        ("inner flag cannot admit receiver captures", |ir, id, p| {
            ir.classes[id as usize].is_inner_class = true;
            ir.classes[id as usize].ctor_args[p as usize]
                .capture
                .as_mut()
                .unwrap()
                .receiver = Some(IrCapturedReceiver::Lambda(None));
        }),
        ("inner flag cannot admit shared holders", |ir, id, p| {
            ir.classes[id as usize].is_inner_class = true;
            let field = ir.classes[id as usize].ctor_args[p as usize]
                .field_index
                .unwrap();
            ir.shared_class_capture_fields
                .insert((id, field), Ty::String);
        }),
    ];
    let (original, class, parameter) = capture_transport();
    let expected = Err(format!(
        "a class capturing its surroundings (`{}`)",
        original.classes[class as usize].fq_name()
    ));
    for (label, mutate) in mutations {
        let mut ir = copy_transport(&original);
        mutate(&mut ir, class, parameter);
        assert_eq!(check_supported(&ir, class), expected, "{label}");
    }
}

#[test]
fn optional_and_upstream_capture_identities_are_separate_from_physical_fields() {
    let (mut ir, class, parameter) = capture_transport();
    ir.classes[class as usize].ctor_args[parameter as usize].capture_identity = None;
    ir.class_capture_identities.remove(&(class, parameter));
    assert_eq!(check_supported(&ir, class), Ok(()));
    let upstream = ClassCaptureIdentity::Field {
        owner: type_name("Upstream"),
        field: 42,
    };
    ir.classes[class as usize].ctor_args[parameter as usize].capture_identity = Some(upstream);
    ir.class_capture_identities
        .insert((class, parameter), upstream);
    assert_eq!(check_supported(&ir, class), Ok(()));
}

#[test]
fn forwarded_ordinals_ignore_preceding_capture_parameters() {
    let mut ir = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../tests/box_regressions/storedClassCapturePrefixes.kt"),
        "StoredClassCapturePrefixes",
    );
    let (class, parameter) = ir
        .classes
        .iter()
        .enumerate()
        .find_map(|(id, class)| {
            let capture = class
                .ctor_args
                .iter()
                .position(|argument| argument.capture.is_some())?;
            let forward = class
                .ctor_args
                .iter()
                .position(|argument| argument.anonymous_super_forward.is_some())?;
            (capture < forward).then_some((id as ClassId, forward))
        })
        .expect("real source combines capture and forwarded operands");
    assert_eq!(
        ir.classes[class as usize].ctor_args[parameter].anonymous_super_forward,
        Some(1)
    );
    assert_eq!(check_supported(&ir, class), Ok(()));
    ir.classes[class as usize].ctor_args[parameter].anonymous_super_forward =
        Some(parameter as u32 + 1);
    assert_eq!(
        check_supported(&ir, class),
        Err(format!(
            "a class capturing its surroundings (`{}`)",
            ir.classes[class as usize].fq_name()
        ))
    );
}

fn interface_delegate_transport() -> (IrFile, ClassId, u32, u32) {
    let ir = krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../tests/box_regressions/anonymousInterfaceDelegateOperands.kt"),
        "AnonymousInterfaceDelegateOperands",
    );
    let operands = ir
        .classes
        .iter()
        .enumerate()
        .flat_map(|(class, declaration)| {
            declaration
                .ctor_args
                .iter()
                .enumerate()
                .filter_map(move |(parameter, argument)| match argument.provenance {
                    IrCtorParameterProvenance::InterfaceDelegate { field, .. } => {
                        Some((class as ClassId, parameter as u32, field))
                    }
                    _ => None,
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        operands.len(),
        4,
        "all four complete anonymous delegate constructions"
    );
    for &(class, _, _) in &operands {
        assert_eq!(check_supported(&ir, class), Ok(()));
    }
    let result = operands[0];
    (ir, result.0, result.1, result.2)
}

#[test]
fn declared_interface_operand_matches_its_single_explicit_initializer_and_forwarder() {
    let (mut ir, class, parameter, field) = interface_delegate_transport();
    let mut pending = vec![ir.classes[class as usize]
        .init_body
        .expect("delegate initializer")];
    let mut stores = Vec::new();
    while let Some(expression) = pending.pop() {
        if let krusty::ir::IrExpr::SetField {
            receiver,
            class: owner,
            index,
            value,
        } = ir.expr(expression)
        {
            if *owner == class && *index == field {
                assert!(matches!(
                    ir.expr(*receiver),
                    krusty::ir::IrExpr::GetValue(0)
                ));
                assert!(
                    matches!(ir.expr(*value), krusty::ir::IrExpr::GetValue(slot) if *slot == parameter + 1)
                );
                stores.push(expression);
            }
        }
        krusty::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    assert_eq!(
        stores.len(),
        1,
        "the producer records one exact operand-to-field store"
    );
    let mut forwarder_fields = Vec::new();
    for &method in &ir.classes[class as usize].methods {
        if !ir.interface_delegation_forwarders.contains(&method) {
            continue;
        }
        let mut pending = vec![ir.functions[method as usize]
            .body
            .expect("selected forwarder")];
        while let Some(expression) = pending.pop() {
            if let krusty::ir::IrExpr::GetField {
                class: owner,
                index,
                ..
            } = ir.expr(expression)
            {
                forwarder_fields.push((*owner, *index));
            }
            krusty::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        }
    }
    assert_eq!(
        forwarder_fields,
        vec![(class, field)],
        "the complete selected interface member reaches that same delegate"
    );
    let IrCtorParameterProvenance::InterfaceDelegate {
        interface,
        field: selected_field,
    } = ir.classes[class as usize].ctor_args[parameter as usize].provenance
    else {
        panic!("selected interface operand")
    };
    let interface_name = interface
        .obj_internal()
        .expect("declared applied interface");
    let renamed = type_name("RenamedReading");
    let mut names = krusty::fx_hash::HashMap::default();
    names.insert(interface_name, renamed);
    ir.remap_classifier_identities(&names);
    assert_eq!(
        ir.classes[class as usize].ctor_args[parameter as usize].provenance,
        IrCtorParameterProvenance::InterfaceDelegate {
            interface: Ty::obj_name(renamed),
            field: selected_field
        }
    );
    assert_eq!(
        check_supported(&ir, class),
        Ok(()),
        "the role's interface follows the same typed name remap"
    );
}

#[test]
fn interface_delegate_prefix_requires_positive_role_and_unique_typed_storage() {
    type Mutation = fn(&mut IrFile, ClassId, u32, u32);
    let mutations: &[(&str, Mutation)] = &[
        ("unproven ordinary Value", |ir, class, parameter, _| {
            ir.classes[class as usize].ctor_args[parameter as usize].provenance =
                IrCtorParameterProvenance::Value;
        }),
        ("unknown target field", |ir, class, parameter, _| {
            let IrCtorParameterProvenance::InterfaceDelegate { field, .. } =
                &mut ir.classes[class as usize].ctor_args[parameter as usize].provenance
            else {
                panic!("interface operand")
            };
            *field = u32::MAX;
        }),
        ("unrelated applied interface", |ir, class, parameter, _| {
            let IrCtorParameterProvenance::InterfaceDelegate { interface, .. } =
                &mut ir.classes[class as usize].ctor_args[parameter as usize].provenance
            else {
                panic!("interface operand")
            };
            *interface = Ty::String;
        }),
        (
            "same classifier with different applied arguments",
            |ir, class, parameter, _| {
                let IrCtorParameterProvenance::InterfaceDelegate { interface, .. } =
                    &mut ir.classes[class as usize].ctor_args[parameter as usize].provenance
                else {
                    panic!("interface operand")
                };
                let name = interface
                    .obj_internal()
                    .expect("same declared interface classifier");
                *interface = Ty::obj_args_name(name, &[Ty::String]);
                assert!(
                    ir.classes[class as usize].interfaces.contains_name(name),
                    "raw classifier membership alone would admit this malformed application"
                );
            },
        ),
        ("duplicate field owner", |ir, class, parameter, _| {
            let argument = ir.classes[class as usize].ctor_args[parameter as usize].clone();
            ir.classes[class as usize].ctor_args.push(argument);
        }),
        ("mutable storage", |ir, class, _, field| {
            ir.classes[class as usize].fields[field as usize].flags = ir.classes[class as usize]
                .fields[field as usize]
                .flags
                .with_is_final(false);
        }),
        ("wrong stored type", |ir, class, _, field| {
            ir.classes[class as usize].fields[field as usize].ty = Ty::Int;
        }),
        (
            "forwarded-super role mixed in",
            |ir, class, parameter, _| {
                ir.classes[class as usize].ctor_args[parameter as usize].anonymous_super_forward =
                    Some(1);
            },
        ),
        ("capture identity mixed in", |ir, class, parameter, _| {
            let identity = ClassCaptureIdentity::Receiver(0);
            ir.classes[class as usize].ctor_args[parameter as usize].capture_identity =
                Some(identity);
            ir.class_capture_identities
                .insert((class, parameter), identity);
        }),
        (
            "constructor-property storage mixed in",
            |ir, class, parameter, field| {
                let argument = &mut ir.classes[class as usize].ctor_args[parameter as usize];
                argument.is_field = true;
                argument.field_index = Some(field);
            },
        ),
        ("second pre-super store", |ir, class, parameter, field| {
            ir.classes[class as usize]
                .pre_super_param_fields
                .push((parameter, field));
        }),
        ("shared holder", |ir, class, _, field| {
            ir.shared_class_capture_fields
                .insert((class, field), Ty::String);
        }),
    ];
    let (original, class, parameter, field) = interface_delegate_transport();
    let expected = Err(format!(
        "a class capturing its surroundings (`{}`)",
        original.classes[class as usize].fq_name()
    ));
    for (label, mutate) in mutations {
        let mut ir = copy_transport(&original);
        mutate(&mut ir, class, parameter, field);
        assert_eq!(check_supported(&ir, class), expected, "{label}");
    }
    assert_eq!(check_supported(&original, class), Ok(()));
}
