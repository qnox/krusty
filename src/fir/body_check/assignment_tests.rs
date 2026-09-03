use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;

fn first_statement_call(body: &FirBody) -> &FirCall {
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(expression) = body
        .statement(statements[0])
        .expect("compound assignment statement")
        .kind
    else {
        panic!("compound assignment must become an expression statement")
    };
    let FirExprKind::Call(call) = &body.expr(expression).expect("operator call").kind else {
        panic!("compound assignment must retain its selected operator call")
    };
    call
}

#[test]
fn nullable_narrow_integer_constant_expression_retains_numeric_conversion() {
    let (body, _) =
        checked_function_body("fun convert() { val value: Byte? = 1 + 1 }\n", "convert");
    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("function body must be a block")
    };
    let FirStatementKind::Local { ty, conversion, .. } = body
        .statement(statements[0])
        .expect("converted local declaration")
        .kind
    else {
        panic!("first statement must be the converted local")
    };
    assert_eq!(ty.get(), Ty::nullable(Ty::Byte));
    assert!(matches!(
        conversion.map(|conversion| conversion.kind),
        Some(FirConversionKind::NumericConversion { to })
            if to.get() == Ty::nullable(Ty::Byte)
    ));
}

#[test]
fn unnamed_local_evaluates_its_initializer_without_allocating_storage() {
    let (body, _) = checked_function_body("fun call() {}\nfun use() { val _ = call() }\n", "use");
    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("function body must be a block")
    };
    let FirStatementKind::Expression(initializer) = body
        .statement(statements[0])
        .expect("unnamed local initializer")
        .kind
    else {
        panic!("an unnamed local must become an initializer expression statement")
    };
    assert!(matches!(
        body.expr(initializer).expect("initializer expression").kind,
        FirExprKind::Call(_)
    ));
}

#[test]
fn member_plus_assign_keeps_stable_target_and_dispatch_receiver() {
    let (body, index) = checked_function_body(
        "class Counter { operator fun plusAssign(value: Int) {} }\n\
         fun update(counter: Counter) { counter += 1 }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn inherited_val_var_intersection_retains_the_setter_facet() {
    let (body, index) = checked_function_body(
        "abstract class ReadOnly { abstract val value: String }\n\
         interface Mutable { var value: String }\n\
         abstract class Intersection : ReadOnly(), Mutable\n\
         fun update(target: Intersection) { target.value = \"OK\" }\n",
        "update",
    );
    let write = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::PropertyWrite { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("intersection assignment must become checked property-write FIR");
    assert!(index
        .property(write.module().expect("module property target"))
        .is_some());
}

#[test]
fn inferred_conditional_intersection_retains_a_mutable_property_facet() {
    let (body, index) = checked_function_body(
        "interface T { var x: String }\n\
         interface A : T { override var x: String }\n\
         interface B : T { override var x: String }\n\
         class C : A, B { override var x: String = \"\" }\n\
         class D : A, B { override var x: String = \"\" }\n\
         fun update(condition: Boolean) {\n\
             val value = if (condition) C() else D()\n\
             value.x = \"OK\"\n\
         }\n",
        "update",
    );
    let target = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::PropertyWrite { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("the inferred intersection write must keep a selected property target");
    assert!(index
        .property(target.module().expect("module property target"))
        .is_some());
}

#[test]
fn compound_member_property_binds_the_shared_receiver_once() {
    let (body, _) = checked_function_body(
        "class Test {\n\
             var Int.value: String get() = \"O\"; set(next) {}\n\
             fun nextReceiver(): Int = 1\n\
             fun test() { nextReceiver().value += \"K\" }\n\
         }\n",
        "test",
    );
    let receivers = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::PropertyRead {
                extension_receiver: Some(receiver),
                ..
            }
            | FirExprKind::PropertyWrite {
                extension_receiver: Some(receiver),
                ..
            } => Some(receiver.value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receivers.len(), 2);
    assert_eq!(receivers[0], receivers[1]);
    assert!(matches!(
        body.expr(receivers[0])
            .expect("bound compound receiver")
            .kind,
        FirExprKind::ValueRead(_)
    ));
}

#[test]
fn generic_member_plus_fallback_uses_the_instantiated_receiver_arguments() {
    let (body, index) = checked_function_body(
        "class Box<T> { operator fun plus(other: Box<T>): Box<T> = this }\n\
         fun update() { var left = Box<String>(); val right = Box<String>(); left += right }\n",
        "update",
    );
    let plus = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("plus")).then_some(call)
        })
        .expect("plus fallback must retain its selected checked call");
    let [FirCallArgument::Expression { value, .. }] = plus.arguments.as_ref() else {
        panic!("plus fallback must retain its right operand")
    };
    assert_eq!(
        body.expr(*value).expect("right operand").ty.get(),
        Ty::obj_args("Box", &[Ty::String]),
    );
}

