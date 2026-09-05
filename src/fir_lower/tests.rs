use crate::fir::{
    BodyOwnerId, CallableId, DeclarationId, ExternalCallableId, FirAnnotationConstruction,
    FirBinaryOperation, FirBody, FirCall, FirCallArgument, FirCallTarget,
    FirCallableReferenceBinding, FirCallableReferenceTarget, FirCapture, FirCatch, FirConstant,
    FirConstructorCall, FirConstructorTarget, FirConversion, FirConversionKind, FirExpr,
    FirExprKind, FirIntrinsic, FirJumpKind, FirLocalCallableRef, FirPropertyReferenceTarget,
    FirPropertyTarget, FirRangeOperation, FirReceiver, FirStatement, FirStatementKind,
    FirTypeParameterRef, FirTypeSubstitution, FirUnaryOperation, FirVarargElement, OriginId,
    ResolvedModuleIndex, ResolvedTy,
};
use crate::ir::{
    Callee, IrBinOp, IrCheckedArgument, IrCheckedOperation, IrConst, IrExpr, IrFile, IrIntrinsic,
    IrNodeOrigin, IrTypeOp,
};
use crate::types::Ty;

use super::lower_body;

#[test]
fn consuming_lowering_materializes_common_ir_roots() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(7));
    let local_value = body.allocate_local_value();
    let one = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(1)),
    });
    let two = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(2)),
    });
    let sum = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Binary {
            operation: FirBinaryOperation::Add,
            lhs: one,
            rhs: two,
        },
    });
    let local = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target: local_value,
            ty: resolved(Ty::Int),
            mutable: false,
            lateinit: false,
            initializer: Some(sum),
            conversion: None,
        },
    });
    body.push_root(local);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();

    assert_eq!(lowered.owner, BodyOwnerId::from_raw(7));
    assert_eq!(lowered.roots.as_ref(), &[3]);
    assert!(matches!(ir.expr(0), IrExpr::Const(IrConst::Int(1))));
    assert!(matches!(ir.expr(1), IrExpr::Const(IrConst::Int(2))));
    assert!(matches!(
        ir.expr(2),
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: 0,
            rhs: 1
        }
    ));
    assert!(matches!(
        ir.expr(3),
        IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(2),
            named: true
        }
    ));
    assert_eq!(ir.fir_origins.len(), ir.exprs.len());
    assert_eq!(ir.fir_origins.get(&3), Some(&IrNodeOrigin::Fir(origin)));
}

#[test]
fn char_arithmetic_result_type_survives_nested_common_lowering() {
    let ir = lower_single_source(
        "fun compare(value: Char): Boolean = (value - 1) <= value\n",
        "CharNestedArithmetic",
    );
    let subtraction = ir
        .exprs
        .iter()
        .enumerate()
        .find_map(|(expression, node)| {
            matches!(
                node,
                IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Sub,
                    ..
                }
            )
            .then_some(expression as u32)
        })
        .expect("lowered Char subtraction");
    assert_eq!(ir.logical_types.get(&subtraction), Some(&Ty::Char));
}

#[test]
fn catch_parameter_name_survives_checked_fir_lowering() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(7));
    let parameter = body.allocate_local_value();
    body.set_debug_value_name(parameter, "failure");
    let one = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(1)),
    });
    let two = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(2)),
    });
    let attempt = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Try {
            body: one,
            catches: Box::new([FirCatch {
                origin,
                parameter,
                parameter_ty: resolved(Ty::obj("java/lang/Exception")),
                body: two,
            }]),
            finally: None,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(attempt),
    });
    body.push_root(root);
    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let catch = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Try { catches, .. } => catches.first(),
            _ => None,
        })
        .expect("checked try expression must retain its catch clause");
    assert_eq!(catch.name.as_deref(), Some("failure"));
}

#[test]
fn local_delegate_storage_name_survives_checked_fir_lowering() {
    let ir = lower_single_source(
        "class Delegate { operator fun getValue(owner: Any?, property: Any?): Int = 1 }\n\
         fun read(): Int { val value: Int by Delegate(); return value }\n",
        "LocalDelegateName",
    );
    assert!(
        ir.value_names.values().any(|name| name == "value$delegate"),
        "common IR must retain the checked delegate-storage debug identity: {:?}",
        ir.value_names
    );
}

#[test]
fn generic_member_delegate_result_keeps_its_erased_call_boundary() {
    let ir = lower_single_source(
        "class Delegate<T>(val value: T) {\n\
             operator fun getValue(owner: Any?, property: Any?): T = value\n\
         }\n\
         class Owner { val number: Int by Delegate(1) }\n",
        "GenericDelegateResult",
    );
    let getter = ir
        .functions
        .iter()
        .find(|function| function.name == "getNumber")
        .expect("generated delegated-property getter");
    let IrExpr::Block { stmts, value: None } = ir.expr(getter.body.expect("concrete getter body"))
    else {
        panic!("getter body must be a statement block");
    };
    let [returned] = stmts.as_slice() else {
        panic!("getter body must contain one return");
    };
    let IrExpr::Return(Some(converted)) = ir.expr(*returned) else {
        panic!("getter must return its delegated call");
    };
    let IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: call,
        type_operand: Ty::Int,
    } = ir.expr(*converted)
    else {
        panic!("generic getValue result must cross an explicit Int coercion");
    };
    assert!(matches!(ir.expr(*call), IrExpr::MethodCall { .. }));
    assert_eq!(
        ir.physical_types.get(call),
        Some(&Ty::obj("kotlin/Any")),
        "the declaration returns its erased T slot before the selected Int result"
    );
}

#[test]
fn lateinit_local_materializes_null_storage_before_checked_reads() {
    let ir = lower_single_source(
        "fun read(): String { lateinit var value: String; return value }\n",
        "LateinitLocalStorage",
    );
    let declaration = ir
        .value_names
        .iter()
        .find_map(|(&expression, name)| (name == "value").then_some(expression))
        .expect("lateinit local declaration");
    assert!(matches!(
        ir.expr(declaration),
        IrExpr::Variable {
            ty: Ty::String,
            init: Some(initializer),
            ..
        } if matches!(ir.expr(*initializer), IrExpr::Const(IrConst::Null))
    ));
    assert!(ir.exprs.iter().any(
        |expression| matches!(expression, IrExpr::LateinitCheck { name, .. } if name == "value")
    ));
}

#[test]
fn uninitialized_array_construction_is_a_direct_common_ir_value() {
    let ir = lower_single_source(
        "fun make(): IntArray = IntArray(5)\n",
        "DirectArrayConstruction",
    );
    assert_eq!(
        ir.exprs
            .iter()
            .filter(|expression| matches!(expression, IrExpr::NewArray { .. }))
            .count(),
        1
    );
    assert!(!ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::Variable {
                ty: Ty::Obj(name, _),
                init: Some(value),
                named: false,
                ..
            } if name.render() == "kotlin/IntArray"
                && matches!(ir.expr(*value), IrExpr::NewArray { .. })
        )
    }));
}

#[test]
fn immutable_local_array_loop_reads_the_source_value_slot() {
    let ir = lower_single_source(
        "fun sum(): Int { val values = IntArray(5); var sum = 0; for (value in values) sum += value; return sum }\n",
        "StableArrayLoop",
    );
    let declaration = ir
        .value_names
        .iter()
        .find_map(|(expression, name)| (name == "values").then_some(*expression))
        .expect("named source array declaration");
    let IrExpr::Variable {
        index: source_slot, ..
    } = ir.expr(declaration)
    else {
        panic!("source array must lower to a variable")
    };
    let size_receiver = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: IrIntrinsic::ArraySize,
                        ..
                    },
                dispatch_receiver: Some(receiver),
                ..
            } => Some(*receiver),
            _ => None,
        })
        .expect("array-size operation");
    assert!(matches!(ir.expr(size_receiver), IrExpr::GetValue(slot) if slot == source_slot));
    assert!(!ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::Variable {
                init: Some(value),
                named: false,
                ..
            } if matches!(ir.expr(*value), IrExpr::GetValue(slot) if slot == source_slot)
        )
    }));
}

#[test]
fn literal_until_bound_and_int_update_need_no_common_ir_temporaries() {
    let ir = lower_single_source(
        "fun sum(): Int { var sum = 0; for (value in 0 until 10) sum += value; return sum }\n",
        "LiteralUntilLoop",
    );
    let update = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::While {
                update: Some(update),
                ..
            } => Some(*update),
            _ => None,
        })
        .expect("counted-loop update");
    let IrExpr::SetValue { value, .. } = ir.expr(update) else {
        panic!("exclusive counted loop must have a direct update")
    };
    assert!(matches!(
        ir.expr(*value),
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            ..
        }
    ));
    assert!(!ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::Variable {
                init: Some(value),
                named: false,
                ..
            } if matches!(
                ir.expr(*value),
                IrExpr::TypeOp { arg, .. }
                    if matches!(ir.expr(*arg), IrExpr::Const(IrConst::Int(10)))
            )
        )
    }));
}

#[test]
fn nonnull_safe_call_selector_flows_directly_into_elvis_result() {
    let ir = lower_single_source(
        "fun length(value: String?): Int = value?.length ?: -1\n",
        "SafeCallElvis",
    );
    assert!(ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::Call {
                callee: Callee::Intrinsic {
                    operation: IrIntrinsic::StringLength,
                    ..
                },
                ..
            }
        )
    }));
    assert!(!ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::Variable {
                ty,
                named: false,
                ..
            } if *ty == Ty::nullable(Ty::Int)
        ) || matches!(
            expression,
            IrExpr::TypeOp {
                type_operand,
                ..
            } if *type_operand == Ty::nullable(Ty::Int)
        )
    }));
}

#[test]
fn sibling_member_properties_keep_checked_identities_until_backend_realization() {
    let ir = lower_source_from_set(
        &[
            (
                "class Label(val text: String)\n\
                 class Counter(var value: Int, val label: Label)\n\
                 class GenericBox<T>(val item: T)\n",
                "Declarations",
            ),
            (
                "fun update(counter: Counter): Int {\n\
                 \x20 val before = counter.value\n\
                 \x20 counter.value = before + 1\n\
                 \x20 return counter.value\n\
                 }\n\
                 fun label(counter: Counter): String = counter.label.text\n\
                 fun item(box: GenericBox<String>): String = box.item\n",
                "Uses",
            ),
        ],
        1,
    );

    let property = |target: &crate::fir::PropertyId| {
        ir.referenced_module_properties
            .get(target)
            .expect("referenced sibling property declaration")
    };
    let value_reads = ir
        .exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. })
                    if property(target).owner.is_some_and(|owner| owner.matches("Counter"))
                        && property(target).name == "value"
            )
        })
        .count();
    let value_writes = ir
        .exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::Checked(IrCheckedOperation::PropertyWrite { target, .. })
                    if property(target).owner.is_some_and(|owner| owner.matches("Counter"))
                        && property(target).name == "value"
            )
        })
        .count();
    assert_eq!(value_reads, 2);
    assert_eq!(value_writes, 1);

    for (owner, name) in [("Counter", "label"), ("Label", "text")] {
        assert!(ir.exprs.iter().any(|expression| {
            matches!(
                expression,
                IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. })
                    if property(target).owner.is_some_and(|candidate| candidate.matches(owner))
                        && property(target).name == name
            )
        }));
    }
    let generic_operation = ir
        .exprs
        .iter()
        .enumerate()
        .find_map(|(operation, expression)| match expression {
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                substitutions,
                ..
            }) if property(target)
                .owner
                .is_some_and(|owner| owner.matches("GenericBox"))
                && property(target).name == "item" =>
            {
                Some((operation as u32, target, substitutions))
            }
            _ => None,
        });
    let (generic_operation, generic_target, generic_substitutions) =
        generic_operation.expect("generic sibling property read");
    assert!(property(generic_target).ty.mentions_ty_param());
    assert!(generic_substitutions.is_empty());
    assert!(ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::TypeOp {
                arg,
                type_operand: Ty::String,
                ..
            } if *arg == generic_operation
        )
    }));
    assert!(!ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::PropertyRead { .. } | IrExpr::PropertyWrite { .. }
        )
    }));
    assert!(ir.property_accessor_jvm_realizations.is_empty());
}

#[test]
fn disabled_assertion_drops_its_unlowered_operand_graph() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(7));
    let condition = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Boolean),
        kind: FirExprKind::Constant(FirConstant::Boolean(false)),
    });
    let assertion = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Unit),
        kind: FirExprKind::Call(FirCall {
            target: FirCallTarget::Intrinsic {
                operation: FirIntrinsic::Assert {
                    mode: crate::types::AssertionMode::AlwaysDisabled,
                },
                receiver: None,
                parameters: Box::new([resolved(Ty::Boolean)]),
                result: resolved(Ty::Unit),
            },
            dispatch_receiver: None,
            extension_receiver: None,
            parameter_types: Box::new([resolved(Ty::Boolean)]),
            arguments: Box::new([FirCallArgument::Expression {
                parameter: 0,
                value: condition,
                conversion: None,
            }]),
            substitutions: Box::new([]),
        }),
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(assertion),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert_eq!(
        ir.exprs.len(),
        1,
        "the disabled condition must not be lowered"
    );
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: IrIntrinsic::Assert {
                    mode: crate::types::AssertionMode::AlwaysDisabled,
                },
                ret: Ty::Unit,
            },
            dispatch_receiver: None,
            args,
        } if args.is_empty()
    ));
}

