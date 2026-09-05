use super::test_support::{
    checked_function_body, checked_function_body_with_features,
    checked_function_body_with_platform, jvm_semantics, jvm_stdlib_semantics, root_expression,
};
use super::*;
use crate::fir::{FirInlineBodyPlan, FirInlineDefaultValue};

#[test]
fn legacy_context_receiver_supplies_an_unqualified_extension_call() {
    let source = "// LANGUAGE: +ContextReceivers\n\
                  class A\n\
                  class B\n\
                  fun B.extensionFunction() {}\n\
                  context(A, B)\n\
                  fun test() { extensionFunction() }\n";
    let features = crate::features::LangFeatures::from_source(source);
    let (body, index) = checked_function_body_with_features(source, "test", &features);
    let callable = index
        .callable(crate::fir::CallableId::from_raw(body.owner().raw()))
        .expect("test callable header");
    assert_eq!(callable.shape.context_parameter_count, 2);
    assert_eq!(callable.shape.context_value_count, 0);
    let call = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::Call(call) = &expression.kind else {
            return None;
        };
        call.extension_receiver.is_some().then_some(call)
    });
    let call = call.expect("extension call checked FIR");
    let receiver = call.extension_receiver.as_ref().expect("context receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn explicit_integer_literal_unary_call_uses_nullable_expected_scalar_type() {
    let (body, _) = checked_function_body("fun box(): Byte? = 1.unaryMinus()", "box");
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("checked unary convention call")
            .ty
            .get(),
        Ty::nullable(Ty::Byte),
    );
}

#[test]
fn projected_map_get_receiver_can_widen_its_key_for_a_nullable_argument() {
    let (body, _) = checked_function_body_with_platform(
        "fun lookup(values: Map<String, String>, key: String?): String? = values[key]\n",
        "lookup",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("Map.get call")
        .kind
    else {
        panic!("Map.get must become a stable checked FIR call")
    };
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn primitive_mod_selects_the_stdlib_extension_as_a_checked_call() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\nfun calculate(left: Double, right: Double): Double = left.mod(right)\n",
        "calculate",
        jvm_stdlib_semantics(),
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("mod call expression");
    let FirExprKind::Call(call) = &expression.kind else {
        panic!("stdlib mod must remain an ordinary checked call")
    };
    assert_eq!(expression.ty.get(), Ty::Double);
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn boolean_not_member_is_a_checked_semantic_unary_operation() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\nfun invert(value: Boolean): Boolean = value.not()\n",
        "invert",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Unary {
        operation: FirUnaryOperation::BooleanNot,
        operand,
    } = &body
        .expr(root_expression(&body))
        .expect("Boolean.not expression")
        .kind
    else {
        panic!("Boolean.not must become a semantic checked unary operation")
    };
    assert_eq!(
        body.expr(*operand).expect("Boolean.not operand").ty.get(),
        Ty::Boolean,
    );
}

#[test]
fn primitive_unary_member_publishes_selected_operation_in_fir() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\nfun promote(value: Byte): Int = value.unaryPlus()\n",
        "promote",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Unary {
        operation: FirUnaryOperation::Identity,
        operand,
    } = body
        .expr(root_expression(&body))
        .expect("Byte.unaryPlus expression")
        .kind
    else {
        panic!("Byte.unaryPlus must become a checked primitive identity operation")
    };
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("selected unary result")
            .ty
            .get(),
        Ty::Int,
    );
    assert_eq!(
        body.expr(operand)
            .expect("source Byte operand")
            .ty
            .get()
            .canonical_semantic(),
        Ty::Byte,
    );
}
use crate::fir::DeclarationFlags;

#[test]
fn member_extension_compare_operator_keeps_both_receivers() {
    let (body, index) = checked_function_body(
        "class C {\n\
             operator fun Int.compareTo(other: Char): Int = 0\n\
             fun compare(left: Int, right: Char): Boolean = left < right\n\
         }\n",
        "compare",
    );
    let FirExprKind::ComparisonCall { call, .. } = &body
        .expr(root_expression(&body))
        .expect("comparison call")
        .kind
    else {
        panic!("member-extension comparison must retain its checked call")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn source_property_getter_abi_name_does_not_shadow_a_member_extension() {
    let (body, index) = checked_function_body(
        "class Request(val path: String)\n\
         class Handler {\n\
             fun Request.getPath(): String = path\n\
             fun test(request: Request): String = request.getPath()\n\
         }\n",
        "test",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("member-extension call")
        .kind
    else {
        panic!("member extension must become checked call FIR")
    };
    let target = call
        .target
        .module()
        .expect("stable source member extension");
    assert_eq!(index.callable_name(target), Some("getPath"));
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_some());
}

#[test]
fn generated_data_class_object_methods_have_stable_fir_targets() {
    let (body, index) = checked_function_body(
        "open class Base {\n\
             override fun toString(): String = \"base\"\n\
             override fun hashCode(): Int = 0\n\
             override fun equals(other: Any?): Boolean = false\n\
         }\n\
         data class Data(val value: String) : Base()\n\
         fun use(value: Data) {\n\
             value.toString()\n\
             value.hashCode()\n\
             value.equals(value)\n\
         }\n",
        "use",
    );

    for expected in ["toString", "hashCode", "equals"] {
        let target = (0..body.expression_count())
            .find_map(|raw| {
                let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind
                else {
                    return None;
                };
                let target = call.target.module()?;
                (index.callable_name(target) == Some(expected)).then_some(target)
            })
            .unwrap_or_else(|| panic!("missing generated {expected} call"));
        let declaration = index
            .callable(target)
            .expect("stable callable header")
            .declaration;
        assert!(
            index
                .declaration_header(declaration)
                .expect("stable declaration header")
                .flags
                .has(DeclarationFlags::COMPILER_GENERATED),
            "{expected} must bind to the generated data-class declaration",
        );
    }
}

#[test]
fn data_class_inherits_final_object_method_instead_of_publishing_a_generated_override() {
    let (body, index) = checked_function_body(
        "abstract class Base { final override fun toString(): String = \"kept\" }\n\
         data class Data(val value: String) : Base()\n\
         fun use(value: Data): String = value.toString()\n",
        "use",
    );

    let target = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("toString")).then_some(target)
        })
        .expect("inherited final toString call");
    let declaration = index.callable(target).expect("stable callable").declaration;
    let header = index
        .declaration_header(declaration)
        .expect("stable declaration header");
    assert!(!header.flags.has(DeclarationFlags::COMPILER_GENERATED));
    let owner = header.owner.expect("member owner");
    assert_eq!(index.declaration_name(owner), Some("Base"));
}

#[test]
fn subtype_owned_override_wins_when_its_supertype_is_also_a_direct_parent() {
    let (body, index) = checked_function_body(
        "interface Contract { fun run(value: String = \"OK\"): Unit }\n\
         open class Implementation : Contract { override fun run(value: String) {} }\n\
         class Combined : Implementation(), Contract\n\
         fun use(): Unit = Combined().run()\n",
        "use",
    );

    let target = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("run")).then_some(target)
        })
        .expect("the inherited override call must have one stable target");
    let declaration = index.callable(target).expect("stable callable").declaration;
    let owner = index
        .declaration_header(declaration)
        .and_then(|header| header.owner)
        .expect("member owner");
    assert_eq!(index.declaration_name(owner), Some("Implementation"));
}

#[test]
fn unrelated_abstract_members_with_the_same_slot_form_one_fake_override() {
    let (body, index) = checked_function_body(
        "interface Left { fun run(): String }\n\
         interface Right { fun run(): String }\n\
         interface Combined : Left, Right\n\
         fun use(value: Combined): String = value.run()\n",
        "use",
    );

    let targets = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("run")).then_some(target)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        targets.len(),
        1,
        "the inherited slot must have one stable target"
    );
}

#[test]
fn delegated_implementation_inherits_a_sibling_interfaces_default() {
    let (body, index) = checked_function_body(
        "interface Body { fun run(value: String): String = value }\n\
         interface Contract { fun run(value: String = \"OK\"): String }\n\
         class Combined(body: Body) : Body by body, Contract\n\
         fun use(value: Combined): String = value.run()\n",
        "use",
    );

    let target = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("run")).then_some(target)
        })
        .expect("the delegated fake override must remain callable with the inherited default");
    assert!(index.callable(target).is_some());
}

#[test]
fn nominally_narrower_generic_bound_wins_overload_specificity() {
    let (body, index) = checked_function_body(
        "interface Root\n\
         class Leaf : Root\n\
         open class Base {\n\
             fun <T : Root> pick(value: T): String = \"root\"\n\
             open fun <T : Leaf> pick(value: T): String = \"leaf\"\n\
         }\n\
         class Derived : Base() {\n\
             override fun <T : Leaf> pick(value: T): String = \"selected\"\n\
         }\n\
         fun use(): String = Derived().pick(Leaf())\n",
        "use",
    );

    let target = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("pick")).then_some(target)
        })
        .expect("the narrower generic overload must have one stable target");
    assert!(index.callable(target).is_some());
}

#[test]
fn structurally_functional_generic_overload_wins_for_untyped_lambda() {
    let (body, index) = checked_function_body(
        "fun <T : Any> choose(value: T): String = \"value\"\n\
         fun <T : Any> choose(value: () -> T): String = value() as String\n\
         fun use(): String = choose { \"OK\" }\n",
        "use",
    );

    let target = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("choose")).then_some(target)
        })
        .expect("the function-shaped overload must have one stable target");
    assert!(index.callable(target).is_some());
}

#[test]
fn unqualified_interface_super_call_ignores_abstract_sibling_slot() {
    let (body, index) = checked_function_body(
        "interface Root { fun test(): String = \"OK\" }\n\
         interface Concrete : Root { override fun test(): String = super.test() }\n\
         interface AbstractSibling : Root { override fun test(): String }\n\
         interface Diamond : Concrete, AbstractSibling {\n\
             override fun test(): String = super.test()\n\
         }\n\
         fun use(value: Diamond): String = value.test()\n",
        "use",
    );

    let target = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("test")).then_some(target)
        })
        .expect("the diamond call must retain one stable target");
    assert!(index.callable(target).is_some());
}

