use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;

#[test]
fn function_value_invoke_keeps_callee_identity_and_final_parameter_types() {
    let (body, _) =
        checked_function_body("fun apply(block: (Int) -> Int): Int = block(2)\n", "apply");
    let FirExprKind::FunctionInvoke {
        callee,
        context_arguments,
        arguments,
        parameter_types,
        result,
        suspend,
    } = &body
        .expr(root_expression(&body))
        .expect("root expression")
        .kind
    else {
        panic!("function value call must become function-invoke FIR")
    };
    assert!(matches!(
        body.expr(*callee).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
    assert_eq!(arguments.len(), 1);
    assert!(context_arguments.is_empty());
    assert_eq!(parameter_types.len(), 1);
    assert_eq!(result.get(), Ty::Int);
    assert!(!suspend);
}

#[test]
fn generic_lower_bounds_preserve_the_common_function_shape() {
    let (body, _) = checked_function_body(
        "open class Result\n\
         class First : Result()\n\
         class Second : Result()\n\
         fun <T> choose(vararg values: T): T = values[0]\n\
         fun run(): Result {\n\
             val selected = choose({ First() }, { Second() })\n\
             return selected()\n\
         }\n",
        "run",
    );

    let invocation = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::FunctionInvoke { .. }))
        .expect("common function lower bound must remain callable");
    assert_eq!(invocation.ty.get(), Ty::obj("Result"));
    let FirExprKind::FunctionInvoke { result, .. } = &invocation.kind else {
        unreachable!()
    };
    assert_eq!(result.get(), Ty::obj("Result"));
}

#[test]
fn branch_lub_preserves_the_common_function_shape() {
    let (body, _) = checked_function_body(
        "open class Result\n\
         class First : Result()\n\
         class Second : Result()\n\
         fun run(first: Boolean): Result {\n\
             val selected = if (first) ({ First() }) else ({ Second() })\n\
             return selected()\n\
         }\n",
        "run",
    );

    let invocation = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::FunctionInvoke { .. }))
        .expect("branch common type must remain callable");
    assert_eq!(invocation.ty.get(), Ty::obj("Result"));
}

#[test]
fn extension_access_binds_receiver_in_checked_fir_before_ordinary_invoke() {
    let (body, _) = checked_function_body(
        "class A\n\
         val action: Any.() -> String = { \"OK\" }\n\
         fun box(): String = A().(action)()\n",
        "box",
    );
    let FirExprKind::FunctionInvoke {
        callee, arguments, ..
    } = &body
        .expr(root_expression(&body))
        .expect("root invocation")
        .kind
    else {
        panic!("bound extension-function value must use ordinary checked invocation")
    };
    assert!(arguments.is_empty());
    let FirExprKind::ExtensionFunctionBinding {
        receiver,
        callable,
        target_parameters,
        receiver_parameter,
        target_result,
        suspend,
    } = &body.expr(*callee).expect("extension binding").kind
    else {
        panic!("extension access must remain a first-class checked FIR expression")
    };
    assert_eq!(*receiver_parameter, 0);
    assert_eq!(target_parameters.len(), 1);
    assert_eq!(target_parameters[0].get(), Ty::obj("kotlin/Any"));
    assert_eq!(target_result.get(), Ty::String);
    assert!(!suspend);
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ConstructorCall(_))
    ));
    assert!(matches!(
        body.expr(*callable).map(|expression| &expression.kind),
        Some(FirExprKind::PropertyRead { .. })
    ));
}

#[test]
fn inapplicable_qualified_constructor_falls_through_to_companion_invoke() {
    let (body, index) = checked_function_body_with_platform(
        "class A {\n\
             class Nested {\n\
                 companion object { operator fun invoke(i: Int) = i }\n\
             }\n\
         }\n\
         fun box() = if (A.Nested(42) == 42) \"OK\" else \"fail\"\n",
        "box",
        jvm_stdlib_semantics(),
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let receiver = call.dispatch_receiver.as_ref()?;
            matches!(
                body.expr(receiver.value).map(|expression| &expression.kind),
                Some(FirExprKind::SingletonValue { .. })
            )
            .then_some(call)
        })
        .expect("selected companion invoke must be a checked FIR call");
    let FirCallTarget::Module(target) = call.target else {
        panic!("source companion invoke must retain stable callable identity")
    };
    assert!(index.callable(target).is_some());
    let receiver = call
        .dispatch_receiver
        .as_ref()
        .expect("companion singleton dispatch receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::SingletonValue { .. })
    ));
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn function_value_invoke_contextually_specializes_a_nested_constructor() {
    let (body, _) = checked_function_body(
        "class NullableBox<T : String?>(val value: T)\n\
         fun apply(block: (NullableBox<String?>) -> NullableBox<String?>): NullableBox<String?> =\n\
             block.invoke(NullableBox(null))\n",
        "apply",
    );
    let constructor = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        matches!(expression.kind, FirExprKind::ConstructorCall(_)).then_some(expression)
    });
    let constructor = constructor.expect("nested constructor call");
    assert_eq!(
        constructor.ty.get(),
        Ty::obj_args("NullableBox", &[Ty::nullable(Ty::String)]),
    );
}

