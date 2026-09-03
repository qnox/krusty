use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;

fn collect_local_call_shapes(body: &FirBody, selected: &mut Vec<(usize, u32, bool)>) {
    for raw in 0..body.expression_count() {
        let expression = body
            .expr(FirExprId::from_raw(raw as u32))
            .expect("FIR expression");
        match &expression.kind {
            FirExprKind::LocalCall {
                target,
                extension_receiver,
                arguments,
                ..
            } => selected.push((
                arguments.len(),
                target.body_depth,
                extension_receiver.is_some(),
            )),
            FirExprKind::Lambda { body, .. } => collect_local_call_shapes(body, selected),
            _ => {}
        }
    }
    for raw in 0..body.statement_count() {
        let statement = body
            .statement(FirStatementId::from_raw(raw as u32))
            .expect("FIR statement");
        if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
            collect_local_call_shapes(body, selected);
        }
    }
}

#[test]
fn local_function_declaration_and_call_use_body_local_callable_identity() {
    let (body, _) = checked_function_body(
        "fun outer(value: Int): Int { fun twice(input: Int): Int = input * 2; return twice(value) }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::LocalFunction {
        callable,
        body: local_body,
        ..
    } = &body.statement(statements[0]).expect("local function").kind
    else {
        panic!("local function must retain its checked nested FIR body")
    };
    assert_eq!(local_body.local_callable(), Some(*callable));
    let FirStatementKind::Expression(return_expression) =
        body.statement(statements[1]).expect("return").kind
    else {
        panic!("return must be an expression statement")
    };
    let FirExprKind::Jump {
        value: Some(value), ..
    } = body
        .expr(return_expression)
        .expect("return expression")
        .kind
    else {
        panic!("return must carry the local call")
    };
    let FirExprKind::LocalCall { target, .. } = &body.expr(value).expect("local call").kind else {
        panic!("local invocation must use a body-local callable reference")
    };
    assert_eq!(target.body_depth, 0);
    assert_eq!(target.callable, *callable);
}

#[test]
fn local_type_parameter_bound_keeps_enclosing_parameter_identity() {
    let (body, _) = checked_function_body(
        "fun <T> outer(value: T): T {\n\
             fun <S : T> identity(argument: S) = argument\n\
             return identity(value)\n\
         }\n",
        "outer",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            &body.expr(FirExprId::from_raw(raw as u32))
                .expect("FIR expression")
                .kind,
            FirExprKind::LocalCall { target, .. } if target.body_depth == 0
        )
    }));
}

#[test]
fn local_class_member_signature_uses_enclosing_local_type_parameter() {
    let (body, _) = checked_function_body(
        "fun box(): String {\n\
             var shared = 0\n\
             fun <T> local(value: T): T {\n\
                 class Holder {\n\
                     val captured = 0\n\
                     fun identity(argument: T): T { shared = captured; return argument }\n\
                 }\n\
                 fun sibling(): T = Holder().identity(value)\n\
                 return sibling()\n\
             }\n\
             return local(\"OK\")\n\
         }\n",
        "box",
    );
    let local_body = (0..body.statement_count())
        .find_map(|raw| {
            let statement = body
                .statement(FirStatementId::from_raw(raw as u32))
                .expect("FIR statement");
            match &statement.kind {
                FirStatementKind::LocalFunction { body, .. }
                    if body.captures().iter().any(|capture| capture.shared_cell) =>
                {
                    Some(body)
                }
                _ => None,
            }
        })
        .expect("the lifted local function must carry the outer mutable cell");
    assert!((0..local_body.statement_count()).any(|raw| {
        matches!(
            &local_body
                .statement(FirStatementId::from_raw(raw as u32))
                .expect("local FIR statement")
                .kind,
            FirStatementKind::LocalDeclaration { captures, .. }
                if captures.iter().any(|capture| {
                    capture.name.as_ref() == "shared"
                        && capture.shared_cell
                        && matches!(
                            &capture.source,
                            FirLocalClassCaptureSource::Captured {
                                enclosing_depth: 0,
                                ..
                            }
                        )
                })
        )
    }));
}

