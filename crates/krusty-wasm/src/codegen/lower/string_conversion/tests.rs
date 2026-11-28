use crate::WasmTarget;
use krusty::ir::{Callee, IrConst, IrExpr, IrFile, IrFunction, IrIntrinsic, IrTypeOp};
use krusty::types::Ty;

fn lower(mut ir: IrFile, value: u32, target: WasmTarget) -> Result<(), String> {
    ir.add_fun(IrFunction {
        name: "decimal".into(),
        params: Vec::new(),
        ret: Ty::String,
        body: Some(value),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let properties = krusty::backend::local_properties::realize(&mut ir).unwrap();
    let mut started = super::super::super::Started::new(target);
    super::super::lower_alone(&ir, &properties, &mut started).map(|_| ())
}

fn receiver(ir: &mut IrFile, ty: Ty) -> u32 {
    let constant = match ty {
        Ty::ULong => IrConst::ULong(u64::MAX),
        _ => IrConst::UInt(u32::MAX),
    };
    let value = ir.add_expr(IrExpr::Const(constant));
    ir.set_logical_type(value, ty);
    value
}

fn conversion(ir: &mut IrFile, source: Ty, receiver: Option<u32>, args: Vec<u32>) -> u32 {
    let value = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Intrinsic {
            operation: IrIntrinsic::UnsignedToString { source },
            ret: Ty::String,
        }),
        dispatch_receiver: receiver,
        args,
    });
    ir.set_logical_type(value, Ty::String);
    value
}

#[test]
fn all_four_unsigned_scalar_widths_lower_through_templates_and_the_selected_intrinsic() {
    for target in WasmTarget::ALL {
        for ty in [Ty::UByte, Ty::UShort, Ty::UInt, Ty::ULong] {
            for intrinsic in [false, true] {
                let mut ir = IrFile::default();
                let receiver = receiver(&mut ir, ty);
                let value = if intrinsic {
                    conversion(&mut ir, ty, Some(receiver), Vec::new())
                } else {
                    ir.add_expr(IrExpr::StringConcat(vec![receiver]))
                };
                ir.set_logical_type(value, Ty::String);
                assert_eq!(lower(ir, value, target), Ok(()), "{ty:?}");
            }
        }
    }
}

#[test]
fn unsigned_decimal_intrinsics_require_the_exact_receiver_type_and_operand_frame() {
    for target in WasmTarget::ALL {
        for (source, actual) in [
            (Ty::UInt, Ty::Int),
            (Ty::UByte, Ty::UInt),
            (Ty::ULong, Ty::UInt),
            (Ty::Int, Ty::Int),
            (Ty::nullable(Ty::UInt), Ty::nullable(Ty::UInt)),
        ] {
            let mut ir = IrFile::default();
            let receiver = receiver(&mut ir, actual);
            let value = conversion(&mut ir, source, Some(receiver), Vec::new());
            assert_eq!(
                lower(ir, value, target),
                Err(
                    "an unsigned string conversion without its exact non-null scalar source".into()
                ),
            );
        }
        for (has_receiver, has_argument) in [(false, false), (false, true), (true, true)] {
            let mut ir = IrFile::default();
            let receiver = receiver(&mut ir, Ty::UInt);
            let args = if has_argument {
                vec![receiver]
            } else {
                Vec::new()
            };
            let value = conversion(&mut ir, Ty::UInt, has_receiver.then_some(receiver), args);
            assert_eq!(
                lower(ir, value, target),
                Err("an unsigned string conversion with malformed operands".into()),
            );
        }
    }
}

#[test]
fn ordinary_string_conversion_supports_nullable_and_erased_unsigned_boxes() {
    for target in WasmTarget::ALL {
        for ty in [Ty::UByte, Ty::UShort, Ty::UInt, Ty::ULong] {
            let mut ir = IrFile::default();
            let null = ir.add_expr(IrExpr::Const(IrConst::Null));
            ir.set_logical_type(null, Ty::nullable(ty));
            let value = ir.add_expr(IrExpr::StringConcat(vec![null]));
            ir.set_logical_type(value, Ty::String);
            assert_eq!(lower(ir, value, target), Ok(()),);

            let mut ir = IrFile::default();
            let scalar = receiver(&mut ir, ty);
            let erased = ir.add_expr(IrExpr::type_op(
                IrTypeOp::ImplicitCoercion,
                scalar,
                Ty::obj_name(krusty::types::wk::any()),
            ));
            ir.set_logical_type(erased, Ty::obj_name(krusty::types::wk::any()));
            let value = ir.add_expr(IrExpr::StringConcat(vec![erased]));
            ir.set_logical_type(value, Ty::String);
            assert_eq!(lower(ir, value, target), Ok(()),);
        }
    }
}