#[test]
fn generated_data_class_copy_keeps_named_and_default_parameter_mapping() {
    let (body, index) = checked_function_body(
        "data class Pair(val left: Int, val right: String)\n\
         fun use(value: Pair): Pair = value.copy(right = \"changed\")\n",
        "use",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("generated copy call")
        .kind
    else {
        panic!("generated data-class copy must become checked call FIR")
    };
    let target = call.target.module().expect("stable generated copy target");
    assert_eq!(index.callable_name(target), Some("copy"));
    assert!(matches!(
        call.arguments.as_ref(),
        [
            FirCallArgument::Expression { parameter: 1, .. },
            FirCallArgument::Default { parameter: 0, .. },
        ]
    ));
}

#[test]
fn generated_data_class_members_may_implement_interface_obligations_needing_backend_bridges() {
    let (body, index) = checked_function_body(
        "interface Contract {\n\
             fun copy(value: Int): Contract\n\
             fun component1(): Any\n\
         }\n\
         data class Data(val value: Int) : Contract\n\
         fun use(value: Contract) {\n\
             value.copy(1)\n\
             value.component1()\n\
         }\n",
        "use",
    );

    for expected in ["copy", "component1"] {
        let target = (0..body.expression_count())
            .find_map(|raw| {
                let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind
                else {
                    return None;
                };
                let target = call.target.module()?;
                (index.callable_name(target) == Some(expected)).then_some(target)
            })
            .unwrap_or_else(|| panic!("missing checked {expected} interface call"));
        assert!(index.callable(target).is_some());
    }
}

#[test]
fn function_values_select_any_members_through_the_ordinary_member_rung() {
    let (body, _) = checked_function_body_with_platform(
        "fun use(value: (Any) -> String): Boolean = value.equals(value)\n",
        "use",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("function-value member call")
        .kind
    else {
        panic!("function-value equals must become checked call FIR")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn source_classifier_without_explicit_parent_inherits_any_members() {
    let (body, _) = checked_function_body_with_platform(
        "class Plain\n\
         fun use(value: Plain): Boolean = value.equals(value)\n",
        "use",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("inherited Any member call")
        .kind
    else {
        panic!("source-class equals must become checked call FIR")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn inapplicable_near_extension_falls_through_to_an_imported_operator_rung() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         operator fun String.rangeTo(right: Int): ClosedRange<String> = this..this\n",
        "rangeTo",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("imported generic range operator")
        .kind
    else {
        panic!("range syntax must retain its selected checked call")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
}

#[test]
fn callable_reference_constraints_contextualize_earlier_integer_literals() {
    let (body, _) = checked_function_body(
        "fun <T, R> choose(x: T, y: T, use: (T?, T) -> R?): R? = use(x, y)\n\
         fun bytes(x: Byte?, y: Byte): String? = \"OK\"\n\
         fun run(): String? = choose(0, 1, ::bytes)\n",
        "run",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("generic call")
        .kind
    else {
        panic!("generic call must become checked call FIR")
    };
    for argument in &call.arguments[..2] {
        let FirCallArgument::Expression { value, .. } = argument else {
            panic!("integer literal must remain an explicit call argument")
        };
        assert_eq!(
            body.expr(*value).expect("integer literal FIR").ty.get(),
            Ty::Byte
        );
    }
    assert!(call
        .substitutions
        .iter()
        .any(|substitution| substitution.value.get() == Ty::Byte));
}

#[test]
fn integer_literal_constraint_selects_one_overloaded_suspend_callable_reference() {
    let (body, index) = checked_function_body(
        "fun <T> choose(value: T, use: suspend (T) -> Int): Int = 0\n\
         fun target(value: Int): Int = 1\n\
         fun target(value: String): Int = 2\n\
         fun run(): Int = choose(42, ::target)\n",
        "run",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("generic call")
        .kind
    else {
        panic!("generic call must become checked call FIR")
    };
    let FirCallArgument::Expression { value, .. } = &call.arguments[1] else {
        panic!("callable reference must remain an explicit argument")
    };
    let FirExprKind::CallableReference { target, .. } = &body
        .expr(*value)
        .expect("checked callable-reference argument")
        .kind
    else {
        panic!("selected overload must become checked callable-reference FIR")
    };
    let callable = target.module().expect("source target callable");
    let declaration = index
        .callable(callable)
        .expect("selected target header")
        .declaration;
    let signature = index
        .signature(declaration)
        .expect("selected target signature");
    assert_eq!(signature.parameters[0].get(), Ty::Int);
    assert!(call
        .substitutions
        .iter()
        .any(|substitution| substitution.value.get() == Ty::Int));
}

#[test]
fn integer_literals_follow_a_selected_type_parameters_scalar_bound() {
    let (body, _) = checked_function_body(
        "fun <T : UByte> keep(value: T): T = value\n\
         fun run(): UByte = keep(1u)\n",
        "run",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("generic bounded call")
        .kind
    else {
        panic!("generic bounded call must become checked call FIR")
    };
    let [FirCallArgument::Expression { value, .. }] = call.arguments.as_ref() else {
        panic!("bounded literal must remain the explicit argument")
    };
    assert_eq!(
        body.expr(*value).expect("unsigned literal FIR").ty.get(),
        Ty::UByte
    );
    assert!(call
        .substitutions
        .iter()
        .any(|substitution| substitution.value.get() == Ty::UByte));
}

#[test]
fn unary_member_extension_operator_keeps_both_receivers() {
    let (body, index) = checked_function_body(
        "class Context { operator fun Any.not(): String = \"OK\" }\n\
         fun receive(context: Context, block: Context.() -> String): String = context.block()\n\
         fun run(value: Any): String = receive(Context()) { !value }\n",
        "run",
    );
    let nested = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Lambda { body, .. } =
                &body.expr(FirExprId::from_raw(raw as u32))?.kind
            else {
                return None;
            };
            Some(body)
        })
        .expect("receiver lambda");
    let call = (0..nested.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &nested.expr(FirExprId::from_raw(raw as u32))?.kind
            else {
                return None;
            };
            let target = call.target.module()?;
            (index
                .callable(target)
                .and_then(|callable| index.callable_name(callable.id))
                == Some("not"))
            .then_some(call)
        })
        .expect("unary member-extension call");
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_some());
    assert!(call.arguments.is_empty());
}

#[test]
fn top_level_primitive_compare_extension_is_selected_when_members_are_inapplicable() {
    let (body, index) = checked_function_body(
        "operator fun Int.compareTo(other: Char): Int = 0\n\
         fun compare(left: Int, right: Char): Boolean = left < right\n",
        "compare",
    );
    let FirExprKind::ComparisonCall { call, .. } = &body
        .expr(root_expression(&body))
        .expect("comparison call")
        .kind
    else {
        panic!("top-level extension comparison must retain its checked call")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn private_top_level_nullable_compare_extension_is_visible_in_its_file() {
    let (body, index) = checked_function_body(
        "private operator fun Long?.compareTo(other: Long?): Int = 0\n\
         fun compare(left: Long?, right: Long?): Boolean = left < right\n",
        "compare",
    );
    let FirExprKind::ComparisonCall { call, .. } = &body
        .expr(root_expression(&body))
        .expect("comparison call")
        .kind
    else {
        panic!("private same-file extension comparison must retain its checked call")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn unsigned_relational_operator_keeps_the_selected_value_class_member_call() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun compare(left: UInt, right: UInt): Boolean = left < right\n",
        "compare",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::ComparisonCall {
        operation: FirBinaryOperation::Less,
        call,
    } = &body
        .expr(root_expression(&body))
        .expect("unsigned comparison call")
        .kind
    else {
        panic!("unsigned comparison must retain its selected compareTo call")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn explicit_unsigned_equals_is_an_ordinary_selected_member_call() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun equal(left: UInt, right: UInt): Boolean = left.equals(right)\n",
        "equal",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("checked equals call")
        .kind
    else {
        panic!("value-class ABI must not leak into checked FIR")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn explicit_builtin_range_to_is_the_same_checked_range_operation_as_operator_syntax() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun range(): IntRange = 1.rangeTo(3)\n",
        "range",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Range {
        operation: FirRangeOperation::Through,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("explicit rangeTo expression")
        .kind
    else {
        panic!("the selected builtin rangeTo declaration must publish checked range FIR")
    };
}

#[test]
fn implicit_receiver_builtin_range_to_publishes_checked_range_operation() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun Int.range(end: Int): IntRange = rangeTo(end)\n",
        "range",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Range {
        operation: FirRangeOperation::Through,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("implicit receiver rangeTo expression")
        .kind
    else {
        panic!("implicit receiver rangeTo must publish checked range FIR")
    };
}

#[test]
fn implicit_receiver_primitive_intrinsic_uses_the_same_checked_operation_as_explicit_call() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun Int.shift(count: Int): Int = shl(count)\n",
        "shift",
        jvm_stdlib_semantics(),
    );
    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::Binary {
            operation: FirBinaryOperation::ShiftLeft,
            ..
        })
    ));
}

#[test]
fn core_primitive_infix_member_is_a_checked_binary_operation() {
    let (body, _) = checked_function_body(
        "fun shift(value: Int, count: Int): Int = value shl count\n",
        "shift",
    );
    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::Binary {
            operation: FirBinaryOperation::ShiftLeft,
            ..
        })
    ));
}

#[test]
fn unsigned_arithmetic_keeps_its_semantic_type_when_range_to_supplies_an_expectation() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun range(): UIntRange = (1u + 2u)..(6u - 1u)\n",
        "range",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(range) = &body
        .expr(root_expression(&body))
        .expect("unsigned range call")
        .kind
    else {
        panic!("unsigned range syntax must retain the selected rangeTo call")
    };
    assert!(matches!(range.target, FirCallTarget::External { .. }));
    assert!(range.dispatch_receiver.is_some());
    assert!(range.extension_receiver.is_none());
    assert_eq!(range.arguments.len(), 1);
    let receiver = range.dispatch_receiver.as_ref().expect("range start").value;
    let FirExprKind::Call(start) = &body.expr(receiver).expect("unsigned plus call").kind else {
        panic!("unsigned plus must retain its selected member call")
    };
    assert!(matches!(start.target, FirCallTarget::External { .. }));
    let FirCallArgument::Expression { value: end, .. } = &range.arguments[0] else {
        panic!("range end must remain an explicit checked argument")
    };
    let FirExprKind::Call(end) = &body.expr(*end).expect("unsigned minus call").kind else {
        panic!("unsigned minus must retain its selected member call")
    };
    assert!(matches!(end.target, FirCallTarget::External { .. }));
}

#[test]
fn inferred_generic_member_result_keeps_the_selected_method_substitution() {
    let _ = checked_function_body(
        "class Value<T>(val value: T)\n\
         class Reader { fun <T> read(input: Value<T>) = input }\n\
         fun result(): String = Reader().read(Value(\"OK\")).value\n",
        "result",
    );
}

#[test]
fn builtin_scalar_compare_to_publishes_its_selected_common_operand() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun compare(left: Int, right: Double): Int = left.compareTo(right)\n",
        "compare",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("primitive compareTo call")
        .kind
    else {
        panic!("primitive compareTo must become an intrinsic checked call")
    };
    let FirCallTarget::Intrinsic {
        operation: FirIntrinsic::PrimitiveCompare { operand },
        receiver: Some(receiver),
        parameters,
        result,
    } = &call.target
    else {
        panic!("primitive compareTo must retain its exact compiler realization")
    };
    assert_eq!(operand.get(), Ty::Double);
    assert_eq!(receiver.get(), Ty::Double);
    assert_eq!(parameters.as_ref(), [ResolvedTy::new(Ty::Double).unwrap()]);
    assert_eq!(result.get(), Ty::Int);
    assert!(matches!(
        call.dispatch_receiver,
        Some(FirReceiver {
            conversion: Some(FirConversion {
                kind: FirConversionKind::NumericWidening { to },
                ..
            }),
            ..
        }) if to.get() == Ty::Double
    ));
}

#[test]
fn unintercepted_coroutine_primitive_is_a_checked_fir_intrinsic() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         import kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn\n\
         suspend fun awaitInternal(): Any? =\n\
             suspendCoroutineUninterceptedOrReturn { continuation -> Unit }\n",
        "awaitInternal",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("coroutine primitive call")
        .kind
    else {
        panic!("the selected coroutine primitive must remain an explicit checked call")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::SuspendCoroutineUninterceptedOrReturn,
            receiver: None,
            ..
        }
    ));
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn safe_coroutine_primitive_is_a_distinct_checked_fir_intrinsic() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         import kotlin.coroutines.suspendCoroutine\n\
         suspend fun await(): String = suspendCoroutine { continuation ->\n\
             continuation.resumeWith(Result.success(\"OK\"))\n\
         }\n",
        "await",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("safe coroutine primitive call")
        .kind
    else {
        panic!("the selected safe coroutine primitive must remain an explicit checked call")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::SuspendCoroutine,
            receiver: None,
            ..
        }
    ));
}

#[test]
fn kotlin_assert_is_a_checked_lazy_fir_intrinsic() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun verify(value: Boolean): Unit = assert(value)\n",
        "verify",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("assert call").kind
    else {
        panic!("the selected kotlin.assert declaration must remain an explicit checked call")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::Assert {
                mode: crate::types::AssertionMode::Runtime,
            },
            receiver: None,
            result,
            ..
        } if result.get() == Ty::Unit
    ));
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn nullable_string_plus_is_a_checked_fir_intrinsic() {
    let (body, _) = checked_function_body(
        "fun concat(left: String?, right: String?): String = left + right\n",
        "concat",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("nullable String.plus call")
        .kind
    else {
        panic!("nullable String.plus must remain an explicit checked call")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::StringPlus,
            receiver: Some(receiver),
            result,
            ..
        } if receiver.get() == Ty::nullable(Ty::String) && result.get() == Ty::String
    ));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn selected_trim_indent_constant_is_folded_into_checked_fir_code_units() {
    let (body, _) = checked_function_body(
        "fun box(): String = \"\"\"${'\\uD800'}x\"\"\".trimIndent()\n",
        "box",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("checked trimIndent result");
    let FirExprKind::Constant(FirConstant::String(value)) = &root.kind else {
        panic!("the selected intrinsic on a constant receiver must be checked as a constant")
    };
    assert_eq!(value.units().collect::<Vec<_>>(), [0xd800, b'x' as u16]);
}

#[test]
fn same_named_source_trim_indent_remains_a_checked_call() {
    let (body, _) = checked_function_body(
        "fun String.trimIndent(): String = \"source\"\n\
         fun box(): String = \"literal\".trimIndent()\n",
        "box",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("checked source extension call");
    assert!(matches!(
        root.kind,
        FirExprKind::Call(FirCall {
            target: FirCallTarget::Module(_),
            ..
        })
    ));
}

#[test]
fn builtin_scalar_relational_operator_compares_the_intrinsic_result_to_zero() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun greater(left: Int, right: Int): Boolean = left > right\n",
        "greater",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::ComparisonCall {
        operation: FirBinaryOperation::Greater,
        call,
    } = &body
        .expr(root_expression(&body))
        .expect("primitive relational call")
        .kind
    else {
        panic!("primitive relational operator must retain compareTo semantics")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::PrimitiveCompare { operand },
            ..
        } if operand.get() == Ty::Int
    ));
}

#[test]
fn char_minus_int_keeps_its_char_result_inside_a_relational_operand() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun compare(value: Char): Boolean = (value - 1) <= value\n",
        "compare",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::ComparisonCall { call, .. } = &body
        .expr(root_expression(&body))
        .expect("Char relational expression")
        .kind
    else {
        panic!("Char relation must retain its selected compareTo call")
    };
    let receiver = call
        .dispatch_receiver
        .as_ref()
        .expect("comparison receiver");
    let subtraction = body.expr(receiver.value).expect("subtraction receiver");
    assert_eq!(subtraction.ty.get(), Ty::Char);
    assert!(matches!(
        subtraction.kind,
        FirExprKind::Binary {
            operation: FirBinaryOperation::Subtract,
            ..
        }
    ));
}

#[test]
fn local_char_minus_int_keeps_its_char_result_inside_a_relational_operand() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun compare(): Boolean {\n\
             if ('z' - 'a' != 25) return false\n\
             val value: Char = Char.MIN_VALUE\n\
             return (value - 1) <= value\n\
         }\n",
        "compare",
        jvm_stdlib_semantics(),
    );
    let subtraction_types = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(
                expression.kind,
                FirExprKind::Binary {
                    operation: FirBinaryOperation::Subtract,
                    ..
                }
            )
            .then_some(expression.ty.get())
        })
        .collect::<Vec<_>>();
    assert_eq!(subtraction_types, vec![Ty::Int, Ty::Char]);
}

#[test]
fn builtin_floating_relational_operator_keeps_ieee_ordering() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun less(left: Double, right: Double): Boolean = left < right\n",
        "less",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Binary {
        operation: FirBinaryOperation::Less,
        lhs,
        rhs,
    } = &body
        .expr(root_expression(&body))
        .expect("primitive floating relation")
        .kind
    else {
        panic!("primitive floating relation must remain an IEEE checked FIR operation")
    };
    assert_eq!(body.expr(*lhs).expect("left operand").ty.get(), Ty::Double);
    assert_eq!(body.expr(*rhs).expect("right operand").ty.get(), Ty::Double);
}

#[test]
fn smartcast_primitive_binary_receiver_keeps_its_unbox_boundary() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun increment(value: Int?): Int? = if (value == null) null else value + 1\n",
        "increment",
        jvm_stdlib_semantics(),
    );
    let lhs = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            match expression.kind {
                FirExprKind::Binary {
                    operation: FirBinaryOperation::Add,
                    lhs,
                    ..
                } => Some(lhs),
                _ => None,
            }
        })
        .expect("checked primitive addition");
    assert!(matches!(
        body.expr(lhs).expect("converted left operand").kind,
        FirExprKind::ImplicitConversion {
            conversion: FirConversion {
                kind: FirConversionKind::SmartCast { to },
                ..
            },
            ..
        } if to.get() == Ty::Int
    ));
}

#[test]
fn class_constructor_precedes_its_companion_value_during_signature_solving() {
    let (body, _) = checked_function_body(
        "fun <T> eval(block: () -> T): T = block()\n\
         class Outer {\n\
             private companion object { val result = \"OK\" }\n\
             class Nested { fun read() = eval { result } }\n\
             fun test() = Nested().read()\n\
         }\n\
         fun box() = Outer().test()\n",
        "box",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("resolved box expression")
            .ty
            .get(),
        Ty::String,
    );
}

#[test]
fn companion_extension_signature_reads_provider_constant_from_implicit_receiver() {
    let (body, _) = checked_function_body_with_platform(
        "fun Int.Companion.maximum() = MAX_VALUE\n\
         fun box(): Int = Int.maximum()\n",
        "box",
        jvm_stdlib_semantics(),
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("resolved maximum call")
            .ty
            .get(),
        Ty::Int,
    );
}

#[test]
fn companion_extension_body_folds_provider_constant_from_implicit_receiver() {
    let (body, _) = checked_function_body_with_platform(
        "fun Int.Companion.maximum(): Int = MAX_VALUE\n",
        "maximum",
        jvm_stdlib_semantics(),
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("checked companion constant read");
    assert_eq!(expression.ty.get(), Ty::Int);
    assert!(matches!(
        expression.kind,
        FirExprKind::Constant(FirConstant::Int(value)) if value == i64::from(i32::MAX)
    ));
}

#[test]
fn generic_call_solves_mixed_lower_constraints_with_its_declared_bound() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T : Number> first(vararg values: T): T = values[0]\n\
         fun choose(): Number = first(1, 4.5)\n",
        "choose",
        jvm_semantics(),
    );
    let root = body.expr(root_expression(&body)).expect("generic call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("mixed numeric lower constraints must retain the selected generic call")
    };
    assert_eq!(root.ty.get(), Ty::obj("kotlin/Number"));
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::obj("kotlin/Number"));
}

