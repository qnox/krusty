use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_semantics,
    jvm_stdlib_semantics, root_expression,
};
use super::*;

#[test]
fn elvis_mixed_numeric_branches_retain_their_number_supertype() {
    let platform = jvm_stdlib_semantics();
    let double_hierarchy = crate::symbol_resolver::applied_hierarchy(&*platform, Ty::Double);
    let int_hierarchy = crate::symbol_resolver::applied_hierarchy(&*platform, Ty::Int);
    assert!(
        double_hierarchy
            .iter()
            .any(|(owner, _, _)| *owner == crate::types::type_name("kotlin/Number")),
        "Double semantic hierarchy: {double_hierarchy:?}"
    );
    assert!(
        int_hierarchy
            .iter()
            .any(|(owner, _, _)| *owner == crate::types::type_name("kotlin/Number")),
        "Int semantic hierarchy: {int_hierarchy:?}"
    );
    let (body, _) = checked_function_body_with_platform(
        "fun processNumber(number: Number): Number = number\n\
         fun use(): Number {\n\
             val double: Double? = 0.0\n\
             return processNumber(double ?: 0)\n\
         }\n",
        "use",
        platform,
    );
    let elvis = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::Elvis { .. }))
        .expect("checked Elvis expression");
    assert_eq!(elvis.ty.get(), Ty::obj("kotlin/Number"));
}

#[test]
fn elvis_generic_fallback_is_constrained_through_the_left_branch_supertype() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun choose(value: MutableList<String>?): List<String> {\n\
             val result = value ?: emptyList()\n\
             return result\n\
         }\n",
        "choose",
        jvm_stdlib_semantics(),
    );
    let result = (0..body.statement_count())
        .find_map(|raw| {
            let statement = body.statement(FirStatementId::from_raw(raw as u32))?;
            let FirStatementKind::Local { ty, .. } = &statement.kind else {
                return None;
            };
            Some(ty.get())
        })
        .expect("checked result local");
    assert_eq!(
        result,
        Ty::obj_args("kotlin/collections/List", &[Ty::String])
    );
}

#[test]
fn elvis_generic_fallback_is_contextually_typed_by_an_outer_call_parameter() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun append(target: MutableList<Int>) {\n\
             target.addAll(null ?: emptyList())\n\
         }\n",
        "append",
        jvm_stdlib_semantics(),
    );
    let elvis = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::Elvis { .. }))
        .expect("checked contextual Elvis argument");
    assert_eq!(
        elvis.ty.get(),
        Ty::obj_args("kotlin/collections/List", &[Ty::Int])
    );
}

#[test]
fn definitely_non_null_reified_throwable_is_a_checked_catch_type() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +AllowReifiedTypeInCatchClause\n\
         inline fun <reified E : Throwable?> catch(value: Throwable): String =\n\
             try { throw value } catch (error: E & Any) { \"OK\" }\n",
        "catch",
        jvm_semantics(),
    );
    let FirExprKind::Try { catches, .. } = &body
        .expr(root_expression(&body))
        .expect("try expression")
        .kind
    else {
        panic!("inline catch body must remain checked try FIR")
    };
    let [catch] = catches.as_ref() else {
        panic!("try expression must retain its catch")
    };
    let Ty::TyParam(_, bound) = catch.parameter_ty.get() else {
        panic!("catch parameter must retain its declaration-owned reified identity")
    };
    assert_eq!(*bound, Ty::obj("kotlin/Throwable"));
}

#[test]
fn try_and_catch_consume_their_own_entry_edge_flow_facts() {
    let (body, index) = checked_function_body_with_platform(
        "class C(val value: String) {\n\
             fun getField(): String = value\n\
             fun getMethod(): Unit {}\n\
         }\n\
         fun test(): Any {\n\
             var result: Any = \"\"\n\
             result = C(\"OK\")\n\
             try {\n\
                 result = result.getField()\n\
             } catch (error: Throwable) {\n\
                 result.getMethod()\n\
             }\n\
             return result\n\
         }\n",
        "test",
        jvm_semantics(),
    );

    let selected = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            index.callable(target).and_then(|callable| {
                let name = index.callable_name(callable.id)?;
                matches!(name, "getField" | "getMethod").then_some(name)
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(selected, ["getField", "getMethod"]);
}

#[test]
fn equality_accepts_a_concrete_inhabitant_of_a_type_parameter_bound() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T : Comparable<Double>> same(value: T, other: Double): Boolean = value == other\n",
        "same",
        jvm_semantics(),
    );
    let root = body
        .expr(root_expression(&body))
        .expect("equality expression");
    assert!(matches!(
        root.kind,
        FirExprKind::Binary {
            operation: FirBinaryOperation::Equal,
            ..
        }
    ));
}