#[test]
fn inapplicable_inner_local_overload_falls_through_to_enclosing_rung() {
    let (body, _) = checked_function_body(
        "fun <T> eval(fn: () -> T) = fn()\n\
         fun box(): String {\n\
             var result = \"\"\n\
             var foo = \"K\"\n\
             fun foo(value: String, ignored: Int) { result += value }\n\
             fun test() {\n\
                 fun foo(value: String) { result += value }\n\
                 eval { foo(\"O\"); foo(foo, 1) }\n\
             }\n\
             test()\n\
             return result\n\
         }\n",
        "box",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("box block").kind
    else {
        panic!("box must have a FIR block")
    };
    let FirStatementKind::Local {
        target: result_value,
        ..
    } = body.statement(statements[0]).expect("result local").kind
    else {
        panic!("first statement must bind result")
    };
    let FirStatementKind::Local {
        target: foo_value, ..
    } = body.statement(statements[1]).expect("foo value local").kind
    else {
        panic!("second statement must bind the value named foo")
    };

    let mut selected = Vec::new();
    collect_local_call_shapes(&body, &mut selected);
    assert!(
        selected.contains(&(1, 1, false)),
        "the nearest one-argument overload must be selected: {selected:?}"
    );
    assert!(
        selected.contains(&(2, 2, false)),
        "the inapplicable inner rung must fall through to the enclosing overload: {selected:?}"
    );

    fn lambda_forwards_selected_callable_captures(
        body: &FirBody,
        result_value: LocalValueId,
        foo_value: LocalValueId,
    ) -> bool {
        for raw in 0..body.expression_count() {
            let expression = body
                .expr(FirExprId::from_raw(raw as u32))
                .expect("FIR expression");
            if let FirExprKind::Lambda { body, .. } = &expression.kind {
                let forwards_result = body.captures().iter().any(|capture| {
                    capture.source == result_value
                        && capture.enclosing_depth == 1
                        && capture.shared_cell
                });
                let forwards_foo = body
                    .captures()
                    .iter()
                    .any(|capture| capture.source == foo_value && capture.enclosing_depth == 1);
                if forwards_result && forwards_foo {
                    return true;
                }
                if lambda_forwards_selected_callable_captures(body, result_value, foo_value) {
                    return true;
                }
            }
        }
        for raw in 0..body.statement_count() {
            let statement = body
                .statement(FirStatementId::from_raw(raw as u32))
                .expect("FIR statement");
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                if lambda_forwards_selected_callable_captures(body, result_value, foo_value) {
                    return true;
                }
            }
        }
        false
    }
    assert!(
        lambda_forwards_selected_callable_captures(&body, result_value, foo_value),
        "the call-site lambda must carry every value required by both selected overloads"
    );
}

#[test]
fn local_function_missing_context_falls_through_to_dispatch_property_invoke() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +ContextParameters\n\
         // WITH_STDLIB\n\
         class Action {\n\
             context(value: Int) operator fun invoke(): String = \"OK\"\n\
         }\n\
         class Owner {\n\
             val action = Action()\n\
             fun run(): String {\n\
                 context(value: String) fun action(): String = \"local\"\n\
                 return with(1) { action() }\n\
             }\n\
         }\n",
        "run",
        jvm_stdlib_semantics(),
    );

    fn has_context_invoke(body: &FirBody) -> bool {
        (0..body.expression_count()).any(|raw| {
            let expression = body
                .expr(FirExprId::from_raw(raw as u32))
                .expect("FIR expression");
            match &expression.kind {
                FirExprKind::Call(call) => {
                    call.dispatch_receiver.is_some() && call.arguments.len() == 1
                }
                FirExprKind::Lambda { body, .. } => has_context_invoke(body),
                _ => false,
            }
        })
    }

    assert!(has_context_invoke(&body));
}

