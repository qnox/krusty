use super::*;
use crate::fir::{DeclarationId, EnclosingInstanceKind, EnclosingInstanceOwner};
use crate::ir::{IrCapturedReceiver, IrConstructorCapture, IrCtorArg, IrField, IrFunction};
use crate::types::Ty;

// These owning transformation fixtures supply closed ownership explicitly. The whole-source
// tests below exercise the actual compiler authority and declaration/source derivation.
fn retire_fixture(ir: &mut IrFile) {
    retire(
        ir,
        &crate::plugins::registry::NativePlugins::none(),
        &HashSet::from_iter([0]),
    )
}

fn captured_class() -> IrFile {
    let mut ir = IrFile::default();
    let mut class = crate::ir::test_support::blank_class("p/Local");
    class.is_source_declared = true;
    class.is_local_class = true;
    class.fields = vec![IrField::new("receiver".into(), Ty::obj("p/Outer")).with_is_final(true)];
    class.ctor_args.push(IrCtorArg {
        name: None,
        context_kind: crate::types::ContextParameterKind::None,
        ty: Ty::obj("p/Outer"),
        declared_ty: None,
        is_field: true,
        field_index: Some(0),
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: Some(IrConstructorCapture {
            source_name: "receiver".into(),
            receiver: Some(IrCapturedReceiver::Enclosing {
                classifier: "p/Outer".into(),
                owner: Some(EnclosingInstanceOwner {
                    declaration: DeclarationId::from_raw(2),
                    kind: EnclosingInstanceKind::Classifier,
                }),
            }),
            crossinline_parameter: None,
        }),
        provenance: IrCtorParameterProvenance::Capture,
        capture_identity: Some(ClassCaptureIdentity::EnclosingClass("p/Outer".into())),
    });
    class.constructor_prefix_count = 1;
    class.ctor_param_count = 1;
    class.pre_super_param_fields.push((0, 0));
    ir.classes.push(class);
    ir.classes
        .push(crate::ir::test_support::blank_class("p/Outer"));
    ir.checked_classifier_classes
        .insert(DeclarationId::from_raw(1), 0);
    ir.checked_classifier_classes
        .insert(DeclarationId::from_raw(2), 1);
    ir.class_capture_identities.insert(
        (0, 0),
        ClassCaptureIdentity::EnclosingClass("p/Outer".into()),
    );
    ir.class_capture_order.insert(0, vec![0].into_boxed_slice());
    ir
}

#[test]
fn an_unread_field_retires_without_changing_constructor_transport() {
    let mut ir = captured_class();
    let operand = ir.add_expr(IrExpr::GetValue(1));
    let effect = ir.add_expr(IrExpr::SetValue {
        var: 8,
        value: operand,
    });
    ir.classes[0].super_args.push(effect);
    let identities = ir.class_capture_identities.clone();
    let order = ir.class_capture_order.clone();
    retire_fixture(&mut ir);
    let class = &ir.classes[0];
    assert!(class.fields.is_empty());
    assert_eq!(class.ctor_param_count, 0);
    assert_eq!(class.constructor_prefix_count, 1);
    assert_eq!(class.ctor_args.len(), 1);
    assert_eq!(class.ctor_args[0].ty, Ty::obj("p/Outer"));
    assert!(!class.ctor_args[0].is_field);
    assert_eq!(class.ctor_args[0].field_index, None);
    assert_eq!(class.pre_super_param_fields, []);
    assert_eq!(class.super_args, [effect]);
    assert!(matches!(ir.expr(effect), IrExpr::SetValue { var: 8, value } if *value == operand));
    assert_eq!(ir.class_capture_identities, identities);
    assert_eq!(ir.class_capture_order, order);
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].constructor_prefix_count, 1);
}

#[test]
fn a_read_in_another_class_or_unattached_template_keeps_storage() {
    for kind in 0..3 {
        let mut ir = captured_class();
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        let read = ir.add_expr(match kind {
            0 => IrExpr::GetField {
                receiver,
                class: 0,
                index: 0,
            },
            1 => IrExpr::LateinitInitialized {
                receiver,
                class: 0,
                index: 0,
            },
            _ => IrExpr::SetField {
                receiver,
                class: 0,
                index: 0,
                value: receiver,
            },
        });
        // An unattached expression is intentional: templates/defaults are not emitted roots yet.
        if kind == 0 {
            ir.functions.push(IrFunction {
                name: "read".into(),
                params: Vec::new(),
                ret: Ty::obj("p/Outer"),
                body: Some(read),
                is_static: false,
                dispatch_receiver: Some("p/Outer".into()),
                param_checks: Vec::new(),
            });
            ir.classes[1].methods.push(0);
        }
        retire_fixture(&mut ir);
        assert_eq!(ir.classes[0].fields.len(), 1);
        assert_eq!(ir.classes[0].ctor_args[0].field_index, Some(0));
    }
}

