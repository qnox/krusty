use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_semantics,
    jvm_stdlib_semantics, root_expression,
};
use super::*;

#[test]
fn destructuring_evaluates_one_initializer_and_keeps_selected_component_identity() {
    let (body, index) = checked_function_body(
        "class Pair(val value: Int) { operator fun component1(): Int = value }\n\
         fun read(pair: Pair): Int { val (value) = pair; return value }\n",
        "read",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Destructure {
        initializer,
        entries,
    } = &body
        .statement(statements[0])
        .expect("destructuring statement")
        .kind
    else {
        panic!("destructuring must have a dedicated checked FIR statement")
    };
    assert!(matches!(
        body.expr(*initializer).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
    assert_eq!(entries.len(), 1);
    let FirDestructureEntry::Binding {
        target: local,
        component,
        ..
    } = entries[0]
    else {
        panic!("entry must bind a local and call component1")
    };
    let FirExprKind::Call(call) = &body.expr(component).expect("component call").kind else {
        panic!("component selection must be a checked call")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert_eq!(
        call.dispatch_receiver
            .expect("component member needs the initializer receiver")
            .value,
        *initializer
    );
    assert!(call.extension_receiver.is_none());

    let FirStatementKind::Expression(return_expression) = body
        .statement(statements[1])
        .expect("return statement")
        .kind
    else {
        panic!("return must be an expression statement")
    };
    let FirExprKind::Jump {
        value: Some(return_value),
        ..
    } = body
        .expr(return_expression)
        .expect("return expression")
        .kind
    else {
        panic!("return must be a checked jump")
    };
    assert!(matches!(
        body.expr(return_value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(target)) if *target == local
    ));
}

#[test]
fn multiple_smartcast_destructure_projects_each_component_receiver() {
    let (body, index) = checked_function_body(
        "interface IC1 { operator fun component1(): String }\n\
         interface IC2 { operator fun component2(): String }\n\
         fun read(value: Any): String {\n\
             if (value is IC1 && value is IC2) {\n\
                 val (first, second) = value\n\
                 return first + second\n\
             }\n\
             return \"\"\n\
         }\n",
        "read",
    );

    let mut components = std::collections::HashMap::new();
    for raw in 0..body.expression_count() {
        let Some(FirExpr {
            kind: FirExprKind::Call(call),
            ..
        }) = body.expr(FirExprId::from_raw(raw as u32))
        else {
            continue;
        };
        let Some(target) = call.target.module() else {
            continue;
        };
        let Some(name @ ("component1" | "component2")) = index.callable_name(target) else {
            continue;
        };
        components.insert(
            name,
            call.dispatch_receiver.expect("component member receiver"),
        );
    }

    let component1 = components
        .get("component1")
        .expect("component1 must resolve through IC1");
    let Some(FirConversion {
        kind: FirConversionKind::SmartCast { to },
        ..
    }) = component1.conversion
    else {
        panic!("component1 must retain its IC1 intersection projection")
    };
    assert_eq!(to.get(), Ty::obj("IC1"));

    let component2 = components
        .get("component2")
        .expect("component2 must resolve through IC2");
    assert_eq!(component1.value, component2.value);
    if let Some(FirConversion {
        kind: FirConversionKind::SmartCast { to },
        ..
    }) = component2.conversion
    {
        assert_eq!(to.get(), Ty::obj("IC2"));
    }
}

#[test]
fn generic_local_extension_component_keeps_body_local_identity_and_specialized_type() {
    let (body, _) = checked_function_body(
        "class Box<T>(val value: T)\n\
         fun read(box: Box<Int>): Int {\n\
             operator fun <R> Box<R>.component1(): R = value\n\
             val (result) = box\n\
             return result\n\
         }\n",
        "read",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::LocalFunction { callable, .. } =
        &body.statement(statements[0]).expect("local component").kind
    else {
        panic!("component declaration must be a body-local callable")
    };
    let FirStatementKind::Destructure {
        initializer,
        entries,
    } = &body.statement(statements[1]).expect("destructure").kind
    else {
        panic!("destructuring must have checked FIR")
    };
    let FirDestructureEntry::Binding { component, ty, .. } = entries[0] else {
        panic!("component must bind the result")
    };
    assert_eq!(ty.get(), Ty::Int);
    let FirExprKind::LocalCall {
        target,
        extension_receiver: Some(receiver),
        arguments,
    } = &body.expr(component).expect("local component call").kind
    else {
        panic!("local extension component must remain a local FIR call")
    };
    assert_eq!(target.callable, *callable);
    assert_eq!(target.body_depth, 0);
    assert_eq!(receiver.value, *initializer);
    assert!(arguments.is_empty());
}

#[test]
fn underscore_destructure_entry_has_no_binding_or_component_call() {
    let (body, _) = checked_function_body(
        "class Pair(val value: Int) { operator fun component1(): Int = value }\n\
         fun ignore(pair: Pair) { val (_) = pair }\n",
        "ignore",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Destructure { entries, .. } = &body
        .statement(statements[0])
        .expect("destructuring statement")
        .kind
    else {
        panic!("destructuring must have a dedicated checked FIR statement")
    };
    assert_eq!(entries.len(), 1);
    assert!(matches!(entries[0], FirDestructureEntry::Ignored { .. }));
}

#[test]
fn escaped_underscore_is_a_binding_while_bare_underscore_is_ignored() {
    let (body, _) = checked_function_body(
        "class Pair(val value: Int) {\n\
             operator fun component1(): Int = value\n\
             operator fun component2(): Int = value + 1\n\
         }\n\
         fun read(pair: Pair): Int { val (_, `_`) = pair; return `_` }\n",
        "read",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Destructure { entries, .. } =
        &body.statement(statements[0]).expect("destructure").kind
    else {
        panic!("destructuring must have checked FIR")
    };
    assert!(matches!(entries[0], FirDestructureEntry::Ignored { .. }));
    assert!(matches!(entries[1], FirDestructureEntry::Binding { .. }));
}

#[test]
fn name_based_destructure_publishes_a_property_read_not_a_getter_call() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
         class Props { val computed: Int get() = 2 }\n\
         fun read(props: Props): Int { val (computed) = props; return computed }\n",
        "read",
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Destructure {
        initializer,
        entries,
    } = &body
        .statement(statements[0])
        .expect("name-based destructuring statement")
        .kind
    else {
        panic!("name-based destructuring must have checked FIR")
    };
    let FirDestructureEntry::Binding { component, .. } = entries[0] else {
        panic!("named component must bind a local")
    };
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body.expr(component).expect("component property read").kind
    else {
        panic!("a selected source property is a FIR property read")
    };
    assert!(index.property(target.module().unwrap()).is_some());
    assert_eq!(
        dispatch_receiver
            .expect("member property uses the destructured value")
            .value,
        *initializer
    );
    assert!(extension_receiver.is_none());
}

#[test]
fn name_based_destructure_keeps_a_dependency_property_getter_identity() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
         fun read(indexed: IndexedValue<String>): String {\n\
             (val value) = indexed\n\
             return value\n\
         }\n",
        "read",
        jvm_semantics(),
    );
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Destructure {
        initializer,
        entries,
    } = &body
        .statement(statements[0])
        .expect("name-based destructuring statement")
        .kind
    else {
        panic!("name-based destructuring must have checked FIR")
    };
    let FirDestructureEntry::Binding { component, .. } = entries[0] else {
        panic!("named component must bind a local")
    };
    let FirExprKind::Call(call) = &body.expr(component).expect("dependency getter call").kind
    else {
        panic!("a dependency property getter must be a checked call")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert_eq!(
        call.dispatch_receiver
            .expect("member getter uses the destructured value")
            .value,
        *initializer
    );
    assert!(call.extension_receiver.is_none());
}

#[test]
fn mutable_destructure_keeps_its_explicit_nullable_write_type() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +NameBasedDestructuring +EnableNameBasedDestructuringShortForm\n\
         fun test(first: Int?, second: Int?): Unit {\n\
             if (first == null || second == null) return\n\
             var [left: Int?, right: Int?] = first to second\n\
             left = null\n\
         }\n",
        "test",
        jvm_stdlib_semantics(),
    );

    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(&body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let entry = statements
        .iter()
        .filter_map(|statement| body.statement(*statement))
        .find_map(|statement| match &statement.kind {
            FirStatementKind::Destructure { entries, .. } => entries.first().copied(),
            _ => None,
        })
        .expect("checked destructuring entry");
    let FirDestructureEntry::Binding { ty, conversion, .. } = entry else {
        panic!("first destructuring entry must bind a local")
    };
    assert_eq!(ty.get(), Ty::nullable(Ty::Int));
    assert!(matches!(
        conversion,
        Some(FirConversion {
            kind: FirConversionKind::NullabilityWidening { to },
            ..
        }) if to.get() == Ty::nullable(Ty::Int)
    ));
}