#[test]
fn postponed_generic_result_inside_an_input_projection_is_fully_solved() {
    let (body, index) = checked_function_body(
        "class Inv<T>\n\
         fun <T : V, U : V, V> accept(x: T, y: Inv<in U>) {}\n\
         fun <E> materialize(): Inv<in Inv<E>?> = Inv()\n\
         fun test(inv: Inv<Int>) { accept(inv, materialize()) }\n",
        "test",
    );
    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            Some((index.callable_name(target)?, call))
        })
        .collect::<Vec<_>>();
    let accept = calls
        .iter()
        .find_map(|(name, call)| (*name == "accept").then_some(*call))
        .expect("outer generic call");
    assert_eq!(accept.substitutions.len(), 3);
    assert!(accept
        .substitutions
        .iter()
        .all(|substitution| !substitution.value.get().mentions_ty_param()));
    let materialize = calls
        .iter()
        .find_map(|(name, call)| (*name == "materialize").then_some(*call))
        .expect("postponed nested call");
    let [element] = materialize.substitutions.as_ref() else {
        panic!("the nested call must publish its completed type argument")
    };
    assert_eq!(element.value.get(), Ty::Nothing);
}

#[test]
fn covariant_star_argument_infers_its_readable_bound_for_a_generic_call() {
    let (body, index) = checked_function_body_with_platform(
        "interface A<out T : Any>\n\
         fun <E : Any> bar(value: A<E>): E = TODO()\n\
         fun use(value: A<*>): Any = bar(value)\n",
        "use",
        jvm_stdlib_semantics(),
    );
    let expression = body.expr(root_expression(&body)).expect("generic call");
    let FirExprKind::Call(call) = &expression.kind else {
        panic!("bar must become a checked FIR call")
    };
    let target = call.target.module().expect("source call target");
    assert_eq!(index.callable_name(target), Some("bar"));
    assert_eq!(expression.ty.get(), Ty::obj("kotlin/Any"));
    let [substitution] = call.substitutions.as_ref() else {
        panic!("bar must publish its inferred E")
    };
    assert_eq!(substitution.value.get(), Ty::obj("kotlin/Any"));
}

#[test]
fn return_only_member_formal_reads_a_recursive_star_capture_before_fir() {
    let (body, index) = checked_function_body(
        "interface Traversable\n\
         interface Entity<S : Entity<S>> : Traversable { fun <T : S> value(): T }\n\
         interface Path : Traversable\n\
         fun use(path: Path, entity: Entity<*>) {\n\
             if (true) path else entity.value()\n\
             return\n\
         }\n",
        "use",
    );
    let (expression, call) = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("value")).then_some((expression, call))
        })
        .expect("captured-result member call");
    let expected = Ty::obj_args(
        "Entity",
        &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
    );
    assert_eq!(expression.ty.get(), expected);
    let [substitution] = call.substitutions.as_ref() else {
        panic!("the selected method must publish its recursive star capture")
    };
    assert_eq!(substitution.value.get(), expected);
    assert!(!substitution.value.get().mentions_ty_param());
    assert!(!substitution.value.get().mentions_pending());
}

#[test]
fn recursive_generic_extension_call_publishes_identity_substitutions() {
    let (body, _) = checked_function_body_with_platform(
        "tailrec suspend fun <T, A> Iterator<T>.foldl(\n\
             acc: A,\n\
             foldFunction: (T, A) -> A\n\
         ): A = if (!hasNext()) acc else foldl(foldFunction(next(), acc), foldFunction)\n",
        "foldl",
        jvm_stdlib_semantics(),
    );

    let recursive = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| {
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            (call.extension_receiver.is_some() && call.substitutions.len() == 2)
                .then_some((expression, call))
        })
        .expect("recursive foldl call must retain its checked identity instantiation");
    assert!(matches!(recursive.0.ty.get(), Ty::TyParam(..)));
    assert!(recursive
        .1
        .substitutions
        .iter()
        .all(|substitution| matches!(substitution.value.get(), Ty::TyParam(..))));
}

#[test]
fn recursive_vararg_lambda_result_accepts_a_lexical_identity_substitution() {
    let (body, _) = checked_function_body_with_platform(
        "tailrec fun <T> recur(vararg items: String, produce: (String) -> T): T =\n\
             if (items.size == 0) produce(\"\") else recur(*items) {\n\
                 produce(items[0])\n\
             }\n",
        "recur",
        jvm_stdlib_semantics(),
    );

    let recursive = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| {
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            (call.substitutions.len() == 1 && matches!(expression.ty.get(), Ty::TyParam(..)))
                .then_some(call)
        })
        .expect("recursive call must retain its lexical type argument");
    assert!(matches!(
        recursive.substitutions[0].value.get(),
        Ty::TyParam(..)
    ));
}

#[test]
fn expected_generic_extension_result_shapes_explicit_lambda_parameters() {
    let (body, _) = checked_function_body_with_platform(
        "suspend fun <T, A> Iterator<T>.foldl(\n\
             acc: A,\n\
             foldFunction: (T, A) -> A\n\
         ): A = if (!hasNext()) acc else foldl(foldFunction(next(), acc), foldFunction)\n\
         suspend fun total(values: Iterator<Int>): Long =\n\
             values.foldl(0) { element: Int, acc: Long -> acc + element }\n",
        "total",
        jvm_stdlib_semantics(),
    );

    let root = body
        .expr(root_expression(&body))
        .expect("contextual foldl call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("foldl must remain a checked extension call")
    };
    assert_eq!(root.ty.get(), Ty::Long);
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(call.substitutions[0].value.get(), Ty::Int);
    assert_eq!(call.substitutions[1].value.get(), Ty::Long);
}

#[test]
fn explicit_vararg_function_type_contextualizes_every_receiver_lambda() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         interface Stroke\n\
         interface Fill\n\
         data class Rectangle(val width: Int, val height: Int)\n\
         open class Ellipse\n\
         data class Circle(val radius: Int) : Ellipse()\n\
         interface Canvas {\n\
             fun rect(rectangle: Rectangle, fill: Fill)\n\
             fun rect(rectangle: Rectangle, stroke: Stroke, fill: Fill?)\n\
             fun rect(rectangle: Rectangle, radius: Double, fill: Fill)\n\
             fun rect(rectangle: Rectangle, radius: Double, stroke: Stroke, fill: Fill?)\n\
             fun circle(circle: Circle, fill: Fill)\n\
             fun circle(circle: Circle, stroke: Stroke, fill: Fill?)\n\
         }\n\
         fun use() {\n\
             val rect = Rectangle(100, 100)\n\
             val circle = Circle(100)\n\
             listOf<Canvas.(Stroke, Fill) -> Unit>(\n\
                 { _, fill -> rect(rect, fill) },\n\
                 { _, fill -> rect(rect, 1.0, fill) },\n\
                 { stroke, fill -> rect(rect, stroke, fill) },\n\
                 { stroke, fill -> rect(rect, 2.0, stroke, fill) },\n\
                 { _, fill -> circle(circle, fill) },\n\
                 { stroke, fill -> circle(circle, stroke, fill) },\n\
             ).forEach { check(it) }\n\
         }\n\
         fun test2() {\n\
             val rect = Rectangle(100, 100)\n\
             val circle = Circle(100)\n\
             val values: List<Canvas.(Stroke, Fill) -> Unit> = listOf(\n\
                 { _, fill -> rect(rect, fill) },\n\
                 { _, fill -> rect(rect, 1.0, fill) },\n\
                 { stroke, fill -> rect(rect, stroke, fill) },\n\
                 { stroke, fill -> rect(rect, 2.0, stroke, fill) },\n\
                 { _, fill -> circle(circle, fill) },\n\
                 { stroke, fill -> circle(circle, stroke, fill) },\n\
             )\n\
         }\n\
         fun check(block: Canvas.(Stroke, Fill) -> Unit) {}\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let receiver_lambdas = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Lambda { body, .. } if body.receiver_type().is_some() => Some(body),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receiver_lambdas.len(), 6);
    assert!(receiver_lambdas
        .iter()
        .all(|lambda| lambda.receiver_type().is_some()));
}

#[test]
fn unconstrained_generic_integer_literal_still_infers_int() {
    let (body, _) = checked_function_body(
        "fun <T> identity(value: T): T = value\n\
         fun use() = identity(0)\n",
        "use",
    );

    let root = body
        .expr(root_expression(&body))
        .expect("generic identity call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("identity must remain a checked generic call")
    };
    assert_eq!(root.ty.get(), Ty::Int);
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::Int);
}

#[test]
fn only_input_type_inference_uses_representable_integer_literal_types() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.test.assertEquals\n\
         fun use(short: Short, long: Long, byte: Byte) {\n\
             assertEquals(-1, short)\n\
             assertEquals(0, long)\n\
             assertEquals(1, byte)\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let mut inferred = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Call(call)
                if matches!(call.target, FirCallTarget::External { .. })
                    && call.substitutions.len() == 1 =>
            {
                Some(call.substitutions[0].value.get())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    inferred.sort_by_key(|ty| format!("{ty:?}"));

    let mut expected = vec![Ty::Short, Ty::Long, Ty::Byte];
    expected.sort_by_key(|ty| format!("{ty:?}"));
    assert_eq!(inferred, expected);
}

#[test]
fn conditional_numeric_lub_is_not_arithmetic_promotion() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.test.assertEquals\n\
         fun use() {\n\
             val value = when (true) {\n\
                 true -> 42\n\
                 else -> 1.0\n\
             }\n\
             assertEquals(42, value)\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let call = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::Call(call)
                if matches!(call.target, FirCallTarget::External { .. })
                    && call.substitutions.len() == 1 =>
            {
                Some(call)
            }
            _ => None,
        })
        .expect("assertEquals must remain an applicable checked call");
    assert_eq!(call.substitutions[0].value.get(), Ty::obj("kotlin/Any"));
}

#[test]
fn only_input_type_selection_contextualizes_a_nested_empty_generic_call() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.test.assertEquals\n\
         fun use(actual: List<String>) {\n\
             assertEquals(listOf(), actual)\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let substitutions = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Call(call) if call.substitutions.len() == 1 => {
                Some(call.substitutions[0].value.get())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(substitutions.contains(&Ty::String));
    assert!(substitutions.contains(&Ty::obj_args("kotlin/collections/List", &[Ty::String])));
}

#[test]
fn only_input_type_selection_does_not_rebind_input_constrained_nested_calls() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.test.assertEquals\n\
         open class Base\n\
         class Derived : Base()\n\
         fun use(values: List<Base>) {\n\
             assertEquals(listOf(Derived()), values.map { it })\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let substitutions = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Call(call) if call.substitutions.len() == 1 => {
                Some(call.substitutions[0].value.get())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(substitutions.contains(&Ty::obj_args("kotlin/collections/List", &[Ty::obj("Base")])));
}

#[test]
fn explicit_lambda_parameter_solves_dependent_reified_call_formals() {
    let (body, index) = checked_function_body(
        "inline fun <reified S : Service<S, E>, reified E : Event<S>> event(\n\
             noinline handler: suspend (E) -> Unit\n\
         ) {}\n\
         interface Service<Self : Service<Self, E>, in E : Event<Self>>\n\
         interface Event<out S : Service<out S, *>>\n\
         class SomeService : Service<SomeService, SomeService.SomeEvent> {\n\
             class SomeEvent : Event<SomeService>\n\
         }\n\
         fun use() { event { value: SomeService.SomeEvent -> } }\n",
        "use",
    );

    let call = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| {
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index
                .callable(target)
                .and_then(|callable| index.callable_name(callable.id))
                == Some("event"))
            .then_some(call)
        })
        .expect("event must remain a checked generic call");
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(call.substitutions[0].value.get(), Ty::obj("SomeService"));
    assert_eq!(
        call.substitutions[1].value.get(),
        Ty::obj("SomeService$SomeEvent")
    );
}

#[test]
fn nested_generic_call_constrains_source_constructor_before_erasure() {
    let (body, _) = checked_function_body_with_platform(
        "class IntArrays<T : Int>(val values: Array<T>)\n\
         fun first(): Int = IntArrays(arrayOf(1)).values[0]\n",
        "first",
        jvm_stdlib_semantics(),
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("indexed constructor result")
            .ty
            .get(),
        Ty::Int,
    );
}

#[test]
fn nested_generic_call_constrains_an_open_outer_parameter_before_contextual_recheck() {
    let (body, index) = checked_function_body_with_platform(
        "interface Convertible\n\
         class NumberValue : Convertible\n\
         fun <T : Convertible> consume(values: Array<T>): Int = values.size\n\
         fun read(): Int = consume(arrayOf(NumberValue(), NumberValue()))\n",
        "read",
        jvm_stdlib_semantics(),
    );

    let consume = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("consume")).then_some(call)
        })
        .expect("outer generic call must retain its checked target");
    assert_eq!(consume.substitutions.len(), 1);
    assert_eq!(consume.substitutions[0].value.get(), Ty::obj("NumberValue"));
    let [FirCallArgument::Expression { value, .. }] = consume.arguments.as_ref() else {
        panic!("consume must retain its nested array-producing call")
    };
    assert_eq!(
        body.expr(*value).expect("nested array call").ty.get(),
        Ty::obj_args("kotlin/Array", &[Ty::obj("NumberValue")])
    );
}

#[test]
fn underscored_type_arguments_publish_constraints_from_dependent_bounds() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +PartiallySpecifiedTypeArguments\n\
         interface Props\n\
         interface Component<P, S>\n\
         class MyProps<T> : Props\n\
         class MyComponent<T> : Component<MyProps<T>, Unit>\n\
         class Builder<P>\n\
         inline fun <P : Props, reified C : Component<P, *>> child(\n\
             noinline handler: Builder<P>.() -> Unit\n\
         ): String = \"OK\"\n\
         fun test(): String {\n\
             child<MyProps<String>, _> {}\n\
             return child<_, MyComponent<String>> {}\n\
         }\n",
        "test",
    );
    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("child")).then_some(call)
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call.substitutions.len() == 2));
    assert_eq!(
        calls[1].substitutions[0].value.get(),
        Ty::obj_args("MyProps", &[Ty::String])
    );
    assert_eq!(
        calls[1].substitutions[1].value.get(),
        Ty::obj_args("MyComponent", &[Ty::String])
    );
}

#[test]
fn bottom_inference_does_not_replace_equalitys_boolean_result_type() {
    let (body, _) = checked_function_body(
        "class Sink<in T>\n\
         fun <E> intersect(left: Sink<E>, right: Sink<E>): E = null as E\n\
         fun test(): Boolean {\n\
             val value = intersect(Sink<Int>(), Sink<String>())\n\
             return value == null && value == value\n\
         }\n",
        "test",
    );
    assert_eq!(
        body.result_type().expect("function result").get(),
        Ty::Boolean
    );
}