#[test]
fn common_ir_receives_the_complete_applied_classifier_hierarchy() {
    let ir = lower_single_source(
        "interface Root<T>\n\
         interface Middle<U> : Root<U>\n\
         class Leaf : Middle<String>\n",
        "Hierarchy",
    );
    let leaf = crate::types::type_name("Leaf");
    let middle = crate::types::type_name("Middle");
    let root = crate::types::type_name("Root");
    let hierarchy = ir
        .classifier_hierarchies
        .get(&leaf)
        .expect("source class hierarchy must cross the FIR/common-IR boundary");

    assert_eq!(
        hierarchy
            .iter()
            .map(|entry| (entry.classifier, entry.applied, entry.depth))
            .collect::<Vec<_>>(),
        vec![
            (leaf, Ty::obj("Leaf"), 0),
            (middle, Ty::obj_args("Middle", &[Ty::String]), 1),
            (root, Ty::obj_args("Root", &[Ty::String]), 2),
        ]
    );
    ir.validate_determined_types()
        .expect("applied hierarchy types must be pending-free");
}

#[test]
fn annotation_class_retention_crosses_the_consuming_fir_boundary() {
    let ir = lower_single_source("annotation class Mark\n", "AnnotationPolicy");
    let mark = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Mark"))
        .expect("annotation class skeleton");

    assert!(mark.is_annotation);
    assert_eq!(
        mark.annotation_retention,
        Some(crate::types::AnnotationRetention::Default)
    );
}

#[test]
fn generic_cast_null_check_follows_the_checked_type_parameter_bound() {
    let ir = lower_single_source(
        "fun <E> nullableBound(value: Any?): E = value as E\n\
         fun <E : Any> nonNullBound(value: Any?): E = value as E\n",
        "GenericCasts",
    );
    let casts = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::TypeOp {
                op,
                type_operand: Ty::TyParam(_, bound),
                ..
            } => Some((*op, **bound)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        casts,
        vec![
            (IrTypeOp::Cast, Ty::nullable(Ty::obj("kotlin/Any"))),
            (IrTypeOp::CastNonNull, Ty::obj("kotlin/Any")),
        ]
    );
}

#[test]
fn nullable_type_test_expands_from_its_checked_target() {
    let ir = lower_single_source(
        "fun acceptsNull(value: Any?): Boolean = value is String?\n",
        "NullableTypeTest",
    );

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            type_operand: Ty::String | Ty::Obj(_, _),
            ..
        }
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            ..
        }
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Or,
            ..
        }
    )));
    assert!(!ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            type_operand,
            ..
        } if type_operand.is_nullable()
    )));
}

#[test]
fn generic_member_set_forms_share_the_checked_declaration_boundary() {
    let ir = lower_single_source(
        "class Generic<T> { operator fun set(index: Int, value: T) {} }\n\
         fun update(target: Generic<Int>) { target.set(0, 1); target[0] = 1 }\n",
        "GenericSet",
    );
    let generic_boundaries = ir
        .exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    type_operand: Ty::TyParam(_, _),
                    ..
                }
            )
        })
        .count();

    assert_eq!(
        generic_boundaries, 2,
        "direct and indexed set must both cross the selected generic declaration boundary"
    );
}

#[test]
fn checked_call_keeps_stable_target_and_final_argument_mapping() {
    let declaration = DeclarationId::from_raw(3);
    let callable = CallableId::from_raw(9);
    let array_ty = Ty::obj("kotlin/IntArray");
    let mut index = ResolvedModuleIndex::default();
    index
        .publish_signature(declaration, [Ty::Int, Ty::String, array_ty], Ty::Long)
        .unwrap();
    index.publish_function(callable, declaration, "selected", false);

    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(7)),
    });
    let vararg_value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(8)),
    });
    let call = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Long),
        kind: FirExprKind::Call(FirCall {
            target: callable.into(),
            dispatch_receiver: None,
            extension_receiver: None,
            parameter_types: Box::new([
                resolved(Ty::Int),
                resolved(Ty::String),
                resolved(array_ty),
            ]),
            arguments: Box::new([
                FirCallArgument::Expression {
                    parameter: 0,
                    value,
                    conversion: None,
                },
                FirCallArgument::Default {
                    parameter: 1,
                    origin,
                },
                FirCallArgument::Vararg {
                    parameter: 2,
                    origin,
                    elements: Box::new([FirVarargElement {
                        value: vararg_value,
                        spread: false,
                        conversion: None,
                    }]),
                },
            ]),
            substitutions: Box::new([]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(call),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &index, &mut ir).unwrap();
    let IrExpr::Checked(IrCheckedOperation::Call {
        target, arguments, ..
    }) = ir.expr(lowered.roots[0])
    else {
        panic!("checked source call must remain a stable semantic operation")
    };
    assert_eq!(*target, callable);
    assert!(matches!(
        arguments.as_slice(),
        [
            IrCheckedArgument::Expression {
                parameter: 0,
                value: 0
            },
            IrCheckedArgument::Default { parameter: 1 },
            IrCheckedArgument::Vararg {
                parameter: 2,
                array_type,
                elements
            }
        ] if *array_type == Ty::obj("kotlin/IntArray") && elements == &vec![(1, false)]
    ));
}

#[test]
fn external_call_keeps_checked_type_substitutions_until_provider_realization() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(23);
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let call = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::String),
        kind: FirExprKind::Call(FirCall {
            target: crate::fir::FirCallTarget::External {
                declaration,
                receiver: None,
                declared_receiver: None,
                parameters: Box::new([]),
                result: resolved(Ty::String),
                declared_result: None,
                suspend: false,
                can_inline: true,
                inline_plan: None,
                extension_receiver_parameter: None,
            },
            dispatch_receiver: None,
            extension_receiver: None,
            parameter_types: Box::new([]),
            arguments: Box::new([]),
            substitutions: Box::new([FirTypeSubstitution {
                parameter: FirTypeParameterRef::External {
                    callable: declaration,
                    ordinal: 0,
                },
                value: resolved(Ty::String),
                additional_bounds: Box::new([]),
            }]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(call),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::Call {
            callee: crate::ir::Callee::External { substitutions, .. },
            ..
        } if matches!(
            substitutions.as_slice(),
            [crate::ir::IrCheckedSubstitution {
                parameter: FirTypeParameterRef::External { callable, ordinal: 0 },
                value: Ty::String,
                additional_bounds,
            }] if *callable == declaration && additional_bounds.is_empty()
        )
    ));
}

#[test]
fn external_property_function_reference_is_materialized_without_lookup() {
    let origin = OriginId::from_raw(0);
    let property = crate::fir::ExternalPropertyId::from_raw(17);
    let target = FirPropertyReferenceTarget::External {
        name: "length".into(),
        reflection_owner: Some(resolved(Ty::String)),
        getter: Box::new(FirPropertyTarget::External {
            property,
            receiver: Some(resolved(Ty::String)),
            parameters: Box::new([]),
            result: resolved(Ty::Int),
            extension_receiver_parameter: None,
            dispatch: crate::fir::FirPropertyDispatch::Ordinary,
        }),
        setter: None,
        extension_receiver: false,
        property_type: resolved(Ty::Int),
    };
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let receiver = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::String),
        kind: FirExprKind::Constant(FirConstant::String("Kotlin".into())),
    });
    let reference = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Fun(crate::types::intern_fnsig(crate::types::FnSig {
            params: vec![],
            ret: Ty::Int,
            context_count: 0,
            has_receiver: false,
            suspend: false,
        }))),
        kind: FirExprKind::PropertyReference {
            target: target.clone(),
            function_type: resolved(Ty::Fun(crate::types::intern_fnsig(crate::types::FnSig {
                params: vec![],
                ret: Ty::Int,
                context_count: 0,
                has_receiver: false,
                suspend: false,
            }))),
            reflective: false,
            binding: crate::fir::FirCallableReferenceBinding::Bound,
            dispatch_receiver: Some(FirReceiver {
                value: receiver,
                conversion: None,
            }),
            extension_receiver: None,
            mutable: false,
            substitutions: Box::new([]),
            adaptation: None,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(reference),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let IrExpr::Lambda {
        captures, arity, ..
    } = ir.expr(lowered.roots[0])
    else {
        panic!("function-typed external property reference must become a lambda")
    };
    assert_eq!(*arity, 0);
    assert_eq!(captures.len(), 1);
    assert!(matches!(
        ir.expr(captures[0]),
        IrExpr::Const(IrConst::String(_))
    ));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead { target, .. })
            if *target == property
    )));
}

#[test]
fn external_member_extension_property_uses_published_receiver_coordinate() {
    let origin = OriginId::from_raw(0);
    let property = crate::fir::ExternalPropertyId::from_raw(23);
    let classifier = crate::types::type_name("dependency/C");
    let dispatch_ty = Ty::obj_name(classifier);
    let mut body = FirBody::new(BodyOwnerId::from_raw(5));
    let dispatch = body.add_expr(FirExpr {
        origin,
        ty: resolved(dispatch_ty),
        kind: FirExprKind::SingletonValue { classifier },
    });
    let extension = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(5)),
    });
    let read = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::String),
        kind: FirExprKind::PropertyRead {
            target: FirPropertyTarget::External {
                property,
                receiver: Some(resolved(dispatch_ty)),
                parameters: Box::new([resolved(Ty::Int)]),
                result: resolved(Ty::String),
                extension_receiver_parameter: Some(0),
                dispatch: crate::fir::FirPropertyDispatch::Ordinary,
            },
            dispatch_receiver: Some(FirReceiver {
                value: dispatch,
                conversion: None,
            }),
            extension_receiver: Some(FirReceiver {
                value: extension,
                conversion: None,
            }),
            context_arguments: Box::new([]),
            substitutions: Box::new([]),
        },
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(read),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();

    let accesses = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                parameters,
                receiver,
                arguments,
                ..
            }) if *target == property => Some((parameters, receiver, arguments)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(accesses.len(), 1);
    let (parameters, dispatch_receiver, arguments) = accesses[0];
    assert_eq!(parameters.as_slice(), &[Ty::Int]);
    assert!(dispatch_receiver.is_some());
    assert_eq!(arguments.len(), 1);
}

#[test]
fn external_super_property_dispatch_survives_common_lowering() {
    let origin = OriginId::from_raw(0);
    let property = crate::fir::ExternalPropertyId::from_raw(24);
    let owner = crate::types::type_name("dependency/Base");
    let receiver_ty = Ty::obj("dependency/Derived");
    let mut body = FirBody::new(BodyOwnerId::from_raw(6));
    let receiver = body.add_expr(FirExpr {
        origin,
        ty: resolved(receiver_ty),
        kind: FirExprKind::Constant(FirConstant::Null),
    });
    let read = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::PropertyRead {
            target: FirPropertyTarget::External {
                property,
                receiver: Some(resolved(receiver_ty)),
                parameters: Box::new([]),
                result: resolved(Ty::Int),
                extension_receiver_parameter: None,
                dispatch: crate::fir::FirPropertyDispatch::Super {
                    owner,
                    interface: false,
                },
            },
            dispatch_receiver: Some(FirReceiver {
                value: receiver,
                conversion: None,
            }),
            extension_receiver: None,
            context_arguments: Box::new([]),
            substitutions: Box::new([]),
        },
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(read),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
            target,
            dispatch: crate::ir::IrPropertyDispatch::Super {
                owner: selected_owner,
                interface: false,
            },
            receiver: Some(_),
            ..
        }) if *target == property && *selected_owner == owner
    )));
}

#[test]
fn external_unbound_reference_trusts_the_checked_receiver_widening() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(19);
    let function_type = Ty::Fun(crate::types::intern_fnsig(crate::types::FnSig {
        params: vec![Ty::String],
        ret: Ty::Boolean,
        context_count: 0,
        has_receiver: false,
        suspend: false,
    }));
    let mut body = FirBody::new(BodyOwnerId::from_raw(5));
    let reference = body.add_expr(FirExpr {
        origin,
        ty: resolved(function_type),
        kind: FirExprKind::CallableReference {
            target: FirCallableReferenceTarget::External {
                declaration,
                receiver: Some(resolved(Ty::nullable(Ty::String))),
                extension_receiver: true,
                parameters: Box::new([]),
                result: resolved(Ty::Boolean),
            },
            function_type: resolved(function_type),
            reflective: false,
            binding: FirCallableReferenceBinding::Unbound,
            dispatch_receiver: None,
            extension_receiver: None,
            substitutions: Box::new([]),
            adaptation: None,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(reference),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::Lambda {
            arity: 1,
            captures,
            ..
        } if captures.is_empty()
    ));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::External { target, .. },
            ..
        } if *target == declaration
    )));
}

#[test]
fn consuming_sink_realizes_positional_same_file_calls_without_checked_wrappers() {
    let ir = lower_single_source(
        "fun selected(value: Int): Int = value\n\
         fun caller(): Int = selected(7)\n",
        "Calls",
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(_),
            args,
            ..
        } if args.len() == 1
    )));
    assert!(!ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(IrCheckedOperation::Call { .. }))));
}

#[test]
fn consuming_sink_realizes_positional_same_file_member_calls() {
    let ir = lower_single_source(
        "class Counter { fun add(value: Int): Int = value }\n\
         fun caller(counter: Counter): Int = counter.add(7)\n",
        "MemberCalls",
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::MethodCall { args, .. } if args.len() == 1
    )));
    assert!(!ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(IrCheckedOperation::Call { .. }))));
}

#[test]
fn positional_member_call_keeps_its_receiver_direct() {
    let ir = lower_single_source(
        "class C {\n    val value = \"x\"\n    fun length(): Int = value.length\n}\n",
        "DirectReceiver",
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Checked(IrCheckedOperation::PropertyRead { .. })
    )));
    assert!(
        !ir.exprs
            .iter()
            .any(|expression| matches!(expression, IrExpr::Variable { named: false, .. })),
        "an already ordered receiver-only call must not manufacture a temporary"
    );
}

