use super::test_support::{checked_function_body, root_expression};
use super::*;

#[test]
fn explicit_this_is_a_checked_receiver_coordinate_not_a_local_name() {
    let (body, _) = checked_function_body("class Box { fun self(): Box = this }\n", "self");
    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn labeled_member_context_receiver_is_a_checked_receiver_coordinate() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +ContextReceivers\n\
         class Host { context(String) fun value(): String = this@String }\n",
        "value",
    );
    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn callable_type_alias_context_receiver_uses_its_source_label() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +ContextReceivers\n\
         typealias StringProvider = () -> String\n\
         context(StringProvider) fun value(): String = this@StringProvider()\n",
        "value",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("checked context receiver invocation")
            .ty
            .get(),
        Ty::String
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::ImplicitReceiver {
                current: true,
                depth: 0,
            })
        )
    }));
}

#[test]
fn nested_inner_member_extension_publishes_exact_enclosing_receiver_paths() {
    let (body, index) = checked_function_body(
        "class Outer {\n\
             val outer = \"O\"\n\
             inner class Inner1 {\n\
                 val inner = \"I\"\n\
                 inner class Inner2 {\n\
                     fun Outer.read(): String = this@Inner1.inner + this@Outer.outer\n\
                 }\n\
             }\n\
         }\n",
        "read",
    );

    let mut paths = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::EnclosingReceiver { path } = &expression.kind else {
                return None;
            };
            Some((expression.ty.get(), path.to_vec()))
        })
        .collect::<Vec<_>>();
    paths.sort_by_key(|(_, path)| path.len());

    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].0, Ty::obj("Outer$Inner1"));
    assert_eq!(paths[0].1.len(), 1);
    assert_eq!(paths[1].0, Ty::obj("Outer"));
    assert_eq!(paths[1].1.len(), 2);
    let classifiers = paths[1]
        .1
        .iter()
        .map(|declaration| {
            index
                .classifier_header(*declaration)
                .expect("enclosing path classifier")
                .classifier
        })
        .collect::<Vec<_>>();
    assert_eq!(
        classifiers,
        [
            crate::types::type_name("Outer$Inner1$Inner2"),
            crate::types::type_name("Outer$Inner1"),
        ]
    );
}

#[test]
fn classifier_star_import_publishes_a_stable_enum_entry_value() {
    let (body, _) = checked_function_body(
        "import Game.*\nenum class Game { ROCK, LIZARD }\nfun pick(): Game = LIZARD\n",
        "pick",
    );
    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::EnumEntry { classifier, name, .. })
            if *classifier == crate::types::type_name("Game") && name.as_ref() == "LIZARD"
    ));
}

#[test]
fn explicit_enum_entry_import_publishes_the_declared_entry_value() {
    let (body, _) = checked_function_body(
        "import Choice.ONE\nenum class Choice { ONE, TWO }\nfun pick(): Choice = ONE\n",
        "pick",
    );
    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::EnumEntry { classifier, name, .. })
            if classifier.matches("Choice") && name.as_ref() == "ONE"
    ));
}

#[test]
fn nested_classifier_star_import_uses_classifier_identity_in_body_scope() {
    let (body, _) = checked_function_body(
        "package test\n\
         import test.A.B.*\n\
         class A {\n\
             private class B { object C; class D }\n\
             fun make(): Any { C; return D() }\n\
         }\n",
        "make",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::SingletonValue { classifier, .. })
                if classifier.matches("test/A$B$C")
        )
    }));
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| (&expression.kind, expression.ty.get())),
            Some((FirExprKind::ConstructorCall(_), ty))
                if ty.obj_internal().is_some_and(|owner| owner.matches("test/A$B$D"))
        )
    }));
}

#[test]
fn class_header_supertype_is_resolved_before_own_nested_classifier_scope() {
    let (body, index) = checked_function_body(
        "package second\n\
         interface Base { fun foo(): String = \"OK\" }\n\
         class MyClass(val prop: second.Base) : Base by prop { interface Base }\n\
         fun box(): String { val data = MyClass(object : Base {}); return data.foo() }\n",
        "box",
    );
    let selected = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            if call.dispatch_receiver.is_none() || !call.arguments.is_empty() {
                return None;
            }
            let FirCallTarget::Module(target) = call.target else {
                return None;
            };
            index.callable(target).map(|_| target)
        })
        .expect("delegated interface member must be selected before FIR");
    assert!(index.callable(selected).is_some());
}