#[test]
fn unqualified_member_call_keeps_stable_target_and_receiver_coordinate() {
    let (body, index) = checked_function_body(
        "class Box { fun answer() = 42; fun read(): Int = answer() }\n",
        "read",
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("member call").kind
    else {
        panic!("unqualified member call must become checked call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    let receiver = call
        .dispatch_receiver
        .expect("unqualified member call needs its selected receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
    assert!(call.extension_receiver.is_none());
}

#[test]
fn top_level_extension_call_keeps_its_stable_target_and_extension_receiver() {
    let (body, index) = checked_function_body(
        "fun String.answer(): Int = length\nfun read(value: String): Int = value.answer()\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("extension call")
        .kind
    else {
        panic!("extension call must become checked call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    let receiver = call
        .extension_receiver
        .expect("extension call needs its value receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn companion_extension_calls_keep_associated_targets_without_runtime_receivers() {
    let (body, index) = checked_function_body(
        "class C\n\
         companion fun C.func(value: String): String = value\n\
         companion fun C.getOk(): String = \"OK\"\n\
         fun box(): String = C.func(C.getOk())\n",
        "box",
    );
    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            Some(call)
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for call in calls {
        let callable = call
            .target
            .module()
            .expect("source companion extension target");
        let declaration = index
            .callable(callable)
            .expect("resolved callable")
            .declaration;
        assert!(index
            .declaration_header(declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
        assert!(call.dispatch_receiver.is_none());
        assert!(call.extension_receiver.is_none());
    }
}

#[test]
fn companion_block_call_shadows_same_named_companion_object_member() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +CompanionBlocks +CompanionExtensions\n\
         class C {\n\
             companion { fun value(): String = \"block\" }\n\
             companion object { fun value(): String = \"object\" }\n\
         }\n\
         fun box(): String = C.value()\n",
        "box",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("companion-block call")
        .kind
    else {
        panic!("companion-block member must become checked call FIR")
    };
    let declaration = index
        .callable(call.target.module().expect("associated source target"))
        .expect("associated callable")
        .declaration;
    assert!(index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_none());
}

#[test]
fn imported_companion_block_call_is_receiverless_and_stably_bound() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +CompanionBlocksAndExtensions\n\
         import C.value\n\
         class C { companion { fun value(): String = \"OK\" } }\n\
         fun box(): String = value()\n",
        "box",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("imported associated call")
        .kind
    else {
        panic!("an imported companion-block member must become checked call FIR")
    };
    let declaration = index
        .callable(call.target.module().expect("associated source target"))
        .expect("associated callable")
        .declaration;
    assert!(index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_none());
}

#[test]
fn inferred_signature_selects_companion_block_call_without_a_value_facet() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +CompanionBlocks +CompanionExtensions\n\
         interface C { companion { fun value() = \"OK\" } }\n\
         fun box() = C.value()\n",
        "box",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("associated call")
            .ty
            .get(),
        Ty::String,
    );
}

#[test]
fn typealiases_share_companion_associated_classifier_calls_without_receivers() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +CompanionExtensions\n\
         class Target<T>\n\
         typealias Foo<T> = Target<T>\n\
         typealias Bar = Target<Int>\n\
         companion fun Foo.foo(): String = \"O\"\n\
         companion fun Bar.bar(): String = \"K\"\n\
         fun box(): String = Foo.foo() + Bar.foo() + Foo.bar() + Bar.bar()\n",
        "box",
    );
    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            call.target.module().map(|target| (call, target))
        })
        .filter(|(_, target)| matches!(index.callable_name(*target), Some("foo" | "bar")))
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 4);
    for (call, target) in calls {
        assert!(index.callable(target).is_some());
        assert!(call.dispatch_receiver.is_none());
        assert!(call.extension_receiver.is_none());
    }
}

#[test]
fn short_bitwise_extension_uses_the_imported_external_callable() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         import kotlin.experimental.*\n\
         fun flip(value: Short): Short = value.inv()\n",
        "flip",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("inv call").kind else {
        panic!("Short.inv must be a checked extension call")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
}

#[test]
fn default_imported_typealias_to_java_sam_is_a_checked_conversion() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun wrap(compare: (String, String) -> Int): Comparator<String> = Comparator(compare)\n",
        "wrap",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::ImplicitConversion {
        conversion:
            FirConversion {
                kind: FirConversionKind::Sam(sam),
                ..
            },
        ..
    } = body
        .expr(root_expression(&body))
        .expect("SAM constructor conversion")
        .kind
    else {
        panic!("Comparator(...) must publish a checked SAM conversion")
    };
    let sam = body.sam_conversion(sam).expect("body-local SAM target");
    assert_eq!(
        sam.classifier,
        crate::types::type_name("java/util/Comparator")
    );
    assert_eq!(sam.method.as_ref(), "compare");
    assert_eq!(sam.parameters.len(), 2);
    assert_eq!(sam.parameters[0].get().non_null(), Ty::String);
    assert_eq!(sam.parameters[1].get().non_null(), Ty::String);
    assert_eq!(sam.result.get(), Ty::Int);
}

#[test]
fn generic_sam_constructor_uses_its_selected_outer_parameter_type() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun order(list: MutableList<Int>) {\n\
             list.sortWith(Comparator { a, b -> b - a })\n\
         }\n",
        "order",
        jvm_stdlib_semantics(),
    );
    let sam = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::ImplicitConversion {
                conversion:
                    FirConversion {
                        kind: FirConversionKind::Sam(sam),
                        ..
                    },
                ..
            } => body.sam_conversion(*sam),
            _ => None,
        })
        .expect("Comparator constructor must retain its checked SAM conversion");
    assert_eq!(
        sam.parameters
            .iter()
            .map(|parameter| parameter.get().non_null())
            .collect::<Vec<_>>(),
        [Ty::Int, Ty::Int],
    );
    assert_eq!(sam.result.get(), Ty::Int);
}

#[test]
fn projected_generic_sam_argument_keeps_the_selected_external_call_realization() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         interface A<T>\n\
         fun <T> order(list: MutableList<A<out T>>) {\n\
             list.sortWith { _, _ -> 1 }\n\
         }\n",
        "order",
        jvm_stdlib_semantics(),
    );
    let call = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| {
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            matches!(call.target, FirCallTarget::External { .. }).then_some(call)
        })
        .expect("sortWith must retain its selected external callable");
    assert!(call.arguments.iter().any(|argument| matches!(
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
fn explicit_generic_sam_constructor_specializes_the_lambda_scope() {
    let (body, _) = checked_function_body(
        "fun interface Transform<T> { fun apply(value: T): T }\n\
         fun make(): Transform<Int> = Transform<Int> { it + 1 }\n",
        "make",
    );
    let sam = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::ImplicitConversion {
                conversion:
                    FirConversion {
                        kind: FirConversionKind::Sam(sam),
                        ..
                    },
                ..
            } => body.sam_conversion(*sam),
            _ => None,
        })
        .expect("explicit generic SAM constructor conversion");
    assert_eq!(
        sam.parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>(),
        [Ty::Int],
    );
    assert_eq!(sam.result.get(), Ty::Int);
}

#[test]
fn fixed_generic_sam_constructor_preserves_dependent_caller_type_parameters() {
    let (body, _) = checked_function_body(
        "fun interface Supplier<S> { fun get(): S }\n\
         fun interface Invoker<A, B : Supplier<A>> { fun invoke(value: B): A }\n\
         fun <A, B : Supplier<A>> make(): Invoker<A, B> =\n\
             Invoker<A, B> { supplier -> supplier.get() }\n",
        "make",
    );
    let sam = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::ImplicitConversion {
                conversion:
                    FirConversion {
                        kind: FirConversionKind::Sam(sam),
                        ..
                    },
                ..
            } => body.sam_conversion(*sam),
            _ => None,
        })
        .expect("fixed generic SAM constructor conversion");
    assert!(sam.parameters[0].get().ty_param_name().is_some());
    assert!(sam.result.get().ty_param_name().is_some());
}

#[test]
fn unsafe_cast_statement_narrows_a_stable_value_for_sam_construction() {
    let (body, _) = checked_function_body(
        "fun interface KRunnable { fun invoke() }\n\
         fun use(value: Any?) {\n\
             value as () -> Unit\n\
             KRunnable(value).invoke()\n\
         }\n",
        "use",
    );
    assert!((0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .any(|expression| matches!(
            expression.kind,
            FirExprKind::ImplicitConversion {
                conversion: FirConversion {
                    kind: FirConversionKind::Sam(_),
                    ..
                },
                ..
            }
        )));
}

#[test]
fn distinct_function_bounds_convert_to_sams_by_their_semantic_shape() {
    let (body, _) = checked_function_body(
        "fun interface Zero { fun invoke() }\n\
         fun interface One { fun invoke(value: Boolean) }\n\
         fun acceptZero(value: Zero) {}\n\
         fun acceptOne(value: One) {}\n\
         fun <T> use(value: T) where T : () -> Unit, T : (Boolean) -> Unit {\n\
             acceptZero(value)\n\
             acceptOne(value)\n\
         }\n",
        "use",
    );
    let conversions = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Call(call) => Some(call),
            _ => None,
        })
        .flat_map(|call| call.arguments.iter())
        .filter(|argument| {
            matches!(
                argument,
                FirCallArgument::Expression {
                    conversion: Some(FirConversion {
                        kind: FirConversionKind::Sam(_),
                        ..
                    }),
                    ..
                }
            )
        })
        .count();
    assert_eq!(conversions, 2);
}

#[test]
fn generic_java_sam_infers_through_a_callable_reference_value() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         class C : Comparable<C> { override fun compareTo(other: C): Int = 0 }\n\
         fun use(): Int {\n\
             val comparator = Comparable<C>::compareTo\n\
             return nullsFirst(comparator).compare(C(), C())\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );
    let call = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| {
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            call.arguments
                .iter()
                .any(|argument| {
                    matches!(
                        argument,
                        FirCallArgument::Expression {
                            conversion: Some(FirConversion {
                                kind: FirConversionKind::Sam(_),
                                ..
                            }),
                            ..
                        }
                    )
                })
                .then_some(call)
        })
        .expect("nullsFirst must retain its selected SAM conversion");
    let [substitution] = call.substitutions.as_ref() else {
        panic!("nullsFirst must publish its inferred type argument")
    };
    assert_eq!(substitution.value.get(), Ty::obj("C"));
    let [FirCallArgument::Expression {
        conversion:
            Some(FirConversion {
                kind: FirConversionKind::Sam(sam),
                ..
            }),
        ..
    }] = call.arguments.as_ref()
    else {
        panic!("the comparator argument must carry the checked SAM conversion")
    };
    let sam = body.sam_conversion(*sam).expect("body-local SAM target");
    assert_eq!(
        sam.classifier,
        crate::types::type_name("java/util/Comparator")
    );
    assert_eq!(
        sam.parameters
            .iter()
            .map(|parameter| parameter.get().non_null())
            .collect::<Vec<_>>(),
        vec![Ty::obj("C"), Ty::obj("C")],
    );
}

#[test]
fn java_static_classifier_call_keeps_external_identity_without_a_receiver() {
    let Some(jdk) = crate::toolchain::jdk_modules() else {
        return;
    };
    let mut classpath = crate::toolchain::classpath_jars_for("");
    classpath.push(jdk);
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
    ));
    let (body, _) = checked_function_body_with_platform(
        "fun load(): Any = Class.forName(\"java.lang.String\")\n",
        "load",
        platform,
    );
    let root = body.expr(root_expression(&body)).expect("Java static call");
    let call = match &root.kind {
        FirExprKind::Call(call) => call,
        FirExprKind::ImplicitConversion { value, conversion }
            if matches!(conversion.kind, FirConversionKind::PlatformNarrowing { .. }) =>
        {
            let FirExprKind::Call(call) = &body.expr(*value).expect("platform producer").kind
            else {
                panic!("platform narrowing must wrap the selected Java call")
            };
            call
        }
        _ => panic!("Java static classifier call must become checked call FIR"),
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn nested_inline_break_keeps_its_enclosing_loop_coordinate_through_lowering() {
    let (body, index) = checked_function_body_with_platform(
        "fun run() { while (true) { \"\".let { it.run { break } } } }\n",
        "run",
        jvm_semantics(),
    );
    let outer_target = (0..body.statement_count())
        .find_map(|raw| {
            let statement = body.statement(FirStatementId::from_raw(raw as u32))?;
            let FirStatementKind::Loop { target, .. } = statement.kind else {
                return None;
            };
            Some(target)
        })
        .expect("outer while target");

    fn inline_break(body: &FirBody) -> Option<(u32, ControlTargetId)> {
        for raw in 0..body.expression_count() {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            match &expression.kind {
                FirExprKind::Jump {
                    kind: FirJumpKind::Break { target_depth },
                    target,
                    value: None,
                } => return Some((*target_depth, *target)),
                FirExprKind::Lambda { body, .. } => {
                    if let Some(jump) = inline_break(body) {
                        return Some(jump);
                    }
                }
                _ => {}
            }
        }
        None
    }
    assert_eq!(inline_break(&body), Some((2, outer_target)));

    let mut ir = crate::ir::IrFile::default();
    crate::fir_lower::lower_body(body, &index, &mut ir)
        .expect("checked inline break must lower without lookup");
    let outer_label = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            crate::ir::IrExpr::While {
                label: Some(label), ..
            } if label.starts_with("$fir_control_") => Some(label.clone()),
            _ => None,
        })
        .expect("lowered outer while label");
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        crate::ir::IrExpr::Break { label: Some(label) } if label == &outer_label
    )));
}

#[test]
fn selected_top_level_inline_run_rechecks_its_lambda_inside_the_loop_scope() {
    let (body, _) = checked_function_body_with_platform(
        "class C { fun runLoop() { while (true) { run { break } } } }\n",
        "runLoop",
        jvm_semantics(),
    );
    let jump = (0..body.expression_count()).find_map(|raw| {
        let FirExprKind::Lambda { body: lambda, .. } =
            &body.expr(FirExprId::from_raw(raw as u32))?.kind
        else {
            return None;
        };
        (0..lambda.expression_count()).find_map(|nested| {
            let expression = lambda.expr(FirExprId::from_raw(nested as u32))?;
            matches!(
                expression.kind,
                FirExprKind::Jump {
                    kind: FirJumpKind::Break { .. },
                    ..
                }
            )
            .then_some(())
        })
    });
    assert_eq!(jump, Some(()));
}

#[test]
fn inline_array_constructor_break_keeps_its_enclosing_loop_coordinate() {
    let (body, _) =
        checked_function_body("fun run() { while (true) { Array(1) { break } } }\n", "run");
    let outer_target = (0..body.statement_count())
        .find_map(|raw| {
            let statement = body.statement(FirStatementId::from_raw(raw as u32))?;
            let FirStatementKind::Loop { target, .. } = statement.kind else {
                return None;
            };
            Some(target)
        })
        .expect("outer while target");

    fn inline_break(body: &FirBody) -> Option<(u32, ControlTargetId)> {
        for raw in 0..body.expression_count() {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            match &expression.kind {
                FirExprKind::Jump {
                    kind: FirJumpKind::Break { target_depth },
                    target,
                    value: None,
                } => return Some((*target_depth, *target)),
                FirExprKind::Lambda { body, .. } => {
                    if let Some(jump) = inline_break(body) {
                        return Some(jump);
                    }
                }
                _ => {}
            }
        }
        None
    }

    assert_eq!(inline_break(&body), Some((1, outer_target)));
}