#[test]
fn equality_accepts_erasure_overlapping_generic_interface_views() {
    let (body, _) = checked_function_body_with_platform(
        "fun same(value: Double, other: Comparable<Float>): Boolean = value == other\n",
        "same",
        jvm_semantics(),
    );
    let root = body
        .expr(root_expression(&body))
        .expect("equality expression");
    assert!(matches!(
        root.kind,
        FirExprKind::Binary {
            operation: FirBinaryOperation::Equal,
            ..
        }
    ));
}

#[test]
fn equality_bound_refines_parameter_and_equal_stable_operand_in_checked_fir() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +StrictEquals\n\
         class A(val n: Int) {\n\
             override fun equals(@EqualityBound(A::class) other: Any?): Boolean = n == other.n\n\
         }\n\
         fun read(x: A?, erased: Any?): Int {\n\
             if (x != null && x == erased) return erased.n\n\
             return 0\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    let receiver = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::PropertyRead {
                dispatch_receiver: Some(receiver),
                ..
            } = &expression.kind
            else {
                return None;
            };
            (expression.ty.get() == Ty::Int).then_some(receiver.value)
        })
        .expect("strict-equality branch must retain the selected property receiver");
    let receiver = body.expr(receiver).expect("checked erased-parameter read");
    assert_eq!(receiver.ty.get(), Ty::obj("A"));
    assert!(matches!(
        receiver.kind,
        FirExprKind::ImplicitConversion {
            conversion: FirConversion {
                kind: FirConversionKind::SmartCast { .. },
                ..
            },
            ..
        }
    ));
}

#[test]
fn stable_top_level_val_flow_intersection_publishes_primitive_read() {
    let (body, _) = checked_function_body_with_platform(
        "val minus: Any = -0.0\n\
         fun less(): Boolean = if (minus is Comparable<*> && minus is Double) minus < 0.0 else false\n",
        "less",
        jvm_semantics(),
    );
    let comparison = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::Binary {
                operation: FirBinaryOperation::Less,
                lhs,
                ..
            } => Some(*lhs),
            FirExprKind::ComparisonCall {
                operation: FirBinaryOperation::Less,
                call,
            } => call
                .dispatch_receiver
                .as_ref()
                .or(call.extension_receiver.as_ref())
                .map(|receiver| receiver.value),
            _ => None,
        })
        .expect("guarded branch must retain primitive comparison FIR");
    let lhs = body.expr(comparison).expect("comparison lhs");
    assert_eq!(lhs.ty.get(), Ty::Double);
    assert!(matches!(lhs.kind, FirExprKind::PropertyRead { .. }));
}

#[test]
fn flow_intersection_survives_nested_early_return_comparisons() {
    let _ = checked_function_body_with_platform(
        "val minus: Any = -0.0\n\
         fun box(): String {\n\
             if (minus is Comparable<*> && minus is Double) {\n\
                 if (minus < 0.0) return \"fail 0\"\n\
                 if (minus != 0.0) return \"fail 1\"\n\
                 if (minus != 0.0F) return \"fail 2\"\n\
             }\n\
             return \"OK\"\n\
         }\n",
        "box",
        jvm_semantics(),
    );
}

#[test]
fn subject_when_else_excludes_an_earlier_null_condition() {
    let _ = checked_function_body(
        "class Failure\n\
         fun consume(value: Failure): String = \"OK\"\n\
         fun test(value: Failure?): String = when (val failure = value) {\n\
             null -> \"none\"\n\
             else -> consume(failure)\n\
         }\n",
        "test",
    );
}

#[test]
fn successful_type_test_on_safe_call_narrows_its_nullable_root() {
    let _ = checked_function_body(
        "open class Operand\n\
         class Statement(val operand: Operand)\n\
         fun consume(value: Operand) {}\n\
         fun test(statement: Statement?) {\n\
             if (statement?.operand is Operand) consume(statement.operand)\n\
         }\n",
        "test",
    );
}