#[test]
fn suspending_call_operand_is_materialized_before_outer_construction() {
    let ir = lower_single_source(
        "class Pair(val first: Int, val second: Int)\n\
         suspend fun next(): Int = 2\n\
         suspend fun make(): Pair = Pair(1, next())\n",
        "SuspendingOperand",
    );
    let next = ir
        .functions
        .iter()
        .position(|function| function.name == "next")
        .expect("suspend callee") as u32;
    let call = ir
        .exprs
        .iter()
        .position(|expression| {
            matches!(
                expression,
                IrExpr::Call {
                    callee: crate::ir::Callee::Local(function),
                    ..
                } if *function == next
            )
        })
        .expect("checked suspend call") as u32;
    let slot = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Variable {
                index,
                init: Some(initializer),
                named: false,
                ..
            } if *initializer == call => Some(*index),
            _ => None,
        })
        .expect("suspending operand must be materialized before its outer construction");
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::New { internal, args, .. }
            if internal.render() == "Pair"
                && args.iter().any(|argument| matches!(ir.expr(*argument), IrExpr::GetValue(value) if *value == slot))
    )));
}

#[test]
fn same_file_named_call_spills_in_source_order_before_parameter_reordering() {
    let ir = lower_single_source(
        "fun selected(left: Int, right: Int): Int = left\n\
         fun caller(): Int = selected(right = 2, left = 1)\n",
        "NamedCalls",
    );
    let selected = ir
        .functions
        .iter()
        .position(|function| function.name == "selected")
        .unwrap() as u32;
    let (wrapper, call) = ir
        .exprs
        .iter()
        .enumerate()
        .find_map(|(wrapper, expression)| {
            let IrExpr::Block {
                stmts,
                value: Some(call),
            } = expression
            else {
                return None;
            };
            matches!(
                ir.expr(*call),
                IrExpr::Call {
                    callee: crate::ir::Callee::Local(target),
                    ..
                } if *target == selected
            )
            .then_some((wrapper as u32, (*call, stmts.clone())))
        })
        .expect("named call normalization block");
    let (_, statements) = call;
    assert_eq!(statements.len(), 2);
    let initializers = statements
        .iter()
        .map(|statement| match ir.expr(*statement) {
            IrExpr::Variable {
                init: Some(initializer),
                ..
            } => match ir.expr(*initializer) {
                IrExpr::Const(IrConst::Int(value)) => *value,
                other => panic!("unexpected named argument initializer: {other:?}"),
            },
            other => panic!("unexpected named argument spill: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(initializers, [2, 1]);
    let IrExpr::Block {
        value: Some(call), ..
    } = ir.expr(wrapper)
    else {
        unreachable!()
    };
    let IrExpr::Call { args, .. } = ir.expr(*call) else {
        unreachable!()
    };
    let slots = args
        .iter()
        .map(|argument| match ir.expr(*argument) {
            IrExpr::GetValue(slot) => *slot,
            other => panic!("unexpected normalized argument: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(slots, [1, 0]);
}

#[test]
fn same_file_defaults_and_varargs_are_consumed_before_backend_emission() {
    let defaults = lower_single_source(
        "fun selected(left: Int = 1, right: Int = 2): Int = left\n\
         fun caller(): Int = selected(right = 7)\n",
        "DefaultCalls",
    );
    assert!(defaults.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::LocalDefault(_),
            args,
            ..
        } if args.len() == 4
    )));
    assert!(!defaults
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(IrCheckedOperation::Call { .. }))));

    let varargs = lower_single_source(
        "fun selected(vararg values: Int): Int = values[0]\n\
         fun caller(): Int = selected(1, 2)\n",
        "VarargCalls",
    );
    assert!(varargs.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Vararg { elements, spreads, .. }
            if elements.len() == 2 && spreads == &vec![false, false]
    )));
    assert!(!varargs
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(IrCheckedOperation::Call { .. }))));
}

#[test]
fn same_file_generic_and_extension_calls_consume_semantic_substitutions_and_receiver_placement() {
    let generic = lower_single_source(
        "fun <T> identity(value: T): T = value\n\
         fun caller(): String = identity(\"ok\")\n",
        "GenericCalls",
    );
    assert!(generic.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(_),
            args,
            ..
        } if args.len() == 1
    )));
    assert!(!generic
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(IrCheckedOperation::Call { .. }))));

    let extension = lower_single_source(
        "fun Int.selected(value: Int): Int = this + value\n\
         fun caller(): Int = 1.selected(2)\n",
        "ExtensionCalls",
    );
    let extension_function = extension
        .functions
        .iter()
        .position(|function| function.name == "selected")
        .unwrap() as u32;
    assert!(extension
        .extension_receiver_fns
        .contains(&extension_function));
    assert!(extension.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(target),
            args,
            ..
        } if *target == extension_function && args.len() == 2
    )));
    assert!(!extension
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(IrCheckedOperation::Call { .. }))));
}

#[test]
fn abstract_member_extension_property_keeps_bodyless_accessors_and_override_dispatch() {
    let ir = lower_single_source(
        "interface Base {\n\
             val Int.a: String\n\
         }\n\
         class A : Base {\n\
             override val Int.a: String get() = \"K\"\n\
             fun read(): String = 1.a\n\
         }\n\
         fun box(): String = A().read()\n",
        "MemberExtensionOverride",
    );

    let base = crate::types::type_name("Base");
    let implementation = crate::types::type_name("A");
    let base_property = &ir.member_ext_props[&base][0];
    let implementation_property = &ir.member_ext_props[&implementation][0];
    assert!(base_property.is_abstract);
    assert!(ir.functions[base_property.getter as usize].body.is_none());
    assert_eq!(
        ir.functions[base_property.getter as usize].params,
        [Ty::Int]
    );
    assert!(!implementation_property.is_abstract);
    assert!(ir.functions[implementation_property.getter as usize]
        .body
        .is_some());
    let (target, dispatch_receiver, extension_receiver) = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                substitutions,
            }) if dispatch_receiver.is_some()
                && extension_receiver.is_some()
                && context_arguments.is_empty()
                && substitutions.is_empty() =>
            {
                Some((*target, *dispatch_receiver, *extension_receiver))
            }
            _ => None,
        })
        .expect(
            "member extension read must retain its checked property identity and both receivers",
        );
    assert!(dispatch_receiver.is_some());
    assert!(extension_receiver.is_some());
    let property = &ir.referenced_module_properties[&target];
    assert_eq!(property.owner, Some(implementation));
    assert_eq!(property.name, "a");
    assert_eq!(property.extension_receiver, Some(Ty::Int));
    assert_eq!(property.ty, Ty::String);
}

#[test]
fn extension_access_lowers_to_a_bound_function_wrapper_without_lookup() {
    let ir = lower_single_source(
        "class A\n\
         val action: Any.() -> String = { \"OK\" }\n\
         fun box(): String = A().(action)()\n",
        "ExtensionBinding",
    );
    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_extension_bind_"))
        .expect("bound extension wrapper") as u32;
    assert_eq!(ir.functions[wrapper as usize].params.len(), 2);
    assert_eq!(
        ir.functions[wrapper as usize].params[0],
        Ty::obj("kotlin/Any")
    );
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::String);
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 0,
            captures,
            ..
        } if *impl_fn == wrapper && captures.len() == 2
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::InvokeFunction { args, params, ret, .. }
            if args.len() == 1 && params == &vec![Ty::obj("kotlin/Any")] && *ret == Ty::String
    )));
}

#[test]
fn local_function_is_lifted_once_and_receives_its_checked_capture() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let captured = body.allocate_local_value();
    let callable = body.allocate_local_callable();
    let four = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(4)),
    });
    let declaration = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target: captured,
            ty: resolved(Ty::Int),
            mutable: false,
            lateinit: false,
            initializer: Some(four),
            conversion: None,
        },
    });
    body.push_root(declaration);

    let mut nested = FirBody::new_local(body.owner(), callable);
    nested.set_debug_name("capturing");
    nested.set_result_type(resolved(Ty::Int));
    nested.set_implicit_return();
    nested.add_capture(FirCapture {
        origin,
        enclosing_depth: 0,
        source: captured,
        ty: resolved(Ty::Int),
        shared_cell: false,
    });
    let read = nested.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::CapturedValueRead {
            enclosing_depth: 0,
            source: captured,
        },
    });
    let nested_root = nested.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(read),
    });
    nested.push_root(nested_root);
    let local_function = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::LocalFunction {
            declaration: crate::fir::BodyLocalCallableDeclarationId::new(body.owner(), 0),
            callable,
            suspend: false,
            body: Box::new(nested),
        },
    });
    body.push_root(local_function);

    let call = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::LocalCall {
            target: FirLocalCallableRef {
                body_depth: 0,
                callable,
                declaration: Some(crate::fir::BodyLocalCallableDeclarationId::new(
                    body.owner(),
                    0,
                )),
                external_capture_arguments: None,
            },
            extension_receiver: None,
            arguments: Box::new([]),
        },
    });
    let call_statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(call),
    });
    body.push_root(call_statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();

    assert_eq!(ir.functions.len(), 1);
    assert!(ir.functions[0].name.starts_with("capturing$fir_"));
    assert_eq!(ir.functions[0].params, vec![Ty::Int]);
    assert!(ir.functions[0].body.is_some());
    let IrExpr::Call {
        callee: crate::ir::Callee::Local(function),
        args,
        ..
    } = ir.expr(lowered.roots[2])
    else {
        panic!("local call must retain its lifted function and bound environment")
    };
    assert_eq!(*function, 0);
    assert!(
        matches!(args.as_slice(), [capture] if matches!(ir.expr(*capture), IrExpr::GetValue(0)))
    );
}

#[test]
fn consuming_sink_preserves_mutable_capture_as_shared_cell_operations() {
    let source = r#"
        fun outer(): Int {
            var value = 1
            fun bump(): Int {
                value = value + 1
                return value
            }
            return bump()
        }
    "#;
    let ir = lower_single_source(source, "MutableCapture");

    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::RefNew { elem: Ty::Int, .. })));
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::RefGet { elem: Ty::Int, .. })));
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::RefSet { elem: Ty::Int, .. })));
    assert_eq!(
        ir.shared_capture_parameters
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [Ty::Int]
    );
}

#[test]
fn consuming_sink_boxes_a_mutable_destructure_capture() {
    let ir = lower_single_source(
        r#"
            class Box {
                operator fun component1(): Int = 1
            }
            fun outer(): Int {
                var (value) = Box()
                val bump = { value = value + 1 }
                bump()
                return value
            }
        "#,
        "MutableDestructureCapture",
    );

    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::RefNew { elem: Ty::Int, .. })));
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::RefGet { elem: Ty::Int, .. })));
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::RefSet { elem: Ty::Int, .. })));
}

#[test]
fn checked_local_suspend_modifier_reaches_common_ir() {
    let ir = lower_single_source(
        r#"
            suspend fun outer(): String {
                suspend fun local(): String = "OK"
                return local()
            }
        "#,
        "LocalSuspend",
    );

    let local = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("local$fir_"))
        .expect("lifted local suspend function") as u32;
    assert!(
        ir.suspend_funs.contains(&local),
        "checked local suspend semantics must reach the target CPS pass"
    );
}

#[test]
fn local_default_call_uses_lifted_default_stub_without_checked_placeholder() {
    let ir = lower_single_source(
        r#"
            fun outer(): Int {
                val base = 1
                fun selected(left: Int = base, right: Int = 2): Int = left + right
                return selected(right = 4)
            }
        "#,
        "LocalDefaults",
    );

    let selected = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("selected$fir_"))
        .expect("lifted local function") as u32;
    let defaults = ir.param_defaults(selected).expect("local defaults");
    assert_eq!(defaults.len(), 3, "capture plus two declared parameters");
    assert!(defaults[0].is_none());
    assert!(defaults[1].is_some());
    assert!(defaults[2].is_some());
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::LocalDefault(function),
            args,
            ..
        } if *function == selected
            && args.len() == 5
            && matches!(ir.expr(args[3]), IrExpr::Const(IrConst::Int(2)))
    )));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn local_vararg_call_materializes_array_at_checked_parameter_slot() {
    let ir = lower_single_source(
        r#"
            fun outer(): Int {
                fun sum(vararg values: Int): Int = values[0] + values[1]
                return sum(1, 2)
            }
        "#,
        "LocalVararg",
    );

    let sum = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("sum$fir_"))
        .expect("lifted local vararg target") as u32;
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(function),
            args,
            ..
        } if *function == sum
            && matches!(args.as_slice(), [array]
                if matches!(ir.expr(*array), IrExpr::Vararg { elements, .. } if elements.len() == 2))
    )));
}

#[test]
fn local_extension_call_materializes_selected_receiver_slot() {
    let ir = lower_single_source(
        r#"
            fun outer(): String {
                fun String.tag(suffix: String): String = this + suffix
                return "O".tag("K")
            }
        "#,
        "LocalExtension",
    );

    let tag = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("tag$fir_"))
        .expect("lifted local extension target") as u32;
    assert_eq!(ir.functions[tag as usize].params, [Ty::String, Ty::String]);
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(function),
            args,
            ..
        } if *function == tag && args.len() == 2
    )));
}

#[test]
fn transitive_mutable_capture_reaches_the_innermost_local_function() {
    let ir = lower_single_source(
        r#"
            fun outer(): Int {
                var value = 1
                fun middle(): Int {
                    fun inner(): Int {
                        value++
                        return value
                    }
                    return inner()
                }
                return middle()
            }
        "#,
        "TransitiveCapture",
    );

    assert_eq!(ir.shared_capture_parameters.len(), 2);
    assert_eq!(
        ir.exprs
            .iter()
            .filter(|expression| matches!(expression, IrExpr::RefSet { elem: Ty::Int, .. }))
            .count(),
        1
    );
}