#[test]
fn explicit_extension_function_receiver_contextualizes_a_generic_factory_call() {
    let (body, _) = checked_function_body(
        "class Box<T>\n\
         fun <T> make(): Box<T> = Box<T>()\n\
         fun <T> identity(block: Box<T>.() -> T): Box<T>.() -> T = block\n\
         fun <T> invoke(block: Box<T>.() -> T): T = identity<T>(block)(make())\n",
        "invoke",
    );

    let root = body
        .expr(root_expression(&body))
        .expect("function invocation");
    let FirExprKind::FunctionInvoke { arguments, .. } = &root.kind else {
        panic!("extension-function value must become a checked function invocation")
    };
    assert_eq!(
        arguments.len(),
        1,
        "the explicit argument supplies the receiver"
    );
}

#[test]
fn chained_extension_property_reference_uses_one_specialized_receiver_shape() {
    let (body, _) = checked_function_body_with_platform(
        "val <T> List<T>.item: T get() = null as T\n\
         val <T> (List<T>.() -> T).result: T get() = null as T\n\
         fun <T> reference(): () -> T = List<T>::item::result\n",
        "reference",
        jvm_semantics(),
    );

    let root = body.expr(root_expression(&body)).expect("reference body");
    assert!(
        matches!(root.kind, FirExprKind::PropertyReference { .. }),
        "chained reference FIR: {:?}",
        root.kind,
    );
}

#[test]
fn callable_reference_result_joins_with_ordinary_generic_argument_before_fir() {
    let (body, _) = checked_function_body(
        "fun <T, R> choose(x: T, y: R, function: (T) -> R): R = function(x)\n\
         fun <T> returnInt(x: T): Int = 1\n\
         fun use(): Any = choose(\"\", \"\", ::returnInt)\n",
        "use",
    );

    let root = body.expr(root_expression(&body)).expect("generic call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("selected generic callable must become checked call FIR")
    };
    assert_eq!(root.ty.get(), Ty::obj("kotlin/Any"));
    assert_eq!(call.substitutions.len(), 2);
}

#[test]
fn ordinary_arguments_complete_a_fully_generic_callable_reference_before_fir() {
    let (body, _) = checked_function_body(
        "fun <T, R> convert(value: T): R = null as R\n\
         fun <T, R> choose(x: T, y: R, function: (T) -> R): R = function(x)\n\
         fun use(): String = choose(1, \"\", ::convert)\n",
        "use",
    );

    let root = body.expr(root_expression(&body)).expect("generic call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("selected generic callable must become checked call FIR")
    };
    assert_eq!(root.ty.get(), Ty::String);
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(call.substitutions[0].value.get(), Ty::Int);
    assert_eq!(call.substitutions[1].value.get(), Ty::String);
}

#[test]
fn outer_generic_bound_completes_a_nested_mutable_collection_constructor_before_fir() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun box(): Set<String> = listOf(1).mapTo(HashSet()) { \"OK\" }\n",
        "box",
        jvm_stdlib_semantics(),
    );

    let expression = body
        .expr(root_expression(&body))
        .expect("mapTo call must become checked FIR");
    assert_eq!(expression.ty.get().type_args(), &[Ty::String]);
}

#[test]
fn generic_constructor_destination_preserves_receiver_lambda_input_before_fir() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun consume(value: Int): String = value.toString()\n\
         fun box(): Set<String> = (0 until 1).mapTo(HashSet()) { consume(it) }\n",
        "box",
        jvm_stdlib_semantics(),
    );

    let expression = body
        .expr(root_expression(&body))
        .expect("mapTo call must become checked FIR");
    assert_eq!(expression.ty.get().type_args(), &[Ty::String]);
}

#[test]
fn sibling_callable_references_jointly_constrain_generic_call_before_fir() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T, R> generic(value: T): R = null as R\n\
         fun <T, R> choose(x: T, y: R, function: (T) -> R,\n\
                           tName: String, rName: String) {}\n\
         fun <T, R> choose(first: (T) -> R, second: (T) -> R,\n\
                           tName: String, rName: String) {}\n\
         fun use(): Unit = choose(Int::toString, ::generic, \"Int\", \"String\")\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let root = body.expr(root_expression(&body)).expect("generic call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("selected generic callable must become checked call FIR")
    };
    assert_eq!(root.ty.get(), Ty::Unit);
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(call.substitutions[0].value.get(), Ty::Int);
    assert_eq!(call.substitutions[1].value.get().non_null(), Ty::String);
}

#[test]
fn partially_specified_call_type_argument_is_inferred_before_fir() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +PartiallySpecifiedTypeArguments\n\
         class Pair<A, B>\n\
         fun <K, T> choose(block: (K) -> T): Pair<K, T> = null as Pair<K, T>\n\
         fun use(): Pair<Int, Float> = choose<Int, _> { 1.0f }\n",
        "use",
    );

    let root = body.expr(root_expression(&body)).expect("generic call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("partially specified generic callable must become checked call FIR")
    };
    assert_eq!(root.ty.get(), Ty::obj_args("Pair", &[Ty::Int, Ty::Float]));
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(call.substitutions[0].value.get(), Ty::Int);
    assert_eq!(call.substitutions[1].value.get(), Ty::Float);
}

#[test]
fn invariant_expected_result_constrains_nullable_callable_reference_result_before_fir() {
    let (body, _) = checked_function_body(
        "data class Pair<A, B>(val first: A, val second: B)\n\
         fun <T, R> bar(x: T, y: R, function: (T) -> R): Pair<T, R?> = Pair(x, y)\n\
         fun <T, R> convert(value: T): R = null as R\n\
         fun use(): Pair<Int?, String?> = bar(null, null, ::convert)\n",
        "use",
    );

    let root = body.expr(root_expression(&body)).expect("generic call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("selected generic callable must become checked call FIR")
    };
    assert_eq!(
        root.ty.get(),
        Ty::obj_args("Pair", &[Ty::nullable(Ty::Int), Ty::nullable(Ty::String)],)
    );
    assert_eq!(call.substitutions.len(), 2);
}

#[test]
fn invariant_expected_result_contextualizes_a_nested_generic_constructor_before_fir() {
    let (body, _) = checked_function_body(
        "class Box<T>(val value: T)\n\
         class Item<T : Any>(val value: T?)\n\
         fun <T> factory(value: T): Box<T> = Box(value)\n\
         fun use(): Box<Item<Any>> = factory(Item(0))\n",
        "use",
    );

    let expected_item = Ty::obj_args("Item", &[Ty::obj("kotlin/Any")]);
    let root = body
        .expr(root_expression(&body))
        .expect("generic factory call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("selected generic factory must become checked call FIR")
    };
    assert_eq!(
        root.ty.get(),
        Ty::obj_args("Box", &[expected_item]),
        "the invariant result must fix the outer callable substitution",
    );
    assert!(call.arguments.iter().any(|argument| {
        let FirCallArgument::Expression { value, .. } = argument else {
            return false;
        };
        body.expr(*value)
            .is_some_and(|expression| expression.ty.get() == expected_item)
    }));
}

#[test]
fn indexed_set_parameter_contextualizes_a_nested_generic_constructor_before_fir() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         class Item<T : Any>(val value: T?)\n\
         fun use(items: ArrayList<Item<Any>>) { items[0] = Item(0) }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let expected_item = Ty::obj_args("Item", &[Ty::obj("kotlin/Any")]);
    assert!((0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32))
            .is_some_and(|expression| {
                expression.ty.get() == expected_item
                    && matches!(expression.kind, FirExprKind::ConstructorCall(_))
            })
    }));
    assert!((0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32))
            .is_some_and(|expression| matches!(expression.kind, FirExprKind::Call(_)))
    }));
}

#[test]
fn postponed_builder_constraint_shapes_a_following_consumer_lambda_before_fir() {
    let (body, _) = checked_function_body(
        "class Concrete\n\
         class Target { fun consume(value: Concrete) {} }\n\
         class Builder<T> {\n\
             fun producer(value: () -> T) {}\n\
             fun consumer(value: (T) -> Unit) {}\n\
         }\n\
         fun <T> build(block: Builder<T>.() -> Unit): Builder<T> = Builder<T>()\n\
         fun use() {\n\
             build {\n\
                 producer { Target() }\n\
                 consumer { it.consume(Concrete()) }\n\
             }\n\
         }\n",
        "use",
    );

    assert!((0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32)).is_some_and(|expression| {
            matches!(&expression.kind, FirExprKind::Call(call) if call.target.module().is_some())
        })
    }));
}

#[test]
fn postponed_receiver_member_contextualizes_an_omitted_generic_alias_constructor() {
    let (body, _) = checked_function_body(
        "class Owner<T : Any> {\n\
             fun <X : Any> constrain(value: Pair<X, T>): Pair<X, T> = value\n\
         }\n\
         fun <T : Any> build(block: Owner<T>.() -> Pair<*, T>) {}\n\
         class Concrete\n\
         class Pair<A, B>\n\
         typealias Source<Y> = Pair<Y, Concrete>\n\
         fun use() { build { constrain(Source()) } }\n",
        "use",
    );

    assert!((0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32))
            .is_some_and(|expression| matches!(expression.kind, FirExprKind::Call(_)))
    }));
}

#[test]
fn postponed_receiver_keeps_collecting_member_argument_lower_bounds() {
    let (body, _) = checked_function_body(
        "interface Sink<T> { fun emit(value: T) }\n\
         fun <T> build(block: Sink<T>.() -> Unit) {}\n\
         fun use() {\n\
             build {\n\
                 emit(1)\n\
                 emit(null)\n\
             }\n\
         }\n",
        "use",
    );

    assert!((0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32))
            .is_some_and(|expression| matches!(expression.kind, FirExprKind::Call(_)))
    }));
}

#[test]
fn function_classifier_extension_accepts_a_semantic_function_literal_receiver() {
    let _ = checked_function_body_with_platform(
        "infix fun <R> Function0<R>.otherwise(alternative: () -> R): R = alternative()\n\
         fun test(): String = ({ throw RuntimeException(\"fail\") } otherwise { \"OK\" })\n",
        "test",
        jvm_stdlib_semantics(),
    );
}

#[test]
fn postponed_builder_receiver_survives_nested_extension_destructuring_constraints() {
    let _ = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun use() {\n\
             buildMap {\n\
                 mapValues { (key: Int, value: String) -> key.toString() + value }\n\
             }\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );
}