#[test]
fn function_value_invoke_keeps_checker_selected_implicit_context_binding() {
    let (body, _) = checked_function_body(
        "class Session\n\
         context(session: Session) fun apply(block: context(Session) () -> Int): Int = block()\n",
        "apply",
    );
    let FirExprKind::FunctionInvoke {
        context_arguments,
        arguments,
        parameter_types,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("function invoke")
        .kind
    else {
        panic!("context function value call must become function-invoke FIR")
    };
    assert_eq!(context_arguments.len(), 1);
    assert!(arguments.is_empty());
    assert_eq!(parameter_types.len(), 1);
    assert!(matches!(
        body.expr(context_arguments[0].value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn smart_cast_this_context_argument_keeps_receiver_identity() {
    let source = "// LANGUAGE: +ContextParameters\n\
                  // WITH_STDLIB\n\
                  interface Scope\n\
                  fun apply(value: Any, block: context(Scope, Scope) () -> Unit) {\n\
                      with(value) {\n\
                          if (this is Scope) block()\n\
                      }\n\
                  }\n";
    let (body, _) = checked_function_body_with_platform(source, "apply", jvm_stdlib_semantics());

    fn has_context_invoke(body: &FirBody) -> bool {
        (0..body.expression_count()).any(|raw| {
            let Some(expression) = body.expr(FirExprId::from_raw(raw as u32)) else {
                return false;
            };
            match &expression.kind {
                FirExprKind::FunctionInvoke {
                    context_arguments, ..
                } if context_arguments.len() == 2 => context_arguments.iter().all(|argument| {
                    matches!(
                        body.expr(argument.value).map(|expression| &expression.kind),
                        Some(FirExprKind::ImplicitReceiver { current: true, .. })
                    )
                }),
                FirExprKind::Lambda { body, .. } => has_context_invoke(body),
                _ => false,
            }
        })
    }

    assert!(has_context_invoke(&body));
}

#[test]
fn top_level_context_function_property_invoke_keeps_property_and_context() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\nval action: context(String) () -> String = { substring(0) }\ncontext(value: String) fun box(): String = action()\n",
        "box",
    );
    let FirExprKind::FunctionInvoke {
        callee,
        context_arguments,
        arguments,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("function invoke")
        .kind
    else {
        panic!("context function property call must become function-invoke FIR")
    };
    let FirExprKind::PropertyRead { target, .. } =
        &body.expr(*callee).expect("selected property callee").kind
    else {
        panic!("callee must retain the selected stable property read")
    };
    assert!(index.property(target.module().unwrap()).is_some());
    assert_eq!(context_arguments.len(), 1);
    assert!(arguments.is_empty());
}

#[test]
fn top_level_receiver_function_property_invoke_keeps_stable_property_callee() {
    let (body, index) = checked_function_body(
        "val action = fun String.(suffix: String): String = this + suffix\n\
         fun box(): String = \"O\".action(\"K\")\n",
        "box",
    );
    let FirExprKind::FunctionInvoke {
        callee, arguments, ..
    } = &body
        .expr(root_expression(&body))
        .expect("receiver function invoke")
        .kind
    else {
        panic!("receiver-function property must become function-invoke FIR")
    };
    let FirExprKind::PropertyRead { target, .. } =
        &body.expr(*callee).expect("selected property callee").kind
    else {
        panic!("callee must retain the selected stable property read")
    };
    assert!(index.property(target.module().unwrap()).is_some());
    assert_eq!(arguments.len(), 2, "receiver plus one value argument");
}

#[test]
fn operator_invoke_keeps_selected_stable_callable() {
    let (body, index) = checked_function_body(
        "class Operation { operator fun invoke(value: Int): Int = value }\n\
         fun apply(operation: Operation): Int = operation(2)\n",
        "apply",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("root expression")
        .kind
    else {
        panic!("operator invoke must become a checked callable application")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn parenthesized_infix_result_selects_extension_operator_invoke() {
    let (body, index) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         operator fun <K, V> Pair<K, V>.invoke(block: (K, V) -> Boolean): Boolean = true\n\
         fun apply(): Boolean = (\"A\" to \"B\") { left, right -> left != right }\n",
        "apply",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("checked invoke call")
        .kind
    else {
        panic!("extension operator invoke must be a checked callable application")
    };
    let target = call
        .target
        .module()
        .expect("source extension invoke target");
    assert_eq!(
        index
            .callable(target)
            .and_then(|callable| index.callable_name(callable.id)),
        Some("invoke")
    );
    assert!(call.dispatch_receiver.is_none());
    let extension_receiver = call
        .extension_receiver
        .as_ref()
        .expect("pair result extension receiver");
    assert!(matches!(
        body.expr(extension_receiver.value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::Call(_))
    ));
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn object_call_selects_invoke_instead_of_a_synthetic_constructor() {
    let (body, index) = checked_function_body(
        "object Operation { operator fun invoke(): Int = 42 }\n\
         fun apply(): Int = Operation()\n",
        "apply",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("object invocation")
        .kind
    else {
        panic!("object call must become a checked invoke application")
    };
    let target = call.target.module().expect("source invoke target");
    assert_eq!(
        index
            .callable(target)
            .and_then(|callable| index.callable_name(callable.id)),
        Some("invoke")
    );
    assert!(call.dispatch_receiver.is_some());
    assert!(call.extension_receiver.is_none());
}

#[test]
fn safe_callable_property_invokes_an_operator_declared_on_its_superinterface() {
    let (body, index) = checked_function_body(
        "interface Action\n\
         operator fun Action.invoke(): String = \"OK\"\n\
         class Operation : Action\n\
         object Holder { val operation = Operation() }\n\
         fun apply(): String = Holder?.operation()!!\n",
        "apply",
    );
    let FirExprKind::TypeOperation {
        operation: FirTypeOperation::NotNullAssertion,
        operand: safe,
        ..
    } = body
        .expr(root_expression(&body))
        .expect("not-null assertion")
        .kind
    else {
        panic!("the source result unwrap must remain explicit")
    };
    let FirExprKind::SafeCall { receiver, selector } = &body.expr(safe).expect("safe call").kind
    else {
        panic!("callable property selection must remain null guarded")
    };
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::SingletonValue { .. })
    ));

    let FirExprKind::Call(call) = &body.expr(*selector).expect("invoke selector").kind else {
        panic!("the safe selector must retain the selected invoke call")
    };
    let invoke = call
        .target
        .module()
        .expect("source extension invoke target");
    assert_eq!(
        index
            .callable(invoke)
            .and_then(|callable| index.callable_name(callable.id)),
        Some("invoke")
    );
    assert!(call.dispatch_receiver.is_none());
    let property_value = call
        .extension_receiver
        .as_ref()
        .expect("property value supplies the invoke extension receiver");
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver: Some(property_owner),
        ..
    } = &body
        .expr(property_value.value)
        .expect("checked property value")
        .kind
    else {
        panic!("invoke receiver must remain a stable checked property read")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert_eq!(property_owner.value, receiver.value);
}

#[test]
fn not_null_assertion_propagates_nullable_expectation_into_generic_call() {
    let (body, _) = checked_function_body(
        "fun <T> produce(): T = 1L as T\n\
         fun consume(): Int = produce()!!\n",
        "consume",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("not-null assertion");
    let FirExprKind::TypeOperation {
        operation: FirTypeOperation::NotNullAssertion,
        operand,
        target,
    } = root.kind
    else {
        panic!("source assertion must remain an explicit checked FIR operation")
    };
    assert_eq!(target.get(), Ty::Int);
    assert_eq!(root.ty.get(), Ty::Int);
    assert_eq!(
        body.expr(operand).expect("generic call operand").ty.get(),
        Ty::nullable(Ty::Int)
    );
}

#[test]
fn explicitly_imported_enum_entry_invoke_keeps_entry_and_extension_target() {
    let (body, index) = checked_function_body(
        "import Choice.ONE\n\
         enum class Choice { ONE, TWO }\n\
         operator fun Choice.invoke(value: Int): Int = value\n\
         fun apply(): Int = ONE(42)\n",
        "apply",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("imported enum entry invocation")
        .kind
    else {
        panic!("the selected extension invoke must become a checked call")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    let receiver = call
        .extension_receiver
        .as_ref()
        .expect("the imported enum entry is the extension receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::EnumEntry { classifier, name, .. })
            if classifier.matches("Choice") && name.as_ref() == "ONE"
    ));
}

#[test]
fn callable_extension_property_contextualizes_invoke_lambda_receiver() {
    let (body, index) = checked_function_body(
        "open class Base\n\
         class Derived : Base()\n\
         class Action { operator fun invoke(block: Action.() -> Unit): Int = 2 }\n\
         val Base.action: Any get() = Any()\n\
         val Derived.action: Action get() = Action()\n\
         fun apply(value: Derived): Int = value.action {}\n",
        "apply",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("property invoke")
        .kind
    else {
        panic!("callable extension property must become a checked invoke call")
    };
    let target = call.target.module().expect("source invoke target");
    assert_eq!(
        index
            .callable(target)
            .and_then(|callable| index.callable_name(callable.id)),
        Some("invoke")
    );
    let receiver = call
        .dispatch_receiver
        .as_ref()
        .expect("Action value dispatches invoke");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::PropertyRead { .. })
    ));
    let [FirCallArgument::Expression { value, .. }] = call.arguments.as_ref() else {
        panic!("invoke must retain one lambda argument")
    };
    let Some(FirExprKind::Lambda {
        body: lambda_body, ..
    }) = body.expr(*value).map(|expression| &expression.kind)
    else {
        panic!("invoke argument must remain a checked lambda")
    };
    assert_eq!(
        lambda_body.receiver_type(),
        Some(ResolvedTy::new(Ty::obj("Action")).unwrap())
    );
}

#[test]
fn generic_inline_invoke_preserves_a_nonlocal_return_as_nothing() {
    let (body, _) = checked_function_body(
        "class Action { inline operator fun <T> invoke(block: () -> T): T = block() }\n\
         fun apply(action: Action): String {\n\
             val value = action { return \"OK\" }\n\
         }\n",
        "apply",
    );
    let has_nothing_call = (0..body.expression_count()).any(|raw| {
        body.expr(FirExprId::from_raw(raw as u32))
            .is_some_and(|expression| {
                matches!(expression.kind, FirExprKind::Call(_))
                    && expression.ty.get() == Ty::Nothing
            })
    });
    assert!(has_nothing_call, "{body:#?}");
}

#[test]
fn qualified_enum_entry_invoke_keeps_entry_and_extension_target() {
    let (body, index) = checked_function_body(
        "enum class Choice { ONE, TWO }\n\
         operator fun Choice.invoke(value: Int): Int = value\n\
         fun apply(): Int = Choice.ONE(42)\n",
        "apply",
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("qualified enum entry invocation")
        .kind
    else {
        panic!("the selected extension invoke must become a checked call")
    };
    assert!(index.callable(call.target.module().unwrap()).is_some());
    assert!(call.dispatch_receiver.is_none());
    let receiver = call
        .extension_receiver
        .as_ref()
        .expect("the qualified enum entry is the extension receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::EnumEntry { classifier, name, .. })
            if classifier.matches("Choice") && name.as_ref() == "ONE"
    ));
}

#[test]
fn inherited_companion_property_uses_nominal_invoke_in_both_signature_and_body_passes() {
    let (body, index) = checked_function_body(
        "interface Action { operator fun invoke(): String }\n\
         abstract class Base(val action: Action)\n\
         interface Host {\n\
             companion object : Base(object : Action {\n\
                 override fun invoke(): String = \"OK\"\n\
             })\n\
         }\n\
         fun use() = Host.Companion.action()\n",
        "use",
    );
    let FirExprKind::Call(call) = &body.expr(root_expression(&body)).expect("invoke call").kind
    else {
        panic!("nominal invoke must become a checked callable application")
    };
    let target = call.target.module().expect("source invoke target");
    assert_eq!(
        index
            .callable(target)
            .and_then(|callable| index.callable_name(callable.id)),
        Some("invoke")
    );
    let receiver = call
        .dispatch_receiver
        .as_ref()
        .expect("property value is the invoke receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::PropertyRead { .. })
    ));
}