#[test]
fn unadapted_local_function_reference_keeps_structural_identity_and_capture() {
    let ir = lower_single_source(
        r#"
            fun apply(value: Int, operation: (Int) -> Int): Int = operation(value)
            fun caller(): Int {
                val base = 40
                fun add(value: Int): Int = base + value
                return apply(2, ::add)
            }
        "#,
        "LocalReference",
    );

    let reference = ir
        .classes
        .iter()
        .find_map(|class| class.func_ref.as_ref())
        .expect("local function-reference carrier");
    assert!(reference.bound);
    assert_eq!(reference.arity, 1);
    assert_eq!(reference.fn_name, "add");
    assert!(reference.local_target.is_some());
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn adapted_local_function_reference_becomes_default_call_wrapper() {
    let ir = lower_single_source(
        r#"
            fun outer(): (String) -> String {
                fun join(value: String, suffix: String = "K"): String = value + suffix
                return ::join
            }
        "#,
        "AdaptedLocalReference",
    );

    let join = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("join$fir_"))
        .expect("lifted local target") as u32;
    assert!(ir.functions.iter().any(|function| {
        function.name.starts_with("$fir_local_ref_")
            && function.params == [Ty::String]
            && function.ret == Ty::String
    }));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::LocalDefault(function),
            args,
            ..
        } if *function == join
            && args.len() == 4
            && matches!(ir.expr(args[2]), IrExpr::Const(IrConst::Int(2)))
    )));
    assert!(ir.classes.iter().any(|class| {
        class
            .func_ref
            .as_ref()
            .is_some_and(|reference| reference.local_target.is_some() && reference.arity == 1)
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn suspend_converted_local_reference_becomes_suspend_wrapper() {
    let ir = lower_single_source(
        r#"
            fun outer(): suspend (Int) -> Int {
                fun increment(value: Int): Int = value + 1
                return ::increment
            }
        "#,
        "SuspendConvertedLocalReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_local_ref_"))
        .expect("suspend conversion wrapper") as u32;
    assert_eq!(ir.functions[wrapper as usize].params, [Ty::Int]);
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::Int);
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(ir.classes.iter().any(|class| {
        class.func_ref.as_ref().is_some_and(|reference| {
            reference.local_target == Some(wrapper) && reference.arity == 1 && !reference.bound
        })
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn suspend_converted_function_value_becomes_checked_forwarding_wrapper() {
    let ir = lower_single_source(
        r#"
            fun convert(block: (Int) -> String): suspend (Int) -> String = block
        "#,
        "SuspendConvertedFunctionValue",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_suspend_delegate_"))
        .expect("suspend function-value conversion wrapper") as u32;
    assert!(matches!(
        ir.functions[wrapper as usize].params.as_slice(),
        [Ty::Fun(source), Ty::Int]
            if !source.suspend && source.params == [Ty::Int] && source.ret == Ty::String
    ));
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::String);
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 2,
            captures,
            sam: None,
            ..
        } if *impl_fn == wrapper && captures.len() == 1
    )));
}

#[test]
fn suspend_converted_source_reference_becomes_suspend_wrapper() {
    let ir = lower_single_source(
        r#"
            fun increment(value: Int): Int = value + 1
            fun outer(): suspend (Int) -> Int = ::increment
        "#,
        "SuspendConvertedSourceReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_callable_ref_"))
        .expect("suspend conversion wrapper") as u32;
    assert_eq!(ir.functions[wrapper as usize].params, [Ty::Int]);
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::Int);
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn direct_suspend_source_reference_becomes_suspend_wrapper() {
    let ir = lower_source_from_set(
        &[
            (
                "suspend inline fun Int.selected(value: Int): Int = this + value",
                "Dependency",
            ),
            (
                "fun reference(): suspend (Int) -> Int = 1::selected",
                "Consumer",
            ),
        ],
        1,
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_callable_ref_"))
        .expect("direct suspend reference wrapper") as u32;
    assert!(ir.suspend_funs.contains(&wrapper));
}

#[test]
fn function_invoke_reference_becomes_a_capturing_suspend_forwarder() {
    let ir = lower_single_source(
        r#"
            fun reference(block: suspend () -> Unit): suspend () -> Unit = block::invoke
        "#,
        "FunctionInvokeReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_invoke_ref_"))
        .expect("function invoke forwarding wrapper") as u32;
    assert_eq!(ir.functions[wrapper as usize].params.len(), 1);
    assert!(matches!(
        ir.functions[wrapper as usize].params[0],
        Ty::Fun(signature) if signature.suspend && signature.params.is_empty()
    ));
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::obj("kotlin/Unit"));
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures,
            ..
        } if *impl_fn == wrapper && captures.len() == 1
    )));
    assert!(ir.exprs.iter().enumerate().any(|(expression, node)| {
        matches!(node, IrExpr::InvokeFunction { args, params, ret, .. }
            if args.is_empty() && params.is_empty() && *ret == Ty::Unit)
            && ir.suspend_calls.get(&(expression as u32)) == Some(&Ty::Unit)
    }));
}

#[test]
fn vararg_adapted_local_reference_packs_wrapper_parameters() {
    let ir = lower_single_source(
        r#"
            fun outer(): (Int, Int) -> Int {
                fun sum(vararg values: Int): Int = values[0] + values[1]
                return ::sum
            }
        "#,
        "AdaptedLocalVarargReference",
    );

    let sum = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("sum$fir_"))
        .expect("lifted local vararg target") as u32;
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(function),
            args,
            ..
        } if *function == sum
            && matches!(args.as_slice(), [array]
                if matches!(ir.expr(*array), IrExpr::Vararg { elements, .. } if elements.len() == 2))
    )));
    assert!(ir.classes.iter().any(|class| {
        class
            .func_ref
            .as_ref()
            .is_some_and(|reference| reference.local_target.is_some() && reference.arity == 2)
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn unbound_local_extension_reference_keeps_receiver_as_structural_parameter() {
    let ir = lower_single_source(
        r#"
            fun outer(): (String, String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return String::tag
            }
        "#,
        "LocalExtensionReference",
    );

    assert!(ir.classes.iter().any(|class| {
        class.func_ref.as_ref().is_some_and(|reference| {
            !reference.bound && reference.arity == 2 && reference.local_target.is_some()
        })
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn bound_local_extension_reference_captures_selected_receiver() {
    let ir = lower_single_source(
        r#"
            fun outer(): (String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return "O"::tag
            }
        "#,
        "BoundLocalExtensionReference",
    );

    assert!(ir.classes.iter().any(|class| {
        class.func_ref.as_ref().is_some_and(|reference| {
            reference.bound && reference.arity == 1 && reference.local_target.is_some()
        })
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn adapted_local_extension_references_materialize_bound_and_unbound_receivers() {
    let ir = lower_single_source(
        r#"
            fun unbound(): (String) -> String {
                fun String.tag(suffix: String = "K"): String = this + suffix
                return String::tag
            }
            fun bound(): () -> String {
                fun String.tag(suffix: String = "K"): String = this + suffix
                return "O"::tag
            }
        "#,
        "AdaptedLocalExtensionReferences",
    );

    assert!(ir.classes.iter().any(|class| {
        class.func_ref.as_ref().is_some_and(|reference| {
            !reference.bound && reference.arity == 1 && reference.local_target.is_some()
        })
    }));
    assert!(ir.classes.iter().any(|class| {
        class.func_ref.as_ref().is_some_and(|reference| {
            reference.bound && reference.arity == 0 && reference.local_target.is_some()
        })
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn suspend_converted_local_extension_references_become_suspend_wrappers() {
    let ir = lower_single_source(
        r#"
            fun unbound(): suspend (String, String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return String::tag
            }
            fun bound(): suspend (String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return "O"::tag
            }
        "#,
        "SuspendLocalExtensionReferences",
    );

    let wrappers = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.name.starts_with("$fir_local_ref_"))
        .map(|(function, _)| function as u32)
        .collect::<Vec<_>>();
    assert_eq!(wrappers.len(), 2);
    assert!(wrappers
        .iter()
        .all(|wrapper| ir.suspend_funs.contains(wrapper)));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn source_callable_references_become_structural_values_or_adapters() {
    let ir = lower_single_source(
        r#"
            fun increment(value: Int): Int = value + 1
            fun withDefault(value: Int, amount: Int = 1): Int = value + amount
            fun sum(vararg values: Int): Int = values[0] + values[1]
            fun <T> identity(value: T): T = value
            fun String.append(value: String): String = this + value
            fun String.appendDefault(value: String = "K"): String = this + value
            class Counter { fun increment(value: Int): Int = value + 1 }
            fun references(counter: Counter) {
                val topLevel = ::increment
                val bound = counter::increment
                val unbound = Counter::increment
                val defaulted: (Int) -> Int = ::withDefault
                val packed: (Int, Int) -> Int = ::sum
                val generic: (String) -> String = ::identity
                val boundExtension = "a"::append
                val unboundExtension = String::append
                val boundDefaultExtension: () -> String = "O"::appendDefault
                val unboundDefaultExtension: (String) -> String = String::appendDefault
                topLevel(1)
                bound(1)
                unbound(counter, 1)
                defaulted(1)
                packed(1, 2)
                generic("value")
                boundExtension("b")
                unboundExtension("a", "b")
                boundDefaultExtension()
                unboundDefaultExtension("O")
            }
        "#,
        "SourceReferences",
    );

    let adapters = ir
        .exprs
        .iter()
        .filter(|expression| matches!(expression, IrExpr::Lambda { .. }))
        .count();
    let structural_references = ir
        .classes
        .iter()
        .filter(|class| class.func_ref.is_some())
        .count();
    assert_eq!(adapters + structural_references, 10);
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn adapted_reference_arguments_keep_checked_target_coercions() {
    let ir = lower_single_source(
        r#"
            fun collect(vararg values: Any) {}
            fun consume(value: Any, tail: Int = 0) {}
            fun references() {
                val packed: (Int, Int) -> Unit = ::collect
                val widened: (Int) -> Unit = ::consume
            }
        "#,
        "AdaptedReferenceCoercions",
    );

    let packed = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Vararg {
                array_type,
                elements,
                ..
            } if array_type.array_elem() == Some(Ty::obj("kotlin/Any")) => Some(elements),
            _ => None,
        })
        .expect("collected Any vararg");
    assert_eq!(packed.len(), 2);
    assert!(packed.iter().all(|element| matches!(
        ir.expr(*element),
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            type_operand,
            ..
        } if *type_operand == Ty::obj("kotlin/Any")
    )));
    assert_eq!(
        ir.exprs
            .iter()
            .filter(|expression| matches!(
                expression,
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    type_operand,
                    ..
                } if *type_operand == Ty::obj("kotlin/Any")
            ))
            .count(),
        3,
        "two collected values and one ordinary adapted parameter cross the checked Any boundary"
    );
}

#[test]
fn adapted_generic_extension_reference_consumes_published_substitution() {
    let ir = lower_single_source(
        r#"
            fun <T> T.generic(vararg values: String, count: Int = 0): Int = count
            fun reference(): Int.(Array<String>, Int) -> Unit = Int::generic
        "#,
        "GenericExtensionReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_callable_ref_"))
        .expect("adapted generic extension-reference wrapper") as u32;
    assert_eq!(
        ir.functions[wrapper as usize].params,
        [
            Ty::Int,
            Ty::obj_args("kotlin/Array", &[Ty::String]),
            Ty::Int,
        ]
    );
    assert!(ir.classes.iter().any(|class| {
        class
            .func_ref
            .as_ref()
            .is_some_and(|reference| reference.local_target == Some(wrapper))
    }));
    assert!(ir.exprs.iter().all(|expression| {
        !matches!(
            expression,
            IrExpr::Checked(IrCheckedOperation::CallableReference { .. })
        )
    }));
}

#[test]
fn size_only_primitive_array_constructor_reference_lowers_mechanically() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "private fun <T> upcast(value: T): T = value\n\
         fun use() { upcast<(Int) -> ByteArray>(::ByteArray)(10) }\n",
        "SizeOnlyPrimitiveArrayReference",
        platform,
    );

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::NewArray { array_type, .. } if *array_type == Ty::obj("kotlin/ByteArray")
    )));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn inline_array_initializer_is_spliced_before_common_ir_escapes() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "fun objectArray() { Array<String>(2) { i -> if (i == 1) return; i.toString() } }\n\
         fun primitiveArray() { IntArray(2) { i -> if (i == 1) return; i } }\n",
        "InlineArrayInitializer",
        platform,
    );

    assert_eq!(ir.inline_only_fns.len(), 2);
    assert!(ir.inline_only_fns.iter().all(|function| {
        ir.functions
            .get(*function as usize)
            .is_some_and(|function| function.body.is_none())
    }));
    assert!(ir.exprs.iter().all(|expression| !matches!(
        expression,
        IrExpr::InvokeFunction { func, .. }
            if matches!(ir.expr(*func), IrExpr::Lambda { inline_body: Some(_), .. })
    )));
}

#[test]
fn suspending_collection_map_is_structural_before_common_ir_escapes() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "suspend fun render(value: Int): Int = value + 1\n\
         suspend fun collect(values: List<Int>): List<Int> =\n\
             values.map { render(it) }\n",
        "SuspendCollectionMap",
        platform,
    );
    let collect = ir
        .functions
        .iter()
        .enumerate()
        .find_map(|(function, declaration)| {
            (declaration.name == "collect").then_some(function as u32)
        })
        .expect("collect common-IR function");
    let body = ir.functions[collect as usize]
        .body
        .expect("collect body must be streamed");
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut has_loop = false;
    let mut has_suspend_call = false;
    let mut has_lambda = false;
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        has_loop |= matches!(ir.expr(expression), IrExpr::While { .. });
        has_suspend_call |= ir.suspend_calls.contains_key(&expression);
        has_lambda |= matches!(ir.expr(expression), IrExpr::Lambda { .. });
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    assert!(
        has_loop,
        "selected map body must already be an ordinary IR loop"
    );
    assert!(
        has_suspend_call,
        "the lambda's checked suspend call must belong to collect before target lowering"
    );
    assert!(
        !has_lambda,
        "a suspending inline map body cannot escape as a non-suspend Function1"
    );
}

#[test]
fn super_call_preserves_checked_default_ordinals_and_source_identity() {
    let ir = lower_single_source(
        "open class Base {\n\
             open fun value(x: Int = 20, y: Int = 3): Int = x + y\n\
         }\n\
         class Derived : Base() {\n\
             fun read(): Int = super.value(y = 4)\n\
         }\n",
        "SuperCallDefaults",
    );

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Super {
                source: Some(_),
                realization: crate::libraries::MemberRealization::Dispatch,
                defaults,
                ..
            },
            args,
            ..
        } if defaults == &[0] && args.len() == 2
    )));
}