#[test]
fn dispatch_property_is_not_an_implicit_context_value_for_invoke_selection() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +ContextParameters\n\
         // WITH_STDLIB\n\
         class Action {\n\
             context(value: Int) operator fun invoke(): String = \"member\"\n\
         }\n\
         context(value: String) operator fun Action.invoke(): String = \"extension\"\n\
         class Owner {\n\
             val action = Action()\n\
             fun run(): String = with(\"\") { action() }\n\
         }\n",
        "run",
        jvm_stdlib_semantics(),
    );

    fn has_extension_invoke(body: &FirBody) -> bool {
        (0..body.expression_count()).any(|raw| {
            let expression = body
                .expr(FirExprId::from_raw(raw as u32))
                .expect("FIR expression");
            match &expression.kind {
                FirExprKind::Call(call) => {
                    call.dispatch_receiver.is_none()
                        && call.extension_receiver.is_some()
                        && call.arguments.len() == 1
                }
                FirExprKind::Lambda { body, .. } => has_extension_invoke(body),
                _ => false,
            }
        })
    }

    assert!(has_extension_invoke(&body));
}

#[test]
fn untyped_nested_lambda_keeps_local_overload_rung_targets() {
    let (body, _) = checked_function_body_with_platform(
        "fun box(): String {\n\
             var result = \"\"\n\
             var foo = \"O\"\n\
             fun foo(value: String, ignored: Int) { result += value }\n\
             run {\n\
                 fun foo(value: String) { result += value }\n\
                 { foo(foo, 1); foo(\"K\") }.let { it() }\n\
             }\n\
             return result\n\
         }\n",
        "box",
        jvm_stdlib_semantics(),
    );

    fn collect(body: &FirBody, calls: &mut Vec<(usize, u32)>) {
        for raw in 0..body.expression_count() {
            let expression = body
                .expr(FirExprId::from_raw(raw as u32))
                .expect("FIR expression");
            match &expression.kind {
                FirExprKind::LocalCall {
                    target, arguments, ..
                } => calls.push((arguments.len(), target.body_depth)),
                FirExprKind::Lambda { body, .. } => collect(body, calls),
                _ => {}
            }
        }
        for raw in 0..body.statement_count() {
            let statement = body
                .statement(FirStatementId::from_raw(raw as u32))
                .expect("FIR statement");
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                collect(body, calls);
            }
        }
    }

    let mut calls = Vec::new();
    collect(&body, &mut calls);
    assert!(
        calls.contains(&(2, 2)),
        "enclosing overload target: {calls:?}"
    );
    assert!(
        calls.contains(&(1, 1)),
        "nearest overload target: {calls:?}"
    );
}

#[test]
fn untyped_nested_lambda_keeps_recursive_local_call_target() {
    let (body, _) = checked_function_body_with_platform(
        "fun outer() {\n\
             fun inner(value: Int) {\n\
                 if (value > 0) {\n\
                     { ignored: Int -> inner(0) }.let { it(1) }\n\
                 }\n\
             }\n\
             inner(1)\n\
         }\n",
        "outer",
        jvm_stdlib_semantics(),
    );

    fn has_recursive_call(body: &FirBody) -> bool {
        (0..body.expression_count()).any(|raw| {
            let expression = body
                .expr(FirExprId::from_raw(raw as u32))
                .expect("FIR expression");
            matches!(&expression.kind, FirExprKind::LocalCall { target, .. } if target.body_depth > 0)
                || matches!(&expression.kind, FirExprKind::Lambda { body, .. } if has_recursive_call(body))
        }) || (0..body.statement_count()).any(|raw| {
            let statement = body
                .statement(FirStatementId::from_raw(raw as u32))
                .expect("FIR statement");
            matches!(&statement.kind, FirStatementKind::LocalFunction { body, .. } if has_recursive_call(body))
        })
    }

    assert!(has_recursive_call(&body));
}

#[test]
fn inapplicable_inner_local_extension_falls_through_to_enclosing_rung() {
    let (body, _) = checked_function_body(
        "fun box(): String {\n\
             var result = \"\"\n\
             fun String.decorate(ignored: Int) { result += this }\n\
             fun test() {\n\
                 fun String.decorate() { result += this }\n\
                 \"O\".decorate()\n\
                 \"K\".decorate(1)\n\
             }\n\
             test()\n\
             return result\n\
         }\n",
        "box",
    );

    let mut selected = Vec::new();
    collect_local_call_shapes(&body, &mut selected);
    assert!(
        selected.contains(&(0, 0, true)),
        "the nearest zero-argument extension must be selected: {selected:?}"
    );
    assert!(
        selected.contains(&(1, 1, true)),
        "the inapplicable inner extension rung must fall through to the enclosing overload: {selected:?}"
    );
}