#[test]
fn conditional_branch_projects_a_flow_intersection_to_its_contextual_type_parameter() {
    let _ = checked_function_body(
        "class A\n\
         fun <T> test(v: T): T {\n\
             val result: T = if (v !is A) v else v\n\
             return result\n\
         }\n",
        "test",
    );
}

#[test]
fn nullable_context_does_not_widen_a_non_null_conditional_join() {
    let _ = checked_function_body(
        "fun choose(value: Double?): String {\n\
             var enabled: Boolean? = null\n\
             val positive: Boolean = value!! > 0.0\n\
             enabled = if (enabled == null) positive else enabled && positive\n\
             return if (enabled) \"OK\" else \"fail\"\n\
         }\n",
        "choose",
    );
}

#[test]
fn assignment_inside_the_current_lambda_updates_its_straight_line_flow_type() {
    let _ = checked_function_body_with_platform(
        "interface I\n\
         interface I2 : I { fun func(): String }\n\
         class A : I2 { override fun func(): String = \"OK\" }\n\
         class B : I2 { override fun func(): String = \"Fail\" }\n\
         fun <T : I2> materialize(): T = A() as T\n\
         var condition = true\n\
         fun myRun(block: () -> String): String = block()\n\
         fun box(): String {\n\
             var value: I = object : I {}\n\
             return myRun {\n\
                 value = when (condition) {\n\
                     true -> materialize()\n\
                     else -> B()\n\
                 }\n\
                 value.func()\n\
             }\n\
         }\n",
        "box",
        jvm_semantics(),
    );
}

#[test]
fn unsigned_is_check_narrows_to_the_semantic_scalar_in_checked_fir() {
    let _ = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun consume(value: UInt): Unit {}\n\
         fun test(value: Any) { if (value is UInt) consume(value) }\n",
        "test",
        jvm_stdlib_semantics(),
    );
}

#[test]
fn nullable_primitive_referential_equality_is_a_checked_fir_operation() {
    let (body, _) = checked_function_body(
        "fun same(left: Int?, right: Int?): Boolean = left === right\n",
        "same",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("checked referential equality");
    assert!(matches!(
        root.kind,
        FirExprKind::Binary {
            operation: FirBinaryOperation::ReferentialEqual,
            ..
        }
    ));
}

#[test]
fn nullable_primitive_and_scalar_identity_keep_semantic_operand_types() {
    let (body, _) = checked_function_body(
        "fun same(value: Double?): Boolean = value === -0.0\n",
        "same",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("checked referential equality");
    let FirExprKind::Binary {
        operation,
        lhs,
        rhs,
    } = root.kind
    else {
        panic!("mixed nullable/scalar identity must remain checked binary FIR")
    };
    assert_eq!(operation, FirBinaryOperation::ReferentialEqual);
    assert_eq!(
        body.expr(lhs).expect("left operand").ty.get(),
        Ty::nullable(Ty::Double)
    );
    assert_eq!(body.expr(rhs).expect("right operand").ty.get(), Ty::Double);
}

#[test]
fn string_referential_inequality_is_a_checked_fir_operation() {
    let (body, _) = checked_function_body(
        "fun different(left: String?, right: String?): Boolean = left !== right\n",
        "different",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("checked String referential inequality");
    assert!(matches!(
        root.kind,
        FirExprKind::Binary {
            operation: FirBinaryOperation::ReferentialNotEqual,
            ..
        }
    ));
}

#[test]
fn referential_null_inequality_narrows_a_nullable_primitive() {
    let _ = checked_function_body(
        "fun consume(vararg values: Float): Unit {}\n\
         fun test(value: Float?): Unit {\n\
             if (value !== null) consume(value)\n\
         }\n",
        "test",
    );
}

#[test]
fn reflective_and_function_views_of_the_same_reference_have_a_function_join() {
    let _ = checked_function_body(
        "fun identity(value: Int): Int = value\n\
         fun <T> keep(value: T): T = value\n\
         fun selected(): (Int) -> Int = keep(::identity ?: ::identity)\n",
        "selected",
    );
}

#[test]
fn inferred_member_extension_has_next_validates_its_semantic_boolean_result() {
    let _ = checked_function_body_with_platform(
        "class It\n\
         class X {\n\
             var ready = true\n\
             operator fun It.hasNext() = if (ready) { ready = false; true } else false\n\
         }\n",
        "hasNext",
        jvm_semantics(),
    );
}

#[test]
fn generic_extension_receiver_infers_from_a_type_parameters_applied_bound() {
    let _ = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun <T : Iterable<*>> test(values: T) {\n\
             val indexed = values.withIndex()\n\
         }\n",
        "test",
        jvm_stdlib_semantics(),
    );
}

#[test]
fn elvis_with_unit_branches_is_retained_as_checked_fir() {
    let (body, _) = checked_function_body(
        "class C { fun run(): Unit {} }\n\
         fun fallback(): Unit {}\n\
         fun test(value: C?) { value?.run() ?: fallback() }\n",
        "test",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::Elvis { .. })
        )
    }));
}