#[test]
fn interface_property_accessors_share_source_order_with_functions() {
    let ir = lower_single_source(
        "interface PropFirstI {\n\
             val answer: Int get() = 42\n\
             fun echo(value: String): String = value\n\
             val tail: Int get() = 7\n\
         }\n",
        "PropFirst",
    );
    let class = ir
        .classes
        .iter()
        .find(|class| class.fq_name_matches("PropFirstI"))
        .expect("checked interface class");
    let published = class
        .methods
        .iter()
        .map(|function| {
            (
                ir.functions[*function as usize].name.as_str(),
                ir.fn_source_order
                    .get(function)
                    .copied()
                    .expect("every declared member must retain source order"),
            )
        })
        .collect::<Vec<_>>();
    let order = |name| {
        published
            .iter()
            .find_map(|(candidate, order)| (*candidate == name).then_some(*order))
            .unwrap_or_else(|| panic!("missing {name}: {published:?}"))
    };
    assert!(order("getAnswer") < order("echo"), "{published:?}");
    assert!(order("echo") < order("getTail"), "{published:?}");
}

#[test]
fn sibling_inline_adapted_reference_uses_the_retained_inline_template() {
    let ir = lower_source_from_set(
        &[
            ("inline fun sibling(value: Int): Int = value", "Dependency"),
            ("fun reference(): (Int) -> Unit = ::sibling", "Caller"),
        ],
        1,
    );

    assert!(ir.functions.iter().any(|function| {
        function.name.starts_with("$fir_callable_ref_") && function.params == [Ty::Int]
    }));
    assert!(!ir.foreign_inline_templates.is_empty());
    assert!(!ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Module {
                name,
                default_call: false,
                ..
            },
            dispatch_receiver: None,
            ..
        } if name == "sibling"
    )));
}

#[test]
fn sibling_suspend_call_retains_its_checked_coroutine_behavior() {
    let ir = lower_source_from_set(
        &[
            ("suspend fun sibling(): Int = 1", "Dependency"),
            ("suspend fun caller(): Int = sibling()", "Caller"),
        ],
        1,
    );

    let (expression, _) = ir
        .exprs
        .iter()
        .enumerate()
        .find(|(_, expression)| {
            matches!(
                expression,
                IrExpr::Call {
                    callee: crate::ir::Callee::Module { name, .. },
                    ..
                } if name == "sibling"
            )
        })
        .expect("sibling suspend call");
    assert_eq!(ir.suspend_calls.get(&(expression as u32)), Some(&Ty::Int));
}

#[test]
fn suspend_inline_call_keeps_source_extent_and_semantic_inline_region() {
    let source = "// WITH_STDLIB\n\
                  suspend fun values(): List<Int> = listOf(1)\n\
                  suspend fun work(): List<Int> =\n\
                  \x20 values().filter { it > 0 }\n";
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB\n// WITH_REFLECT");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let ir = lower_single_source_with_platform(
        source,
        "SuspendInlineRegion",
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        )),
    );

    assert_eq!(
        ir.source_line_count,
        source.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1
    );
    assert_eq!(ir.suspend_calls.len(), 1);
    let suspension = *ir.suspend_calls.keys().next().unwrap();
    assert_eq!(ir.expr_source_lines.get(&suspension), Some(&4));
    assert!(!ir.inline_call_sites.is_empty());
    assert!(!ir.inline_regions.is_empty());
}

#[test]
fn block_trailing_expression_keeps_its_checked_statement_line() {
    let ir = lower_single_source(
        "fun cond(): Boolean = false\n\
         fun act() {}\n\
         fun loop() {\n\
             while (cond()) {\n\
                 act()\n\
             }\n\
         }\n",
        "BlockTrailingLine",
    );
    let call = ir
        .exprs
        .iter()
        .enumerate()
        .find_map(|(expression, node)| {
            let IrExpr::Call {
                callee: Callee::Local(function),
                ..
            } = node
            else {
                return None;
            };
            (ir.functions[*function as usize].name == "act").then_some(expression as u32)
        })
        .expect("act call");
    assert_eq!(ir.expr_lines.get(&call), Some(&5));
}

#[test]
fn companion_extension_references_lower_as_receiverless_static_values() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_REFLECT"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "class C\n\
         companion fun C.function(): String = \"OK\"\n\
         companion val C.property: String = \"OK\"\n\
         fun references() {\n\
             val function: () -> String = C::function\n\
             val property: () -> String = C::property\n\
             function()\n\
             property()\n\
         }\n",
        "CompanionExtensionReferences",
        platform,
    );

    let receiverless_references = ir
        .exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::Lambda {
                    arity: 0,
                    captures,
                    ..
                } if captures.is_empty()
            )
        })
        .count();
    assert!(receiverless_references >= 1);
    assert!(ir.exprs.iter().all(|expression| {
        !matches!(
            expression,
            IrExpr::Checked(
                IrCheckedOperation::CallableReference { .. }
                    | IrCheckedOperation::PropertyReference { .. }
            )
        )
    }));
}

#[test]
fn companion_associated_function_body_and_call_have_no_runtime_receiver_slot() {
    let ir = lower_single_source(
        "class C\n\
         companion fun C.echo(value: String): String = value\n\
         fun box(): String = C.echo(\"OK\")\n",
        "CompanionAssociatedCall",
    );

    let echo = ir
        .functions
        .iter()
        .position(|function| function.name == "echo")
        .expect("associated function") as u32;
    assert_eq!(ir.functions[echo as usize].params, [Ty::String]);
    assert!(!ir.extension_receiver_fns.contains(&echo));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(function),
            args,
            dispatch_receiver: None,
        } if *function == echo && args.len() == 1
    )));
}

#[test]
fn implicit_inner_constructor_reference_captures_the_checked_outer_receiver() {
    let ir = lower_single_source(
        r#"
            class Outer {
                inner class Inner(val value: Int)
                fun reference(): (Int) -> Inner = ::Inner
            }
        "#,
        "BoundInnerConstructorReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("inner-constructor adapter") as u32;
    assert_eq!(
        ir.functions[wrapper as usize].params,
        [Ty::obj("Outer"), Ty::Int]
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures,
            sam: None,
            inline_body: None,
        } if *impl_fn == wrapper && captures.len() == 1
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::New { internal, args, .. }
            if internal.matches("Outer$Inner") && args.len() == 2
    )));
}

#[test]
fn ordinary_inner_constructor_keeps_source_property_after_physical_outer_prefix() {
    let ir = lower_single_source(
        r#"
            class Outer(val prefix: String) {
                inner class Inner(val value: String) {
                    fun result(): String = prefix + value
                }
                fun make(): Inner = Inner("OK")
            }
        "#,
        "InnerConstructorProperty",
    );

    let inner = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Outer$Inner"))
        .expect("inner class");
    assert_eq!(inner.constructor_prefix_count, 1);
    assert_eq!(inner.ctor_args.len(), 2);
    assert_eq!(inner.ctor_args[1].name.as_deref(), Some("value"));
    assert!(inner.ctor_args[1].is_field);
}

#[test]
fn inner_property_initializer_offsets_source_parameters_after_enclosing_prefix() {
    let ir = lower_single_source(
        r#"
            class Outer(val prefix: String) {
                inner class Inner(val suffix: String) {
                    val value = prefix + suffix
                }
            }
        "#,
        "InnerInitializerParameter",
    );

    let inner = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Outer$Inner"))
        .expect("inner class");
    let body = inner.init_body.expect("property initializer");
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut reads_source_suffix = false;
    let mut reads_enclosing_prefix_as_source = false;
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::GetValue(2) => reads_source_suffix = true,
            IrExpr::GetValue(1) => reads_enclosing_prefix_as_source = true,
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    assert!(reads_source_suffix);
    assert!(!reads_enclosing_prefix_as_source);
}

#[test]
fn structural_nested_constructor_reference_retains_its_adapter_identity() {
    let ir = lower_single_source(
        r#"
            class A {
                class Nested(val value: String)
            }
            fun reference(): (String) -> A.Nested = A::Nested
        "#,
        "NestedConstructorReference",
    );

    let adapter = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("constructor-reference adapter") as u32;
    let reference = ir
        .classes
        .iter()
        .find_map(|class| class.func_ref.as_ref())
        .expect("structural constructor-reference class");
    assert_eq!(reference.local_target, Some(adapter));
    assert!(ir.private_methods.contains(&adapter));
}

#[test]
fn inner_super_constructor_uses_the_checked_constructor_prefix_receiver() {
    let ir = lower_single_source(
        r#"
            open class Outer {
                open inner class Base
            }
            fun make() {
                val derived = object : Outer() {
                    inner class Derived : Base()
                }
                derived.Derived()
            }
        "#,
        "InnerSuperOuter",
    );

    let derived = ir
        .classes
        .iter()
        .find(|class| class.superclass.matches("Outer$Base"))
        .expect("inner Derived class");
    assert_eq!(derived.super_ctor_params, [Ty::obj("Outer")]);
    assert_eq!(derived.super_args.len(), 1);
    assert!(matches!(
        ir.expr(derived.super_args[0]),
        IrExpr::GetValue(1)
    ));
}

#[test]
fn external_inner_constructor_prepends_the_checked_outer_receiver() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(17);
    let mut body = FirBody::new(BodyOwnerId::from_raw(3));
    let outer = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj("dependency/Outer")),
        kind: FirExprKind::Constant(FirConstant::Null),
    });
    let construction = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj("dependency/Outer$Inner")),
        kind: FirExprKind::ConstructorCall(FirConstructorCall {
            target: FirConstructorTarget::External {
                declaration,
                classifier: crate::types::type_name("dependency/Outer$Inner"),
                parameters: Box::new([]),
                annotation: None,
            },
            outer_parameter: Some(resolved(Ty::obj("dependency/Outer"))),
            outer_receiver: Some(FirReceiver {
                value: outer,
                conversion: None,
            }),
            parameter_types: Box::new([]),
            arguments: Box::new([]),
            substitutions: Box::new([]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(construction),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::New {
            internal,
            args,
            ctor_params: Some(parameters),
            ctor_desc: None,
            external_target: Some(target),
        } if internal.matches("dependency/Outer$Inner")
            && args.as_slice() == [0]
            && parameters.is_empty()
            && *target == declaration
    ));
}

#[test]
fn external_annotation_construction_reaches_common_ir_without_provider_lookup() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(19);
    let interface = crate::types::type_name("dependency/Holder$Marker");
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::String),
        kind: FirExprKind::Constant(FirConstant::String("OK".into())),
    });
    let construction = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj_name(interface)),
        kind: FirExprKind::ConstructorCall(FirConstructorCall {
            target: FirConstructorTarget::External {
                declaration,
                classifier: interface,
                parameters: Box::new([resolved(Ty::String)]),
                annotation: Some(Box::new(FirAnnotationConstruction {
                    members: Box::new([("name".into(), resolved(Ty::String))]),
                    defaults: Box::new([None]),
                })),
            },
            outer_parameter: None,
            outer_receiver: None,
            parameter_types: Box::new([resolved(Ty::String)]),
            arguments: Box::new([FirCallArgument::Expression {
                parameter: 0,
                value,
                conversion: None,
            }]),
            substitutions: Box::new([]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(construction),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let construction = lowered.roots[0];
    assert!(matches!(
        ir.expr(construction),
        IrExpr::New {
            internal,
            external_target: Some(target),
            ..
        } if *internal == interface && *target == declaration
    ));
    let annotation = ir
        .annotation_constructions
        .get(&construction)
        .expect("checked annotation plan");
    assert_eq!(annotation.interface, interface);
    assert_eq!(annotation.members, vec![("name".to_string(), Ty::String)]);
    assert_eq!(annotation.defaults, vec![None]);
}

#[test]
fn unbound_inner_constructor_reference_maps_outer_and_vararg_parameters() {
    let ir = lower_single_source(
        r#"
            class Outer {
                inner class Inner(val value: Int, vararg val rest: String)
            }
            fun reference(): (Outer, Int, String) -> Outer.Inner = Outer::Inner
        "#,
        "UnboundInnerConstructorReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("adapted inner-constructor adapter") as u32;
    assert_eq!(
        ir.functions[wrapper as usize].params,
        [Ty::obj("Outer"), Ty::Int, Ty::String]
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 3,
            captures,
            ..
        } if *impl_fn == wrapper && captures.is_empty()
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::New { internal, args, .. }
            if internal.matches("Outer$Inner")
                && args.len() == 3
                && matches!(ir.expr(args[2]), IrExpr::Vararg { elements, .. } if elements.len() == 1)
    )));
}

#[test]
fn reflective_unbound_inner_constructor_reference_keeps_structural_identity() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_REFLECT"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        r#"
            import kotlin.reflect.KFunction1
            class Outer { inner class Inner }
            fun reference(): KFunction1<Outer, Outer.Inner> = Outer::Inner
        "#,
        "ReflectiveInnerConstructorReference",
        platform,
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("inner-constructor adapter") as u32;
    assert_eq!(ir.functions[wrapper as usize].params, [Ty::obj("Outer")]);
    let reference = ir
        .classes
        .iter()
        .find_map(|class| class.func_ref.as_ref())
        .expect("reflective constructor reference class");
    assert_eq!(reference.local_target, Some(wrapper));
    assert!(!reference.bound);
    assert!(matches!(reference.dispatch, crate::ir::FrDispatch::Static));
    assert_eq!(
        reference.owner_class,
        Some(crate::types::type_name("Outer$Inner"))
    );
    assert_eq!(reference.fn_name, "<init>");
    assert_eq!(reference.param_tys, [Ty::obj("Outer")]);
    assert_eq!(reference.target_param_tys, [Ty::obj("Outer")]);
    assert_eq!(reference.reflection_target_ret_ty, Some(Ty::Unit));
}

