use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_semantics,
    jvm_stdlib_semantics, root_expression,
};
use super::*;

#[test]
fn capture_free_lambda_owns_a_body_local_callable_and_checked_body() {
    let (body, _) = checked_function_body("fun make(): () -> Int = { 42 }\n", "make");
    let FirExprKind::Lambda {
        callable,
        body: lambda_body,
    } = &body.expr(root_expression(&body)).expect("lambda").kind
    else {
        panic!("lambda must become a body-local checked callable")
    };
    assert_eq!(lambda_body.local_callable(), Some(*callable));
    assert!(lambda_body.parameters().is_empty());
    assert!(matches!(
        lambda_body
            .statement(lambda_body.roots()[0])
            .map(|statement| &statement.kind),
        Some(FirStatementKind::Expression(_))
    ));
}

#[test]
fn effect_only_lambda_result_publishes_its_unit_value_widening() {
    let (body, _) = checked_function_body(
        "fun consume(block: () -> Any?) {}\n\
         fun test() { consume { while (false) {} } }\n",
        "test",
    );
    let lambda_body = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Lambda { body, .. } = &expression.kind else {
                return None;
            };
            Some(body.as_ref())
        })
        .expect("checked lambda");
    assert_eq!(
        lambda_body.result_type().map(ResolvedTy::get),
        Some(Ty::nullable(Ty::obj("kotlin/Any")))
    );
    let FirStatementKind::Expression(result) = lambda_body
        .statement(lambda_body.roots()[0])
        .expect("lambda result root")
        .kind
    else {
        panic!("lambda result must be an expression statement")
    };
    assert!(matches!(
        lambda_body.expr(result).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitConversion {
            value,
            conversion: FirConversion {
                kind: FirConversionKind::NullabilityWidening { to },
                ..
            },
        }) if to.get() == Ty::nullable(Ty::obj("kotlin/Any"))
            && lambda_body.expr(*value).is_some_and(|value| value.ty.get() == Ty::Unit)
    ));
}

#[test]
fn lambda_parameters_get_body_local_value_identities() {
    let (body, _) =
        checked_function_body("fun make(): (Int) -> Int = { value -> value }\n", "make");
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body.expr(root_expression(&body)).expect("lambda").kind
    else {
        panic!("lambda must become checked FIR")
    };
    let [parameter] = lambda_body.parameters() else {
        panic!("lambda must publish its value parameter")
    };
    let FirStatementKind::Expression(result) = lambda_body
        .statement(lambda_body.roots()[0])
        .expect("lambda root")
        .kind
    else {
        panic!("lambda root must be an expression")
    };
    assert!(matches!(
        lambda_body.expr(result).map(|expression| &expression.kind),
        Some(FirExprKind::Block {
            result: Some(value),
            ..
        }) if matches!(
            lambda_body.expr(*value).map(|expression| &expression.kind),
            Some(FirExprKind::ValueRead(target)) if *target == parameter.value
        )
    ));
}

#[test]
fn lambda_capture_uses_an_identity_path_instead_of_a_source_name() {
    let (body, _) = checked_function_body("fun make(value: Int): () -> Int = { value }\n", "make");
    let [source_parameter] = body.parameters() else {
        panic!("enclosing function parameter")
    };
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body.expr(root_expression(&body)).expect("lambda").kind
    else {
        panic!("lambda must become checked FIR")
    };
    let [capture] = lambda_body.captures() else {
        panic!("lambda must publish one capture")
    };
    assert_eq!(capture.enclosing_depth, 0);
    assert_eq!(capture.source, source_parameter.value);
}

#[test]
fn sibling_lambdas_use_one_shared_cell_when_either_writes_the_capture() {
    let (body, _) = checked_function_body(
        "fun consume(write: () -> Unit, read: () -> Double) {}\n\
         fun use() {\n\
             var value = 0.0\n\
             consume({ value = 1.0 }, { value })\n\
         }\n",
        "use",
    );
    let captures = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?;
            let FirExprKind::Lambda {
                body: lambda_body, ..
            } = &expression.kind
            else {
                return None;
            };
            let [capture] = lambda_body.captures() else {
                return None;
            };
            Some(*capture)
        })
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[0].source, captures[1].source);
    assert!(captures.iter().all(|capture| capture.shared_cell));
}