#[test]
fn extension_plus_assign_keeps_stable_target_and_extension_receiver() {
    let (body, index) = checked_function_body(
        "class Counter\n\
         operator fun Counter.plusAssign(value: Int) {}\n\
         fun update(counter: Counter) { counter += 1 }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn generic_extension_plus_assign_keeps_argument_inference_in_checked_fir() {
    let (body, index) = checked_function_body(
        "class Scope<T>\n\
         operator fun <T> Scope<String>.plusAssign(value: Scope<T>) {}\n\
         fun update(scope: Scope<String>) { scope += scope }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 1);
    let FirCallArgument::Expression { conversion, .. } = call.arguments[0] else {
        panic!("plusAssign value must be a checked expression argument")
    };
    assert!(conversion.is_none());
}

#[test]
fn receiver_instantiates_callable_parameter_before_plus_assign_lambda_checking() {
    let (body, _) = checked_function_body(
        "class MyList<T>\n\
         operator fun <T> MyList<T>.plusAssign(element: T) {}\n\
         val functions = MyList<(Int) -> Int>()\n\
         fun update() { functions += { it -> it } }\n",
        "update",
    );
    let call = first_statement_call(&body);
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
fn plus_assign_contextually_types_nested_generic_rhs_calls() {
    let (body, _) = checked_function_body_with_platform(
        "var map: Map<Any, Set<Any>> = emptyMap()\n\
         fun update() { map += \"OK\" to emptySet() }\n",
        "update",
        jvm_stdlib_semantics(),
    );
    let expected = Ty::obj_args(
        "kotlin/Pair",
        &[
            Ty::String,
            Ty::obj_args("kotlin/collections/Set", &[Ty::obj("kotlin/Any")]),
        ],
    );
    let expression_types = (0..body.expression_count())
        .filter_map(|raw| {
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| expression.ty.get())
        })
        .collect::<Vec<_>>();
    assert!(
        expression_types.iter().any(|&actual| actual == expected),
        "the nested Pair/emptySet RHS must publish its selected expected result type: {expression_types:?}"
    );
}

#[test]
fn contextual_extension_plus_assign_keeps_context_before_value_argument() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\n\
         class Counter\n\
         context(scope: Int) operator fun Counter.plusAssign(value: Int) {}\n\
         context(scope: Int) fun update(counter: Counter) { counter += 1 }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.extension_receiver.is_some());
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
fn expression_target_plus_assign_keeps_the_selected_call_without_desugared_syntax() {
    let (body, index) = checked_function_body(
        "class Counter { operator fun plusAssign(value: Int) {} }\n\
         fun counter(): Counter = Counter()\n\
         fun update() { counter() += 1 }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn extension_index_set_keeps_stable_target_and_extension_receiver() {
    let (body, index) = checked_function_body(
        "class Box\n\
         operator fun Box.set(index: Int, value: String) {}\n\
         fun update(box: Box) { box[0] = \"value\" }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 2);
}

#[test]
fn generic_member_set_calls_retain_receiver_applied_parameter_types() {
    let (body, index) = checked_function_body(
        "class Generic<T> { operator fun set(index: Int, value: T) {} }\n\
         fun update(target: Generic<Int>) { target.set(0, 1); target[0] = 1 }\n",
        "update",
    );
    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("set")).then_some(call)
        })
        .collect::<Vec<_>>();

    assert_eq!(
        calls.len(),
        2,
        "direct and indexed forms must both be checked calls"
    );
    assert!(calls.iter().all(|call| {
        call.parameter_types
            .iter()
            .map(|parameter| parameter.get())
            .eq([Ty::Int, Ty::Int])
    }));
    let declaration = index
        .callable(calls[0].target.module().unwrap())
        .expect("selected module callable")
        .declaration;
    assert!(matches!(
        index.signature(declaration).unwrap().parameters[1].get(),
        Ty::TyParam(..)
    ));
}

#[test]
fn inapplicable_builtin_array_index_falls_through_to_extension_operators() {
    let (write_body, write_index) = checked_function_body(
        "operator fun IntArray.set(index: Long, value: Int) {}\n\
         fun update(array: IntArray, index: Long) { array[index] = 1 }\n",
        "update",
    );
    let write = first_statement_call(&write_body);
    assert!(write_index
        .callable(write.target.module().unwrap())
        .is_some());
    assert!(write.extension_receiver.is_some());
    assert_eq!(write.arguments.len(), 2);

    let (read_body, read_index) = checked_function_body(
        "operator fun IntArray.get(index: Long): Int = 1\n\
         fun read(array: IntArray, index: Long): Int = array[index]\n",
        "read",
    );
    let FirExprKind::Call(read) = &read_body
        .expr(root_expression(&read_body))
        .expect("indexed extension read")
        .kind
    else {
        panic!("the inapplicable built-in array get must fall through to a checked call")
    };
    assert!(read_index.callable(read.target.module().unwrap()).is_some());
    assert!(read.extension_receiver.is_some());
    assert_eq!(read.arguments.len(), 1);
}