#[test]
fn consuming_lowering_attaches_selected_sam_target_to_lambda() {
    let ir = lower_single_source(
        "fun interface Action { fun run(value: Int): String }\n\
         fun consume(action: Action): String = \"OK\"\n\
         fun make(): String = consume { value -> \"$value\" }\n",
        "SamConversion",
    );

    let conversions = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                sam: Some(target), ..
            } => Some(target),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(conversions.len(), 1);
    assert_eq!(conversions[0].method, "run");
    assert_eq!(conversions[0].parameters[0], Ty::Int);
    assert_eq!(conversions[0].result, Ty::String);
}

#[test]
fn fun_interface_constructor_reference_returns_a_checked_sam_delegate() {
    let ir = lower_single_source(
        "fun interface Action { fun run(value: Int): String }\n\
         fun reference(): ((Int) -> String) -> Action = ::Action\n",
        "SamConstructorReference",
    );

    let constructor = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_classifier_ref_"))
        .expect("SAM constructor-reference adapter") as u32;
    let delegates = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                arity: 1,
                captures,
                sam: Some(target),
                inline_body: None,
            } if captures.len() == 1 => Some((*impl_fn, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(delegates.len(), 1);
    assert!(delegates[0].1.classifier.matches("Action"));
    assert_eq!(delegates[0].1.method, "run");
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures,
            sam: None,
            ..
        } if *impl_fn == constructor && captures.is_empty()
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::InvokeFunction { params, ret, .. }
            if params == &[Ty::Int] && *ret == Ty::String
    )));
}

#[test]
fn consuming_lowering_materializes_source_object_value() {
    let ir = lower_single_source(
        "object Values { val answer: Int = 42 }\n\
         fun read(): Int = Values.answer\n",
        "ObjectValue",
    );

    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::SingletonValue { .. })));
}

#[test]
fn consuming_lowering_retains_constructor_initialized_storage_read_by_custom_getter() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "class A {\n\
             val value: String\n\
                 get() = field + \"K\"\n\
             constructor(initial: String) { value = initial }\n\
         }\n",
        "ConstructorInitializedCustomGetter",
        platform,
    );

    let class = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("A"))
        .expect("class A");
    assert!(class.fields.iter().any(|field| field.name == "value"));
    assert!(class.properties.iter().any(|property| {
        property.name == "value" && property.backing_field.is_some() && property.getter.is_some()
    }));
}

#[test]
fn consuming_lowering_materializes_object_const_as_owned_static() {
    let ir = lower_single_source(
        "class Class { object Obj { const val Const = \"const\" } }\n",
        "ObjectConst",
    );

    let object = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Class$Obj"))
        .expect("nested object class");
    let (static_id, constant) = ir
        .statics
        .iter()
        .enumerate()
        .find(|(_, property)| property.name == "Const")
        .expect("object constant static");
    assert!(constant.is_const);
    assert_eq!(constant.owner, Some(object.fq_name));
    assert!(object.fields.iter().all(|field| field.name != "Const"));
    assert!(object
        .properties
        .iter()
        .all(|property| property.name != "Const"));
    assert_eq!(
        ir.declared_class_statics.get(&object.fq_name),
        Some(&vec![static_id as u32])
    );
}

#[test]
fn consuming_lowering_keeps_object_const_access_semantic_until_target_realization() {
    let ir = lower_single_source(
        "object Left { const val marker = \"$\"; const val answer = \"1234$marker\" }\n\
         object Right { const val marker = \"$\"; const val answer = \"1234$marker\" }\n\
         fun read(): String {\n\
             if (Left.answer !== Right.answer) return \"bad\"\n\
             return \"OK\"\n\
         }\n",
        "ObjectConstRead",
    );

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Checked(IrCheckedOperation::PropertyRead { .. })
    )));
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::SingletonValue { .. })));
}

#[test]
fn consuming_lowering_preserves_class_literal_without_checked_placeholder() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(13));
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::String),
        kind: FirExprKind::Constant(FirConstant::String("value".to_string().into())),
    });
    let unbound = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj("kotlin/reflect/KClass")),
        kind: FirExprKind::ClassLiteral {
            classifier: Some(resolved(Ty::String)),
            value: None,
        },
    });
    let bound = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj("kotlin/reflect/KClass")),
        kind: FirExprKind::ClassLiteral {
            classifier: None,
            value: Some(value),
        },
    });
    for expression in [unbound, bound] {
        let statement = body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(expression),
        });
        body.push_root(statement);
    }
    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();

    let literals = ir
        .exprs
        .iter()
        .filter(|expression| matches!(expression, IrExpr::KClassLiteral { .. }))
        .count();
    assert_eq!(literals, 2);
}

#[test]
fn same_file_inline_call_specializes_reified_types_fixed_by_callable_argument() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "fun <T, R> generic(value: T): R = value as R\n\
         inline fun <reified T, reified R> inspect(\n\
             value: T, result: R, operation: (T) -> R\n\
         ) { T::class; R::class }\n\
         fun use() { inspect(\"value\", 1, ::generic) }\n",
        "ReifiedCallableArgument",
        platform,
    );

    let classifiers = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::KClassLiteral {
                classifier: Some(classifier),
                ..
            } => Some(*classifier),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(classifiers.contains(&Ty::String), "{classifiers:?}");
    assert!(classifiers.contains(&Ty::Int), "{classifiers:?}");
}

#[test]
fn same_file_inline_call_specializes_reified_type_inside_nested_inline_lambda() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "inline fun <reified T> countOfType(values: List<Any>): Int =\n\
             values.count { it is T }\n\
         fun use(values: List<Any>): Int = countOfType<String>(values)\n",
        "NestedReifiedLambda",
        platform,
    );

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            type_operand: Ty::String,
            ..
        }
    )));
}

#[test]
fn same_file_inline_call_specializes_sized_array_allocation_type() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "inline fun <reified T> pair(first: T, second: T): Array<T> =\n\
             Array<T>(2) { if (it == 0) first else second }\n\
         fun use(): Array<String> = pair<String>(\"p\", \"q\")\n",
        "ReifiedSizedArray",
        platform,
    );

    let body = ir
        .functions
        .iter()
        .find(|function| function.name == "use")
        .and_then(|function| function.body)
        .expect("ordinary caller body");
    let string_array = Ty::obj_args("kotlin/Array", &[Ty::String]);
    assert!(expression_subtree_contains(
        &ir,
        body,
        &|_, expression| matches!(
            expression,
            IrExpr::NewArray { array_type, .. } if *array_type == string_array
        )
    ));
    assert!(!expression_subtree_contains(
        &ir,
        body,
        &|_, expression| matches!(
            expression,
            IrExpr::NewArray { array_type, .. } if array_type.mentions_ty_param()
        )
    ));
}

#[test]
fn cross_source_inline_call_consumes_retained_fir_as_a_non_emitted_template() {
    let ir = lower_source_from_set(
        &[
            (
                "inline fun <reified T> checkedCast(value: Any): T = value as T",
                "Library",
            ),
            (
                "fun use(): String = checkedCast<String>(\"OK\")",
                "Consumer",
            ),
        ],
        1,
    );

    assert_eq!(ir.foreign_inline_templates.len(), 1);
    assert!(ir
        .foreign_inline_templates
        .iter()
        .all(|function| ir.inline_only_fns.contains(function)));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::TypeOp {
            op: IrTypeOp::CastNonNull,
            type_operand: Ty::String,
            ..
        }
    )));
    assert!(ir.exprs.iter().all(|expression| !matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Module { name, .. },
            ..
        } if name == "checkedCast"
    )));
}

#[test]
fn nested_generic_inline_expansion_specializes_its_result_storage() {
    let ir = lower_source_from_set(
        &[
            (
                "inline fun <T, R> T.mapValue(f: (T) -> R): R = f(this)\n\
                 inline fun <T, R> T.forwardMap(f: (T) -> R): R = mapValue(f)",
                "Library",
            ),
            ("fun use(): Int = 1.forwardMap { it + 1 }", "Consumer"),
        ],
        1,
    );
    let body = ir
        .functions
        .iter()
        .find(|function| function.name == "use")
        .and_then(|function| function.body)
        .expect("consumer body");

    assert!(!expression_subtree_contains(&ir, body, &|expression, _| {
        ir.inline_regions.contains(&expression) && ir.physical_types.contains_key(&expression)
    }));
    assert!(!expression_subtree_contains(&ir, body, &|_, expression| {
        let IrExpr::Variable {
            ty,
            init: Some(initial),
            named: false,
            ..
        } = expression
        else {
            return false;
        };
        ty.canonical_semantic() == Ty::Int
            && matches!(ir.expr(*initial), IrExpr::Const(IrConst::Null))
    }));
}

#[test]
fn unused_foreign_inline_body_is_not_lowered_into_the_active_source() {
    let ir = lower_source_from_set(
        &[
            (
                "inline fun <reified T> unused(value: Any): T = value as T",
                "Library",
            ),
            ("fun use(): String = \"OK\"", "Consumer"),
        ],
        1,
    );

    assert!(ir.foreign_inline_templates.is_empty());
    assert!(ir
        .functions
        .iter()
        .all(|function| function.name != "unused"));
}

#[test]
fn inline_function_and_accessor_carry_nested_local_classifier_bodies() {
    let sources = [
        (
            r#"inline fun first(): String = object {
                       fun read(): String {
                           abstract class Abstract
                           open class Open
                           data class Data(val value: Int)
                           Open()
                           Data(1)
                           return "O"
                       }
                   }.read()

                   inline val second: String get() = object {
                       fun read(): String {
                           fun local() {}
                           class Local
                           local()
                           Local()
                           return "K"
                       }
                   }.read()"#,
            "Library",
        ),
        ("fun use(): String = first() + second", "Consumer"),
    ];
    let library = lower_source_from_set(&sources, 0);
    let consumer = lower_source_from_set(&sources, 1);

    assert!(library.foreign_inline_templates.is_empty());
    assert!(library
        .classes
        .iter()
        .any(|class| class.fq_name.render().contains("Local")));
    assert!(!consumer.foreign_inline_templates.is_empty());
    for class in &consumer.classes {
        assert!(
            class.fields.len() >= class.ctor_args.iter().filter(|argument| argument.is_field).count(),
            "every selected constructor storage argument must have an explicit common-IR field: {class:?}",
        );
    }
}

#[test]
fn consuming_sink_attaches_member_body_through_stable_classifier_identity() {
    let ir = lower_single_source(
        "class Box { fun answer(): Int = 42 }\n\
         fun read(box: Box): Int = box.answer()\n",
        "MemberSink",
    );

    assert_eq!(ir.checked_classifier_classes.len(), 1);
    let class = &ir.classes[0];
    assert_eq!(class.methods.len(), 1);
    assert_eq!(ir.functions[class.methods[0] as usize].name, "answer");
    assert!(ir.functions[class.methods[0] as usize].body.is_some());
}

#[test]
fn consuming_sink_publishes_generic_declaration_shapes_without_syntax_lookup() {
    let ir = lower_single_source(
        "interface Left\n\
         interface Right\n\
         class Box<out T> where T : Left, T : Right\n\
         fun <T : Any> id(value: T): T = value\n",
        "Generics",
    );

    let class = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Box"))
        .expect("generic source class");
    assert_eq!(class.type_params, ["T"]);
    let class_signature = ir.class_signature("Box").expect("class generic signature");
    assert_eq!(class_signature.type_params.len(), 1);
    assert_eq!(
        class_signature.type_params[0].variance,
        crate::types::TypeVariance::Out
    );
    assert_eq!(class_signature.type_params[0].bounds.len(), 2);
    assert!(class_signature.type_params[0]
        .bounds
        .iter()
        .all(|(_, is_interface)| *is_interface));

    let (function, _) = ir
        .functions
        .iter()
        .enumerate()
        .find(|(_, function)| function.name == "id")
        .expect("generic source function");
    let function_signature = ir
        .signatures
        .get(&(function as u32))
        .expect("function generic signature");
    assert_eq!(function_signature.type_params.len(), 1);
    assert_eq!(function_signature.type_params[0].name, "T");
    assert_eq!(function_signature.type_params[0].bounds.len(), 1);
    assert_eq!(
        function_signature.params,
        [Ty::ty_param(
            &function_signature.type_params[0].semantic_name,
            Ty::obj("kotlin/Any")
        )]
    );
    assert_eq!(
        function_signature.ret,
        function_signature.params.first().copied()
    );
}

#[test]
fn generic_value_class_underlying_uses_its_declared_type_parameter_identity() {
    let ir = lower_single_source(
        "// LANGUAGE: +GenericInlineClassParameter\n\
         inline class ICInt<T : Int>(val value: T)\n\
         inline class ICIcInt<T : ICInt<Int>>(val value: T)\n",
        "GenericValueClassUnderlying",
    );

    for name in ["ICInt", "ICIcInt"] {
        let class = ir
            .classes
            .iter()
            .find(|class| class.fq_name.matches(name))
            .expect("generic value class");
        let signature = ir
            .class_signature(name)
            .expect("generic value-class signature");
        let underlying = class
            .fields
            .first()
            .expect("value-class underlying field")
            .ty;
        let crate::types::Ty::TyParam(semantic, _) = underlying else {
            panic!("generic value-class underlying is not its type parameter: {underlying:?}");
        };
        assert!(signature
            .type_params
            .iter()
            .any(|parameter| parameter.semantic_name == semantic));
    }
}