#[test]
fn nullable_bottom_elvis_does_not_constrain_generic_unit_fallback_to_nothing() {
    let _ = checked_function_body(
        "fun <T> produce(block: () -> T): T = block()\n\
         fun launch(block: () -> Unit) {}\n\
         fun test() {\n\
             launch {\n\
                 null ?: produce { val completed = true }\n\
             }\n\
         }\n",
        "test",
    );
}

#[test]
fn when_mixed_scalar_reference_and_unit_branches_publish_any() {
    let (body, _) = checked_function_body_with_platform(
        "fun read(value: Any): String {\n\
             val result = when (value) {\n\
                 is String -> value.toString()\n\
                 is Long -> value + 10\n\
                 else -> {}\n\
             }\n\
             return result.toString()\n\
         }\n",
        "read",
        jvm_semantics(),
    );
    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("function body must be a checked block")
    };
    let FirStatementKind::Local { ty, .. } =
        body.statement(statements[0]).expect("result local").kind
    else {
        panic!("when result must initialize a checked local")
    };
    assert_eq!(ty.get(), Ty::obj("kotlin/Any"));
}

#[test]
fn when_smartcast_of_star_projected_property_selects_member_overloads() {
    let (body, index) = checked_function_body_with_platform(
        "class Foo\n\
         class Bar\n\
         class Container<out T>(val item: T)\n\
         class Binder {\n\
             fun choose(subject: Container<*>): String = when (subject.item) {\n\
                 is Foo -> consume(subject.item)\n\
                 is Bar -> consume(subject.item)\n\
                 else -> \"\"\n\
             }\n\
             private fun consume(value: Foo): String = \"foo\"\n\
             private fun consume(value: Bar): String = \"bar\"\n\
         }\n",
        "choose",
        jvm_semantics(),
    );

    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let target = call.target.module()?;
            let callable = index.callable(target)?;
            (index.callable_name(callable.id) == Some("consume")).then_some((call, callable))
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);

    let selected = calls
        .iter()
        .map(|(call, callable)| {
            let signature = index
                .signature(callable.declaration)
                .expect("selected consume signature");
            let [parameter] = signature.parameters.as_ref() else {
                panic!("consume overload must have one parameter")
            };
            let [FirCallArgument::Expression { value, .. }] = call.arguments.as_ref() else {
                panic!("consume call must retain one checked argument")
            };
            let argument = body.expr(*value).expect("checked consume argument");
            assert!(matches!(argument.kind, FirExprKind::PropertyRead { .. }));
            assert_eq!(argument.ty, *parameter);
            parameter.get()
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        selected,
        [Ty::obj("Foo"), Ty::obj("Bar")].into_iter().collect()
    );
}