#[test]
fn a_semantic_enclosing_edge_keeps_its_implicit_storage() {
    let mut ir = captured_class();
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.add_expr(IrExpr::EnclosingInstance {
        receiver,
        inner: "p/Local".into(),
        outer: "p/Outer".into(),
    });
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].fields.len(), 1);
}

#[test]
fn physical_compaction_preserves_other_fields_and_their_side_effects() {
    let mut ir = captured_class();
    ir.classes[0]
        .fields
        .push(IrField::new("declared".into(), Ty::Int));
    ir.classes[0]
        .properties
        .push(crate::ir::IrProperty::stored_value(
            "declared".into(),
            Ty::Int,
            1,
        ));
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let value = ir.add_expr(IrExpr::SetValue {
        var: 7,
        value: receiver,
    });
    let write = ir.add_expr(IrExpr::SetField {
        receiver,
        class: 0,
        index: 1,
        value,
    });
    let read = ir.add_expr(IrExpr::GetField {
        receiver,
        class: 0,
        index: 1,
    });
    ir.classes[0].init_body = Some(write);
    ir.shared_class_capture_fields.insert((0, 1), Ty::Int);
    ir.classes[0].properties[0].delegate_field = Some(1);
    let owner = ir.classes[0].fq_name;
    ir.member_ext_props.insert(
        owner,
        vec![crate::ir::MemberExtProp {
            name: "extension".into(),
            diagnostic_span: None,
            getter_diagnostic_span: None,
            setter_diagnostic_span: None,
            source_order: 0,
            receiver: Ty::String,
            ty: Ty::Int,
            is_var: false,
            is_abstract: false,
            modifiers: Default::default(),
            delegate_field: Some(1),
            getter: 0,
            setter: None,
            visibility: crate::types::Visibility::Private,
            type_params: Vec::new(),
            accessor_annotations: Default::default(),
        }],
    );
    let property = crate::fir::PropertyId::from_raw(0);
    ir.local_property_layouts.insert(
        property,
        IrLocalPropertyLayout::Member {
            class: 0,
            owner,
            backing_field: Some(1),
            getter: None,
            setter: None,
            interface: false,
            name: "declared".into(),
            ty: Ty::Int,
            mutable: false,
            private: true,
            context_parameters: Vec::new(),
            property: 0,
        },
    );
    ir.checked_interface_delegation_fields
        .insert((DeclarationId::from_raw(1), 0), 1);
    let mut declared = ir.classes[0].ctor_args[0].clone();
    declared.provenance = IrCtorParameterProvenance::Value;
    declared.ty = Ty::Int;
    declared.capture = None;
    declared.capture_identity = None;
    declared.field_index = Some(1);
    ir.classes[0].ctor_args.push(declared);
    ir.classes[0].pre_super_param_fields.push((1, 1));
    ir.classes[0].ctor_param_count = 2;
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].fields.len(), 1);
    assert_eq!(ir.classes[0].properties[0].backing_field, Some(0));
    assert_eq!(ir.shared_class_capture_fields.get(&(0, 0)), Some(&Ty::Int));
    assert_eq!(ir.classes[0].properties[0].delegate_field, Some(0));
    assert_eq!(ir.member_ext_props[&owner][0].delegate_field, Some(0));
    assert!(matches!(
        ir.local_property_layouts[&property],
        IrLocalPropertyLayout::Member {
            backing_field: Some(0),
            ..
        }
    ));
    assert_eq!(
        ir.checked_interface_delegation_fields[&(DeclarationId::from_raw(1), 0)],
        0
    );
    assert_eq!(ir.classes[0].ctor_args[1].field_index, Some(0));
    assert_eq!(ir.classes[0].pre_super_param_fields, [(1, 0)]);
    assert_eq!(ir.classes[0].ctor_param_count, 1);
    assert_eq!(ir.classes[0].init_body, Some(write));
    assert!(
        matches!(ir.expr(write), IrExpr::SetField { index: 0, value: actual, .. } if *actual == value)
    );
    assert!(matches!(ir.expr(read), IrExpr::GetField { index: 0, .. }));
    assert!(matches!(ir.expr(value), IrExpr::SetValue { var: 7, .. }));
}