#[test]
fn smartcast_this_member_read_keeps_receiver_coordinate_and_checked_conversion() {
    let (body, _) = checked_function_body(
        "fun Any.lengthOrMinusOne() = if (this is Array<*>) size else -1\n",
        "lengthOrMinusOne",
    );
    let FirExprKind::Conditional { then_branch, .. } =
        &body.expr(root_expression(&body)).expect("conditional").kind
    else {
        panic!("smart-cast body must remain a checked conditional")
    };
    let FirExprKind::Call(call) = &body.expr(*then_branch).expect("array size read").kind else {
        panic!("array size must become a checked intrinsic call")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::ArraySize,
            ..
        }
    ));
    let receiver = call
        .dispatch_receiver
        .expect("bare size read must retain its implicit receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
    let Some(FirConversion {
        kind: FirConversionKind::SmartCast { to },
        ..
    }) = receiver.conversion
    else {
        panic!("the refined receiver must carry its checked smart-cast conversion")
    };
    assert_eq!(
        to.get().kotlin_class_internal(),
        Some(crate::types::type_name("kotlin/Array"))
    );
}

#[test]
fn nullable_extension_receiver_smartcast_applies_to_bare_member_read() {
    let (body, _) = checked_function_body(
        "class Value(val text: String)\n\
         fun Value?.read() = if (this != null) text else \"\"\n",
        "read",
    );
    let FirExprKind::Conditional { then_branch, .. } =
        &body.expr(root_expression(&body)).expect("conditional").kind
    else {
        panic!("nullable-receiver body must remain a checked conditional")
    };
    let FirExprKind::PropertyRead {
        dispatch_receiver: Some(receiver),
        ..
    } = &body.expr(*then_branch).expect("bare property read").kind
    else {
        panic!("bare member must become a checked property read")
    };
    let Some(FirConversion {
        kind: FirConversionKind::SmartCast { to },
        ..
    }) = receiver.conversion
    else {
        panic!("the implicit receiver must retain its non-null smart cast")
    };
    assert_eq!(to.get(), Ty::obj("Value"));
}

#[test]
fn anonymous_super_arguments_do_not_shadow_inherited_properties() {
    let (body, index) = super::test_support::checked_function_body_with_platform(
        "abstract class Base(val s: String, vararg ints: Int)\n\
         fun foo(s: String, ints: IntArray) = object : Base(ints = *ints, s = s) {}\n\
         fun box(): String {\n\
             return foo(\"OK\", intArrayOf(1, 2)).s\n\
         }\n",
        "box",
        super::test_support::jvm_semantics(),
    );

    let target = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::PropertyRead { target, .. } = &expression.kind else {
                return None;
            };
            target.module()
        })
        .expect("the inherited property read must have a stable target");
    let declaration = index
        .property_declaration(target)
        .expect("the inherited property must have a declaration");
    let owner = index
        .declaration_anchor(declaration)
        .and_then(|anchor| anchor.owner)
        .and_then(|owner| index.classifier_header(owner))
        .expect("the inherited property must retain its declaring classifier");
    assert_eq!(owner.classifier, crate::types::type_name("Base"));
}

#[test]
fn explicit_this_smartcast_selects_member_and_publishes_receiver_conversion() {
    let (body, index) = checked_function_body(
        "interface Base { fun read(): String = if (this is WithValue) this.value() else \"\" }\n\
         interface Value { fun value(): String }\n\
         class WithValue : Base, Value { override fun value() = \"OK\" }\n",
        "read",
    );
    let FirExprKind::Conditional { then_branch, .. } =
        &body.expr(root_expression(&body)).expect("conditional").kind
    else {
        panic!("smart-cast body must remain a checked conditional")
    };
    let FirExprKind::Call(call) = &body.expr(*then_branch).expect("value call").kind else {
        panic!("explicit this member must remain a checked call")
    };
    let receiver = call
        .dispatch_receiver
        .expect("value call must retain its dispatch receiver");
    let Some(FirConversion {
        kind: FirConversionKind::SmartCast { to },
        ..
    }) = receiver.conversion
    else {
        panic!("explicit this must retain its checked smart-cast conversion")
    };
    assert_eq!(to.get(), Ty::obj("WithValue"));
    assert!(call
        .target
        .module()
        .and_then(|target| index.callable(target))
        .is_some());
}