#[test]
fn member_extension_call_keeps_both_selected_receivers() {
    let (body, index) = checked_function_body(
        "class Scope { fun String.answer(): Int = length; fun read(value: String): Int = value.answer() }\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("member extension call")
        .kind
    else {
        panic!("member extension call must become checked call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    let dispatch = call
        .dispatch_receiver
        .expect("member extension needs its selected dispatch receiver");
    assert!(matches!(
        body.expr(dispatch.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
    assert!(call.extension_receiver.is_some());
}

#[test]
fn imported_singleton_member_extension_keeps_dispatch_and_extension_receivers() {
    let (body, index) = checked_function_body(
        "import Scope.answer\n\
         object Scope { fun Boolean.answer(): Int = 42 }\n\
         fun use(): Int = true.answer()\n",
        "use",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("member extension call")
        .kind
    else {
        panic!("imported member extension must become checked call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    let dispatch = call
        .dispatch_receiver
        .as_ref()
        .expect("object supplies dispatch receiver");
    assert!(matches!(
        body.expr(dispatch.value).map(|expression| &expression.kind),
        Some(FirExprKind::SingletonValue { .. })
    ));
    assert!(call.extension_receiver.is_some());
}

#[test]
fn inapplicable_implicit_receiver_extension_falls_through_to_top_level() {
    let (body, index) = checked_function_body(
        "fun Scope.choose(): String = \"wrong\"\n\
         fun choose(block: () -> String): String = block()\n\
         class Scope { fun test(): String = choose { \"OK\" } }\n",
        "test",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("selected top-level call")
        .kind
    else {
        panic!("the applicable receiver-less rung must produce checked call FIR")
    };
    let declaration = index
        .callable(call.target.module().expect("source callable"))
        .expect("stable selected callable");
    assert_eq!(
        index
            .signature(declaration.declaration)
            .expect("selected callable signature")
            .parameters
            .len(),
        1
    );
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_none());
}

#[test]
fn applicable_top_level_rung_is_not_replaced_by_an_imported_receiver_extension() {
    let (body, index) = checked_function_body_with_platform(
        "fun <R> run(block: () -> R): R = block()\n\
         class Scope { fun test(): String = run { \"OK\" } }\n",
        "test",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("selected top-level call")
        .kind
    else {
        panic!("the applicable receiver-less rung must produce checked call FIR")
    };
    let declaration = index
        .callable(call.target.module().expect("same-module callable"))
        .expect("stable selected callable");
    assert_eq!(index.callable_name(declaration.id), Some("run"));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_none());
    let FirCallArgument::Expression { value, .. } = &call.arguments[0] else {
        panic!("run block must remain an explicit argument")
    };
    let FirExprKind::Lambda { body: lambda, .. } =
        &body.expr(*value).expect("checked lambda argument").kind
    else {
        panic!("run argument must remain a checked lambda")
    };
    assert_eq!(lambda.receiver_type(), None);
}

#[test]
fn smart_casted_implicit_receiver_call_retains_its_conversion() {
    let (body, index) = checked_function_body(
        "sealed interface Op\n\
         data class Create(val path: String, val n: Int = 7) : Op\n\
         fun Op.renamed(p: String): Op = when (this) {\n\
             is Create -> copy(path = p)\n\
             else -> this\n\
         }\n",
        "renamed",
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("copy")).then_some(call)
        })
        .expect("smart-cast branch must retain the selected copy call");
    let conversion = call
        .dispatch_receiver
        .and_then(|receiver| receiver.conversion)
        .expect("narrowed implicit receiver must publish a conversion");
    let FirConversionKind::SmartCast { to } = conversion.kind else {
        panic!("narrowed implicit receiver must use a smart-cast conversion")
    };
    assert_eq!(to.get(), Ty::obj("Create"));
}

#[test]
fn inapplicable_function_value_falls_through_to_implicit_receiver_extension() {
    let (body, index) = checked_function_body(
        "interface Flow<T>\n\
         interface FlowCollector<T> { fun emit(value: T) }\n\
         fun <T, R> Flow<T>.transform(block: FlowCollector<R>.(T) -> Unit): Flow<R> =\n\
             this as Flow<R>\n\
         fun <T, R : Any> Flow<T>.mapNotNull(transform: (T) -> R?): Flow<R> =\n\
             transform { value ->\n\
                 val transformed = transform(value) ?: return@transform\n\
                 return@transform emit(transformed)\n\
             }\n",
        "mapNotNull",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("selected transform call")
        .kind
    else {
        panic!("the implicit-receiver extension must become a checked call")
    };
    let target = call
        .target
        .module()
        .expect("source extension must keep its stable identity");
    assert_eq!(index.callable_name(target), Some("transform"));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
}

#[test]
fn anonymous_object_member_keeps_enclosing_extension_receiver_label() {
    let (body, _) = checked_function_body(
        "interface Flow<T> { fun collect(collector: FlowCollector<T>) }\n\
         fun interface FlowCollector<T> { fun emit(value: T) }\n\
         inline fun <T, R> Flow<T>.transform(\n\
             crossinline block: FlowCollector<R>.(T) -> Unit\n\
         ): Flow<R> = object : Flow<R> {\n\
             override fun collect(collector: FlowCollector<R>) {\n\
                 this@transform.collect { value -> collector.block(value) }\n\
             }\n\
         }\n",
        "transform",
    );
    assert!(matches!(
        body.expr(root_expression(&body))
            .expect("anonymous object construction")
            .kind,
        FirExprKind::AnonymousObject(_)
    ));
}

#[test]
fn safe_member_call_wraps_the_already_selected_stable_call() {
    let (body, index) = checked_function_body(
        "class Box { fun answer() = 42 }\nfun read(box: Box?): Int? = box?.answer()\n",
        "read",
    );
    let FirExprKind::SafeCall { selector, .. } =
        body.expr(root_expression(&body)).expect("safe call").kind
    else {
        panic!("safe call must retain an explicit null-guarded selector")
    };
    let FirExprKind::Call(call) = &body.expr(selector).expect("selected call").kind else {
        panic!("safe selector must be the selected call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_some());
}

#[test]
fn safe_call_on_nullable_type_parameter_selects_member_from_non_null_bound() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T : Number?> convert(value: T) { value?.toInt() }\n",
        "convert",
        jvm_semantics(),
    );
    let safe = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::SafeCall { .. }))
        .expect("type-parameter member call must retain its null guard");
    let FirExprKind::SafeCall { selector, .. } = safe.kind else {
        unreachable!()
    };
    let FirExprKind::Call(call) = &body.expr(selector).expect("selected toInt call").kind else {
        panic!("safe selector must retain the checked member call")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.dispatch_receiver.is_some());
}

#[test]
fn selected_super_call_retains_named_and_default_argument_mapping() {
    let (body, _) = checked_function_body(
        "open class Base {\n\
             open fun value(x: Int = 20, y: Int = 3): Int = x + y\n\
         }\n\
         class Derived : Base() {\n\
             fun read(): Int = super.value(y = 4)\n\
         }\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("selected super call")
        .kind
    else {
        panic!("super call must become checked call FIR")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Super {
            source: Some(_),
            realization: crate::libraries::MemberRealization::Dispatch,
            ..
        }
    ));
    assert!(call.dispatch_receiver.is_some());
    assert!(call
        .arguments
        .iter()
        .any(|argument| { matches!(argument, FirCallArgument::Expression { parameter: 1, .. }) }));
    assert!(call
        .arguments
        .iter()
        .any(|argument| { matches!(argument, FirCallArgument::Default { parameter: 0, .. }) }));
}

#[test]
fn interface_super_call_applies_transitive_type_arguments() {
    let (body, _) = checked_function_body(
        "interface A<T, U : Any, V : Any> {\n\
             fun foo(t: T, u: U): V? = null\n\
         }\n\
         interface B<T, V : Any> : A<T, Int, V>\n\
         interface Runnable\n\
         class C : B<String, Runnable> {\n\
             fun read(): Runnable? = super.foo(\"\", 0)\n\
         }\n",
        "read",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("selected interface super call");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("interface super call must become checked call FIR")
    };
    assert!(matches!(call.target, FirCallTarget::Super { .. }));
    assert_eq!(root.ty.get(), Ty::nullable(Ty::obj("Runnable")));
}

#[test]
fn interface_super_call_uses_provider_normalized_diamond_override() {
    let (body, _) = checked_function_body(
        "interface Base { fun test(): String = \"OK\" }\n\
         interface Left : Base { override fun test(): String = super.test() }\n\
         interface Right : Base { override fun test(): String }\n\
         interface Diamond : Left, Right {\n\
             override fun test(): String = super.test()\n\
             fun read(): String = super.test()\n\
         }\n",
        "read",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::Call(FirCall {
                target: FirCallTarget::Super { .. },
                ..
            }))
        )
    }));
}

#[test]
fn member_extension_super_call_uses_the_enclosing_class_dispatch_receiver() {
    let (body, _) = checked_function_body(
        "open class Base { open fun value(): String = \"base\" }\n\
         class ExtensionReceiver\n\
         class Owner : Base() {\n\
             fun ExtensionReceiver.read(): String = super<Base>.value()\n\
         }\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("selected super call")
        .kind
    else {
        panic!("member-extension super call must become checked call FIR")
    };
    assert!(matches!(call.target, FirCallTarget::Super { .. }));
    let receiver = call
        .dispatch_receiver
        .expect("super call must retain its class dispatch receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: false,
            depth: 1,
        })
    ));
}

#[test]
fn anonymous_interface_delegate_accepts_a_lexical_super_call() {
    let _ = checked_function_body(
        "interface Value { fun text(): String }\n\
         class ValueImpl : Value { override fun text(): String = \"OK\" }\n\
         open class Factory { open fun make(): Value = ValueImpl() }\n\
         class Owner : Factory() {\n\
             override fun make(): Value = ValueImpl()\n\
             fun wrapped(): Value = object : Value by super.make() {}\n\
        }\n",
        "wrapped",
    );
}

#[test]
fn inline_lambda_can_name_its_enclosing_function_return_target() {
    let (body, _) = checked_function_body(
        "inline fun doCall(block: () -> Any): Any = block()\n\
         fun test(): String {\n\
             doCall { return@test \"OK\" }\n\
             return \"unreachable\"\n\
         }\n",
        "test",
    );

    fn named_function_return(body: &FirBody) -> Option<u32> {
        for raw in 0..body.expression_count() {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            match &expression.kind {
                FirExprKind::Jump {
                    kind: FirJumpKind::Return { target_depth },
                    value: Some(_),
                    ..
                } if *target_depth > 0 => return Some(*target_depth),
                FirExprKind::Lambda { body, .. } => {
                    if let Some(depth) = named_function_return(body) {
                        return Some(depth);
                    }
                }
                _ => {}
            }
        }
        None
    }

    assert_eq!(named_function_return(&body), Some(1));
}

#[test]
fn selected_member_sam_recheck_keeps_the_calls_implicit_return_label() {
    let (body, _) = checked_function_body(
        "fun interface Collector<T> { fun emit(value: T) }\n\
         interface Flow<T> { fun collect(collector: Collector<T>) }\n\
         fun <T> Flow<T>.transform(action: (T) -> Unit) {\n\
             collect { value -> return@collect action(value) }\n\
         }\n",
        "transform",
    );

    fn local_return(body: &FirBody) -> bool {
        (0..body.expression_count()).any(|raw| {
            let Some(expression) = body.expr(FirExprId::from_raw(raw as u32)) else {
                return false;
            };
            match &expression.kind {
                FirExprKind::Jump {
                    kind: FirJumpKind::Return { target_depth },
                    ..
                } => *target_depth == 0,
                FirExprKind::Lambda { body, .. } => local_return(body),
                _ => false,
            }
        })
    }

    assert!(local_return(&body));
}

#[test]
fn safe_numeric_conversion_guards_the_conversion_operand() {
    let (body, _) = checked_function_body_with_platform(
        "fun convert(value: Int?): Byte? = value?.toByte()\n",
        "convert",
        jvm_semantics(),
    );
    let FirExprKind::SafeCall { receiver, selector } = body
        .expr(root_expression(&body))
        .expect("safe conversion")
        .kind
    else {
        panic!("safe conversion must retain an explicit null guard")
    };
    let FirExprKind::ImplicitConversion {
        value,
        conversion:
            FirConversion {
                kind: FirConversionKind::NumericConversion { to },
                ..
            },
    } = body.expr(selector).expect("conversion selector").kind
    else {
        panic!("safe numeric conversion must remain a checked conversion")
    };
    assert_eq!(receiver.value, value);
    assert_eq!(to.get(), Ty::Byte);
}

#[test]
fn safe_builtin_range_to_guards_the_checked_range_start() {
    let (body, _) = checked_function_body_with_platform(
        "fun range(value: Int?, end: Int): IntRange? = value?.rangeTo(end)\n",
        "range",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::SafeCall { receiver, selector } = body
        .expr(root_expression(&body))
        .expect("safe range call")
        .kind
    else {
        panic!("safe rangeTo must retain an explicit null guard")
    };
    let FirExprKind::Range {
        operation: FirRangeOperation::Through,
        start,
        ..
    } = body.expr(selector).expect("range selector").kind
    else {
        panic!("safe rangeTo selector must remain checked range FIR")
    };
    assert_eq!(receiver.value, start);
}

#[test]
fn safe_local_extension_call_guards_the_selected_extension_receiver() {
    let (body, _) = checked_function_body(
        "class Box\n\
         fun use(value: Box?): Unit {\n\
             fun Box.local(): Unit {}\n\
             value?.local()\n\
         }\n",
        "use",
    );
    let safe = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(expression.kind, FirExprKind::SafeCall { .. }).then_some(expression)
        })
        .expect("safe local-extension call");
    let FirExprKind::SafeCall { receiver, selector } = safe.kind else {
        unreachable!()
    };
    let FirExprKind::LocalCall {
        extension_receiver: Some(selected),
        ..
    } = body.expr(selector).expect("local-call selector").kind
    else {
        panic!("safe local extension must retain its selected local callable")
    };
    assert_eq!(receiver, selected);
}

#[test]
fn safe_receiver_function_call_guards_its_explicit_extension_receiver() {
    let (body, _) = checked_function_body(
        "class Box\n\
         fun call(block: Box.() -> Unit, value: Any?): Unit {\n\
             (value as? Box)?.block()\n\
         }\n",
        "call",
    );
    let safe = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(expression.kind, FirExprKind::SafeCall { .. }).then_some(expression)
        })
        .expect("safe receiver-function call");
    let FirExprKind::SafeCall { receiver, selector } = safe.kind else {
        unreachable!()
    };
    let FirExprKind::FunctionInvoke {
        callee, arguments, ..
    } = &body
        .expr(selector)
        .expect("receiver-function selector")
        .kind
    else {
        panic!("safe receiver-function selector must retain checked invoke FIR")
    };
    let FirCallArgument::Expression {
        parameter: 0,
        value,
        ..
    } = arguments[0]
    else {
        panic!("receiver-function extension receiver must be parameter zero")
    };
    assert_eq!(receiver.value, value);
    assert_ne!(receiver.value, *callee);
}