#[test]
fn indexed_increment_keeps_member_extension_dispatch_and_value_receivers() {
    let (body, index) = checked_function_body(
        "class Provider {\n\
             operator fun Long.get(index: Int): Long = this\n\
             operator fun Long.set(index: Int, value: Long) {}\n\
             fun update() { var value = 0L; value[0]++ }\n\
         }\n",
        "update",
    );
    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            (call.dispatch_receiver.is_some() && call.extension_receiver.is_some()).then_some(call)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls.len(),
        2,
        "indexed ++ must retain selected get and set calls"
    );
    assert!(calls
        .iter()
        .all(|call| index.callable(call.target.module().unwrap()).is_some()));
}

#[test]
fn extension_increment_keeps_its_omitted_default_argument() {
    let (body, index) = checked_function_body(
        "class A\n\
         operator fun A.inc(message: String = \"OK\"): A = this\n\
         fun update() { var value = A(); value++; ++value }\n",
        "update",
    );
    let calls = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("inc")).then_some(call)
        })
        .collect::<Vec<_>>();

    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| {
        call.extension_receiver.is_some()
            && matches!(
                call.arguments.as_ref(),
                [FirCallArgument::Default { parameter: 0, .. }]
            )
    }));
}

#[test]
fn contextual_extension_increment_keeps_the_selected_context_argument() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\n\
         class Counter\n\
         context(scope: Int) operator fun Counter.inc(): Counter = this\n\
         context(scope: Int) fun update() { var value = Counter(); value++ }\n",
        "update",
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("inc")).then_some(call)
        })
        .expect("increment must retain its selected convention call");
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn indexed_set_keeps_an_omitted_default_between_index_and_value() {
    let (body, index) = checked_function_body(
        "class Box { operator fun set(first: Int, second: Int = 1, value: String) {} }\n\
         fun update(box: Box) { box[0] = \"value\" }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert_eq!(call.arguments.len(), 3);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
    assert!(matches!(
        call.arguments[1],
        FirCallArgument::Expression { parameter: 2, .. }
    ));
    assert!(matches!(
        call.arguments[2],
        FirCallArgument::Default { parameter: 1, .. }
    ));
}

#[test]
fn contextual_extension_index_set_keeps_context_before_source_operands() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\nclass Box\ncontext(scope: Int) operator fun Box.set(index: Int, value: String) {}\ncontext(scope: Int) fun update(box: Box) { box[0] = \"value\" }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.parameter_types.len(), 3);
    assert_eq!(call.arguments.len(), 3);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
}

#[test]
fn indexed_set_keeps_repeated_vararg_indices_before_trailing_value() {
    let (body, index) = checked_function_body(
        "object Store { operator fun set(vararg indices: String, value: String) {} }\n\
         fun update() { Store[\"a\", \"b\", \"c\"] = \"value\" }\n",
        "update",
    );
    let call = first_statement_call(&body);
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert_eq!(call.arguments.len(), 4);
    assert!(call.arguments[..3].iter().all(|argument| matches!(
        argument,
        FirCallArgument::Vararg { parameter: 0, elements, .. } if elements.len() == 1
    )));
    assert!(matches!(
        call.arguments[3],
        FirCallArgument::Expression { parameter: 1, .. }
    ));
}

#[test]
fn contextual_extension_index_get_keeps_context_before_source_operands() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\nclass Box\ncontext(scope: Int) operator fun Box.get(index: Int): String = \"value\"\ncontext(scope: Int) fun read(box: Box): String = box[0]\n",
        "read",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("indexed get call")
        .kind
    else {
        panic!("indexed get must retain the selected convention call")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 2);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
}

#[test]
fn mutable_local_approximates_a_smart_cast_initializer_to_its_declared_type() {
    let (body, _) = checked_function_body(
        "open class Base\n\
         class Derived : Base()\n\
         fun widen(value: Base): Base {\n\
             if (value is Derived) {\n\
                 var copy = value\n\
                 copy = Base()\n\
                 return copy\n\
             }\n\
             return value\n\
         }\n",
        "widen",
    );
    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("function body must be a block")
    };
    let FirStatementKind::Expression(conditional) =
        body.statement(statements[0]).expect("if statement").kind
    else {
        panic!("first statement must be the conditional")
    };
    let FirExprKind::Conditional { then_branch, .. } =
        body.expr(conditional).expect("conditional expression").kind
    else {
        panic!("if statement must retain conditional FIR")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(then_branch).expect("then block").kind
    else {
        panic!("then branch must be a block")
    };
    let FirStatementKind::Local { ty, mutable, .. } =
        body.statement(statements[0]).expect("mutable copy").kind
    else {
        panic!("first guarded statement must be the mutable local")
    };
    assert!(mutable);
    assert_eq!(ty.get(), Ty::obj("Base"));
}

#[test]
fn unreachable_val_write_is_not_published_to_checked_fir() {
    let (body, _) = checked_function_body(
        "class Box(val value: String)\n\
         fun read(box: Box): String {\n\
             return box.value\n\
             box.value = \"unreachable\"\n\
         }\n",
        "read",
    );
    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("function body must be a block")
    };
    assert_eq!(
        statements.len(),
        1,
        "the unreachable assignment must not cross the checked-FIR boundary"
    );
}