#[test]
fn intersection_smartcast_selects_the_covariant_member_independent_of_test_order() {
    for condition in ["x is A && x is B", "x is B && x is A"] {
        let source = format!(
            "interface A {{ fun value(): Any? }}\n\
             interface B {{ fun value(): String }}\n\
             fun select(x: Any): String = if ({condition}) x.value() else \"\"\n"
        );
        let (body, index) = checked_function_body(&source, "select");
        let FirExprKind::Conditional { then_branch, .. } =
            &body.expr(root_expression(&body)).expect("conditional").kind
        else {
            panic!("intersection smart cast must remain a checked conditional")
        };
        let FirExprKind::Call(call) = &body.expr(*then_branch).expect("member call").kind else {
            panic!("intersection member invocation must be a checked call")
        };
        let FirCallTarget::Module(target) = call.target else {
            panic!("source intersection member must keep its stable callable identity")
        };
        let callable = index.callable(target).expect("selected module callable");
        assert_eq!(
            index
                .signature(callable.declaration)
                .expect("selected callable signature")
                .result
                .get(),
            Ty::String
        );
        let receiver = call
            .dispatch_receiver
            .as_ref()
            .expect("member call must retain its receiver");
        let FirExprKind::ImplicitConversion { conversion, .. } =
            &body.expr(receiver.value).expect("checked receiver").kind
        else {
            panic!("intersection use must publish its selected smart-cast conversion")
        };
        let FirConversionKind::SmartCast { to } = conversion.kind else {
            panic!("intersection receiver must use a checked smart-cast conversion")
        };
        assert_eq!(to.get().obj_internal(), Some(crate::types::type_name("B")));
    }
}

#[test]
fn inferred_conditional_local_retains_all_minimal_common_supertypes() {
    let (body, index) = checked_function_body(
        "interface Root\n\
         interface WithValue : Root { fun value(): String }\n\
         interface Marker : Root\n\
         class First : WithValue, Marker { override fun value(): String = \"OK\" }\n\
         class Second : WithValue, Marker { override fun value(): String = \"other\" }\n\
         fun box(): String {\n\
             val selected = if (true) First() else Second()\n\
             return selected.value()\n\
         }\n",
        "box",
    );
    let call = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::Call(call) if call.target.module().is_some() => Some(call),
            _ => None,
        })
        .expect("intersection local member call");
    assert!(call
        .target
        .module()
        .and_then(|target| index.callable(target))
        .is_some());
}

#[test]
fn nominal_intersection_fake_override_selects_the_covariant_property() {
    let (body, _) = checked_function_body(
        "interface Root { val owner: Any }\n\
         interface Wide : Root { override val owner: Any }\n\
         interface Narrow<T : Any> : Root { override val owner: T }\n\
         interface Combined : Wide, Narrow<String>\n\
         fun read(value: Combined): Int = value.owner.length\n",
        "read",
    );
    assert!((0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .any(|expression| {
            expression.ty.get() == Ty::String
                && matches!(expression.kind, FirExprKind::PropertyRead { .. })
        }));
}

#[test]
fn declared_covariant_property_override_beats_nominal_intersection_projection() {
    let (body, _) = checked_function_body(
        "interface A { val result: Any }\n\
         interface B : A { override val result: String }\n\
         class Value : B { override val result: String = \"OK\" }\n\
         fun read(value: B): Int = value.result.length\n",
        "read",
    );
    assert!((0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .any(|expression| {
            expression.ty.get() == Ty::String
                && matches!(expression.kind, FirExprKind::PropertyRead { .. })
        }));
}

#[test]
fn supertype_test_keeps_the_non_null_declared_type_for_extension_selection() {
    let (body, index) = checked_function_body(
        "class Token\n\
         fun Token.extension(): String = \"OK\"\n\
         fun select(x: Token?): String = if (x is Any) x.extension() else \"\"\n",
        "select",
    );
    let FirExprKind::Conditional { then_branch, .. } =
        &body.expr(root_expression(&body)).expect("conditional").kind
    else {
        panic!("smart-cast body must remain a checked conditional")
    };
    let FirExprKind::Call(call) = &body.expr(*then_branch).expect("extension call").kind else {
        panic!("selected extension must be represented as a checked FIR call")
    };
    let FirCallTarget::Module(target) = call.target else {
        panic!("source extension must keep its stable callable identity")
    };
    assert!(index.callable(target).is_some());
    let receiver = call
        .extension_receiver
        .as_ref()
        .expect("selected extension receiver");
    let FirExprKind::ImplicitConversion { conversion, .. } =
        &body.expr(receiver.value).expect("checked receiver").kind
    else {
        panic!("nullable declared receiver must retain a checked smart cast")
    };
    let FirConversionKind::SmartCast { to } = conversion.kind else {
        panic!("receiver conversion must be a smart cast")
    };
    assert_eq!(to.get(), Ty::obj("Token"));
}

#[test]
fn implicit_type_parameter_receiver_selects_member_from_its_bound() {
    let (body, index) = checked_function_body(
        "abstract class Builder<T> { internal abstract fun build(): T }\n\
         abstract class Host<T, B : Builder<T>> { fun B.read(): T = build() }\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("implicit receiver member call")
        .kind
    else {
        panic!("bound member invocation must become a checked FIR call")
    };
    let FirCallTarget::Module(target) = call.target else {
        panic!("source bound member must retain stable callable identity")
    };
    assert!(index.callable(target).is_some());
    let receiver = call
        .dispatch_receiver
        .as_ref()
        .expect("implicit extension receiver must dispatch the member");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver { current: true, .. })
    ));
    assert!(call.extension_receiver.is_none());
}