#[test]
fn unknown_coordinates_keep_the_changed_owner_unchanged() {
    let mut ir = captured_class();
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.add_expr(IrExpr::GetField {
        receiver,
        class: 0,
        index: 9,
    });
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].fields.len(), 1);
    assert_eq!(ir.classes[0].ctor_args[0].field_index, Some(0));
    assert_eq!(ir.classes[0].pre_super_param_fields, [(0, 0)]);
}

#[test]
fn an_unrelated_orphan_coordinate_is_not_revalidated_or_changed() {
    let mut ir = captured_class();
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let orphan = ir.add_expr(IrExpr::GetField {
        receiver,
        class: 1,
        index: 9,
    });
    retire_fixture(&mut ir);
    assert!(ir.classes[0].fields.is_empty());
    assert_eq!(ir.classes[0].constructor_prefix_count, 1);
    assert!(matches!(
        ir.expr(orphan),
        IrExpr::GetField {
            class: 1,
            index: 9,
            ..
        }
    ));
}

#[test]
fn incomplete_owner_proof_does_not_prevent_an_independent_owner_retiring() {
    let mut ir = captured_class();
    let mut independent = ir.classes[0].clone();
    independent.fq_name = "p/Independent".into();
    ir.classes.push(independent);
    ir.checked_classifier_classes
        .insert(DeclarationId::from_raw(3), 2);
    ir.class_capture_identities.insert(
        (2, 0),
        ClassCaptureIdentity::EnclosingClass("p/Outer".into()),
    );
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.add_expr(IrExpr::GetField {
        receiver,
        class: 0,
        index: 9,
    });
    retire(
        &mut ir,
        &crate::plugins::registry::NativePlugins::none(),
        &HashSet::from_iter([0, 2]),
    );
    assert_eq!(ir.classes[0].fields.len(), 1);
    assert_eq!(ir.classes[0].ctor_args[0].field_index, Some(0));
    assert!(ir.classes[2].fields.is_empty());
    assert_eq!(ir.classes[2].constructor_prefix_count, 1);
    assert_eq!(
        ir.class_capture_identities[&(2, 0)],
        ClassCaptureIdentity::EnclosingClass("p/Outer".into())
    );
}

#[test]
fn unpartitionable_delegate_owner_and_opaque_signatures_keep_storage() {
    let mut ir = captured_class();
    ir.checked_interface_delegation_fields
        .insert((DeclarationId::from_raw(99), 0), 0);
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].fields.len(), 1);
    assert_eq!(
        ir.checked_interface_delegation_fields[&(DeclarationId::from_raw(99), 0)],
        0
    );

    let mut ir = captured_class();
    let signatures = vec![("declared".to_string(), "Lp/Outer;".to_string())];
    ir.insert_field_signatures("p/Local", signatures.clone());
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].fields.len(), 1);
    assert_eq!(ir.field_signatures("p/Local"), Some(&signatures));
}

#[test]
fn an_interface_delegate_keeps_its_operand_and_moves_only_its_storage_coordinate() {
    let mut ir = captured_class();
    let interface = Ty::obj("p/Interface");
    ir.classes[0].interfaces.push("p/Interface");
    ir.classes[0].supertypes.push(interface);
    ir.classes[0]
        .fields
        .push(IrField::new("delegate".into(), interface).with_is_final(true));
    let mut argument = ir.classes[0].ctor_args[0].clone();
    argument.ty = interface;
    argument.is_field = false;
    argument.field_index = None;
    argument.capture = None;
    argument.capture_identity = None;
    argument.provenance = IrCtorParameterProvenance::InterfaceDelegate {
        interface,
        field: 1,
    };
    ir.classes[0].ctor_args.push(argument);
    ir.classes[0].constructor_prefix_count = 2;
    ir.class_capture_order.remove(&0);
    ir.checked_interface_delegation_fields
        .insert((DeclarationId::from_raw(1), 0), 1);
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let operand = ir.add_expr(IrExpr::GetValue(2));
    let store = ir.add_expr(IrExpr::SetField {
        receiver,
        class: 0,
        index: 1,
        value: operand,
    });
    ir.classes[0].init_body = Some(store);
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].constructor_prefix_count, 2);
    assert_eq!(ir.classes[0].ctor_args.len(), 2);
    assert_eq!(
        ir.classes[0].ctor_args[1].provenance,
        IrCtorParameterProvenance::InterfaceDelegate {
            interface,
            field: 0
        }
    );
    assert_eq!(ir.classes[0].ctor_args[1].field_index, None);
    assert_eq!(
        ir.checked_interface_delegation_fields[&(DeclarationId::from_raw(1), 0)],
        0
    );
    assert!(
        matches!(ir.expr(store), IrExpr::SetField { index: 0, value, .. } if *value == operand)
    );
}