#[test]
fn guarded_and_asserted_receivers_capture_their_non_null_flow_type() {
    let (body, index) = checked_function_body(
        "class Token\n\
         fun <T> T.hold(block: (T) -> Unit) = block(this)\n\
         fun consume(value: Token) {}\n\
         fun test(value: Token?) {\n\
             value?.hold { consume(value) }\n\
             value!!.hold { consume(value) }\n\
         }\n",
        "test",
    );
    let captures = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Lambda {
                body: lambda_body, ..
            } = &body.expr(FirExprId::from_raw(raw as u32))?.kind
            else {
                return None;
            };
            let [capture] = lambda_body.captures() else {
                return None;
            };
            let checked_smart_cast = (0..lambda_body.expression_count()).any(|raw| {
                let Some(expression) = lambda_body.expr(FirExprId::from_raw(raw as u32)) else {
                    return false;
                };
                let FirExprKind::Call(call) = &expression.kind else {
                    return false;
                };
                let Some(target) = call.target.module() else {
                    return false;
                };
                index.callable_name(target) == Some("consume")
                    && matches!(
                        call.arguments.as_ref(),
                        [FirCallArgument::Expression {
                            conversion: Some(FirConversion {
                                kind: FirConversionKind::SmartCast { to },
                                ..
                            }),
                            ..
                        }] if to.get() == Ty::obj("Token")
                    )
            });
            Some((*capture, checked_smart_cast))
        })
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2);
    assert!(captures
        .iter()
        .all(|(capture, _)| !capture.ty.get().mentions_pending()));
    assert!(captures.iter().all(|(_, checked)| *checked));
}