#[test]
fn nested_sealed_subclass_supertype_uses_the_nearest_lexical_classifier() {
    let (body, _) = checked_function_body_with_platform(
        "interface Interpreter { interface Intermediary }\n\
         class CountingInterpreter {\n\
             sealed interface Intermediary : Interpreter.Intermediary {\n\
                 class Keep : Intermediary\n\
             }\n\
         }\n\
         fun read(step: CountingInterpreter.Intermediary): Int = when (step) {\n\
             is CountingInterpreter.Intermediary.Keep -> 1\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::Int));
}

#[test]
fn early_return_enum_exclusion_makes_the_remaining_when_exhaustive() {
    let (body, _) = checked_function_body_with_platform(
        "enum class Choice { A, B, C }\n\
         fun read(value: Choice): Int {\n\
             if (value == Choice.A) return 1\n\
             return when (value) {\n\
                 Choice.B -> 2\n\
                 Choice.C -> 3\n\
             }\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::Int));
}

#[test]
fn early_return_boolean_exclusion_makes_the_remaining_when_exhaustive() {
    let (body, _) = checked_function_body_with_platform(
        "fun read(value: Boolean): Int {\n\
             if (value) return 1\n\
             return when (value) {\n\
                 false -> 2\n\
             }\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::Int));
}

#[test]
fn nested_boolean_equality_projects_nullable_exclusions_to_the_remaining_when() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +DataFlowBasedExhaustiveness\n\
         fun read(value: Boolean?): Int {\n\
             if ((value == true) == false) return 1\n\
             return when (value) {\n\
                 true -> 2\n\
             }\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::Int));
}

#[test]
fn early_return_sealed_exclusion_makes_the_remaining_when_exhaustive() {
    let (body, _) = checked_function_body_with_platform(
        "sealed interface Choice\n\
         class A : Choice\n\
         class B : Choice\n\
         fun read(value: Choice): Int {\n\
             if (value is A) return 1\n\
             return when (value) {\n\
                 is B -> 2\n\
             }\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::Int));
}

#[test]
fn short_circuit_exit_excludes_the_sealed_singleton_from_the_remaining_when() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +DataFlowBasedExhaustiveness\n\
         sealed class Choice {\n\
             object A : Choice()\n\
             object B : Choice()\n\
             object C : Choice()\n\
         }\n\
         fun read(value: Choice): Int {\n\
             (value is Choice.A) && return 1\n\
             return when (value) {\n\
                 Choice.B -> 2\n\
                 Choice.C -> 3\n\
             }\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::Int));
}

#[test]
fn unconditional_return_argument_terminates_a_block_body() {
    let (body, _) = checked_function_body_with_platform(
        "fun consume(first: Any, second: Any) {}\n\
         fun read(flag: Boolean): String {\n\
             var result = \"OK\"\n\
             consume(if (flag) 1 else 2, return result)\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::String));
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::Jump {
                kind: FirJumpKind::Return { .. },
                ..
            })
        )
    }));
}

#[test]
fn deferred_val_keeps_the_precise_try_assignment_result_type() {
    let (body, _) = checked_function_body_with_platform(
        "interface I\n\
         interface I2 : I { fun func(): String }\n\
         class Value : I2 { override fun func(): String = \"OK\" }\n\
         fun <T : I2> materialize(): T = Value() as T\n\
         fun use(): String {\n\
             val value: I\n\
             value = try { materialize() } catch (error: Throwable) { Value() }\n\
             return value.func()\n\
         }\n",
        "use",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(ResolvedTy::get), Some(Ty::String));
    assert!((0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32))
            .is_some_and(|expression| matches!(expression.kind, FirExprKind::Call(_)))
    }));
}

#[test]
fn do_while_condition_keeps_body_block_declarations_in_scope() {
    let (body, _) = checked_function_body_with_platform(
        "fun read(): String {\n\
             do {\n\
                 val marker = 1\n\
             } while (marker != 1)\n\
             return \"OK\"\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("function body must be a checked block")
    };
    let FirStatementKind::Loop {
        header: FirLoopHeader::DoWhile { condition },
        ..
    } = body
        .statement(statements[0])
        .expect("do-while statement")
        .kind
    else {
        panic!("first statement must be a checked do-while loop")
    };
    assert_eq!(
        body.expr(condition).map(|expression| expression.ty.get()),
        Some(Ty::Boolean)
    );
}

#[test]
fn null_branch_bottom_type_selects_any_nullable_extension_receiver() {
    let (body, _) = checked_function_body_with_platform(
        "fun String?.fallback(): String = this ?: \"OK\"\n\
         fun read(value: Int?): String {\n\
             if (value == null) return value.fallback()\n\
             return value.toString()\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::String));
}

#[test]
fn non_null_this_proof_updates_bare_implicit_receiver_lookup() {
    let (body, _) = checked_function_body_with_platform(
        "class Receiver { fun value(): String = \"OK\" }\n\
         fun Receiver?.read(): String = if (this != null) value() else \"FAIL\"\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(|ty| ty.get()), Some(Ty::String));
}

#[test]
fn return_in_for_range_header_terminates_the_enclosing_function() {
    let (body, _) = checked_function_body_with_platform(
        "fun read(): String {\n\
             for (value in 1 .. return \"OK\") {}\n\
         }\n",
        "read",
        jvm_semantics(),
    );

    assert_eq!(body.result_type().map(ResolvedTy::get), Some(Ty::String));
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::Jump {
                kind: FirJumpKind::Return { .. },
                ..
            })
        )
    }));
}