#[test]
fn legacy_implicit_field_mapping_keeps_the_entire_owner_layout() {
    let mut ir = captured_class();
    let mut implicit = ir.classes[0].ctor_args[0].clone();
    implicit.field_index = None;
    ir.classes[0].ctor_args.push(implicit);
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].fields.len(), 1);
    assert_eq!(ir.classes[0].ctor_args[0].field_index, Some(0));
    assert_eq!(ir.classes[0].ctor_args[1].field_index, None);
}

#[test]
fn receiver_owner_must_be_the_exact_declared_classifier() {
    for kind in 0..3 {
        let mut ir = captured_class();
        let capture = ir.classes[0].ctor_args[0].capture.as_mut().unwrap();
        let Some(IrCapturedReceiver::Enclosing { owner, .. }) = capture.receiver.as_mut() else {
            unreachable!()
        };
        match kind {
            0 => *owner = None,
            1 => owner.as_mut().unwrap().declaration = DeclarationId::from_raw(1),
            _ => owner.as_mut().unwrap().kind = EnclosingInstanceKind::EnumEntry,
        }
        retire_fixture(&mut ir);
        assert_eq!(ir.classes[0].fields.len(), 1);
    }
}

#[test]
fn deferred_plugin_and_opaque_consumers_keep_storage() {
    let mut ir = captured_class();
    let plugins =
        crate::plugins::registry::PluginRegistry::with_builtins().every_native_extension();
    assert!(!plugins.is_empty());
    retire(&mut ir, &plugins, &HashSet::from_iter([0]));
    assert_eq!(ir.classes[0].fields.len(), 1);
    ir.add_expr(IrExpr::PluginPlaceholder {
        plugin: "fixture",
        kind: "deferred",
        exprs: Vec::new(),
        data: Vec::new(),
        types: Vec::new(),
    });
    retire_fixture(&mut ir);
    assert_eq!(ir.classes[0].fields.len(), 1);
}

#[test]
fn missing_closed_source_authority_and_synthetic_roles_keep_storage() {
    let mut ir = captured_class();
    retire(
        &mut ir,
        &crate::plugins::registry::NativePlugins::none(),
        &HashSet::default(),
    );
    assert_eq!(ir.classes[0].fields.len(), 1);
    for role in 0..3 {
        let mut ir = captured_class();
        match role {
            0 => ir.classes[0].is_script = true,
            1 => ir.classes[0].is_compiler_generated = true,
            _ => ir.classes[0].is_source_declared = false,
        }
        retire_fixture(&mut ir);
        assert_eq!(ir.classes[0].fields.len(), 1);
    }
}

#[test]
fn specialized_copy_and_source_snapshots_keep_their_storage() {
    for (copy, source) in [(0, 2), (2, 0)] {
        let mut ir = captured_class();
        let mut copied = ir.classes[0].clone();
        copied.fq_name = "p/Copy".into();
        ir.classes.push(copied);
        let snapshot = crate::ir::IrSpecializedAnonymousClass {
            source,
            order: 0,
            caller_declaration: DeclarationId::from_raw(1),
            caller: None,
            caller_is_default: false,
            caller_source_name: "caller".into(),
            inline_callee: crate::fir::CallableId::from_raw(0),
            inline_callee_source_name: "callee".into(),
            crossinline_operands: Vec::new(),
            method_clones: Default::default(),
            bindings: Default::default(),
            reified_bindings: Default::default(),
            field_count: 1,
            property_count: 0,
            omitted_capture_fields: 0,
            caller_property_uses: Vec::new(),
            pending_property_roots: Vec::new(),
        };
        ir.specialized_anonymous_classes
            .insert(copy, snapshot.clone());
        retire_fixture(&mut ir);
        assert_eq!(ir.classes[0].fields.len(), 1);
        assert_eq!(ir.specialized_anonymous_classes[&copy], snapshot);
    }
}

#[derive(Default)]
struct RecordedSource(std::cell::RefCell<Vec<IrFile>>);

impl crate::backend::Backend for RecordedSource {
    type State = ();

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        crate::compilation_target::CompilationTarget::Jvm
    }

    fn lower_ir_file(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        _: &mut (),
        _: &mut crate::diag::DiagSink,
    ) -> Vec<crate::backend::Artifact> {
        if matches!(file.kind, crate::backend::CheckedIrUnitKind::Source) {
            self.0.borrow_mut().push(file.ir);
        }
        Vec::new()
    }

    fn finalize(&self, _: (), _: &str) -> Vec<crate::backend::Artifact> {
        Vec::new()
    }
}