#[test]
fn top_level_call_keeps_checker_selected_implicit_context_argument() {
    let (body, index) = checked_function_body(
        "class Session\n\
         context(session: Session) fun answer(value: Int): Int = value\n\
         context(session: Session) fun read(): Int = answer(42)\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("context call")
        .kind
    else {
        panic!("context call must become checked call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert_eq!(call.arguments.len(), 2);
    let FirCallArgument::Expression {
        parameter: 0,
        value: context,
        ..
    } = call.arguments[0]
    else {
        panic!("first call argument must be the selected context binding")
    };
    assert!(matches!(
        body.expr(context).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
    assert!(matches!(
        call.arguments[1],
        FirCallArgument::Expression { parameter: 1, .. }
    ));
}

#[test]
fn member_call_keeps_dispatch_receiver_and_implicit_context_argument() {
    let (body, index) = checked_function_body(
        "class Session\n\
         class Host {\n\
         context(session: Session) fun answer(value: Int): Int = value\n\
         context(session: Session) fun read(): Int = answer(42)\n\
         }\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("context member call")
        .kind
    else {
        panic!("context member call must become checked call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_some());
    assert_eq!(call.arguments.len(), 2);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
    assert!(matches!(
        call.arguments[1],
        FirCallArgument::Expression { parameter: 1, .. }
    ));
}

#[test]
fn missing_context_on_nearer_implicit_receiver_falls_through_to_enclosing_receiver() {
    let (body, index) = checked_function_body_with_platform(
        "// LANGUAGE: +ContextParameters\n\
         class A {\n\
             context(value: String) fun choose(): String = \"A\"\n\
             fun Inner.use(): String = with(\"\") { choose() }\n\
         }\n\
         class Inner {\n\
             context(value: Int) fun choose(): String = \"Inner\"\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let FirExprKind::Call(with_call) = &body.expr(root_expression(&body)).expect("with call").kind
    else {
        panic!("receiver-lambda builder must remain checked call FIR")
    };
    let FirCallArgument::Expression { value, .. } = with_call.arguments[1] else {
        panic!("with block must remain an explicit checked argument")
    };
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body.expr(value).expect("with receiver lambda").kind
    else {
        panic!("with block must remain checked lambda FIR")
    };

    let call = (0..lambda_body.expression_count())
        .find_map(|raw| {
            let expression = lambda_body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("choose")).then_some(call)
        })
        .expect("applicable enclosing implicit-receiver call");
    let target = index
        .callable(call.target.module().expect("source choose target"))
        .expect("resolved callable header");
    let owner = index
        .declaration_header(target.declaration)
        .and_then(|header| header.owner)
        .expect("member callable owner");
    assert_eq!(index.declaration_name(owner), Some("A"));
    assert_eq!(call.arguments.len(), 1);
    assert!(call.dispatch_receiver.is_some());
}

#[test]
fn contextual_member_shapes_lambda_after_removing_implicit_context_parameters() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +ContextParameters\n\
         class Host {\n\
             var result: Int = 0\n\
             context(any: Any) inline fun calculate(block: (Int) -> Unit) {}\n\
             fun use() { calculate { result = it } }\n\
         }\n",
        "use",
    );

    let call = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            Some(call)
        })
        .expect("contextual member call");
    assert_eq!(call.arguments.len(), 2);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
    assert!(matches!(
        call.arguments[1],
        FirCallArgument::Expression { parameter: 1, .. }
    ));

    let lambda = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Lambda { body, .. } = &expression.kind else {
                return None;
            };
            Some(body.as_ref())
        })
        .expect("checked lambda argument");
    let [parameter] = lambda.parameters() else {
        panic!("lambda must own its implicit it parameter")
    };
    assert_eq!(parameter.ty, ResolvedTy::new(Ty::Int).unwrap());
}

#[test]
fn named_call_arguments_keep_source_order_and_selected_parameter_ordinals() {
    let (body, _) = checked_function_body(
        "fun choose(first: Int, second: Int): Int = first\n\
         fun read(): Int = choose(second = 2, first = 1)\n",
        "read",
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("named call").kind
    else {
        panic!("named call must become checked call FIR")
    };
    assert_eq!(call.arguments.len(), 2);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 1, .. }
    ));
    assert!(matches!(
        call.arguments[1],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
}

#[test]
fn omitted_default_is_recorded_after_source_arguments() {
    let (body, _) = checked_function_body(
        "fun choose(first: Int = 1, second: Int): Int = first\n\
         fun read(): Int = choose(second = 2)\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("defaulted call")
        .kind
    else {
        panic!("defaulted call must become checked call FIR")
    };
    assert!(matches!(
        call.arguments.as_ref(),
        [
            FirCallArgument::Expression { parameter: 1, .. },
            FirCallArgument::Default { parameter: 0, .. }
        ]
    ));
}

#[test]
fn indexed_get_records_its_omitted_default_parameter() {
    let (body, index) = checked_function_body(
        "class Box { operator fun get(index: Int, fallback: String = \"OK\"): String = fallback }\n\
         fun read(box: Box): String = box[0]\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("indexed get call")
        .kind
    else {
        panic!("selected indexed get must become checked call FIR")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(matches!(
        call.arguments.as_ref(),
        [
            FirCallArgument::Expression { parameter: 0, .. },
            FirCallArgument::Default { parameter: 1, .. }
        ]
    ));
}

#[test]
fn source_call_varargs_keep_source_order_and_spread_decisions() {
    let (body, _) = checked_function_body(
        "fun join(vararg values: String): Int = 0\n\
         fun read(values: Array<String>): Int = join(\"first\", *values, \"last\")\n",
        "read",
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("vararg call").kind
    else {
        panic!("vararg source call must become checked call FIR")
    };
    assert_eq!(call.arguments.len(), 3);
    for (argument, spread) in call.arguments.iter().zip([false, true, false]) {
        let FirCallArgument::Vararg {
            parameter: 0,
            elements,
            ..
        } = argument
        else {
            panic!("each source vararg operand must retain an ordered FIR entry")
        };
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].spread, spread);
    }
}

#[test]
fn omitted_source_call_vararg_is_an_explicit_empty_pack() {
    let (body, _) = checked_function_body(
        "fun join(vararg values: String): Int = 0\nfun read(): Int = join()\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("empty vararg call")
        .kind
    else {
        panic!("empty vararg source call must become checked call FIR")
    };
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Vararg {
            parameter: 0,
            elements,
            ..
        }] if elements.is_empty()
    ));
}

#[test]
fn named_whole_array_vararg_is_not_repacked() {
    let (body, _) = checked_function_body(
        "fun join(vararg values: String): Int = 0\n\
         fun read(values: Array<String>): Int = join(values = values)\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("whole-array vararg call")
        .kind
    else {
        panic!("whole-array vararg call must become checked call FIR")
    };
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn spread_generic_array_call_is_contextualized_as_the_whole_vararg_array() {
    let (body, _) = checked_function_body_with_platform(
        "fun consume(vararg values: Unit, tail: Any) {}\n\
         fun read() {\n\
             consume({}(), tail = {}())\n\
             consume(values = *arrayOf({}()), tail = {}())\n\
         }\n",
        "read",
        jvm_stdlib_semantics(),
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            call.arguments
                .iter()
                .any(|argument| {
                    matches!(argument, FirCallArgument::Vararg { elements, .. }
                    if elements.first().is_some_and(|element| element.spread))
                })
                .then_some(call)
        })
        .expect("spread vararg call");
    let [FirCallArgument::Vararg { elements, .. }, ..] = call.arguments.as_ref() else {
        panic!("selected vararg call must retain its spread operand")
    };
    assert!(elements[0].spread);
    assert_eq!(
        body.expr(elements[0].value)
            .expect("contextualized arrayOf")
            .ty
            .get(),
        Ty::obj_args("kotlin/Array", &[Ty::Unit]),
    );
}

#[test]
fn explicit_generic_context_argument_publishes_contextual_long_literal() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters +ExplicitContextArguments\n\
         context(value: T) fun <T> identity(): T = value\n\
         fun read(): Long = identity<Long>(value = 1)\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("generic call")
        .kind
    else {
        panic!("generic context call must become checked call FIR")
    };
    let [FirCallArgument::Expression {
        parameter: 0,
        value,
        conversion: None,
    }] = call.arguments.as_ref()
    else {
        panic!("contextually typed argument must retain its final parameter and type")
    };
    assert!(matches!(
        body.expr(*value),
        Some(FirExpr {
            ty,
            kind: FirExprKind::Constant(FirConstant::Long(1)),
            ..
        }) if ty.get() == Ty::Long
    ));
    assert!(index.callable(call.target.module().unwrap()).is_some());
}

#[test]
fn primitive_argument_to_reference_parameter_publishes_boxing_conversion() {
    let (body, _) = checked_function_body(
        "fun consume(value: Any): Any = value\nfun read(value: Int): Any = consume(value)\n",
        "read",
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("call").kind else {
        panic!("ordinary call must become checked call FIR")
    };
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression {
            parameter: 0,
            conversion: Some(FirConversion {
                kind: FirConversionKind::NullabilityWidening { to },
                ..
            }),
            ..
        }] if to.get() == Ty::obj("kotlin/Any")
    ));
}

#[test]
fn foreach_with_callable_reference_keeps_the_selected_external_call_without_splice_plan() {
    let (body, _) = checked_function_body_with_platform(
        "class Item\n\
         class Collector { fun add(item: Item): Collector = this }\n\
         fun read(collector: Collector): Unit { listOf(Item()).forEach(collector::add) }\n",
        "read",
        jvm_semantics(),
    );

    let foreach = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::Call(call) = &expression.kind else {
            return None;
        };
        let FirCallTarget::External { inline_plan, .. } = &call.target else {
            return None;
        };
        let reference = call.arguments.iter().find_map(|argument| match argument {
            FirCallArgument::Expression { value, .. }
                if matches!(
                    body.expr(*value).map(|expression| &expression.kind),
                    Some(FirExprKind::CallableReference { .. })
                ) =>
            {
                Some(value)
            }
            _ => None,
        })?;
        Some((inline_plan, reference))
    });
    let Some((inline_plan, _)) = foreach else {
        panic!("forEach must retain its checked callable-reference argument")
    };
    assert!(
        inline_plan.is_none(),
        "a callable-reference value has no lambda body to splice"
    );
}

#[test]
fn suspending_map_publishes_a_complete_declaration_scoped_collection_plan() {
    let (body, _) = checked_function_body_with_platform(
        "operator fun <K, V> Map<K, V>.iterator(): Iterator<Map.Entry<K, V>> =\n\
             emptyList<Map.Entry<K, V>>().iterator()\n\
         suspend fun render(entry: Map.Entry<String, Int>): String = entry.key\n\
         suspend fun collect(values: Map<String, Int>): List<String> =\n\
             values.map { render(it) }\n",
        "collect",
        jvm_stdlib_semantics(),
    );

    let plan = (0..body.expression_count()).find_map(|raw| {
        let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
            return None;
        };
        let FirCallTarget::External {
            inline_plan: Some(plan),
            ..
        } = &call.target
        else {
            return None;
        };
        matches!(plan.as_ref(), FirInlineBodyPlan::CollectionTransform { .. })
            .then_some(plan.as_ref())
    });
    let Some(FirInlineBodyPlan::CollectionTransform {
        lambda_parameter,
        flatten,
        iterator,
        factory,
        append,
        accumulator,
        append_parameter,
        ..
    }) = plan
    else {
        panic!("selected map call must publish its complete checked structural plan")
    };
    assert_eq!(*lambda_parameter, 0);
    assert!(!flatten);
    assert!(matches!(iterator.target, FirCallTarget::External { .. }));
    assert_ne!(factory, append);
    assert_eq!(
        accumulator.get(),
        Ty::obj_args("kotlin/collections/MutableList", &[Ty::String])
    );
    assert_eq!(append_parameter.get(), Ty::nullable(Ty::obj("kotlin/Any")));
}

#[test]
fn suspend_inline_finally_plan_is_fully_checked_and_opaque() {
    let classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB\n// WITH_COROUTINES");
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
    ));
    let (body, _) = checked_function_body_with_platform(
        "import kotlinx.coroutines.sync.Mutex\n\
         import kotlinx.coroutines.sync.withLock\n\
         suspend fun read(mutex: Mutex): String = mutex.withLock { \"OK\" }\n",
        "read",
        platform,
    );
    let plan = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::Call(call) = &expression.kind else {
            return None;
        };
        let FirCallTarget::External {
            inline_plan: Some(plan),
            ..
        } = &call.target
        else {
            return None;
        };
        matches!(
            plan.as_ref(),
            FirInlineBodyPlan::SuspendBeforeLambdaFinally { .. }
        )
        .then_some(plan.as_ref())
    });
    let Some(FirInlineBodyPlan::SuspendBeforeLambdaFinally {
        lambda_parameter,
        state_parameter,
        state_default,
        enter,
        cleanup,
    }) = plan
    else {
        panic!("withLock must publish its selected structural plan in checked FIR")
    };
    assert_eq!((*lambda_parameter, *state_parameter), (1, 0));
    assert_eq!(*state_default, FirInlineDefaultValue::Null);
    assert_eq!(enter.parameters.len(), 1);
    assert_eq!(cleanup.parameters.len(), 1);
    assert!(enter.suspend);
    assert!(!cleanup.suspend);
    assert_ne!(enter.declaration, cleanup.declaration);
}

#[test]
fn java_sam_platform_number_is_explicitly_narrowed_before_primitive_operator() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun test(sizes: java.util.HashMap<String, Int>) {\n\
             var total = 0\n\
             sizes.forEach { name, count -> total += count + name.length }\n\
         }\n",
        "test",
        jvm_stdlib_semantics(),
    );

    let lambda = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Lambda { body, .. } = &expression.kind else {
                return None;
            };
            Some(body.as_ref())
        })
        .expect("Java BiConsumer argument must remain a checked FIR lambda");
    let (source, target) = (0..lambda.expression_count())
        .find_map(|raw| {
            let expression = lambda.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Binary {
                operation: FirBinaryOperation::Add,
                lhs,
                ..
            } = expression.kind
            else {
                return None;
            };
            let converted = lambda.expr(lhs)?;
            let FirExprKind::ImplicitConversion {
                value,
                conversion:
                    FirConversion {
                        kind: FirConversionKind::SmartCast { to },
                        ..
                    },
            } = converted.kind
            else {
                return None;
            };
            Some((lambda.expr(value)?.ty.get(), to.get()))
        })
        .expect("the selected primitive plus must own its platform-value conversion");
    assert_eq!(source, Ty::platform_nullable(Ty::Int));
    assert_eq!(target, Ty::Int);
}

#[test]
fn named_context_parameter_is_a_value_not_an_implicit_member_receiver() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\n\
         class Scope { fun selected(): String = \"member\" }\n\
         fun selected(): String = \"top-level\"\n\
         context(scope: Scope) fun contextual(): String = scope.selected()\n\
         context(scope: Scope) fun use(): String = selected() + contextual()\n",
        "use",
    );

    let selected = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("selected")).then_some((call, target))
        })
        .expect("top-level selected call");
    let declaration = index
        .callable(selected.1)
        .expect("stable selected callable")
        .declaration;
    assert!(
        index
            .declaration_header(declaration)
            .expect("stable selected declaration")
            .owner
            .is_none(),
        "a named context parameter must not expose member callables unqualified",
    );
    assert!(selected.0.dispatch_receiver.is_none());

    let contextual = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("contextual")).then_some(call)
        })
        .expect("forwarded contextual call");
    let [FirCallArgument::Expression { value, .. }] = contextual.arguments.as_ref() else {
        panic!("contextual call must carry its selected context value")
    };
    assert!(matches!(
        body.expr(*value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn sam_constructor_accepts_flattened_value_parameters_for_context_parameters() {
    let (body, _) = checked_function_body(
        "class A\n\
         fun interface Action { context(a: A) fun run(value: String): String }\n\
         fun target(a: A, value: String): String = value\n\
         fun make(): Action = Action(::target)\n",
        "make",
    );

    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitConversion {
            conversion: FirConversion {
                kind: FirConversionKind::Sam(_),
                ..
            },
            ..
        })
    ));
}

#[test]
fn sam_constructor_accepts_a_nominal_callable_supertype() {
    let (body, _) = checked_function_body_with_platform(
        "fun interface Action { fun Int.run(): String }\n\
         class Target : (Int) -> String { override fun invoke(value: Int): String = \"OK\" }\n\
         fun make(): Action = Action(Target())\n",
        "make",
        jvm_semantics(),
    );

    assert!(matches!(
        body.expr(root_expression(&body))
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitConversion {
            conversion: FirConversion {
                kind: FirConversionKind::Sam(_),
                ..
            },
            ..
        })
    ));
}

#[test]
fn regular_function_value_to_suspend_function_is_an_explicit_checked_conversion() {
    let (body, _) = checked_function_body(
        "fun convert(block: (Int) -> String): suspend (Int) -> String = block\n",
        "convert",
    );

    let FirExprKind::ImplicitConversion {
        conversion:
            FirConversion {
                kind: FirConversionKind::SuspendFunction { from, to },
                ..
            },
        ..
    } = body
        .expr(root_expression(&body))
        .expect("converted function value")
        .kind
    else {
        panic!("regular-to-suspend adaptation must be explicit checked FIR")
    };
    assert!(matches!(
        from.get(),
        Ty::Fun(signature)
            if !signature.suspend
                && signature.params == [Ty::Int]
                && signature.ret == Ty::String
    ));
    assert!(matches!(
        to.get(),
        Ty::Fun(signature)
            if signature.suspend
                && signature.params == [Ty::Int]
                && signature.ret == Ty::String
    ));
}

#[test]
fn functional_intersection_selects_the_matching_constituent_for_suspend_conversion() {
    let (body, _) = checked_function_body(
        "fun consume(block: suspend (Int) -> String): Unit {}\n\
         fun <T> test(value: T): Unit where T : () -> String, T : (Int) -> String {\n\
             consume(value)\n\
         }\n",
        "test",
    );

    let conversion = (0..body.expression_count()).find_map(|raw| {
        let FirExprKind::Call(call) = &body
            .expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?
            .kind
        else {
            return None;
        };
        call.arguments.iter().find_map(|argument| {
            let FirCallArgument::Expression {
                conversion: Some(conversion),
                ..
            } = argument
            else {
                return None;
            };
            matches!(conversion.kind, FirConversionKind::SuspendFunction { .. })
                .then_some(conversion)
        })
    });
    let Some(FirConversion {
        kind: FirConversionKind::SuspendFunction { from, to },
        ..
    }) = conversion
    else {
        panic!("the matching callable intersection constituent must be converted")
    };
    assert!(matches!(
        from.get(),
        Ty::Fun(signature)
            if !signature.suspend
                && signature.params == [Ty::Int]
                && signature.ret == Ty::String
    ));
    assert!(matches!(
        to.get(),
        Ty::Fun(signature)
            if signature.suspend
                && signature.params == [Ty::Int]
                && signature.ret == Ty::String
    ));
}