#[test]
fn primitive_companion_value_selects_lexical_local_extension() {
    let (body, _) = checked_function_body_with_platform(
        "fun box(): String {\n\
             fun Int.Companion.local(): String = \"OK\"\n\
             return Int.local()\n\
         }\n",
        "box",
        jvm_stdlib_semantics(),
    );

    let (target, receiver) = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::LocalCall {
                target,
                extension_receiver: Some(receiver),
                ..
            } = &expression.kind
            else {
                return None;
            };
            Some((target.clone(), receiver.value))
        })
        .expect("the companion extension invocation must remain a checked local call");
    assert_eq!(target.body_depth, 0);
    assert!(matches!(
        body.expr(receiver).map(|expression| &expression.kind),
        Some(FirExprKind::SingletonValue { classifier })
            if classifier.matches("kotlin/Int$Companion")
    ));
}

#[test]
fn bare_local_extension_call_falls_through_to_enclosing_rung() {
    let (body, _) = checked_function_body(
        "fun String.box(): String {\n\
             var result = \"\"\n\
             fun String.decorate(ignored: Int) { result += this }\n\
             fun test() {\n\
                 fun String.decorate() { result += this }\n\
                 decorate()\n\
                 decorate(1)\n\
             }\n\
             test()\n\
             return result\n\
         }\n",
        "box",
    );

    fn collect_local_extension_calls(body: &FirBody, selected: &mut Vec<(usize, u32)>) {
        for raw in 0..body.expression_count() {
            let expression = body
                .expr(FirExprId::from_raw(raw as u32))
                .expect("FIR expression");
            match &expression.kind {
                FirExprKind::LocalCall {
                    target,
                    extension_receiver: Some(_),
                    arguments,
                } => selected.push((arguments.len(), target.body_depth)),
                FirExprKind::Lambda { body, .. } => collect_local_extension_calls(body, selected),
                _ => {}
            }
        }
        for raw in 0..body.statement_count() {
            let statement = body
                .statement(FirStatementId::from_raw(raw as u32))
                .expect("FIR statement");
            if let FirStatementKind::LocalFunction { body, .. } = &statement.kind {
                collect_local_extension_calls(body, selected);
            }
        }
    }

    let mut selected = Vec::new();
    collect_local_extension_calls(&body, &mut selected);
    assert!(
        selected.contains(&(0, 0)),
        "the nearest bare extension must be selected: {selected:?}"
    );
    assert!(
        selected.contains(&(1, 1)),
        "the bare call must fall through to the applicable enclosing extension: {selected:?}"
    );
}

#[test]
fn recursive_local_call_points_to_the_enclosing_body_declaration() {
    let (body, _) = checked_function_body(
        "fun outer(): Int { fun loop(value: Int): Int = if (value == 0) 0 else loop(value - 1); return loop(2) }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::LocalFunction {
        callable,
        body: local_body,
        ..
    } = &body.statement(statements[0]).expect("local function").kind
    else {
        panic!("local function must retain its checked nested FIR body")
    };
    let root = root_expression(local_body);
    let FirExprKind::Conditional { else_branch, .. } = local_body.expr(root).expect("if").kind
    else {
        panic!("recursive body must be conditional")
    };
    let FirExprKind::LocalCall { target, .. } =
        &local_body.expr(else_branch).expect("recursive call").kind
    else {
        panic!("recursive branch must retain its local target")
    };
    assert_eq!(target.body_depth, 1);
    assert_eq!(target.callable, *callable);
}