#[test]
fn unreachable_trailing_lambda_is_erased_with_its_captured_locals() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun consume(block: () -> Unit) { block() }\n\
         fun remove() {\n\
             throw Exception(\"stop\")\n\
             var captured = 0\n\
             consume { captured = 1 }\n\
         }\n",
        "remove",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Block {
        result: Some(result),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("checked function block")
        .kind
    else {
        panic!("function must retain its checked block")
    };
    assert!(matches!(
        body.expr(*result).map(|expression| &expression.kind),
        Some(FirExprKind::Block {
            statements,
            result: None,
        }) if statements.is_empty()
    ));
    assert!(!(0..body.expression_count()).any(|raw| matches!(
        body.expr(FirExprId::from_raw(raw as u32))
            .map(|expression| &expression.kind),
        Some(FirExprKind::Lambda { .. })
    )));
}

#[test]
fn lambda_in_nested_inner_class_captures_full_outer_receiver_depth() {
    let source = "class Outer {\n\
                      val value = \"OK\"\n\
                      inner class Inner1 {\n\
                          inner class Inner2 {\n\
                              fun read(): String = { value }()\n\
                          }\n\
                      }\n\
                  }\n";
    let (body, _) = checked_function_body(source, "read");

    fn has_outer_capture(body: &FirBody) -> bool {
        (0..body.expression_count()).any(|raw| {
            let Some(expression) = body.expr(FirExprId::from_raw(raw as u32)) else {
                return false;
            };
            match &expression.kind {
                FirExprKind::CapturedImplicitReceiver {
                    enclosing_depth: 0,
                    current: false,
                    depth: 2,
                    ..
                } => true,
                FirExprKind::Lambda { body, .. } => has_outer_capture(body),
                _ => false,
            }
        })
    }

    assert!(has_outer_capture(&body));
}

#[test]
fn receiver_lambda_records_receiver_type_and_selected_implicit_dispatch() {
    let (body, index) = checked_function_body(
        "class Box { fun answer(): Int = 42 }\nfun make(): Box.() -> Int = { answer() }\n",
        "make",
    );
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body
        .expr(root_expression(&body))
        .expect("lambda expression")
        .kind
    else {
        panic!("receiver lambda must become a checked local callable")
    };
    assert!(lambda_body.receiver_type().is_some());
    let root = lambda_body
        .expr(root_expression(lambda_body))
        .expect("receiver member call");
    let call_expression = match &root.kind {
        FirExprKind::Block {
            result: Some(result),
            ..
        } => lambda_body.expr(*result).expect("lambda block result"),
        _ => root,
    };
    let FirExprKind::Call(call) = &call_expression.kind else {
        panic!(
            "receiver lambda body must retain the selected member call, got {:?}",
            call_expression.kind
        )
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    let receiver = call
        .dispatch_receiver
        .expect("receiver member call needs the lambda receiver");
    assert!(matches!(
        lambda_body
            .expr(receiver.value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn delegated_generic_inline_lambda_uses_expected_types_and_keeps_nested_trailing_lambdas() {
    let (body, _) = checked_function_body_with_platform(
        "interface Transform<A : Any, B : Any> {\n\
             fun run(value: A): B\n\
             companion object {\n\
                 inline fun <reified A : Any, reified B : Any> build(\n\
                     crossinline transform: (A) -> B\n\
                 ): Transform<A, B> = object : Transform<A, B> {\n\
                     override fun run(value: A): B = transform(value)\n\
                 }\n\
             }\n\
         }\n\
         class Input(val text: String)\n\
         class Output { var text: String? = null }\n\
         class Wrapped(val input: Input?)\n\
         class Host {\n\
             companion object : Transform<Wrapped, Output> by Transform.build(\n\
                 transform = { Output().apply { text = it.input?.text } }\n\
             )\n\
         }\n\
         fun use(): String = Host.run(Wrapped(Input(\"OK\"))).text ?: \"fail\"\n",
        "use",
        jvm_semantics(),
    );

    assert!(!body.roots().is_empty());
}

#[test]
fn context_lambda_records_context_types_and_selected_receiver_coordinate() {
    let (body, index) = checked_function_body(
        "class Session(val token: String)\n\
         fun action(): context(Session) () -> String = { token }\n",
        "action",
    );
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body
        .expr(root_expression(&body))
        .expect("lambda expression")
        .kind
    else {
        panic!("context lambda must become a checked local callable")
    };
    assert_eq!(lambda_body.context_receiver_types().len(), 1);
    assert!(lambda_body.receiver_type().is_none());
    let root = lambda_body
        .expr(root_expression(lambda_body))
        .expect("lambda block");
    let FirExprKind::Block {
        result: Some(result),
        ..
    } = root.kind
    else {
        panic!("lambda must retain its block result")
    };
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        ..
    } = &lambda_body
        .expr(result)
        .expect("context property read")
        .kind
    else {
        panic!("context property access must retain its selected property")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    let receiver = dispatch_receiver.expect("context property needs its selected receiver");
    assert!(matches!(
        lambda_body
            .expr(receiver.value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver { .. })
    ));
}

#[test]
fn selected_generic_receiver_lambda_uses_the_selected_context_shape() {
    let (body, index) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         // LANGUAGE: +ContextParameters\n\
         typealias StringProvider = () -> String\n\
         context(function: StringProvider) fun doSomething(): String = function()\n\
         fun use(): String = with({ \"\" }) { doSomething() }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let FirExprKind::Call(with_call) = &body.expr(root_expression(&body)).expect("with call").kind
    else {
        panic!("generic receiver-lambda call must remain checked call FIR")
    };
    let FirCallArgument::Expression { value, .. } = &with_call.arguments[1] else {
        panic!("with block must remain an explicit checked argument")
    };
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body.expr(*value).expect("with receiver lambda").kind
    else {
        panic!("with block must remain checked lambda FIR")
    };
    assert!(matches!(
        lambda_body.receiver_type().map(ResolvedTy::get),
        Some(Ty::Fun(signature)) if signature.params.is_empty() && signature.ret == Ty::String
    ));

    let call = (0..lambda_body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &lambda_body.expr(FirExprId::from_raw(raw as u32))?.kind
            else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("doSomething")).then_some(call)
        })
        .expect("contextual call inside specialized receiver lambda");
    let [FirCallArgument::Expression {
        value, parameter, ..
    }] = call.arguments.as_ref()
    else {
        panic!("doSomething must retain exactly one selected context operand")
    };
    assert_eq!(*parameter, 0);
    assert!(matches!(
        lambda_body.expr(*value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver { .. })
    ));
}

#[test]
fn named_context_anonymous_function_binds_its_context_value() {
    let (body, _) = checked_function_body(
        "class Marker(val text: String)\n\
         fun action(): context(Marker) () -> String =\n\
             context(marker: Marker) fun(): String = marker.text\n",
        "action",
    );
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &body
        .expr(root_expression(&body))
        .expect("anonymous function")
        .kind
    else {
        panic!("named context anonymous function must become checked FIR")
    };
    assert_eq!(lambda_body.context_receiver_types().len(), 1);
    assert_eq!(lambda_body.context_value_count(), 1);
    assert_eq!(lambda_body.parameters().len(), 1);
}

#[test]
fn lambda_in_context_member_captures_the_outer_dispatch_coordinate() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +ContextParameters\n\
         class Marker\n\
         fun runOnce(block: () -> String): String = block()\n\
         class Host {\n\
             context(marker: Marker)\n\
             fun result(): String = runOnce { hostValue() }\n\
         }\n\
         context(host: Host)\n\
         fun hostValue(): String = \"OK\"\n",
        "result",
    );
    let lambda_body = (0..body.expression_count()).find_map(|raw| {
        let expression = FirExprId::from_raw(u32::try_from(raw).ok()?);
        match &body.expr(expression)?.kind {
            FirExprKind::Lambda { body, .. } => Some(body.as_ref()),
            _ => None,
        }
    });
    let lambda_body = lambda_body.expect("map argument lambda");
    let [capture] = lambda_body.implicit_receiver_captures() else {
        panic!("lambda must capture exactly the enclosing Host receiver")
    };
    assert_eq!(capture.enclosing_depth, 0);
    assert!(capture.current);
    assert_eq!(capture.depth, 0);
    assert_eq!(
        capture.ty.get().kotlin_class_internal(),
        Some(crate::types::type_name("Host"))
    );
}

#[test]
fn sam_argument_retains_the_selected_interface_method_shape() {
    let (body, _) = checked_function_body(
        "fun interface Action { fun run(value: Int): String }\n\
         fun consume(action: Action): String = \"OK\"\n\
         fun make(): String = consume { value -> \"$value\" }\n",
        "make",
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("call").kind else {
        panic!("SAM argument must belong to the checked source call")
    };
    let FirCallArgument::Expression {
        conversion:
            Some(FirConversion {
                kind: FirConversionKind::Sam(sam),
                ..
            }),
        ..
    } = call.arguments[0]
    else {
        panic!("selected SAM conversion must be explicit in FIR")
    };
    let sam = body.sam_conversion(sam).expect("body-local SAM target");
    assert_eq!(sam.method.as_ref(), "run");
    assert_eq!(sam.parameters.len(), 1);
    assert_eq!(sam.parameters[0].get(), Ty::Int);
    assert_eq!(sam.result.get(), Ty::String);
    assert!(!sam.suspend);
    assert!(!sam.nullable);
}

#[test]
fn selected_sam_recheck_keeps_members_from_every_type_parameter_bound() {
    let (body, index) = checked_function_body(
        "interface X { fun foo(): String = \"O\" }\n\
         interface Z { fun bar(): String = \"K\" }\n\
         interface A : X, Z\n\
         interface B : X, Z\n\
         fun interface Action<T : X> { fun accept(value: T) }\n\
         fun <T> select(left: T, right: T): T = left\n\
         class G<T>(private val value: T) where T : X, T : Z {\n\
             fun check(action: Action<in T>) { action.accept(value) }\n\
         }\n\
         fun run() {\n\
             val g = select(G<A>(object : A {}), G<B>(object : B {}))\n\
             g.check { it.foo(); it.bar() }\n\
         }\n",
        "run",
    );

    let lambda_body = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?;
        match &expression.kind {
            FirExprKind::Lambda { body, .. } => Some(body.as_ref()),
            _ => None,
        }
    });
    let lambda_body = lambda_body.expect("selected SAM argument must remain checked FIR");
    let selected = (0..lambda_body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &lambda_body
                .expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?
                .kind
            else {
                return None;
            };
            let target = call.target.module()?;
            let name = index
                .callable(target)
                .and_then(|callable| index.callable_name(callable.id))?;
            let receiver = call.dispatch_receiver.as_ref()?;
            let receiver_ty = lambda_body.expr(receiver.value)?.ty.get();
            matches!(name, "foo" | "bar").then_some((name, receiver_ty))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected,
        [("foo", Ty::obj("X")), ("bar", Ty::obj("Z")),],
        "each intersection member must carry its concrete selected receiver view",
    );
}

#[test]
fn postponed_call_solution_rechecks_earlier_lambda_body_with_concrete_parameter() {
    let (body, index) = checked_function_body(
        "class Concrete\n\
         open class TargetBase { val member: Concrete = Concrete() }\n\
         class Target : TargetBase()\n\
         class Builder<T>\n\
         fun consume(value: TargetBase) {}\n\
         fun <T> build(first: Builder<T>.(T) -> T, second: Builder<T>.(T) -> Unit) {}\n\
         fun run() {\n\
             build(\n\
                 { it.member; Target() },\n\
                 { consume(it) },\n\
             )\n\
         }\n",
        "run",
    );

    let selected = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?;
        let FirExprKind::Lambda {
            body: lambda_body, ..
        } = &expression.kind
        else {
            return None;
        };
        let property = (0..lambda_body.expression_count()).find_map(|nested| {
            let expression = lambda_body.expr(FirExprId::from_raw(u32::try_from(nested).ok()?))?;
            matches!(expression.kind, FirExprKind::PropertyRead { .. }).then_some(expression)
        })?;
        Some((lambda_body.as_ref(), property))
    });
    let (lambda_body, property) =
        selected.expect("first lambda must retain its checked member read");
    let [parameter] = lambda_body.parameters() else {
        panic!("receiver lambda must publish its one value parameter")
    };
    assert_eq!(parameter.ty.get(), Ty::obj("TargetBase"));
    assert_eq!(property.ty.get(), Ty::obj("Concrete"));
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver: Some(receiver),
        ..
    } = &property.kind
    else {
        panic!("concrete member must remain a selected property read")
    };
    assert!(index
        .property_declaration(target.module().expect("source property target"))
        .is_some());
    assert_eq!(
        lambda_body
            .expr(receiver.value)
            .expect("checked lambda parameter read")
            .ty
            .get(),
        Ty::obj("TargetBase"),
    );
}

#[test]
fn covariant_receiver_lambda_input_fixes_empty_nested_generic_results() {
    let (body, index) = checked_function_body(
        "class Flow<out T>\n\
         class Collector<in T>\n\
         fun <T> flow(block: suspend Collector<T>.() -> Unit): Flow<T> = Flow<T>()\n\
         fun <T> Flow<T>.flatMap(mapper: suspend (T) -> Flow<T>): Flow<T> = this\n\
         fun select(input: Flow<Int>): Flow<Int> = input.flatMap {\n\
             if (it == 1) flow {} else flow {}\n\
         }\n",
        "select",
    );

    let outer = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?;
        match &expression.kind {
            FirExprKind::Lambda { body, .. } => Some(body.as_ref()),
            _ => None,
        }
    });
    let outer = outer.expect("flatMap argument must remain a checked suspend lambda");
    let nested_flows = (0..outer.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &outer
                .expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?
                .kind
            else {
                return None;
            };
            let target = call.target.module()?;
            (index
                .callable(target)
                .and_then(|callable| index.callable_name(callable.id))
                == Some("flow"))
            .then_some(call)
        })
        .collect::<Vec<_>>();
    assert_eq!(nested_flows.len(), 2);
    for call in nested_flows {
        assert_eq!(call.substitutions.len(), 1);
        assert_eq!(call.substitutions[0].value.get(), Ty::Int);
    }
}

