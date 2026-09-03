use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_semantics,
    jvm_stdlib_semantics, root_expression,
};
use super::*;

#[test]
fn top_level_function_reference_keeps_only_stable_callable_identity() {
    let (body, index) = checked_function_body(
        "fun double(value: Int): Int = value * 2\n\
         fun reference(): (Int) -> Int = ::double\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        binding,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("callable reference")
        .kind
    else {
        panic!("top-level reference must become checked callable-reference FIR")
    };
    assert!(index
        .callable(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Static);
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn extension_function_reference_exposes_reflection_members() {
    let (body, _) = checked_function_body_with_platform(
        "fun Int.baz(): Int = this\n\
         fun reference(): String = Int::baz.name\n",
        "reference",
        jvm_stdlib_semantics(),
    );

    assert_eq!(
        body.expr(root_expression(&body))
            .expect("KCallable.name access")
            .ty
            .get(),
        Ty::String
    );
}

#[test]
fn top_level_lateinit_initialization_test_keeps_the_selected_property_identity() {
    let (body, index) = checked_function_body_with_platform(
        "lateinit var state: String\n\
         fun ready(): Boolean = ::state.isInitialized\n",
        "ready",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Binary {
        operation: FirBinaryOperation::ReferentialNotEqual,
        lhs,
        rhs: _,
    } = body
        .expr(root_expression(&body))
        .expect("lateinit initialization test")
        .kind
    else {
        panic!("isInitialized must become a checked raw-field/null comparison")
    };
    let FirExprKind::LateinitFieldRead { target } = body
        .expr(lhs)
        .expect("raw top-level lateinit field read")
        .kind
    else {
        panic!("isInitialized must retain its stable property target")
    };
    let declaration = index
        .property_declaration(target)
        .expect("stable top-level property declaration");
    assert!(index
        .declaration_header(declaration)
        .expect("lateinit declaration header")
        .flags
        .has(crate::fir::DeclarationFlags::LATEINIT));
}

#[test]
fn companion_extension_function_reference_is_static_and_receiverless() {
    let (body, index) = checked_function_body(
        "class C\n\
         companion fun C.answer(): String = \"OK\"\n\
         fun reference(): () -> String = C::answer\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        function_type,
        binding,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("companion extension function reference")
        .kind
    else {
        panic!("companion extension function reference must become checked FIR")
    };
    let declaration = index
        .callable(target.module().expect("stable source callable"))
        .expect("companion extension declaration");
    assert!(index
        .declaration_header(declaration.declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
    let Ty::Fun(signature) = function_type.get() else {
        panic!("callable reference must retain its function type")
    };
    assert!(signature.params.is_empty());
    assert_eq!(*binding, FirCallableReferenceBinding::Static);
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn contextual_companion_reference_skips_inapplicable_instance_member() {
    let (body, index) = checked_function_body(
        "class C {\n\
             fun baz(value: Int) {}\n\
             companion { fun baz(value: String): String = value }\n\
         }\n\
         fun consume(block: () -> String): String = block()\n\
         fun consume(block: (String) -> String): String = block(\"OK\")\n\
         fun box(): String = consume(C::baz)\n",
        "box",
    );
    let reference = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::CallableReference {
                target,
                function_type,
                binding,
                ..
            } => Some((target, function_type, binding)),
            _ => None,
        })
        .expect("context-selected callable reference");
    let declaration = index
        .callable(reference.0.module().expect("stable associated target"))
        .expect("associated callable")
        .declaration;
    assert!(index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
    assert_eq!(reference.1.get(), Ty::fun(vec![Ty::String], Ty::String));
    assert_eq!(*reference.2, FirCallableReferenceBinding::Static);
}

#[test]
fn companion_extension_property_reference_is_static_and_receiverless() {
    let (body, index) = checked_function_body_with_platform(
        "class C\n\
         companion val C.answer: String = \"OK\"\n\
         fun reference(): () -> String = C::answer\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference {
        target,
        function_type,
        binding,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("companion extension property reference")
        .kind
    else {
        panic!("companion extension property reference must become checked FIR")
    };
    let declaration = index
        .property_declaration(target.module().expect("stable source property"))
        .expect("companion extension declaration");
    assert!(index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
    let Ty::Fun(signature) = function_type.get() else {
        panic!("property reference must retain its invocation type")
    };
    assert!(signature.params.is_empty());
    assert_eq!(*binding, FirCallableReferenceBinding::Static);
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn elvis_context_allows_a_nullable_callable_reference_result() {
    let (body, _) = checked_function_body(
        "class Value<T : Any>(val value: T)\n\
         fun <T, R> T.map(block: (T) -> R): R = block(this)\n\
         fun nullable(value: Value<String>?): String? = null\n\
         fun reference(value: Value<String>?): String = value.map(::nullable) ?: \"OK\"\n",
        "reference",
    );
    let root = body.expr(root_expression(&body)).expect("elvis expression");
    let FirExprKind::Elvis { lhs, rhs: _ } = root.kind else {
        panic!("the checked body must retain the elvis operation")
    };
    assert_eq!(
        body.expr(lhs).expect("nullable left branch").ty.get(),
        Ty::nullable(Ty::String),
    );
}

#[test]
fn non_function_expected_type_does_not_suppress_natural_unbound_method_reference() {
    let (body, _) = checked_function_body_with_platform(
        "interface Target { fun invoke() }\n\
         fun reference(): Any? = Target::invoke\n",
        "reference",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::CallableReference {
        function_type,
        binding,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("unbound method reference")
        .kind
    else {
        panic!("the natural method reference must become checked callable-reference FIR")
    };
    assert_eq!(
        function_type.get(),
        Ty::fun(vec![Ty::obj("Target")], Ty::Unit)
    );
    assert_eq!(*binding, FirCallableReferenceBinding::Unbound);
}

#[test]
fn primitive_array_constructor_reference_keeps_checked_classifier_operation() {
    let (body, _) = checked_function_body_with_platform(
        "fun reference(): (Int, (Int) -> Char) -> CharArray = ::CharArray\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::CallableReference {
        target:
            FirCallableReferenceTarget::Classifier {
                classifier,
                operation: crate::fir::FirClassifierCallable::ArrayConstructor { element },
                parameters,
                result,
            },
        binding,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("array constructor reference")
        .kind
    else {
        panic!("array constructor reference must become a checked classifier operation")
    };
    assert!(classifier.matches("kotlin/CharArray"));
    assert_eq!(element.get(), Ty::Char);
    assert_eq!(parameters.len(), 2);
    assert_eq!(result.get(), Ty::obj("kotlin/CharArray"));
    assert_eq!(*binding, FirCallableReferenceBinding::Static);
}

#[test]
fn specialized_generic_constructor_reference_keeps_its_stable_constructor_identity() {
    let (body, index) = checked_function_body(
        "class Wrapper<T>(val value: T)\n\
         fun reference(): (String) -> Wrapper<String> = ::Wrapper\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target:
            FirCallableReferenceTarget::Constructor {
                target,
                classifier,
                parameters,
                result,
                ..
            },
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("generic constructor reference")
        .kind
    else {
        panic!("a specialized source constructor must keep checked constructor FIR")
    };
    let FirConstructorTarget::Module(target) = target else {
        panic!("a source constructor reference must retain a module identity")
    };
    assert!(index.callable(*target).is_some());
    assert!(classifier.matches("Wrapper"));
    assert_eq!(parameters.len(), 1);
    assert_eq!(parameters[0].get(), Ty::String);
    assert_eq!(result.get(), Ty::obj_args("Wrapper", &[Ty::String]));
}

#[test]
fn dependency_constructor_reference_keeps_its_external_declaration_identity() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.reflect.KFunction2\n\
         fun reference(): KFunction2<String?, Throwable?, Throwable> = ::Throwable\n",
        "reference",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::CallableReference {
        target:
            FirCallableReferenceTarget::Constructor {
                target:
                    FirConstructorTarget::External {
                        declaration: _,
                        classifier,
                        parameters: selected_parameters,
                    },
                parameters,
                result,
                ..
            },
        reflective,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("dependency constructor reference")
        .kind
    else {
        panic!("a dependency constructor reference must keep checked constructor FIR")
    };
    assert_eq!(selected_parameters.as_ref(), parameters.as_ref());
    assert_eq!(parameters.len(), 2);
    assert!(matches!(
        parameters[0].get(),
        Ty::Nullable(_) | Ty::PlatformNullable(_)
    ));
    assert_eq!(parameters[0].get().non_null(), Ty::String);
    assert!(matches!(
        parameters[1].get(),
        Ty::Nullable(_) | Ty::PlatformNullable(_)
    ));
    assert_eq!(
        parameters[1].get().non_null().kotlin_class_internal(),
        Some(*classifier)
    );
    assert_eq!(result.get().kotlin_class_internal(), Some(*classifier));
    assert!(*reflective);
}

#[test]
fn star_projected_recursive_bound_extension_reference_reaches_checked_fir() {
    let (body, _) = checked_function_body_with_platform(
        "class Recursive<T : Comparable<T>>\n\
         fun <T : Comparable<T>> Recursive<T>.select(value: T): T = value\n\
         fun reference() { val selected = Recursive<*>::select }\n",
        "reference",
        jvm_semantics(),
    );
    assert!((0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32))
            .is_some_and(|expression| {
                matches!(expression.kind, FirExprKind::CallableReference { .. })
            })
    }));
}

#[test]
fn local_class_member_reference_uses_the_body_local_inferred_result() {
    let (body, index) = checked_function_body(
        "fun reference(): String {\n\
             class Id<T> { fun invoke(value: T) = value }\n\
             val selected = Id<String>::invoke\n\
             return selected(Id<String>(), \"OK\")\n\
         }\n",
        "reference",
    );
    let reference = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        matches!(expression.kind, FirExprKind::CallableReference { .. }).then_some(expression)
    });
    let Some(FirExpr {
        kind:
            FirExprKind::CallableReference {
                target,
                function_type,
                ..
            },
        ..
    }) = reference
    else {
        panic!("local-class member reference must become checked FIR")
    };
    assert!(index
        .callable(target.module().expect("stable local member target"))
        .is_some());
    let Ty::Fun(signature) = function_type.get() else {
        panic!("local member reference must retain a final function type")
    };
    assert_eq!(signature.params.len(), 2);
    assert_eq!(signature.params[1], Ty::String);
    assert_eq!(signature.ret, Ty::String);
}

#[test]
fn bound_member_reference_keeps_selected_target_and_captured_receiver() {
    let (body, index) = checked_function_body(
        "class Box { fun answer(): Int = 42 }\n\
         fun reference(box: Box): () -> Int = box::answer\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        binding,
        dispatch_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("callable reference")
        .kind
    else {
        panic!("bound member reference must become checked callable-reference FIR")
    };
    assert!(index
        .callable(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Bound);
    let receiver = dispatch_receiver.expect("bound reference captures its receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn bound_function_invoke_reference_keeps_both_checked_function_shapes() {
    let (body, _) = checked_function_body(
        "fun reference(block: suspend () -> Unit): suspend () -> Unit = block::invoke\n",
        "reference",
    );
    let FirExprKind::FunctionInvokeReference {
        callee,
        target_parameters,
        target_result,
        target_suspend,
        reference_parameters,
        reference_result,
        suspend,
    } = &body
        .expr(root_expression(&body))
        .expect("function invoke reference")
        .kind
    else {
        panic!("a bound function-value invoke reference must become checked FIR")
    };
    assert!(matches!(
        body.expr(*callee).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
    assert!(target_parameters.is_empty());
    assert_eq!(target_result.get(), Ty::Unit);
    assert!(*target_suspend);
    assert!(reference_parameters.is_empty());
    assert_eq!(reference_result.get(), Ty::Unit);
    assert!(*suspend);
}

#[test]
fn unbound_member_reference_does_not_evaluate_classifier_syntax() {
    let (body, index) = checked_function_body(
        "class Box { fun answer(): Int = 42 }\n\
         fun reference(): (Box) -> Int = Box::answer\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        binding,
        dispatch_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("callable reference")
        .kind
    else {
        panic!("unbound member reference must become checked callable-reference FIR")
    };
    assert!(index
        .callable(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Unbound);
    assert!(dispatch_receiver.is_none());
}

#[test]
fn top_level_property_reference_keeps_stable_property_identity_and_mutability() {
    let (body, index) = checked_function_body_with_platform(
        "var answer: Int = 42\n\
         fun reference(): () -> Int = ::answer\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference {
        target,
        binding,
        dispatch_receiver,
        extension_receiver,
        mutable,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("property reference")
        .kind
    else {
        panic!("top-level property reference must become checked property-reference FIR")
    };
    assert!(index
        .property_declaration(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Static);
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
    assert!(*mutable);
}

#[test]
fn bound_member_property_reference_keeps_receiver_and_stable_property_identity() {
    let (body, index) = checked_function_body_with_platform(
        "class Box(val answer: Int)\n\
         fun reference(box: Box): () -> Int = box::answer\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference {
        target,
        binding,
        dispatch_receiver,
        extension_receiver,
        mutable,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("property reference")
        .kind
    else {
        panic!("bound member property reference must become checked property-reference FIR")
    };
    assert!(index
        .property_declaration(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Bound);
    assert!(dispatch_receiver.is_some());
    assert!(extension_receiver.is_none());
    assert!(!mutable);
}

#[test]
fn unbound_member_property_reference_does_not_evaluate_classifier_syntax() {
    let (body, index) = checked_function_body_with_platform(
        "class Box(val answer: Int)\n\
         fun reference(): (Box) -> Int = Box::answer\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference {
        target,
        binding,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("property reference")
        .kind
    else {
        panic!("unbound member property reference must become checked property-reference FIR")
    };
    assert!(index
        .property_declaration(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Unbound);
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn bound_extension_property_reference_keeps_extension_receiver_separate() {
    let (body, index) = checked_function_body_with_platform(
        "class Box\n\
         val Box.answer: Int get() = 42\n\
         fun reference(box: Box): () -> Int = box::answer\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference {
        target,
        binding,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("property reference")
        .kind
    else {
        panic!("bound extension property reference must become checked property-reference FIR")
    };
    assert!(index
        .property_declaration(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Bound);
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_some());
}

#[test]
fn unbound_generic_extension_property_reference_keeps_receiver_function_shape() {
    let (body, index) = checked_function_body_with_platform(
        "val <T> List<T>.item: T get() = null as T\n\
         fun <T> reference(): List<T>.() -> T = List<T>::item\n",
        "reference",
        jvm_semantics(),
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("generic extension-property reference");
    let FirExprKind::PropertyReference {
        target, binding, ..
    } = &expression.kind
    else {
        panic!("extension property reference must remain checked property-reference FIR")
    };
    assert!(index
        .property_declaration(target.module().expect("module property target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Unbound);
    assert!(matches!(
        expression.ty.get(),
        Ty::Fun(signature)
            if signature.has_receiver
                && signature.params.len() == 1
                && matches!(signature.params[0], Ty::Obj(owner, _) if owner.matches("kotlin/collections/List"))
    ));
}

#[test]
fn specialized_generic_extension_property_reference_publishes_final_accessor_shape() {
    let (body, index) = checked_function_body_with_platform(
        "val <T> T.item: T get() = this\n\
         fun reference(): (Int) -> Int = Int::item\n",
        "reference",
        jvm_semantics(),
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("specialized extension-property reference");
    let FirExprKind::PropertyReference { target, .. } = &expression.kind else {
        panic!("generic extension property must remain a checked property reference")
    };
    let FirPropertyReferenceTarget::SpecializedModule {
        property,
        receiver,
        extension_receiver,
        property_type,
    } = target
    else {
        panic!("source property reference must carry its selected specialized callable view")
    };
    let int = ResolvedTy::new(Ty::Int).expect("Int is a publishable FIR type");
    assert!(index.property_declaration(*property).is_some());
    assert_eq!(*receiver, Some(int));
    assert!(*extension_receiver);
    assert_eq!(*property_type, int);
}

#[test]
fn dependency_property_reference_keeps_exact_external_getter_and_receiver_shape() {
    let (body, _) = checked_function_body_with_platform(
        "fun reference(): (String) -> Int = String::length\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference {
        target,
        binding,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("dependency property reference")
        .kind
    else {
        panic!("dependency property reference must become checked property-reference FIR")
    };
    let FirPropertyReferenceTarget::External {
        getter,
        setter,
        extension_receiver: target_is_extension,
        property_type,
        ..
    } = target
    else {
        panic!("dependency property reference must keep an external accessor identity")
    };
    let FirPropertyTarget::External {
        receiver,
        parameters,
        result,
        ..
    } = getter.as_ref()
    else {
        panic!("dependency getter must be external")
    };
    assert_eq!(receiver.map(ResolvedTy::get), Some(Ty::String));
    assert!(parameters.is_empty());
    assert_eq!(result.get(), Ty::Int);
    assert_eq!(property_type.get(), Ty::Int);
    assert!(setter.is_none());
    assert!(!target_is_extension);
    assert_eq!(*binding, FirCallableReferenceBinding::Unbound);
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn bound_dependency_property_reference_keeps_checked_receiver() {
    let (body, _) = checked_function_body_with_platform(
        "fun reference(): () -> Int = \"Kotlin\"::length\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference {
        target,
        binding,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("bound dependency property reference")
        .kind
    else {
        panic!("bound dependency property reference must become checked FIR")
    };
    assert!(matches!(
        target,
        FirPropertyReferenceTarget::External { .. }
    ));
    assert_eq!(*binding, FirCallableReferenceBinding::Bound);
    assert!(dispatch_receiver.is_some());
    assert!(extension_receiver.is_none());
}

#[test]
fn bound_dependency_method_reference_keeps_provider_facet_when_no_body_local_member_exists() {
    let (body, _) = checked_function_body_with_platform(
        "fun reference(): (Int) -> Char = \"KOTLIN\"::get\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::CallableReference {
        target: FirCallableReferenceTarget::External { receiver, .. },
        binding,
        dispatch_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("bound dependency method reference")
        .kind
    else {
        panic!("bound dependency method reference must become checked external-reference FIR")
    };
    assert_eq!(receiver.map(ResolvedTy::get), Some(Ty::String));
    assert_eq!(*binding, FirCallableReferenceBinding::Bound);
    assert!(dispatch_receiver.is_some());
}

#[test]
fn classifier_property_reference_keeps_explicit_semantic_operation() {
    let (body, _) = checked_function_body_with_platform(
        "enum class Choice { FIRST }\n\
         fun reference(): Unit { val selected = Choice::entries }\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::PropertyReference { target, .. } = &(0..body.expression_count())
        .find_map(|raw| {
            body.expr(FirExprId::from_raw(raw as u32))
                .filter(|expression| {
                    matches!(expression.kind, FirExprKind::PropertyReference { .. })
                })
        })
        .expect("classifier property reference")
        .kind
    else {
        panic!("classifier property reference must become checked FIR")
    };
    let FirPropertyReferenceTarget::Classifier {
        owner,
        property,
        property_type,
    } = target
    else {
        panic!("classifier property reference must keep its semantic operation")
    };
    assert!(owner.matches("Choice"));
    assert_eq!(*property, FirClassifierProperty::EnumEntries);
    assert_eq!(
        property_type.get(),
        Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj("Choice")])
    );
}

#[test]
fn adapted_top_level_reference_keeps_complete_default_argument_plan() {
    let (body, index) = checked_function_body(
        "fun join(value: String, suffix: String = \"K\"): String = value + suffix\n\
         fun reference(): (String) -> String = ::join\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        adaptation: Some(adaptation),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("adapted reference")
        .kind
    else {
        panic!("adapted source reference must become checked callable-reference FIR")
    };
    assert!(index
        .callable(target.module().expect("module target"))
        .is_some());
    assert_eq!(
        adaptation.arguments.as_ref(),
        [
            FirAdaptedReferenceArgument::Value(0),
            FirAdaptedReferenceArgument::Default,
        ]
    );
    assert_eq!(adaptation.parameter_types.len(), 1);
}

#[test]
fn selected_suspend_sam_context_replaces_ambiguous_callable_reference_probe() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +SuspendConversion\n\
         fun interface Action { suspend fun invoke() }\n\
         fun consume(action: Action) {}\n\
         fun target() {}\n\
         suspend fun target(value: String = \"\"): Int = 0\n\
         fun run() { consume(::target) }\n",
        "run",
    );

    let reference = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(expression.kind, FirExprKind::CallableReference { .. }).then_some(expression)
        })
        .expect("selected SAM argument must retain its checked callable reference");
    let FirExprKind::CallableReference {
        target,
        adaptation: Some(adaptation),
        ..
    } = &reference.kind
    else {
        panic!("regular target must carry its suspend conversion adapter")
    };
    assert!(adaptation.suspend_conversion);
    assert!(adaptation.arguments.is_empty());
    let callable = index
        .callable(target.module().expect("selected source target"))
        .expect("selected callable header");
    let signature = index
        .signature(callable.declaration)
        .expect("selected callable signature");
    assert!(signature.parameters.is_empty());
    assert_eq!(signature.result.get(), Ty::Unit);
}

#[test]
fn bound_defaulted_reference_adapts_to_a_zero_argument_sam() {
    let (body, _) = checked_function_body(
        "fun interface Action { fun invoke() }\n\
         fun accept(action: Action) {}\n\
         class C { fun target(value: String = \"OK\"): String = value }\n\
         fun run(c: C) { accept(c::target) }\n",
        "run",
    );
    let reference = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::CallableReference { .. }))
        .expect("bound adapted reference");
    let FirExprKind::CallableReference {
        adaptation: Some(adaptation),
        ..
    } = &reference.kind
    else {
        panic!("bound reference must retain its default-argument adapter")
    };
    assert_eq!(
        adaptation.arguments.as_ref(),
        [FirAdaptedReferenceArgument::Default]
    );
    assert!((0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Call(call) => Some(call),
            _ => None,
        })
        .flat_map(|call| call.arguments.iter())
        .any(|argument| matches!(
            argument,
            FirCallArgument::Expression {
                conversion: Some(FirConversion {
                    kind: FirConversionKind::Sam(_),
                    ..
                }),
                ..
            }
        )));
}

#[test]
fn adapted_vararg_reference_keeps_collected_value_ordinals() {
    let (body, _) = checked_function_body(
        "fun join(vararg values: String): String = \"\"\n\
         fun reference(): (String, String) -> String = ::join\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        adaptation: Some(adaptation),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("adapted reference")
        .kind
    else {
        panic!("adapted vararg reference must become checked callable-reference FIR")
    };
    assert_eq!(
        adaptation.arguments.as_ref(),
        [FirAdaptedReferenceArgument::Vararg {
            values: vec![0, 1].into_boxed_slice(),
            whole_array: false,
        }]
    );
}

#[test]
fn imported_object_reference_keeps_selected_singleton_classifier_and_vararg_plan() {
    let (body, index) = checked_function_body(
        "import Host.join\n\
         object Host { fun join(vararg values: String): String = \"\" }\n\
         fun reference(): (String) -> String = ::join\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        binding,
        dispatch_receiver: Some(dispatch_receiver),
        adaptation: Some(adaptation),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("adapted object reference")
        .kind
    else {
        panic!("an imported object member must become a bound checked callable reference")
    };
    assert!(index
        .callable(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Bound);
    assert!(matches!(
        body.expr(dispatch_receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::SingletonValue { classifier }) if classifier.matches("Host")
    ));
    assert_eq!(
        adaptation.arguments.as_ref(),
        [FirAdaptedReferenceArgument::Vararg {
            values: vec![0].into_boxed_slice(),
            whole_array: false,
        }]
    );
}

#[test]
fn source_typealias_to_unit_keeps_the_target_singleton_value() {
    let (body, _) = checked_function_body(
        "typealias TestResult = Unit\n\
         fun make(): TestResult = TestResult\n",
        "make",
    );
    assert!(matches!(
        body.expr(root_expression(&body)).map(|expression| &expression.kind),
        Some(FirExprKind::SingletonValue { classifier }) if classifier.matches("kotlin/Unit")
    ));
}

#[test]
fn unbound_generic_member_reference_uses_applied_classifier_parameters() {
    let (body, _) = checked_function_body_with_platform(
        "class C : Comparable<C> {\n\
             override fun compareTo(other: C): Int = 0\n\
         }\n\
         fun reference() = Comparable<C>::compareTo\n",
        "reference",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::CallableReference { function_type, .. } = &body
        .expr(root_expression(&body))
        .expect("unbound member reference")
        .kind
    else {
        panic!("unbound generic member must become checked callable-reference FIR")
    };
    let Ty::Fun(signature) = function_type.get() else {
        panic!("callable reference must retain its exact function type")
    };
    assert_eq!(
        signature.params.as_ref(),
        [
            Ty::obj_args("kotlin/Comparable", &[Ty::obj("C")]),
            Ty::obj("C")
        ]
    );
    assert_eq!(signature.ret, Ty::Int);
}

#[test]
fn adapted_bound_member_reference_keeps_receiver_and_argument_plan() {
    let (body, index) = checked_function_body(
        "class Box { fun join(value: String, suffix: String = \"K\"): String = value + suffix }\n\
         fun reference(box: Box): (String) -> String = box::join\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        binding,
        dispatch_receiver,
        adaptation: Some(adaptation),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("adapted reference")
        .kind
    else {
        panic!("adapted member reference must become checked callable-reference FIR")
    };
    assert!(index
        .callable(target.module().expect("module target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Bound);
    assert!(dispatch_receiver.is_some());
    assert_eq!(
        adaptation.arguments.as_ref(),
        [
            FirAdaptedReferenceArgument::Value(0),
            FirAdaptedReferenceArgument::Default,
        ]
    );
}

#[test]
fn adapted_unbound_member_reference_keeps_receiver_outside_the_target_argument_plan() {
    let (body, index) = checked_function_body(
        "class Box { fun join(value: Int, vararg suffixes: String): Unit {} }\n\
         fun reference(): Box.(Int) -> Unit = Box::join\n",
        "reference",
    );
    let FirExprKind::CallableReference {
        target,
        binding,
        dispatch_receiver,
        adaptation: Some(adaptation),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("adapted unbound reference")
        .kind
    else {
        panic!("adapted unbound member reference must become checked FIR")
    };
    assert!(index
        .callable(target.module().expect("module member target"))
        .is_some());
    assert_eq!(*binding, FirCallableReferenceBinding::Unbound);
    assert!(dispatch_receiver.is_none());
    assert_eq!(
        adaptation.arguments.as_ref(),
        [
            FirAdaptedReferenceArgument::Value(1),
            FirAdaptedReferenceArgument::Vararg {
                values: Box::new([]),
                whole_array: false,
            },
        ]
    );
}

#[test]
fn adapted_unbound_dependency_extension_keeps_receiver_outside_the_target_argument_plan() {
    let (body, _) = checked_function_body_with_platform(
        "fun reference(): (String, Int) -> String = String::padEnd\n",
        "reference",
        jvm_semantics(),
    );
    let FirExprKind::CallableReference {
        target:
            FirCallableReferenceTarget::External {
                receiver,
                extension_receiver,
                parameters,
                ..
            },
        binding,
        adaptation: Some(adaptation),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("adapted unbound extension reference")
        .kind
    else {
        panic!("adapted dependency extension must become checked external-reference FIR")
    };
    assert_eq!(*binding, FirCallableReferenceBinding::Unbound);
    assert_eq!(receiver.map(ResolvedTy::get), Some(Ty::String));
    assert!(*extension_receiver);
    assert_eq!(parameters.len(), 2);
    assert_eq!(
        adaptation.arguments.as_ref(),
        [
            FirAdaptedReferenceArgument::Value(1),
            FirAdaptedReferenceArgument::Default,
        ]
    );
    assert_eq!(adaptation.parameter_types.len(), 2);
}

#[test]
fn unbound_class_literal_keeps_resolved_classifier_without_source_receiver() {
    let (body, _) = checked_function_body_with_platform(
        "fun literal(): kotlin.reflect.KClass<String> = String::class\n",
        "literal",
        jvm_semantics(),
    );
    let FirExprKind::ClassLiteral { classifier, value } = body
        .expr(root_expression(&body))
        .expect("class literal")
        .kind
    else {
        panic!("unbound class literal must become checked class-literal FIR")
    };
    assert!(classifier.is_some());
    assert!(value.is_none());
}

#[test]
fn bare_array_class_literal_keeps_a_star_projected_classifier() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +BareArrayClassLiteral\n\
         fun literal() = Array::class\n",
        "literal",
        jvm_semantics(),
    );
    let FirExprKind::ClassLiteral { classifier, value } = body
        .expr(root_expression(&body))
        .expect("class literal")
        .kind
    else {
        panic!("bare Array literal must become checked class-literal FIR")
    };
    assert!(value.is_none());
    assert_eq!(
        classifier.map(|classifier| classifier.get()),
        Some(Ty::obj_args(
            "kotlin/Array",
            &[Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any")))]
        ))
    );
}

#[test]
fn kotlin_2_4_accepts_a_bare_array_class_literal_without_an_opt_in_directive() {
    let (body, _) = checked_function_body_with_platform(
        "fun literal() = Array::class\n",
        "literal",
        jvm_semantics(),
    );
    let FirExprKind::ClassLiteral { classifier, value } = body
        .expr(root_expression(&body))
        .expect("class literal")
        .kind
    else {
        panic!("bare Array literal must become checked class-literal FIR")
    };
    assert!(value.is_none());
    assert_eq!(
        classifier.map(|classifier| classifier.get()),
        Some(Ty::obj_args(
            "kotlin/Array",
            &[Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any")))]
        ))
    );
}

#[test]
fn bound_class_literal_keeps_checked_value_expression() {
    let (body, _) = checked_function_body_with_platform(
        "fun literal(value: String): kotlin.reflect.KClass<out String> = value::class\n",
        "literal",
        jvm_semantics(),
    );
    let expression = body.expr(root_expression(&body)).expect("class literal");
    assert_eq!(
        expression.ty.get(),
        Ty::obj_args("kotlin/reflect/KClass", &[Ty::out_projection(Ty::String)])
    );
    let FirExprKind::ClassLiteral { classifier, value } = expression.kind else {
        panic!("bound class literal must become checked class-literal FIR")
    };
    assert!(classifier.is_none());
    assert!(matches!(
        value
            .and_then(|value| body.expr(value))
            .map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn generic_reference_result_does_not_widen_against_an_unresolved_outer_result() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T> id(value: T): T = value\n\
         fun <T> apply(value: T?): T? = value?.let(::id)\n",
        "apply",
        jvm_semantics(),
    );

    assert!(body
        .expr(root_expression(&body))
        .expect("safe let call")
        .ty
        .get()
        .is_nullable());
}

#[test]
fn generic_reference_result_infers_through_expected_supertype() {
    let (body, _) = checked_function_body_with_platform(
        "fun <K, V> make(): MutableMap<K, V> = mutableMapOf()\n\
         fun reference(): () -> Map<String, String> = ::make\n",
        "reference",
        jvm_stdlib_semantics(),
    );

    let FirExprKind::CallableReference { function_type, .. } = &body
        .expr(root_expression(&body))
        .expect("generic callable reference")
        .kind
    else {
        panic!("the generic function must become checked callable-reference FIR")
    };
    assert_eq!(
        function_type.get(),
        Ty::fun(
            Vec::new(),
            Ty::obj_args("kotlin/collections/Map", &[Ty::String, Ty::String]),
        )
    );
}

#[test]
fn unbound_member_reference_constrains_open_enclosing_call_slots() {
    let (body, index) = checked_function_body_with_platform(
        "fun <T, R> generic(value: T): R = TODO()\n\
         inline fun <reified T, reified R> consume(\n\
             first: (T) -> R, second: (T) -> R, tName: String, rName: String\n\
         ): Unit {}\n\
         fun use(): Unit { consume(Int::toString, ::generic, \"Int\", \"String\") }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let reference = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| {
            let FirExprKind::CallableReference {
                target: FirCallableReferenceTarget::External { .. },
                function_type,
                ..
            } = &expression.kind
            else {
                return false;
            };
            let Ty::Fun(function) = function_type.get() else {
                return false;
            };
            function.params.as_ref() == [Ty::Int] && function.ret.non_null() == Ty::String
        })
        .unwrap_or_else(|| panic!("Int::toString reference missing from {body:#?}"));
    let FirExprKind::CallableReference { function_type, .. } = &reference.kind else {
        unreachable!("selected expression is a callable reference")
    };
    let Ty::Fun(function) = function_type.get() else {
        panic!("selected reference must retain a function type")
    };
    assert_eq!(function.params.as_ref(), [Ty::Int]);
    assert_eq!(function.ret.non_null(), Ty::String);
    let consume = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| {
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            call.target
                .module()
                .filter(|target| index.callable_name(*target) == Some("consume"))
                .map(|_| call)
        })
        .expect("consume call");
    assert_eq!(
        consume
            .substitutions
            .iter()
            .map(|substitution| substitution.value.get())
            .collect::<Vec<_>>(),
        [Ty::Int, Ty::String],
    );
}

#[test]
fn vararg_reference_accepts_a_covariant_whole_reference_array_without_repacking() {
    let (body, index) = checked_function_body_with_platform(
        "fun total(vararg values: Number): Int = values.size\n\
         fun reference(): (Array<Int>) -> Int = ::total\n",
        "reference",
        jvm_semantics(),
    );

    let FirExprKind::CallableReference {
        target,
        adaptation: None,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("vararg reference")
        .kind
    else {
        panic!("a whole reference array must not be repacked")
    };
    let callable = index
        .callable(target.module().expect("module vararg target"))
        .expect("resolved vararg callable");
    assert_eq!(
        index.signature(callable.declaration).unwrap().parameters[0].get(),
        Ty::obj_args("kotlin/Array", &[Ty::obj("kotlin/Number")]),
    );
}

#[test]
fn inferred_fun_interface_constructor_reference_property_is_invokable() {
    let (body, _) = checked_function_body_with_platform(
        "fun interface Action { fun run(): Unit }\n\
         val factory = ::Action\n\
         fun make(): Action = factory {}\n",
        "make",
        jvm_semantics(),
    );

    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::FunctionInvoke { .. })
    ));
}

#[test]
fn inferred_member_reference_property_retains_its_reflection_api() {
    let (body, _) = checked_function_body_with_platform(
        "class C {\n\
             fun OK(): Unit {}\n\
             companion object { val result = C::OK }\n\
         }\n\
         fun box(): String = C.result.name\n",
        "box",
        jvm_semantics(),
    );

    assert_eq!(
        body.expr(root_expression(&body))
            .expect("reflection name read")
            .ty
            .get(),
        Ty::String,
    );
}

#[test]
fn redundant_reflection_supertype_check_keeps_mutable_property_arity() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.reflect.KMutableProperty\n\
         class Box(value: String) {\n\
             var text: String = value\n\
                 private set\n\
             fun update(): Unit {\n\
                 val property = Box::text\n\
                 if (property !is KMutableProperty<*>) return\n\
                 property.set(this, \"OK\")\n\
             }\n\
         }\n",
        "update",
        jvm_semantics(),
    );

    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::Call(_))
        )
    }));
}