#[test]
fn local_function_capture_keeps_value_identity_type_and_shared_cell_decision() {
    let (body, _) = checked_function_body(
        "fun outer(): Int { var value = 1; fun read(): Int = value; value = 2; return read() }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Local { target, .. } =
        body.statement(statements[0]).expect("captured local").kind
    else {
        panic!("first statement must bind the captured local")
    };
    let FirStatementKind::LocalFunction {
        body: local_body, ..
    } = &body.statement(statements[1]).expect("local function").kind
    else {
        panic!("second statement must be the local function")
    };
    assert_eq!(local_body.captures().len(), 1);
    assert_eq!(local_body.captures()[0].source, target);
    assert!(local_body.captures()[0].shared_cell);
}

#[test]
fn enclosing_local_function_carries_a_descendant_capture_across_a_lambda() {
    let (body, _) = checked_function_body(
        "inline fun <T> materialize(block: () -> T): T = block()\n\
         fun outer(value: String): () -> String {\n\
         fun enclosing(): () -> String = materialize {\n\
             fun <T> T.nested(): String = value\n\
             value::nested\n\
         }\n\
         return enclosing()\n\
         }\n",
        "outer",
    );
    let [source_parameter] = body.parameters() else {
        panic!("outer function parameter")
    };
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("outer block").kind
    else {
        panic!("outer function must be a FIR block")
    };
    let FirStatementKind::LocalFunction {
        body: enclosing_body,
        ..
    } = &body
        .statement(statements[0])
        .expect("enclosing local function")
        .kind
    else {
        panic!("first statement must declare the enclosing local function")
    };
    let [enclosing_capture] = enclosing_body.captures() else {
        panic!("enclosing local function must carry its descendant's capture")
    };
    assert_eq!(enclosing_capture.enclosing_depth, 0);
    assert_eq!(enclosing_capture.source, source_parameter.value);
}

#[test]
fn local_function_write_uses_a_shared_captured_value_target() {
    let (body, _) = checked_function_body(
        "fun outer(): Int { var value = 1; fun bump(): Int { value = value + 1; return value }; return bump() }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::LocalFunction {
        body: local_body, ..
    } = &body.statement(statements[1]).expect("local function").kind
    else {
        panic!("second statement must be the local function")
    };
    assert!(local_body.captures()[0].shared_cell);
    let FirExprKind::Block {
        statements: local_statements,
        ..
    } = &local_body
        .expr(root_expression(local_body))
        .expect("local block")
        .kind
    else {
        panic!("local function must have a block")
    };
    let FirStatementKind::Expression(write) = local_body
        .statement(local_statements[0])
        .expect("capture write")
        .kind
    else {
        panic!("capture write must be an expression statement")
    };
    assert!(matches!(
        local_body.expr(write).expect("capture write").kind,
        FirExprKind::CapturedValueWrite { .. }
    ));
}

#[test]
fn captured_increment_forms_read_and_write_the_shared_value() {
    let (body, _) = checked_function_body(
        "fun outer(): Int { var value = 1; fun bump(): Int { value++; return value++ }; return bump() }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::LocalFunction {
        body: local_body, ..
    } = &body.statement(statements[1]).expect("local function").kind
    else {
        panic!("second statement must be the local function")
    };
    assert!(local_body.captures()[0].shared_cell);
    let captured_writes = (0..local_body.expression_count())
        .filter(|raw| {
            let expression = FirExprId::from_raw(u32::try_from(*raw).unwrap());
            matches!(
                local_body.expr(expression).expect("FIR expression").kind,
                FirExprKind::CapturedValueWrite { .. }
            )
        })
        .count();
    assert_eq!(captured_writes, 2);
}

#[test]
fn local_function_reference_uses_body_local_callable_identity() {
    let (body, _) = checked_function_body(
        "fun outer(): () -> Int { fun answer(): Int = 42; return ::answer }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::LocalFunction { callable, .. } =
        &body.statement(statements[0]).expect("local function").kind
    else {
        panic!("first statement must declare the local function")
    };
    let FirStatementKind::Expression(return_expression) =
        body.statement(statements[1]).expect("return").kind
    else {
        panic!("second statement must return the reference")
    };
    let FirExprKind::Jump {
        value: Some(reference),
        ..
    } = body
        .expr(return_expression)
        .expect("return expression")
        .kind
    else {
        panic!("return must contain the local reference")
    };
    let FirExprKind::LocalCallableReference {
        target, adaptation, ..
    } = &body.expr(reference).expect("local reference").kind
    else {
        panic!("local reference must retain a body-local callable coordinate")
    };
    assert_eq!(target.body_depth, 0);
    assert_eq!(target.callable, *callable);
    assert!(adaptation.is_none());
}

#[test]
fn local_extension_receiver_resolves_a_preceding_local_classifier_before_fir() {
    let (body, _) = checked_function_body(
        "fun box(): String {\n\
             class A\n\
             fun A.foo(): String = \"OK\"\n\
             val reference = A::foo\n\
             return reference(A())\n\
         }\n",
        "box",
    );

    let reference = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(expression.kind, FirExprKind::LocalCallableReference { .. })
                .then_some(expression)
        })
        .expect("local extension reference must become checked FIR");
    assert_eq!(reference.ty.get().fun_ret(), Some(Ty::String));
    assert!(matches!(
        reference.ty.get().non_null(),
        Ty::Fun(signature)
            if signature.params.len() == 1
                && signature.params[0]
                    .kotlin_class_internal()
                    .is_some_and(|owner| owner.render().contains("$A"))
    ));
}