#[test]
fn consuming_sink_publishes_one_recursive_function_bound() {
    let ir = lower_single_source(
        "interface Bound<T>\nfun <T : Bound<T>> pick(value: T): T = value\n",
        "RecursiveBound",
    );

    let function = ir
        .package_functions
        .iter()
        .find(|function| function.name == "pick")
        .expect("package function declaration metadata");
    let parameter = function
        .type_params
        .first()
        .expect("recursive type parameter");
    assert_eq!(
        parameter.bounds,
        [Ty::obj_args(
            "Bound",
            &[Ty::ty_param(
                &parameter.semantic_name,
                Ty::obj_args(
                    "Bound",
                    &[Ty::ty_param(
                        &parameter.semantic_name,
                        Ty::nullable(Ty::obj("kotlin/Any")),
                    )],
                ),
            )],
        )],
    );
}

#[test]
fn enclosing_receiver_path_lowers_from_stable_classifier_edges() {
    let ir = lower_single_source(
        "class Outer {\n\
             val outer = \"O\"\n\
             inner class Inner1 {\n\
                 val inner = \"I\"\n\
                 inner class Inner2 {\n\
                     fun Outer.read(): String = this@Inner1.inner + this@Outer.outer\n\
                 }\n\
             }\n\
         }\n",
        "EnclosingReceiver",
    );
    let inner2 = crate::types::type_name("Outer$Inner1$Inner2");
    let inner1 = crate::types::type_name("Outer$Inner1");
    let outer = crate::types::type_name("Outer");

    assert!(ir.exprs.iter().any(|expression| {
        let IrExpr::EnclosingInstance {
            receiver,
            inner,
            outer: selected_outer,
        } = expression
        else {
            return false;
        };
        if *inner != inner1 || *selected_outer != outer {
            return false;
        }
        matches!(
            ir.expr(*receiver),
            IrExpr::EnclosingInstance {
                inner,
                outer,
                ..
            } if *inner == inner2 && *outer == inner1
        )
    }));
}

#[test]
fn consuming_sink_keeps_primary_and_secondary_constructor_semantics() {
    let ir = lower_single_source(
        "class Built(val number: Int, val text: String = \"default\") {\n\
             constructor(flag: Boolean = true) : this(text = \"chosen\", number = 1) { flag }\n\
         }\n",
        "Built",
    );

    assert_eq!(ir.checked_constructor_bodies.len(), 2);
    let mut constructors = ir.checked_constructor_bodies.values().collect::<Vec<_>>();
    constructors.sort_by_key(|constructor| constructor.ordinal);
    let primary = constructors[0];
    assert_eq!(primary.ordinal, 0);
    assert_eq!(primary.parameters.len(), 2);
    assert!(primary.defaults[0].is_none());
    assert!(primary.defaults[1].is_some());
    let class = &ir.classes[primary.class as usize];
    assert_eq!(class.ctor_args.len(), 2);
    assert!(class.ctor_args[0].is_field);
    assert!(class.ctor_args[1].is_field);
    assert!(!class.ctor_args[0].has_default);
    assert!(class.ctor_args[1].has_default);
    assert_eq!(class.fields[0].default, None);
    assert_eq!(
        class.fields[1].default,
        Some(IrConst::String("default".into()))
    );
    let secondary = constructors[1];
    assert_eq!(secondary.ordinal, 1);
    assert_eq!(secondary.parameters, [("flag".to_owned(), Ty::Boolean)]);
    assert!(secondary.defaults[0].is_some());
    assert!(matches!(
        ir.expr(secondary.delegation.expect("selected this-delegation")),
        IrExpr::Block {
            stmts,
            value: Some(value),
        } if stmts.len() == 2
            && matches!(ir.expr(*value), IrExpr::Block { stmts, value: None } if stmts.is_empty())
    ));
    assert_eq!(class.secondary_ctors.len(), 1);
    assert_eq!(class.secondary_ctors[0].delegate_args.len(), 2);
    assert!(matches!(
        class.secondary_ctors[0].delegate,
        crate::ir::CtorDelegateTarget::This {
            to_primary: true,
            ..
        }
    ));
    assert!(secondary.body.is_some());
}

#[test]
fn secondary_constructor_merges_checked_vararg_chunks_and_keeps_metadata_shape() {
    let ir = lower_single_source(
        "open class Parent(vararg values: String)\n\
         class Child : Parent {\n\
             constructor(vararg values: String) : super(\"O\", *values, \"K\") {}\n\
         }\n",
        "SecondaryVararg",
    );
    let child = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Child"))
        .expect("Child common class");
    let [constructor] = child.secondary_ctors.as_slice() else {
        panic!(
            "one Child secondary constructor: {:?}",
            child.secondary_ctors
        )
    };
    assert_eq!(constructor.vararg_index, Some(0));
    let [argument] = constructor.delegate_args.as_slice() else {
        panic!("one packed super-constructor argument")
    };
    assert!(matches!(
        ir.expr(*argument),
        IrExpr::Vararg {
            elements,
            spreads,
            ..
        } if elements.len() == 3 && spreads == &[false, true, false]
    ));
}

#[test]
fn inner_secondary_constructor_carries_its_checked_enclosing_prefix() {
    let ir = lower_single_source(
        "open class Base()\n\
         class Outer {\n\
             inner class Inner : Base {\n\
                 val stored: Any\n\
                 constructor(stored: Any) { this.stored = stored }\n\
             }\n\
         }\n",
        "InnerSecondaryPrefix",
    );

    let class = ir
        .classes
        .iter()
        .find(|class| class.fq_name == crate::types::type_name("Outer$Inner"))
        .expect("inner class common IR");
    assert_eq!(class.constructor_prefix_count, 1);
    assert_eq!(class.secondary_ctors.len(), 1);
    let constructor = &class.secondary_ctors[0];
    assert_eq!(constructor.prefix_params, [Ty::obj("Outer")]);
    assert_eq!(constructor.params, [Ty::obj("kotlin/Any")]);
    let body = constructor.body.expect("secondary constructor body");
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut stores_value_from_slot_two = false;
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::SetField { value, .. } = ir.expr(expression) {
            stores_value_from_slot_two |= matches!(ir.expr(*value), IrExpr::GetValue(2));
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    assert!(stores_value_from_slot_two);
}

#[test]
fn consuming_sink_keeps_property_initializer_and_accessor_bodies() {
    let ir = lower_single_source(
        "val answer: Int = 42\n\
         val computed: Int get() = answer\n\
         class Box { var value: Int = 1; get() = field; set(next) { field = next } }\n",
        "Properties",
    );

    assert_eq!(ir.checked_properties.len(), 3);
    let property = |name: &str| {
        ir.checked_properties
            .values()
            .find(|property| property.name == name)
            .unwrap_or_else(|| panic!("missing property {name}"))
    };
    assert!(property("answer").initializer.is_some());
    assert!(property("answer").getter.is_none());
    assert!(property("computed").initializer.is_none());
    assert!(property("computed").getter.is_some());
    assert!(property("value").initializer.is_some());
    assert!(property("value").getter.is_some());
    assert!(property("value").setter.is_some());
    assert!(property("value").class.is_some());
    assert!(matches!(
        ir.expr(property("value").getter.expect("getter body")),
        IrExpr::GetField { .. }
    ));
}

#[test]
fn consuming_sink_keeps_class_initializers_and_enum_entry_construction() {
    let class_ir = lower_single_source(
        "class Box { val value: Int = 1; init { value } }\n",
        "Initializers",
    );
    assert_eq!(class_ir.checked_class_initializers.len(), 1);
    let initializer = &class_ir.checked_class_initializers[0];
    assert!(matches!(
        class_ir.expr(initializer.body),
        IrExpr::Block { .. }
    ));

    let enum_ir = lower_single_source("enum class Choice { FIRST, SECOND }\n", "Choices");
    assert_eq!(enum_ir.checked_enum_entry_bodies.len(), 2);
    let mut names = enum_ir
        .checked_enum_entry_bodies
        .values()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["FIRST", "SECOND"]);
    assert!(enum_ir
        .checked_enum_entry_bodies
        .values()
        .all(|entry| { matches!(enum_ir.expr(entry.construction), IrExpr::New { .. }) }));
}

#[test]
fn consuming_sink_keeps_sealed_subclasses_from_the_stable_header() {
    let ir = lower_single_source(
        "sealed class Root\n\
         class Direct : Root()\n\
         class Container { class Nested : Root() }\n",
        "Sealed",
    );
    let root = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Root"))
        .expect("sealed root class");
    let mut subclasses = root.sealed_subclasses.iter_rendered().collect::<Vec<_>>();
    subclasses.sort_unstable();
    assert_eq!(subclasses, ["Container$Nested", "Direct"]);
}

#[test]
fn consuming_sink_materializes_source_iterator_protocol() {
    let ir = lower_single_source(
        "class WordsIterator {\n\
             operator fun hasNext(): Boolean = false\n\
             operator fun next(): String = \"word\"\n\
         }\n\
         class Words { operator fun iterator(): WordsIterator = WordsIterator() }\n\
         fun run(words: Words) { for (word in words) { word } }\n",
        "Iterator",
    );
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::While { .. })));
    assert!(!ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn consuming_sink_materializes_context_iterator_operands() {
    let ir = lower_single_source(
        "// LANGUAGE: +ContextReceivers\n\
         class Context\n\
         class Values\n\
         class ValuesIterator {\n\
             operator fun hasNext(): Boolean = false\n\
             operator fun next(): Int = 1\n\
         }\n\
         context(Context)\n\
         operator fun Values.iterator(): ValuesIterator = ValuesIterator()\n\
         context(Context)\n\
         fun run() { for (value in Values()) { value } }\n",
        "ContextIterator",
    );
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::While { .. })));
    assert!(!ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn consuming_sink_retains_member_function_context_arity() {
    let ir = lower_single_source(
        "// LANGUAGE: +ContextReceivers\n\
         class Foo {\n\
             context(Int)\n\
             fun four(dummy: Any?): Int = this@Int\n\
         }\n",
        "MemberContextFunction",
    );
    let function = ir
        .functions
        .iter()
        .enumerate()
        .find_map(|(id, function)| (function.name == "four").then_some(id as u32))
        .expect("member context function");

    assert_eq!(ir.fn_context_counts.get(&function), Some(&1));
}

#[test]
fn consuming_sink_lowers_member_extension_iterator_protocol() {
    let ir = lower_single_source(
        "class It { operator fun hasNext(): Boolean = false }\n\
         class C { operator fun iterator(): It = It() }\n\
         class X {\n\
             operator fun It.next(): Int = 5\n\
             fun run() { for (value in C()) { value } }\n\
         }\n",
        "MemberExtensionIterator",
    );
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::While { .. })));
    assert!(!ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn classifier_realization_keeps_companion_identity_without_name_inference() {
    let ir = lower_single_source(
        "class Box { companion object Named { val answer: Int = 42 } }\n",
        "Companion",
    );
    let companion = ir
        .classes
        .iter()
        .find(|class| class.is_companion)
        .expect("semantic companion class");
    let outer = ir
        .classes
        .iter()
        .find(|class| class.companion_class == Some(companion.fq_name))
        .expect("outer-to-companion declaration edge");
    assert_eq!(outer.fq_name.segment_ref(), "Box");
    assert_eq!(companion.fq_name.nested_segment_ref(), "Named");
    let answer = ir
        .checked_properties
        .values()
        .find(|property| property.name == "answer")
        .expect("companion property");
    assert_eq!(
        answer.class,
        Some(
            ir.classes
                .iter()
                .position(|class| class.is_companion)
                .unwrap() as u32
        )
    );
    assert!(answer
        .flags
        .has(crate::fir::DeclarationFlags::HAS_INITIALIZER));
}

#[test]
fn constructor_delegation_remains_distinct_from_object_construction() {
    let declaration = DeclarationId::from_raw(5);
    let constructor = CallableId::from_raw(6);
    let mut index = ResolvedModuleIndex::default();
    index
        .publish_signature(declaration, [Ty::Int], Ty::Unit)
        .unwrap();
    index.publish_constructor(constructor, declaration);

    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(5));
    let argument = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(9)),
    });
    let delegation = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::ConstructorDelegation(FirConstructorCall {
            target: FirConstructorTarget::Module(constructor),
            outer_parameter: None,
            outer_receiver: None,
            parameter_types: Box::new([resolved(Ty::Int)]),
            arguments: Box::new([FirCallArgument::Expression {
                parameter: 0,
                value: argument,
                conversion: None,
            }]),
            substitutions: Box::new([]),
        }),
    });
    body.push_root(delegation);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &index, &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::Checked(IrCheckedOperation::ConstructorDelegation {
            target: crate::ir::IrCheckedConstructorTarget::Module(target),
            arguments,
            ..
        }) if *target == constructor && matches!(arguments.as_slice(), [IrCheckedArgument::Expression { parameter: 0, value: 0 }])
    ));
}

#[test]
fn checked_increment_lowers_without_recovering_an_operator() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(2));
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(41)),
    });
    let increment = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Unary {
            operation: FirUnaryOperation::Increment,
            operand: value,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(increment),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            type_operand: Ty::Int,
            ..
        }
    ));
}

#[test]
fn block_bodied_local_function_keeps_its_explicit_return() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(8));
    let callable = body.allocate_local_callable();
    let mut nested = FirBody::new_local(body.owner(), callable);
    nested.set_debug_name("explicit");
    nested.set_result_type(resolved(Ty::Int));
    let value = nested.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(3)),
    });
    let returned = nested.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Nothing),
        kind: FirExprKind::Jump {
            kind: FirJumpKind::Return { target_depth: 0 },
            target: crate::fir::ControlTargetId::from_raw(0),
            value: Some(value),
        },
    });
    let nested_root = nested.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(returned),
    });
    nested.push_root(nested_root);
    let declaration = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::LocalFunction {
            declaration: crate::fir::BodyLocalCallableDeclarationId::new(body.owner(), 0),
            callable,
            suspend: false,
            body: Box::new(nested),
        },
    });
    body.push_root(declaration);

    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let IrExpr::Block { stmts, value } = ir.expr(ir.functions[0].body.unwrap()) else {
        panic!("lifted body must be a block")
    };
    assert!(value.is_none());
    assert!(
        matches!(stmts.as_slice(), [root] if matches!(ir.expr(*root), IrExpr::Return(Some(_))))
    );
}