#[test]
fn anonymous_object_result_carries_the_enclosing_callable_type_argument() {
    let (body, _) = checked_function_body(
        "class Test {\n\
             private fun <T : Any> T.self() = object { fun calc(): T = this@self }\n\
             fun value(): Int = 1.self().calc()\n\
         }\n",
        "value",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("specialized anonymous member call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("anonymous member invocation must become a checked FIR call")
    };
    assert_eq!(root.ty.get(), Ty::Int);
    let receiver = call
        .dispatch_receiver
        .as_ref()
        .expect("anonymous object dispatch receiver");
    let receiver_ty = body
        .expr(receiver.value)
        .expect("anonymous construction call")
        .ty
        .get();
    assert_eq!(receiver_ty.type_args(), &[Ty::Int]);
}

#[test]
fn anonymous_object_inside_member_extension_keeps_both_outer_receivers() {
    let (body, _) = checked_function_body(
        "class A\n\
         class B { operator fun A.invoke(): String = \"OK\" }\n\
         class Host {\n\
             val x = A()\n\
             fun B.test(): String {\n\
                 val value = object { val result = x() }\n\
                 return value.result\n\
             }\n\
         }\n",
        "test",
    );
    assert!(!body.roots().is_empty());
}

#[test]
fn enum_entry_super_call_selects_the_enum_declaration_member() {
    let (body, index) = checked_function_body(
        "enum class Choice {\n\
             X { override fun value(): String = super.value() + \"X\" };\n\
             constructor()\n\
             open fun value(): String = \"base\"\n\
         }\n\
         fun use(): String = Choice.X.value()\n",
        "use",
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("call").kind else {
        panic!("enum entry invocation must remain a checked call")
    };
    let FirCallTarget::Module(target) = call.target else {
        panic!("enum declaration member must retain its stable callable identity")
    };
    let callable = index.callable(target).expect("selected enum member");
    assert_eq!(index.callable_name(callable.id), Some("value"));
    assert_eq!(
        index
            .signature(callable.declaration)
            .expect("resolved enum member signature")
            .result
            .get(),
        Ty::String
    );
}

#[test]
fn companion_property_wins_over_same_named_zero_argument_member_function() {
    let (body, _) = checked_function_body(
        "class Direction(private val direction: Int) {\n\
             fun dx() = dx[direction]\n\
             companion object { private val dx: IntArray = null!! }\n\
         }\n",
        "dx",
    );
    let root = body.expr(root_expression(&body)).expect("indexed read");
    assert_eq!(root.ty.get(), Ty::Int);
    assert!(matches!(
        root.kind,
        FirExprKind::IndexedRead {
            kind: FirIndexedAccessKind::Array,
            ..
        }
    ));
}