#[test]
fn flow_intersection_type_argument_preserves_all_bounds_in_checked_fir() {
    let (body, index) = checked_function_body(
        "interface A\n\
         interface B\n\
         inline fun <reified K> select(x: K): K where K : A, K : B = x\n\
         fun test(value: Any) { if (value is A && value is B) select(value) }\n",
        "test",
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("select")).then_some(call)
        })
        .expect("intersection-constrained generic call");
    let [substitution] = call.substitutions.as_ref() else {
        panic!("select must publish its inferred reified type argument")
    };
    assert_eq!(substitution.value.get(), Ty::obj("A"));
    assert_eq!(
        substitution
            .additional_bounds
            .iter()
            .map(|bound| bound.get())
            .collect::<Vec<_>>(),
        vec![Ty::obj("B")]
    );
    let [FirCallArgument::Expression {
        conversion:
            Some(FirConversion {
                kind: FirConversionKind::SmartCast { to },
                ..
            }),
        ..
    }] = call.arguments.as_ref()
    else {
        panic!("the argument must retain its primary intersection projection")
    };
    assert_eq!(to.get(), Ty::obj("A"));
}

#[test]
fn generic_identity_retains_an_incomparable_declared_smart_cast_constituent() {
    let (body, index) = checked_function_body(
        "abstract class A { abstract fun o(): String }\n\
         interface B\n\
         fun <T> id(x: T): T = x\n\
         fun test(a: A?): String { if (a is B) return id(a).o(); return \"\" }\n",
        "test",
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("id")).then_some(call)
        })
        .expect("intersection-constrained identity call");
    let [substitution] = call.substitutions.as_ref() else {
        panic!("id must publish its inferred type argument")
    };
    assert_eq!(substitution.value.get(), Ty::obj("A"));
    assert_eq!(
        substitution
            .additional_bounds
            .iter()
            .map(|bound| bound.get())
            .collect::<Vec<_>>(),
        vec![Ty::obj("B")]
    );
}

#[test]
fn context_suspend_value_selects_extension_declared_with_receiver_notation() {
    let (body, index) = checked_function_body(
        "fun (suspend String.() -> Unit).start(value: String): Unit {}\n\
         fun run(block: suspend context(String) () -> Unit): Unit { block.start(\"OK\") }\n",
        "run",
    );

    let call = (0..body.expression_count()).find_map(|raw| {
        let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
            return None;
        };
        call.target.module().map(|target| (call, target))
    });
    let Some((call, target)) = call else {
        panic!("context-shaped suspend value must keep its selected extension call")
    };
    assert_eq!(
        index
            .callable(target)
            .and_then(|callable| index.callable_name(callable.id)),
        Some("start"),
    );
    assert!(call.extension_receiver.is_some());
}

#[test]
fn safe_any_member_call_remains_resolvable_on_a_bottom_narrowed_type_parameter() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T> compare(value: T?): Boolean? {\n\
             if (value == null) return value?.equals(1)\n\
             return false\n\
         }\n",
        "compare",
        jvm_stdlib_semantics(),
    );

    let safe_call = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::SafeCall { .. }))
        .expect("bottom-narrowed safe call");
    assert_eq!(safe_call.ty.get(), Ty::nullable(Ty::Boolean));
    let FirExprKind::SafeCall { selector, .. } = safe_call.kind else {
        unreachable!()
    };
    assert!(matches!(
        body.expr(selector).map(|expression| &expression.kind),
        Some(FirExprKind::Call(_))
    ));
}

#[test]
fn parameterized_upper_bound_keeps_member_result_type_in_checked_fir() {
    let (body, _) = checked_function_body_with_platform(
        "class Holder<T : MutableList<String>>(val value: T) {\n\
             fun read(index: Int): String = value.get(index)\n\
         }\n",
        "read",
        jvm_stdlib_semantics(),
    );

    let root = body.expr(root_expression(&body)).expect("member call");
    assert_eq!(root.ty.get(), Ty::String);
    let FirExprKind::Call(call) = &root.kind else {
        panic!("bounded-receiver member access must become checked call FIR")
    };
    let FirCallTarget::External { result, .. } = call.target else {
        panic!("the selected MutableList member must keep its external identity")
    };
    assert_eq!(result.get(), Ty::String);
}

#[test]
fn extension_call_target_keeps_the_checked_specialized_result_type() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun adapt(value: Result<Int>): Result<Int> = value.onFailure { }\n",
        "adapt",
        jvm_stdlib_semantics(),
    );

    let call = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let FirCallTarget::External {
                result,
                declared_result,
                ..
            } = &call.target
            else {
                return None;
            };
            call.extension_receiver
                .is_some()
                .then_some((*result, *declared_result))
        })
        .expect("onFailure must remain a checked external extension call");
    assert_eq!(call.0.get(), Ty::obj_args("kotlin/Result", &[Ty::Int]));
    assert_eq!(
        call.1
            .map(ResolvedTy::get)
            .map(Ty::non_null)
            .and_then(Ty::obj_internal),
        Some(crate::types::type_name("kotlin/Result")),
    );
}

#[test]
fn collection_plus_contextualizes_a_generic_constructor_argument_through_its_supertype() {
    let (body, _) = checked_function_body_with_platform(
        "fun read(): List<Int> {\n\
             val values: MutableCollection<Int> = ArrayList()\n\
             return values + ArrayList()\n\
         }\n",
        "read",
        jvm_stdlib_semantics(),
    );

    let plus = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            (call.extension_receiver.is_some() && expression.ty.get().type_args() == [Ty::Int])
                .then_some(call)
        })
        .expect("collection plus must remain a checked extension call");
    let [FirCallArgument::Expression { value, .. }] = plus.arguments.as_ref() else {
        panic!("collection plus must retain its contextual constructor argument")
    };
    assert_eq!(
        body.expr(*value).expect("constructor argument").ty.get(),
        Ty::obj_args("java/util/ArrayList", &[Ty::Int])
    );
}

#[test]
fn receiver_instantiates_callable_parameter_before_explicit_extension_lambda_checking() {
    let (body, index) = checked_function_body(
        "class MyList<T>\n\
         operator fun <T> MyList<T>.plusAssign(element: T) {}\n\
         val functions = MyList<(Int) -> Int>()\n\
         fun update() { functions.plusAssign({ it -> it }) }\n",
        "update",
    );

    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index
                .callable(target)
                .and_then(|callable| index.callable_name(callable.id))
                == Some("plusAssign"))
            .then_some(call)
        })
        .expect("explicit extension call");
    let [FirCallArgument::Expression { value, .. }] = call.arguments.as_ref() else {
        panic!("plusAssign must keep its checked lambda argument")
    };
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body.expr(*value).expect("lambda argument").kind
    else {
        panic!("plusAssign argument must remain a checked FIR lambda")
    };
    let [parameter] = lambda_body.parameters() else {
        panic!("the contextual lambda must own its implicit parameter")
    };
    assert_eq!(parameter.ty, ResolvedTy::new(Ty::Int).unwrap());
}

#[test]
fn explicit_function_classifier_star_projection_contextually_types_lambda() {
    let (body, _) = checked_function_body_with_platform(
        "fun accept(block: Function1<Any?, *>): Any? = block(\"OK\")\n\
         fun box(): Any? = accept { it }\n",
        "box",
        jvm_stdlib_semantics(),
    );

    let (lambda, lambda_body) = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Lambda {
                body: lambda_body, ..
            } = &expression.kind
            else {
                return None;
            };
            Some((expression, lambda_body))
        })
        .expect("selected function-classifier parameter must retain a checked FIR lambda");
    let nullable_any = Ty::nullable(Ty::obj("kotlin/Any"));
    assert_eq!(lambda.ty.get(), Ty::fun(vec![nullable_any], nullable_any));
    let [parameter] = lambda_body.parameters() else {
        panic!("the contextual lambda must own its implicit parameter")
    };
    assert_eq!(parameter.ty, ResolvedTy::new(nullable_any).unwrap());
}

#[test]
fn explicit_external_generic_vararg_element_contextually_installs_receiver_lambda() {
    let (body, index) = checked_function_body_with_platform(
        "class Canvas { fun rect(value: Int) {} }\n\
         fun box() { listOf<Canvas.() -> Unit>({ rect(1) }) }\n",
        "box",
        jvm_stdlib_semantics(),
    );

    let lambda_body = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Lambda {
                body: lambda_body, ..
            } = &expression.kind
            else {
                return None;
            };
            Some(lambda_body)
        })
        .expect("generic vararg element must retain a checked receiver lambda");
    assert_eq!(
        lambda_body.receiver_type().map(ResolvedTy::get),
        Some(Ty::obj("Canvas"))
    );
    assert!((0..lambda_body.expression_count()).any(|raw| {
        let Some(expression) = lambda_body.expr(FirExprId::from_raw(raw as u32)) else {
            return false;
        };
        let FirExprKind::Call(call) = &expression.kind else {
            return false;
        };
        call.target
            .module()
            .and_then(|target| index.callable_name(target))
            == Some("rect")
    }));
}

#[test]
fn every_explicit_external_generic_vararg_element_gets_its_receiver_lambda_shape() {
    let (body, _) = checked_function_body_with_platform(
        "class Canvas { fun rect(value: Int) {} }\n\
         fun box() { listOf<Canvas.() -> Unit>({ rect(1) }, { rect(2) }) }\n",
        "box",
        jvm_stdlib_semantics(),
    );

    let receiver_types = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Lambda {
                body: lambda_body, ..
            } = &expression.kind
            else {
                return None;
            };
            Some(lambda_body.receiver_type().map(ResolvedTy::get))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        receiver_types,
        vec![Some(Ty::obj("Canvas")), Some(Ty::obj("Canvas"))]
    );
}

#[test]
fn nominal_suspend_function_supertype_selects_function_extension() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.coroutines.*\n\
         class Task : suspend () -> Int {\n\
             override suspend fun invoke(): Int = 42\n\
         }\n\
         fun launch(task: Task) {\n\
             task.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
         }\n",
        "launch",
        jvm_stdlib_semantics(),
    );

    let call = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let FirCallTarget::External {
                receiver: Some(receiver),
                result,
                ..
            } = &call.target
            else {
                return None;
            };
            matches!(receiver.get(), Ty::Fun(signature) if signature.suspend)
                .then_some((call, receiver, result))
        })
        .expect("startCoroutine must be selected as a checked function-type extension");
    assert_eq!(call.1.get(), Ty::fun_suspend(Vec::new(), Ty::Int));
    assert_eq!(call.2.get(), Ty::Unit);
    let extension = call
        .0
        .extension_receiver
        .expect("nominal task must remain the runtime extension receiver");
    assert_eq!(
        body.expr(extension.value)
            .expect("checked nominal receiver")
            .ty
            .get(),
        Ty::obj("Task")
    );
}

#[test]
fn postponed_provider_lambda_uses_the_enclosing_stable_type_parameter() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.coroutines.*\n\
         fun <T> launch(task: suspend () -> T) {\n\
             var result: Result<T>? = null\n\
             task.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })\n\
         }\n",
        "launch",
        jvm_stdlib_semantics(),
    );

    let callback = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(expression.kind, FirExprKind::Lambda { .. }).then_some(expression)
        })
        .expect("Continuation callback must become a checked FIR lambda");
    let Ty::Fun(signature) = callback.ty.get() else {
        panic!("Continuation callback must retain its checked function type")
    };
    let [Ty::Obj(result, arguments)] = signature.params.as_slice() else {
        panic!("Continuation callback must receive Result<T>")
    };
    assert!(result.matches("kotlin/Result"));
    assert!(matches!(arguments, [Ty::TyParam(name, _)] if name.starts_with('\0')));
}

#[test]
fn array_typealias_constructors_keep_expanded_checked_array_shapes() {
    let (body, _) = checked_function_body(
        "typealias BoolArray = Array<Boolean>\n\
         typealias IArray = IntArray\n\
         typealias GenericArray<T> = Array<T>\n\
         fun construct() {\n\
             BoolArray(1) { true }\n\
             IArray(1) { 42 }\n\
             GenericArray<Int>(1) { 42 }\n\
         }\n",
        "construct",
    );

    let mut arrays = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(expression.kind, FirExprKind::ArrayConstruction { .. })
                .then_some(expression.ty.get())
        })
        .collect::<Vec<_>>();
    arrays.sort_by_key(|ty| ty.source_name());
    assert_eq!(arrays.len(), 3);
    assert!(arrays.contains(&Ty::obj_args("kotlin/Array", &[Ty::Boolean])));
    assert!(arrays.contains(&Ty::obj("kotlin/IntArray")));
    assert!(arrays.contains(&Ty::obj_args("kotlin/Array", &[Ty::Int])));
}

#[test]
fn bottom_array_initializer_keeps_the_contextual_element_type() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun use(): String {\n\
             val unused: Array<Int> = Array(42, return \"OK\")\n\
         }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let (element_type, initializer) = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::ArrayConstruction {
                element_type,
                initializer: Some(initializer),
                ..
            } = expression.kind
            else {
                return None;
            };
            Some((element_type, initializer))
        })
        .expect("checked contextual array construction");
    assert_eq!(element_type.get(), Ty::Int);
    assert!(matches!(
        body.expr(initializer).map(|expression| &expression.kind),
        Some(FirExprKind::Jump {
            kind: FirJumpKind::Return { .. },
            ..
        })
    ));
}

#[test]
fn generic_overloads_use_the_primary_class_bound_for_declaration_identity() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun <T> choose(value: T): Any = value\n\
         fun <T> choose(value: T): CharSequence where T : Comparable<T> = \"comparable\"\n\
         fun <T> choose(value: T): String where T : Comparable<T>, T : Number = \"number\"\n\
         fun selected(value: Int): String = choose(value)\n",
        "selected",
        jvm_stdlib_semantics(),
    );
    let root = body
        .expr(root_expression(&body))
        .expect("selected generic overload call");
    assert_eq!(root.ty.get(), Ty::String);
    assert!(matches!(root.kind, FirExprKind::Call(_)));
}

#[test]
fn multiline_member_call_keeps_the_selected_call_source_and_end_lines() {
    let (body, index) = checked_function_body(
        "class Recorder {\n\
             suspend fun save(label: String, value: Int): Int = value\n\
         }\n\
         suspend fun work(recorder: Recorder): Int {\n\
             val result = recorder.save(\n\
                 value = 3,\n\
                 label = \"entry\",\n\
             )\n\
             return result\n\
         }\n",
        "work",
    );
    let call = (0..body.expression_count())
        .map(|raw| FirExprId::from_raw(raw as u32))
        .find(|expression| {
            let Some(FirExpr {
                kind: FirExprKind::Call(call),
                ..
            }) = body.expr(*expression)
            else {
                return false;
            };
            call.target
                .module()
                .and_then(|target| index.callable_name(target))
                == Some("save")
        })
        .expect("selected suspend member call");
    assert_eq!(
        body.expression_debug_lines(call),
        FirExpressionDebugLines { source: 5, end: 8 }
    );
}