#[test]
fn generic_local_extension_signature_uses_the_preceding_local_classifier_scope() {
    let (body, _) = checked_function_body(
        "fun box(): String {\n\
             class A\n\
             fun <T> A.echo(value: T): T = value\n\
             val reference: (A, String) -> String = A::echo\n\
             return reference(A(), \"OK\")\n\
         }\n",
        "box",
    );

    let reference = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            matches!(expression.kind, FirExprKind::LocalCallableReference { .. })
                .then_some(expression)
        })
        .expect("generic local extension reference must become checked FIR");
    assert!(matches!(
        reference.ty.get().non_null(),
        Ty::Fun(signature)
            if signature.params.len() == 2
                && signature.params[0]
                    .obj_internal()
                    .is_some_and(|internal| internal.render().ends_with("$A"))
                && signature.params[1] == Ty::String
                && signature.ret == Ty::String
    ));
}

#[test]
fn adapted_local_function_reference_keeps_complete_argument_plan() {
    let (body, _) = checked_function_body(
        "fun outer(): (String) -> String {\n\
         fun join(value: String, suffix: String = \"K\"): String = value + suffix\n\
         return ::join\n\
         }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(return_expression) =
        body.statement(statements[1]).expect("return").kind
    else {
        panic!("second statement must return the adapted reference")
    };
    let FirExprKind::Jump {
        value: Some(reference),
        ..
    } = body
        .expr(return_expression)
        .expect("return expression")
        .kind
    else {
        panic!("return must contain the adapted reference")
    };
    let FirExprKind::LocalCallableReference {
        adaptation: Some(adaptation),
        ..
    } = &body.expr(reference).expect("local reference").kind
    else {
        panic!("adapted local reference must retain its checker-selected plan")
    };
    assert_eq!(
        adaptation.arguments.as_ref(),
        [
            FirAdaptedReferenceArgument::Value(0),
            FirAdaptedReferenceArgument::Default,
        ]
    );
}

#[test]
fn context_local_function_call_keeps_selected_context_binding() {
    let (body, _) = checked_function_body(
        "class Session\n\
         context(session: Session) fun outer(): Int {\n\
         context(current: Session) fun local(value: Int): Int = value\n\
         return local(42)\n\
         }\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(return_expression) =
        body.statement(statements[1]).expect("return").kind
    else {
        panic!("second statement must return the local call")
    };
    let FirExprKind::Jump {
        value: Some(call), ..
    } = body
        .expr(return_expression)
        .expect("return expression")
        .kind
    else {
        panic!("return must contain the local call")
    };
    let FirExprKind::LocalCall { arguments, .. } = &body.expr(call).expect("local call").kind
    else {
        panic!("context local invocation must remain a local call")
    };
    assert_eq!(arguments.len(), 2);
    assert!(matches!(
        arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
    assert!(matches!(
        arguments[1],
        FirCallArgument::Expression { parameter: 1, .. }
    ));
}

#[test]
fn contextual_local_extension_call_keeps_selected_implicit_receiver() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +ContextParameters\nclass Prefix\nclass Target\ncontext(prefix: Prefix) fun Target.outer(): String {\ncontext(current: Prefix) fun Target.read(): String = \"OK\"\nreturn read()\n}\n",
        "outer",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(return_expression) =
        body.statement(statements[1]).expect("return").kind
    else {
        panic!("second statement must return the local extension call")
    };
    let FirExprKind::Jump {
        value: Some(call), ..
    } = body
        .expr(return_expression)
        .expect("return expression")
        .kind
    else {
        panic!("return must contain the local extension call")
    };
    let FirExprKind::LocalCall {
        extension_receiver,
        arguments,
        ..
    } = &body.expr(call).expect("local extension call").kind
    else {
        panic!("contextual local extension invocation must remain a local call")
    };
    let receiver = extension_receiver.expect("local extension call needs its selected receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver { .. })
    ));
    assert_eq!(arguments.len(), 1);
    assert!(matches!(
        arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
}

#[test]
fn local_nullable_increment_extension_is_a_checked_local_call() {
    let (body, _) = checked_function_body(
        "fun box(): String {\n\
             operator fun Int?.inc(): Int = (this ?: 0) + 1\n\
             var counter: Int? = null\n\
             counter++\n\
             return if (counter == 1) \"OK\" else \"fail\"\n\
         }\n",
        "box",
    );

    let increment = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        matches!(
            expression.kind,
            FirExprKind::LocalCall {
                extension_receiver: Some(_),
                ref arguments,
                ..
            } if arguments.is_empty()
        )
        .then_some(expression)
    });
    assert!(
        increment.is_some(),
        "local inc convention must retain its selected body-local callable identity"
    );
}

fn has_local_extension_call_with_arguments(body: &FirBody, argument_count: usize) -> bool {
    (0..body.expression_count()).any(|raw| {
        let expression = body
            .expr(FirExprId::from_raw(raw as u32))
            .expect("FIR expression");
        matches!(
            expression.kind,
            FirExprKind::LocalCall {
                extension_receiver: Some(_),
                ref arguments,
                ..
            } if arguments.len() == argument_count
        ) || matches!(&expression.kind, FirExprKind::Lambda { body, .. }
            if has_local_extension_call_with_arguments(body, argument_count))
    }) || (0..body.statement_count()).any(|raw| {
        let statement = body
            .statement(FirStatementId::from_raw(raw as u32))
            .expect("FIR statement");
        matches!(&statement.kind, FirStatementKind::LocalFunction { body, .. }
            if has_local_extension_call_with_arguments(body, argument_count))
    })
}

#[test]
fn local_nullable_binary_extension_is_a_checked_local_operator_call() {
    let (body, _) = checked_function_body(
        "fun add(value: Int?): Int {\n\
             operator fun Int?.plus(argument: Int): Int = argument + 2\n\
             return value + 1\n\
         }\n",
        "add",
    );
    assert!(has_local_extension_call_with_arguments(&body, 1));
}

#[test]
fn local_index_get_extension_is_a_checked_local_operator_call() {
    let (body, _) = checked_function_body(
        "class Indexed(val value: Int)\n\
         fun read(): Int {\n\
             operator fun Indexed.get(index: Int): Int = index * value\n\
             val target = Indexed(11)\n\
             return target[2]\n\
         }\n",
        "read",
    );
    assert!(has_local_extension_call_with_arguments(&body, 1));
}

#[test]
fn local_plus_assign_extension_is_a_checked_local_operator_call() {
    let (body, _) = checked_function_body(
        "class Accumulator(var value: Int)\n\
         fun update(): Accumulator {\n\
             operator fun Accumulator.plusAssign(argument: Int) { value += argument }\n\
             var target = Accumulator(1)\n\
             target += 1\n\
             return target\n\
         }\n",
        "update",
    );
    assert!(has_local_extension_call_with_arguments(&body, 1));
}