fn expression_subtree_contains(
    ir: &IrFile,
    root: crate::ir::ExprId,
    predicate: &impl Fn(crate::ir::ExprId, &IrExpr) -> bool,
) -> bool {
    fn visit(
        ir: &IrFile,
        expression: crate::ir::ExprId,
        predicate: &impl Fn(crate::ir::ExprId, &IrExpr) -> bool,
        seen: &mut std::collections::HashSet<crate::ir::ExprId>,
    ) -> bool {
        if !seen.insert(expression) {
            return false;
        }
        if predicate(expression, ir.expr(expression)) {
            return true;
        }
        let mut found = false;
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
            if !found && visit(ir, child, predicate, seen) {
                found = true;
            }
        });
        found
    }

    visit(ir, root, predicate, &mut std::collections::HashSet::new())
}

#[test]
fn lambda_callable_and_inline_template_own_distinct_return_control_flow() {
    let ir = lower_single_source(
        "fun make(): () -> Int = fun(): Int { return 1 }\n",
        "DistinctLambdaReturnBodies",
    );
    let (implementation, inline) = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                inline_body: Some(inline),
                ..
            } => Some((*impl_fn, *inline)),
            _ => None,
        })
        .expect("anonymous function value");
    let callable = ir.functions[implementation as usize]
        .body
        .expect("materialized callable body");

    assert!(expression_subtree_contains(
        &ir,
        callable,
        &|_, expression| { matches!(expression, IrExpr::Return(Some(_))) }
    ));
    assert!(!expression_subtree_contains(
        &ir,
        inline,
        &|expression, node| {
            matches!(node, IrExpr::Return(_)) || ir.checked_return_depths.contains_key(&expression)
        }
    ));
    assert!(expression_subtree_contains(
        &ir,
        inline,
        &|_, expression| { matches!(expression, IrExpr::While { .. }) }
    ));
}

#[test]
fn inline_expansion_inside_lambda_consumes_its_return_depth_once() {
    let ir = lower_single_source(
        "inline fun apply(value: Int, block: (Int) -> Int): Int { return block(value) }\n\
         fun make(): () -> Int = { apply(1) { it + 1 } }\n",
        "NestedInlineReturnOwnership",
    );
    let outer_inline = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                inline_body: Some(inline),
                ..
            } => Some(*inline),
            _ => None,
        })
        .next_back()
        .expect("outer lambda inline template");

    assert!(!expression_subtree_contains(
        &ir,
        outer_inline,
        &|expression, node| {
            !matches!(node, IrExpr::Return(_)) && ir.checked_return_depths.contains_key(&expression)
        }
    ));
}

#[test]
fn lambda_with_non_local_return_has_no_standalone_callable_realization() {
    let ir = lower_single_source(
        "inline fun outer(block: () -> Unit) { inner { block() } }\n\
         inline fun inner(block: () -> Unit) { block() }\n\
         fun probe(hit: Boolean): Int {\n\
             outer { if (hit) return 7 }\n\
             return -1\n\
         }\n",
        "NonLocalReturnRealization",
    );
    let implementations = ir
        .lambda_origins
        .iter()
        .filter_map(|(function, origin)| {
            (origin.implementation_name == "probe").then_some(*function)
        })
        .collect::<Vec<_>>();
    assert_eq!(implementations.len(), 1, "probe lambda implementations");
    assert!(
        ir.inline_only_fns.contains(&implementations[0]),
        "a non-local return cannot be emitted in the lambda's own callable descriptor"
    );
    let probe = ir
        .functions
        .iter()
        .find(|function| function.name == "probe")
        .and_then(|function| function.body)
        .expect("probe body");
    assert!(!expression_subtree_contains(
        &ir,
        probe,
        &|_, expression| matches!(
            expression,
            IrExpr::Lambda { impl_fn, .. } if *impl_fn == implementations[0]
        )
    ));
}

#[test]
fn unit_lambda_callable_materializes_unit_on_an_explicit_bare_return() {
    let ir = lower_single_source(
        "fun make(): () -> Unit = fun() { return }\n",
        "UnitLambdaBareReturn",
    );
    let implementation = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ => None,
        })
        .expect("anonymous Unit function value");
    let callable = ir.functions[implementation as usize]
        .body
        .expect("materialized callable body");

    assert_eq!(
        ir.functions[implementation as usize].ret,
        Ty::obj("kotlin/Unit")
    );
    assert!(expression_subtree_contains(
        &ir,
        callable,
        &|_, expression| {
            let IrExpr::Return(Some(value)) = expression else {
                return false;
            };
            matches!(ir.expr(*value), IrExpr::UnitInstance)
        }
    ));
}

#[test]
fn default_expressions_are_lowered_but_not_inserted_into_body_roots() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(10));
    let default = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(12)),
    });
    body.add_default_value(crate::fir::FirDefaultValue {
        origin,
        parameter: 1,
        ty: resolved(Ty::Int),
        value: default,
    });

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(lowered.roots.is_empty());
    assert_eq!(lowered.defaults.as_ref(), &[(1, 0)]);
    assert!(matches!(ir.expr(0), IrExpr::Const(IrConst::Int(12))));
}

#[test]
fn implicit_non_unit_callable_result_is_a_terminal_return_statement() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(10));
    body.set_result_type(resolved(Ty::Int));
    body.set_implicit_return();
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(12)),
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(value),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let callable_body = super::finish_callable_body(
        &mut ir,
        lowered.roots.into_vec(),
        lowered.result_type.unwrap(),
        lowered.implicit_return,
        false,
        origin,
    )
    .unwrap();

    let IrExpr::Block { stmts, value } = ir.expr(callable_body) else {
        panic!("callable body must be a block")
    };
    assert!(value.is_none());
    assert!(matches!(
        stmts.as_slice(),
        [returned] if matches!(ir.expr(*returned), IrExpr::Return(Some(_)))
    ));
}

#[test]
fn unit_widening_materializes_a_value_after_the_source_effect() {
    let origin = OriginId::from_raw(0);
    let target = resolved(Ty::nullable(Ty::obj("kotlin/Any")));
    let mut body = FirBody::new(BodyOwnerId::from_raw(10));
    let effect = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Unit),
        kind: FirExprKind::Block {
            statements: Box::new([]),
            result: None,
        },
    });
    let widened = body.add_expr(FirExpr {
        origin,
        ty: target,
        kind: FirExprKind::ImplicitConversion {
            value: effect,
            conversion: FirConversion {
                origin,
                kind: FirConversionKind::NullabilityWidening { to: target },
            },
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(widened),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let IrExpr::Block {
        stmts,
        value: Some(value),
    } = ir.expr(lowered.roots[0])
    else {
        panic!("Unit widening must run the source effect and then yield a value")
    };
    assert_eq!(stmts.len(), 1);
    assert!(matches!(ir.expr(*value), IrExpr::UnitInstance));
}

#[test]
fn common_ir_sink_consumes_scheduled_function_body_and_defaults() {
    let source = "fun sum(value: Int = 1): Int = value + 2";
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source).with_file_stem("Sink")],
        Box::new(crate::libraries::EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary =
        streamed.ordinary_body_work(&analysis.files[0], crate::fir::SourceFileId::from_raw(0));
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    let mut ir = IrFile::default();
    let mut sink =
        super::CommonIrBodySink::new(&index, crate::fir::SourceFileId::from_raw(0), &mut ir)
            .unwrap();
    sink.accept_default_arguments(&index, &mut default_arguments)
        .unwrap();
    for work in ordinary {
        let mut indexed_sink = sink.indexed(&index);
        crate::fir::check_and_dispatch_body(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked type info"),
            crate::fir::SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut indexed_sink,
        )
        .unwrap();
    }
    sink.finish(&index).unwrap();

    assert_eq!(ir.functions.len(), 1);
    assert_eq!(ir.functions[0].name, "sum");
    assert_eq!(ir.functions[0].params, vec![Ty::Int]);
    assert_eq!(
        ir.fn_params.get(&0).and_then(|info| info.defaults.as_ref()),
        Some(&vec![Some(0)])
    );
    assert!(ir.functions[0].body.is_some());
}

#[test]
fn common_ir_sink_prepares_inline_bodies_before_ordinary_callers() {
    let source = "inline fun twice(value: Int): Int = value * 2\nfun answer(): Int = twice(21)";
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source).with_file_stem("InlineSink")],
        Box::new(crate::libraries::EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary =
        streamed.ordinary_body_work(&analysis.files[0], crate::fir::SourceFileId::from_raw(0));
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    assert_eq!(inline_bodies.len(), 1);
    let mut ir = IrFile::default();
    let mut sink =
        super::CommonIrBodySink::new(&index, crate::fir::SourceFileId::from_raw(0), &mut ir)
            .unwrap();
    sink.accept_default_arguments(&index, &mut default_arguments)
        .unwrap();
    sink.accept_inline_bodies(&index, &mut inline_bodies)
        .unwrap();
    assert_eq!(inline_bodies.len(), 1);
    assert_eq!(sink.attached_body_count(), 1);

    let mut no_inline_insertions = crate::fir::InlineBodyStore::default();
    for work in ordinary {
        let mut indexed_sink = sink.indexed(&index);
        crate::fir::check_and_dispatch_body(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked type info"),
            crate::fir::SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut no_inline_insertions,
            &mut indexed_sink,
        )
        .unwrap();
    }
    assert_eq!(sink.attached_body_count(), 2);
    sink.finish(&index).unwrap();
    assert!(no_inline_insertions.is_empty());
}

#[test]
fn unsigned_range_containment_stays_semantic_until_backend_realization() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(12));
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::UInt),
        kind: FirExprKind::Constant(FirConstant::UInt(2)),
    });
    let start = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::UInt),
        kind: FirExprKind::Constant(FirConstant::UInt(1)),
    });
    let end = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::UInt),
        kind: FirExprKind::Constant(FirConstant::UInt(3)),
    });
    let contains = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Boolean),
        kind: FirExprKind::InRange {
            operation: FirRangeOperation::Through,
            comparison: resolved(Ty::UInt),
            value,
            start,
            end,
            negated: false,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(contains),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::Checked(IrCheckedOperation::RangeContains {
            operation: FirRangeOperation::Through,
            counter: Ty::UInt,
            negated: false,
            ..
        })
    ));
}

fn lower_single_source(source: &str, stem: &str) -> IrFile {
    lower_single_source_with_platform(source, stem, Box::new(crate::libraries::EmptySymbolSource))
}

fn lower_single_source_with_platform(
    source: &str,
    stem: &str,
    platform: Box<dyn crate::libraries::SemanticPlatform>,
) -> IrFile {
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source).with_file_stem(stem)],
        platform,
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary =
        streamed.ordinary_body_work(&analysis.files[0], crate::fir::SourceFileId::from_raw(0));
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    let retained_inline_count = inline_bodies.len();
    let mut ir = IrFile::default();
    let mut sink =
        super::CommonIrBodySink::new(&index, crate::fir::SourceFileId::from_raw(0), &mut ir)
            .unwrap();
    sink.accept_default_arguments(&index, &mut default_arguments)
        .unwrap();
    sink.accept_inline_bodies(&index, &mut inline_bodies)
        .unwrap();
    for work in ordinary {
        let mut indexed_sink = sink.indexed(&index);
        crate::fir::check_and_dispatch_body(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked type info"),
            crate::fir::SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut indexed_sink,
        )
        .unwrap();
    }
    if let Err(error) = sink.finish(&index) {
        panic!(
            "common FIR sink failed: {error:?}; callable realizations={:?}; declarations={:?}",
            ir.checked_callable_functions,
            (0..index.declaration_count())
                .map(|raw| {
                    let declaration = DeclarationId::from_raw(raw as u32);
                    (
                        declaration,
                        index.declaration_anchor(declaration),
                        index.declaration_header(declaration),
                        index
                            .callable_for_declaration(declaration)
                            .and_then(|callable| index.callable_name(callable.id)),
                    )
                })
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(inline_bodies.len(), retained_inline_count);
    ir
}

fn lower_source_from_set(sources: &[(&str, &str)], active_source: usize) -> IrFile {
    #[derive(Default)]
    struct DeferredBodies(Vec<(crate::fir::BodyOwnerId, FirBody)>);

    impl crate::fir::CheckedBodySink for DeferredBodies {
        fn accept_finalized(&mut self, owner: crate::fir::BodyOwnerId, body: FirBody) {
            self.0.push((owner, body));
        }
    }

    let inputs = sources
        .iter()
        .map(|(source, stem)| crate::source::SourceInput::kotlin(*source).with_file_stem(*stem))
        .collect::<Vec<_>>();
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::libraries::EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let source_id = crate::fir::SourceFileId::from_raw(active_source as u32);
    let ordinary = streamed.ordinary_body_work(&analysis.files[active_source], source_id);
    let (index, inline_bodies, mut default_arguments, mut source_map) =
        streamed.module.into_parts();
    let mut ir = IrFile::default();
    let mut sink = super::CommonIrBodySink::new(&index, source_id, &mut ir).unwrap();
    sink.accept_default_arguments(&index, &mut default_arguments)
        .unwrap();
    sink.accept_inline_bodies(&index, &inline_bodies).unwrap();
    for work in ordinary {
        let mut deferred = DeferredBodies::default();
        crate::fir::check_and_dispatch_body(
            &analysis.files[active_source],
            analysis.types[active_source]
                .as_ref()
                .expect("checked type info"),
            source_id,
            work,
            &index,
            source_map.origins_mut(),
            &mut crate::fir::InlineBodyStore::default(),
            &mut deferred,
        )
        .unwrap();
        for (owner, body) in deferred.0 {
            sink.accept_streamed_body(&index, &inline_bodies, owner, body)
                .unwrap();
        }
    }
    sink.finish(&index).unwrap();
    ir
}

fn resolved(ty: Ty) -> ResolvedTy {
    ResolvedTy::new(ty).unwrap()
}