fn completed_source(source: &str) -> IrFile {
    let mut paths = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        paths.push(jdk);
    }
    let libraries = crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(paths),
    ))
    .expect("JVM declaration provider");
    let inputs = [crate::source::SourceInput::kotlin(source).with_file_stem("CaptureStorage")];
    let stems = ["CaptureStorage".to_owned()];
    let mut diagnostics = crate::diag::DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(libraries)),
        &crate::features::LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );
    let backend = RecordedSource::default();
    crate::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let mut files = backend.0.into_inner();
    assert_eq!(files.len(), 1);
    files.remove(0)
}

#[test]
fn whole_constructor_lambda_has_no_field_and_keeps_the_exact_receiver_operand() {
    // This complete official program already has its shared runtime-corpus obligation. Here the
    // real compiler stream additionally checks the completed common-IR storage/transport contract.
    let ir = completed_source(include_str!(
        "../../fir/body_check/local_class_tests/fixtures/localSuperclassOuterCapture.kt"
    ));
    let locals = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_source_declared && c.is_local_class)
        .collect::<Vec<_>>();
    let [(id, class)] = locals.as_slice() else {
        panic!("one declared local class")
    };
    assert!(class.fields.is_empty());
    assert_eq!(class.ctor_param_count, 0);
    assert_eq!(class.constructor_prefix_count, 1);
    assert_eq!(class.ctor_args.len(), 1);
    let argument = &class.ctor_args[0];
    assert!(!argument.is_field);
    assert_eq!(argument.field_index, None);
    assert_eq!(argument.provenance, IrCtorParameterProvenance::Capture);
    assert_eq!(class.pre_super_param_fields, []);
    let Some(ClassCaptureIdentity::EnclosingClass(owner)) = argument.capture_identity else {
        panic!("declared enclosing receiver")
    };
    assert_eq!(argument.ty, Ty::obj_name(owner));
    assert_eq!(
        ir.class_capture_identities.get(&(*id as u32, 0)),
        Some(&ClassCaptureIdentity::EnclosingClass(owner))
    );
    let mut pending = class
        .super_arg_prelude
        .iter()
        .chain(&class.super_args)
        .copied()
        .collect::<Vec<_>>();
    let mut seen = HashSet::default();
    let mut captures = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda {
            impl_fn,
            captures: operands,
            ..
        } = ir.expr(expression)
        {
            captures.push((impl_fn, operands));
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    let [(function, operands)] = captures.as_slice() else {
        panic!("one superclass lambda")
    };
    assert_eq!(operands.len(), 1);
    assert!(matches!(ir.expr(operands[0]), IrExpr::GetValue(1)));
    let declaration = ir
        .checked_classifier_classes
        .iter()
        .find_map(|(&declaration, &class)| (class == *id as u32).then_some(declaration))
        .expect("selected original source classifier");
    let [identity] = ir.function_parameter_identities(**function).unwrap() else {
        panic!("one exact receiver capture parameter")
    };
    let crate::ir::IrParameterRole::CapturedValue { capture, .. } = identity.role else {
        panic!("constructor operand remains a captured value")
    };
    assert_eq!(
        capture.declaration,
        crate::ir::IrCapturedDeclaration::ConstructorReceiver {
            owner: declaration,
            parameter: 0,
        }
    );
}

#[test]
fn whole_later_member_and_inner_class_reads_keep_their_storage() {
    let member = completed_source(include_str!("fixtures/enclosingReceiverMemberStorage.kt"));
    let locals = member
        .classes
        .iter()
        .filter(|c| c.is_local_class)
        .collect::<Vec<_>>();
    assert_eq!(locals.len(), 1);
    assert_eq!(locals[0].fields.len(), 2);
    assert_eq!(locals[0].constructor_prefix_count, 2);
    assert_eq!(
        locals[0]
            .ctor_args
            .iter()
            .map(|a| (a.is_field, a.field_index))
            .collect::<Vec<_>>(),
        [(true, Some(0)), (true, Some(1))]
    );
    let nested = completed_source(include_str!("fixtures/enclosingReceiverNestedStorage.kt"));
    let locals = nested
        .classes
        .iter()
        .filter(|c| c.is_local_class && !c.is_inner_class)
        .collect::<Vec<_>>();
    assert_eq!(locals.len(), 1);
    assert_eq!(locals[0].fields.len(), 1);
    assert_eq!(locals[0].ctor_args[0].field_index, Some(0));
    let inners = nested
        .classes
        .iter()
        .filter(|c| c.is_inner_class)
        .collect::<Vec<_>>();
    assert_eq!(inners.len(), 1);
    assert_eq!(inners[0].fields.len(), 1);
    assert_eq!(inners[0].ctor_args[0].field_index, Some(0));
}