#[test]
fn unsigned_scalar_intrinsics_still_reject_nullable_and_erased_receivers() {
    for target in WasmTarget::ALL {
        for ty in [Ty::UByte, Ty::UShort, Ty::UInt, Ty::ULong] {
            for actual in [Ty::nullable(ty), Ty::obj_name(krusty::types::wk::any())] {
                let mut ir = IrFile::default();
                let receiver = receiver(&mut ir, actual);
                let value = conversion(&mut ir, ty, Some(receiver), Vec::new());
                assert_eq!(
                    lower(ir, value, target),
                    Err(
                        "an unsigned string conversion without its exact non-null scalar source"
                            .into()
                    )
                );
            }
        }
    }
}

fn nameless_layout(is_abstract: bool, is_sealed: bool) -> krusty::ir::IrClass {
    let mut class = krusty::ir::IrClass::synthetic(krusty::types::type_name("sample/Nameless"));
    class.is_abstract = is_abstract;
    class.is_sealed = is_sealed;
    class
}

/// A real erased or hierarchy-typed parameter, not a constant whose initializer narrows it.
fn rendering_parameter(ty: Ty) -> IrFile {
    let mut ir = IrFile::default();
    let parameter = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(parameter, ty);
    let text = ir.add_expr(IrExpr::StringConcat(vec![parameter]));
    ir.set_logical_type(text, Ty::String);
    ir.add_fun(IrFunction {
        name: "render".to_owned(),
        params: vec![ty],
        ret: Ty::String,
        body: Some(text),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir
}

fn lower_reach(
    mut ir: IrFile,
    started: &mut crate::codegen::Started,
) -> Result<crate::codegen::any_member_reach::FileReach, String> {
    let properties = krusty::backend::local_properties::realize(&mut ir).unwrap();
    super::super::lower_alone(&ir, &properties, started).map(|file| file.reach)
}

#[test]
fn only_a_nameless_layout_with_runtime_instances_blocks_a_reference_string_conversion() {
    for target in WasmTarget::ALL {
        for (is_abstract, is_sealed) in [(true, false), (false, true), (false, false)] {
            for receiver in [Ty::obj("kotlin/Any"), Ty::obj("sample/Nameless")] {
                let mut ir = rendering_parameter(receiver);
                ir.classes.push(nameless_layout(is_abstract, is_sealed));
                let mut started = crate::codegen::Started::new(target);
                let result = lower_reach(ir, &mut started).map(|_| ());
                if is_abstract || is_sealed {
                    assert_eq!(result, Ok(()), "{target:?}, {receiver:?}");
                } else {
                    let receiver_name = if receiver == Ty::obj("kotlin/Any") {
                        "kotlin.Any"
                    } else {
                        "sample.Nameless"
                    };
                    assert_eq!(
                        result,
                        Err(format!(
                            "a `toString` of `{receiver_name}` reaching a runtime object this \
                             target cannot print; first witness: a generated class \
                             `sample.Nameless` without a source name"
                        )),
                        "{target:?}",
                    );
                }
            }
        }
    }
}

#[test]
fn only_runtime_instances_contribute_a_bodiless_class_to_another_files_erased_call() {
    for target in WasmTarget::ALL {
        for (is_abstract, is_sealed) in [(true, false), (false, true), (false, false)] {
            for class_first in [false, true] {
                let mut declaration = IrFile::default();
                declaration
                    .classes
                    .push(nameless_layout(is_abstract, is_sealed));
                let call = rendering_parameter(Ty::obj("kotlin/Any"));
                let files = if class_first {
                    [declaration, call]
                } else {
                    [call, declaration]
                };
                let mut started = crate::codegen::Started::new(target);
                let mut module_reach = crate::codegen::any_member_reach::ModuleReach::default();
                for (index, file) in files.into_iter().enumerate() {
                    let reached = module_reach.add(lower_reach(file, &mut started).unwrap());
                    let expected = if index == 1 && !is_abstract && !is_sealed {
                        Some(
                            "a `toString` of `kotlin.Any` reaching an object another file creates"
                                .to_owned(),
                        )
                    } else {
                        None
                    };
                    assert_eq!(reached, expected, "{target:?}, class first={class_first}");
                }
            }
        }
    }
}

/// Lower real class records with the same frozen declaration construction consumes.
fn lower_value_layout_reach(
    mut ir: IrFile,
    classifier: krusty::types::TypeName,
    underlying: Ty,
    started: &mut crate::codegen::Started,
) -> Result<crate::codegen::any_member_reach::FileReach, String> {
    let mut shape = krusty::libraries::LibraryType::declaration_header();
    shape.value_underlying = Some(underlying);
    shape.value_declaration = Some(krusty::types::DeclaredValueClass {
        property: "storage".into(),
        underlying,
        type_parameters: Box::new([]),
    });
    let facts = krusty::backend::BackendModuleFacts::from_classifiers([(classifier, shape)], [])
        .expect("complete value-class declaration");
    let classifiers = krusty::backend::CheckedBackendClassifiers::new(
        &facts,
        &krusty::libraries::EmptySymbolSource,
    );
    let properties = krusty::backend::local_properties::realize(&mut ir).unwrap();
    super::super::tests::lower_with_classifiers(&ir, &properties, started, &facts, &classifiers)
        .map(|file| file.reach)
}

#[test]
fn runtime_value_storage_layouts_do_not_contribute_impossible_instances_to_erased_calls() {
    for target in WasmTarget::ALL {
        for (name, underlying, runtime_owned) in [
            ("kotlin/UByteArray", Ty::obj("kotlin/ByteArray"), true),
            ("kotlin/UShortArray", Ty::obj("kotlin/ShortArray"), true),
            ("kotlin/UIntArray", Ty::obj("kotlin/IntArray"), true),
            ("kotlin/ULongArray", Ty::obj("kotlin/LongArray"), true),
            ("sample/ArrayHolder", Ty::obj("kotlin/ByteArray"), false),
        ] {
            let classifier = krusty::types::type_name(name);
            for layout_first in [false, true] {
                let mut layout = IrFile::default();
                let mut class = krusty::ir::IrClass::synthetic(classifier);
                class.is_value = true;
                layout.classes.push(class);
                let call = rendering_parameter(Ty::obj("kotlin/Any"));
                let files = if layout_first {
                    [layout, call]
                } else {
                    [call, layout]
                };
                let mut started = crate::codegen::Started::new(target);
                let mut reach = crate::codegen::any_member_reach::ModuleReach::default();
                for (index, file) in files.into_iter().enumerate() {
                    let actual = reach.add(
                        lower_value_layout_reach(file, classifier, underlying, &mut started)
                            .expect("the layout and erased call lower separately"),
                    );
                    let expected = (index == 1 && !runtime_owned).then(|| {
                        "a `toString` of `kotlin.Any` reaching an object another file creates"
                            .to_owned()
                    });
                    assert_eq!(
                        actual, expected,
                        "{target:?}, {name}, layout first={layout_first}"
                    );
                }
            }
            let mut ir = rendering_parameter(Ty::obj("kotlin/Any"));
            let mut class = krusty::ir::IrClass::synthetic(classifier);
            class.is_value = true;
            ir.classes.push(class);
            let mut started = crate::codegen::Started::new(target);
            let actual = lower_value_layout_reach(ir, classifier, underlying, &mut started);
            if runtime_owned {
                assert!(
                    actual.is_ok(),
                    "{target:?}, {name}: {:?}",
                    actual.as_ref().err()
                );
            } else {
                assert_eq!(
                    actual.err(),
                    Some("a `toString` of `kotlin.Any` reaching a runtime object this target cannot print; first witness: a generated class `sample.ArrayHolder` without a source name".to_owned()),
                    "{target:?}",
                );
            }
        }
    }
}