#[test]
fn nearer_source_extension_shapes_lambda_before_a_more_specific_imported_extension() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T, R> T.map(transform: suspend (T) -> R): suspend () -> R =\n\
             { transform(this) }\n\
         fun use(): suspend () -> String? =\n\
             Result.success(\"OK\").map<Result<String>, String?> { it.getOrNull() }\n",
        "use",
        jvm_stdlib_semantics(),
    );

    let lambda = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Lambda { body, .. } = &body
                .expr(FirExprId::from_raw(u32::try_from(raw).ok()?))?
                .kind
            else {
                return None;
            };
            (!body.parameters().is_empty()).then_some(body.as_ref())
        })
        .expect("map transform must remain a checked suspend lambda");
    assert_eq!(
        lambda.parameters()[0].ty.get(),
        Ty::obj_args("kotlin/Result", &[Ty::String]),
    );
}

#[test]
fn nullable_function_value_to_nullable_sam_records_conditional_conversion() {
    let (body, _) = checked_function_body(
        "fun interface Action { fun run() }\n\
         fun consume(action: Action?) {}\n\
         fun maybe(flag: Boolean): (() -> Unit)? = if (flag) null else {{}}\n\
         fun use() { consume(maybe(true)) }\n",
        "use",
    );
    let call = (0..body.expression_count()).find_map(|raw| {
        let expression = FirExprId::from_raw(u32::try_from(raw).ok()?);
        match &body.expr(expression)?.kind {
            FirExprKind::Call(call)
                if call.arguments.iter().any(|argument| {
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
                }) =>
            {
                Some(call)
            }
            _ => None,
        }
    });
    let call = call.expect("consume call");
    let FirCallArgument::Expression {
        conversion:
            Some(FirConversion {
                kind: FirConversionKind::Sam(sam),
                ..
            }),
        ..
    } = call.arguments[0]
    else {
        panic!("nullable SAM conversion must be explicit in FIR")
    };
    assert!(
        body.sam_conversion(sam)
            .expect("body-local SAM target")
            .nullable
    );
}
