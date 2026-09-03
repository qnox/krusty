use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_semantics,
    jvm_stdlib_semantics, root_expression,
};
use super::*;

#[test]
fn dependency_super_property_keeps_property_identity_and_non_virtual_dispatch() {
    let (body, _) = checked_function_body_with_platform(
        "class A : java.util.ArrayList<String>() {\n\
             fun read(): Int = super.size\n\
         }\n",
        "read",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::PropertyRead {
        target:
            FirPropertyTarget::External {
                dispatch: crate::fir::FirPropertyDispatch::Super { owner, interface },
                ..
            },
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("super property read")
        .kind
    else {
        panic!("dependency super property must remain a checked property access")
    };
    assert!(owner.matches("java/util/ArrayList"));
    assert!(!interface);
}

#[test]
fn legacy_enum_entries_priority_selects_companion_property_first() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +EnumEntries -PrioritizedEnumEntries\n\
         enum class Choice {;\n\
             companion object { val entries = \"OK\" }\n\
         }\n\
         fun use(): String = Choice.entries\n",
        "use",
    );

    assert_eq!(
        body.expr(root_expression(&body))
            .expect("companion property read")
            .ty
            .get(),
        Ty::String,
    );
}

#[test]
fn top_level_property_read_keeps_only_its_stable_property_identity() {
    let (body, index) = checked_function_body("val answer = 42\nfun read() = answer\n", "read");
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        context_arguments: _,
        substitutions,
    } = &body
        .expr(root_expression(&body))
        .expect("root expression")
        .kind
    else {
        panic!("top-level property must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
    assert!(substitutions.is_empty());
}

#[test]
fn imported_object_extension_property_keeps_its_selected_singleton_dispatch() {
    let (body, _) = checked_function_body(
        "import C.ext\n\
         object C { val Int.ext: Int get() = this }\n\
         fun read(): Int = 1.ext\n",
        "read",
    );
    let FirExprKind::PropertyRead {
        dispatch_receiver: Some(dispatch),
        extension_receiver: Some(_),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("root expression")
        .kind
    else {
        panic!("imported object extension property must keep both semantic receivers")
    };
    assert!(matches!(
        body.expr(dispatch.value).expect("singleton dispatch").kind,
        FirExprKind::SingletonValue { classifier, .. } if classifier.matches("C")
    ));
}

#[test]
fn jvm_field_annotation_does_not_change_checked_companion_property_access() {
    let (body, index) = checked_function_body_with_platform(
        "class C { companion object { @JvmField var value: String = \"OK\" } }\n\
         fun read(): String = C.value\n",
        "read",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver: Some(dispatch),
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("companion property read")
        .kind
    else {
        panic!("a companion property must keep its semantic singleton receiver")
    };
    assert!(index
        .property_declaration(target.module().expect("source companion property"))
        .is_some());
    assert!(extension_receiver.is_none());
    assert!(matches!(
        body.expr(dispatch.value).expect("companion singleton").kind,
        FirExprKind::SingletonValue { classifier, .. } if classifier.matches("C$Companion")
    ));
}

#[test]
fn anonymous_property_initializer_uses_the_enclosing_extension_parameter_shape() {
    let (body, _) = checked_function_body(
        "class Box<T>\n\
         open class Holder<T>\n\
         fun <T> Box<T>.make(): Holder<T> = object : Holder<T>() {\n\
             val current: Box<T> = this@make\n\
         }\n\
         fun use(): Int = 1\n",
        "use",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("use result")
            .ty
            .get(),
        Ty::Int,
    );
}

#[test]
fn inferred_anonymous_override_does_not_retry_an_inherited_property_forever() {
    let (body, _) = checked_function_body(
        "open class Parent(open val value: String)\n\
         class Outer(val prefix: String) {\n\
             fun make(suffix: String) = object : Parent(\"fail\") {\n\
                 override val value = this@Outer.prefix + suffix\n\
             }\n\
         }\n\
         fun use(): String = Outer(\"O\").make(\"K\").value\n",
        "use",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("use result")
            .ty
            .get(),
        Ty::String,
    );
}

#[test]
fn inline_extension_property_accessor_sees_its_reified_type_parameter() {
    let (body, _) = checked_function_body_with_platform(
        "val <reified T> T.typeName: String\n\
             inline get() = T::class.simpleName ?: \"\"\n\
         fun read(): String = \"value\".typeName\n",
        "read",
        jvm_stdlib_semantics(),
    );

    assert_eq!(
        body.expr(root_expression(&body))
            .expect("inline extension property read")
            .ty
            .get(),
        Ty::String
    );
}

#[test]
fn coroutine_context_property_becomes_a_checked_current_continuation_operation() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.coroutines.CoroutineContext\n\
         import kotlin.coroutines.coroutineContext\n\
         suspend fun current(): CoroutineContext = coroutineContext\n",
        "current",
        jvm_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("coroutine context read")
        .kind
    else {
        panic!("coroutineContext must become a checked intrinsic call")
    };
    assert!(matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::CoroutineContext,
            receiver: None,
            ref parameters,
            ..
        } if parameters.is_empty()
    ));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_none());
    assert!(call.arguments.is_empty());
}

#[test]
fn dependency_static_field_read_keeps_only_its_provider_identity() {
    let Some(jdk) = crate::toolchain::jdk_modules() else {
        return;
    };
    let mut classpath = crate::toolchain::classpath_jars_for("");
    classpath.push(jdk);
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
    ));
    let (body, _) = checked_function_body_with_platform(
        "fun output(): java.io.PrintStream? = System.out\n",
        "output",
        platform,
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        context_arguments,
        substitutions,
    } = &body
        .expr(root_expression(&body))
        .expect("static field read")
        .kind
    else {
        panic!("a selected dependency field must become checked property FIR")
    };
    assert!(matches!(target, FirPropertyTarget::External { .. }));
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
    assert!(context_arguments.is_empty());
    assert!(substitutions.is_empty());
}

#[test]
fn context_property_read_keeps_the_selected_context_operand() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\ncontext(scope: String)\nval message: String get() = scope\ncontext(scope: String)\nfun read(): String = message\n",
        "read",
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        context_arguments,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("context property read")
        .kind
    else {
        panic!("context property must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
    let [context] = context_arguments.as_ref() else {
        panic!("context property must retain exactly one selected context operand")
    };
    assert!(matches!(
        body.expr(context.value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn generic_context_property_is_specialized_from_the_selected_context_value() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +ContextParameters\n\
         class Result<T>(val x: T)\n\
         context(value: Result<T>) val <T> result: Result<T> get() = value\n\
         fun <T> Result<T>.read(): T = with(result) { x }\n",
        "read",
        jvm_stdlib_semantics(),
    );
    let read = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| {
            matches!(
                &expression.kind,
                FirExprKind::PropertyRead {
                    context_arguments,
                    ..
                } if context_arguments.len() == 1
            )
        })
        .expect("generic context property read");
    let Ty::Obj(result, arguments) = read.ty.get() else {
        panic!("generic context property must retain its applied result type")
    };
    assert!(result.matches("Result"));
    let [argument] = arguments.as_ref() else {
        panic!("Result must retain its selected type argument")
    };
    assert!(argument
        .ty_param_name()
        .is_some_and(|name| name.starts_with('\0')));
}

#[test]
fn same_named_member_properties_rank_accessibility_before_context_specificity() {
    let (ordinary, ordinary_index) = checked_function_body(
        "// LANGUAGE: +ContextParameters
         class PublicValue(val value: Int) {
             context(scope: String) private val value: String get() = scope
         }
         context(scope: String)
         fun read(value: PublicValue): Int = value.value
",
        "read",
    );
    let FirExprKind::PropertyRead {
        target,
        context_arguments,
        ..
    } = &ordinary
        .expr(root_expression(&ordinary))
        .expect("ordinary property read")
        .kind
    else {
        panic!("same-named ordinary property must become checked property FIR")
    };
    assert!(context_arguments.is_empty());
    let declaration = target.module().expect("module property target");
    assert_eq!(
        ordinary_index
            .signature(ordinary_index.property_declaration(declaration).unwrap())
            .unwrap()
            .result
            .get(),
        Ty::Int,
    );

    let (contextual, contextual_index) = checked_function_body(
        "// LANGUAGE: +ContextParameters
         class ContextValue(private val value: Int) {
             context(scope: String) val value: String get() = scope
         }
         context(scope: String)
         fun read(value: ContextValue): String = value.value
",
        "read",
    );
    let FirExprKind::PropertyRead {
        target,
        context_arguments,
        ..
    } = &contextual
        .expr(root_expression(&contextual))
        .expect("context property read")
        .kind
    else {
        panic!("same-named context property must become checked property FIR")
    };
    assert_eq!(context_arguments.len(), 1);
    let declaration = target.module().expect("module property target");
    assert_eq!(
        contextual_index
            .signature(contextual_index.property_declaration(declaration).unwrap())
            .unwrap()
            .result
            .get(),
        Ty::String,
    );
}

#[test]
fn bare_extension_property_read_keeps_its_selected_lambda_receiver() {
    let (body, index) = checked_function_body(
        "val Int.answer: String get() = \"OK\"\n\
         fun receive(block: Int.() -> String): String = 1.block()\n\
         fun read(): String = receive { answer }\n",
        "read",
    );
    let nested = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Lambda { body, .. } = &expression.kind else {
                return None;
            };
            Some(body)
        })
        .expect("receiver lambda");
    let root = root_expression(nested);
    let property = match &nested.expr(root).expect("lambda body").kind {
        FirExprKind::Block {
            result: Some(result),
            ..
        } => *result,
        other => panic!("receiver lambda must retain its result, got {other:?}"),
    };
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver: Some(receiver),
        ..
    } = &nested.expr(property).expect("property read").kind
    else {
        panic!(
            "bare extension property must retain its selected receiver, got {:?}",
            nested.expr(property)
        )
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert!(dispatch_receiver.is_none());
    assert!(matches!(
        nested
            .expr(receiver.value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver { current: true, .. })
    ));
}

#[test]
fn explicit_member_property_read_keeps_stable_target_and_dispatch_receiver() {
    let (body, index) = checked_function_body(
        "class Box(val value: Int)\nfun read(box: Box): Int = box.value\n",
        "read",
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("root expression")
        .kind
    else {
        panic!("member property must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    let receiver = dispatch_receiver.expect("member read needs a dispatch receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
    assert!(extension_receiver.is_none());
}

fn only_block_statement(body: &FirBody) -> FirExprId {
    let FirExprKind::Block { statements, .. } =
        &body.expr(root_expression(body)).expect("root block").kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(expression) = body
        .statement(statements[0])
        .expect("assignment statement")
        .kind
    else {
        panic!("assignment must be an expression statement")
    };
    expression
}

#[test]
fn top_level_property_write_keeps_only_its_stable_property_identity() {
    let (body, index) =
        checked_function_body("var answer = 42\nfun write() { answer = 7 }\n", "write");
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("top-level assignment must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn explicit_member_property_write_keeps_stable_target_and_dispatch_receiver() {
    let (body, index) = checked_function_body(
        "class Box(var value: Int)\nfun write(box: Box) { box.value = 7 }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("member assignment must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    let receiver = dispatch_receiver.expect("member write needs a dispatch receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
    assert!(extension_receiver.is_none());
}

#[test]
fn primitive_write_to_nullable_property_publishes_the_boxing_boundary() {
    let (body, _) = checked_function_body(
        "class Box(var value: Int?)\n\
         fun write(box: Box, value: Int) { box.value = value }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        conversion:
            Some(FirConversion {
                kind: FirConversionKind::NullabilityWidening { to },
                ..
            }),
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("Int to Int? property assignment must publish its boxing boundary")
    };
    assert_eq!(to.get(), Ty::nullable(Ty::Int));
}

#[test]
fn inherited_val_and_var_facets_form_a_writable_fake_override() {
    let (body, index) = checked_function_body(
        "abstract class A { abstract val value: String }\n\
         interface B { var value: String }\n\
         abstract class C : A(), B\n\
         fun write(value: C) { value.value = \"OK\" }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("fake-override assignment must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().expect("stable source property"))
        .is_some());
    assert!(dispatch_receiver.is_some());
}

#[test]
fn explicit_write_uses_the_checked_mutability_of_an_inferred_local_class_property() {
    let _ = checked_function_body(
        "fun box() {\n\
             class Local {\n\
                 var count = 0\n\
                 fun copy() {\n\
                     val result = Local()\n\
                     result.count += 1\n\
                 }\n\
             }\n\
             Local().copy()\n\
         }\n",
        "box",
    );
}

#[test]
fn contextual_member_property_write_keeps_the_selected_context_operand() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +ContextParameters\n\
         class Scope\n\
         class Box {\n\
             context(scope: Scope)\n\
             var value: String\n\
                 get() = \"\"\n\
                 set(value) {}\n\
         }\n\
         context(scope: Scope) fun write(box: Box) { box.value = \"OK\" }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        extension_receiver,
        context_arguments,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("context property write")
        .kind
    else {
        panic!("context property assignment must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert!(dispatch_receiver.is_some());
    assert!(extension_receiver.is_none());
    assert_eq!(context_arguments.len(), 1);
    assert!(matches!(
        body.expr(context_arguments[0].value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn implicit_member_property_read_keeps_the_selected_receiver_coordinate() {
    let (body, index) = checked_function_body(
        "class Box(val value: Int) { fun read(): Int = value }\n",
        "read",
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("property read")
        .kind
    else {
        panic!("implicit member read must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    let receiver = dispatch_receiver.expect("implicit member read needs a receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn implicit_member_property_write_keeps_the_selected_receiver_coordinate() {
    let (body, index) = checked_function_body(
        "class Box(var value: Int) { fun write() { value = 7 } }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("implicit member write must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    let receiver = dispatch_receiver.expect("implicit member write needs a receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn top_level_extension_property_read_keeps_the_extension_receiver() {
    let (body, index) = checked_function_body(
        "val String.tag: Int get() = length\nfun read(value: String): Int = value.tag\n",
        "read",
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("property read")
        .kind
    else {
        panic!("extension read must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert!(dispatch_receiver.is_none());
    let receiver = extension_receiver.expect("extension property needs its value receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn bare_type_parameter_extension_property_boxes_a_primitive_receiver() {
    let (body, _) = checked_function_body(
        "val <T> T.tag: String get() = \"K\"\nfun read(): String = 1.tag\n",
        "read",
    );
    let FirExprKind::PropertyRead {
        extension_receiver: Some(receiver),
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("generic extension property read")
        .kind
    else {
        panic!("generic extension property must retain its receiver")
    };
    assert_eq!(
        body.expr(receiver.value)
            .expect("primitive receiver")
            .ty
            .get(),
        Ty::Int
    );
    assert!(matches!(
        receiver.conversion.map(|conversion| conversion.kind),
        Some(FirConversionKind::NullabilityWidening { to }) if to.get().is_reference()
    ));
}

#[test]
fn generic_extension_properties_are_selected_by_receiver_bounds() {
    let (body, index) = checked_function_body(
        "class C<T>(val value: T)\n\
         val <T: Any?> C<T>.label: String get() = \"nullable\"\n\
         val <T: Any> C<T>.label: String get() = \"nonnull\"\n\
         fun read(nullable: C<String?>, nonnull: C<String>): String =\n\
             nullable.label + nonnull.label\n",
        "read",
    );
    let targets = (0..body.expression_count())
        .filter_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::PropertyRead {
                target,
                extension_receiver: Some(_),
                ..
            } = &expression.kind
            else {
                return None;
            };
            target.module()
        })
        .collect::<Vec<_>>();
    let [nullable, nonnull] = targets.as_slice() else {
        panic!("both bounded extension-property reads must reach checked FIR: {targets:?}")
    };
    assert_ne!(nullable, nonnull);
    assert!(index.property_declaration(*nullable).is_some());
    assert!(index.property_declaration(*nonnull).is_some());
}

#[test]
fn type_parameter_member_scope_uses_its_applied_upper_bound() {
    let (body, index) = checked_function_body(
        "interface State { val ok: Boolean }\n\
         fun <T : State> read(value: T): Boolean = value.ok\n",
        "read",
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("upper-bound property read");
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver: Some(_),
        extension_receiver: None,
        ..
    } = &expression.kind
    else {
        panic!("upper-bound property must become a checked member read")
    };
    assert!(index
        .property_declaration(target.module().expect("source bound property"))
        .is_some());
    assert_eq!(expression.ty.get(), Ty::Boolean);
}

#[test]
fn caller_type_parameter_retains_applied_library_upper_bound() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T : Result<*>> identity(value: T): T = value\n",
        "identity",
        jvm_stdlib_semantics(),
    );
    let ty = body
        .expr(root_expression(&body))
        .expect("bounded parameter read")
        .ty
        .get();
    let Ty::TyParam(_, bound) = ty else {
        panic!("bounded parameter must retain its symbolic identity: {ty:?}")
    };
    assert_eq!(
        bound.kotlin_class_internal(),
        Some(crate::types::type_name("kotlin/Result")),
        "the applied library bound must survive into checked body typing",
    );
    assert_eq!(bound.type_args().len(), 1);
}

#[test]
fn star_applied_library_classifier_exposes_member_property() {
    let (body, _) = checked_function_body_with_platform(
        "fun read(value: Result<*>): Boolean = value.isSuccess\n",
        "read",
        jvm_stdlib_semantics(),
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("star-applied library property read");
    assert!(matches!(
        expression.kind,
        FirExprKind::PropertyRead {
            dispatch_receiver: Some(_),
            extension_receiver: None,
            ..
        }
    ));
    assert_eq!(expression.ty.get(), Ty::Boolean);
}

#[test]
fn library_bounded_type_parameter_exposes_member_property() {
    let (body, _) = checked_function_body_with_platform(
        "fun <T : Result<*>> read(value: T): Boolean = value.isSuccess\n",
        "read",
        jvm_stdlib_semantics(),
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("bounded library property read");
    assert!(matches!(
        expression.kind,
        FirExprKind::PropertyRead {
            dispatch_receiver: Some(_),
            extension_receiver: None,
            ..
        }
    ));
    assert_eq!(expression.ty.get(), Ty::Boolean);
}

#[test]
fn generic_member_lambda_retains_caller_type_parameter_bound() {
    let (body, _) = checked_function_body(
        "interface State { val ok: Boolean }\n\
         class Box<T> { fun go(block: (T) -> Boolean): Boolean = false }\n\
         fun <T : State> read(box: Box<T>): Boolean = box.go { it.ok }\n",
        "read",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("generic member call")
            .ty
            .get(),
        Ty::Boolean,
    );
}

#[test]
fn member_extension_property_read_keeps_both_selected_receivers() {
    let (body, index) = checked_function_body(
        "class Scope { val String.tag: Int get() = length; fun read(value: String): Int = value.tag }\n",
        "read",
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("property read")
        .kind
    else {
        panic!("member extension read must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    let dispatch = dispatch_receiver.expect("member extension needs dispatch receiver");
    assert!(matches!(
        body.expr(dispatch.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
    let extension = extension_receiver.expect("member extension needs extension receiver");
    assert!(matches!(
        body.expr(extension.value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
}

#[test]
fn top_level_extension_property_write_keeps_the_extension_receiver() {
    let (body, index) = checked_function_body(
        "var String.tag: Int\n    get() = length\n    set(value) {}\nfun write(target: String) { target.tag = 7 }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("extension write must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_some());
}

#[test]
fn imported_object_extension_property_write_keeps_its_selected_singleton_dispatch() {
    let (body, _) = checked_function_body(
        "import C.ext\n\
         object C { var Int.ext: Int get() = this; set(value) {} }\n\
         fun write() { 1.ext = 2 }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        dispatch_receiver: Some(dispatch),
        extension_receiver: Some(_),
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("imported object extension property write must keep both semantic receivers")
    };
    assert!(matches!(
        body.expr(dispatch.value).expect("singleton dispatch").kind,
        FirExprKind::SingletonValue { classifier, .. } if classifier.matches("C")
    ));
}

#[test]
fn companion_extension_properties_keep_associated_targets_without_runtime_receivers() {
    let (body, index) = checked_function_body(
        "class C\n\
         companion val C.readonly = \"O\"\n\
         companion var C.mutable = \"\"\n\
         companion fun C.getOk(): String { mutable = \"K\"; return readonly + mutable }\n",
        "getOk",
    );
    let mut reads = 0;
    let mut writes = 0;
    for raw in 0..body.expression_count() {
        let expression = body
            .expr(FirExprId::from_raw(raw as u32))
            .expect("dense FIR expression arena");
        let (target, dispatch_receiver, extension_receiver) = match &expression.kind {
            FirExprKind::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                ..
            } => {
                reads += 1;
                (target, dispatch_receiver, extension_receiver)
            }
            FirExprKind::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver,
                ..
            } => {
                writes += 1;
                (target, dispatch_receiver, extension_receiver)
            }
            _ => continue,
        };
        let property = target
            .module()
            .expect("source companion extension property");
        let declaration = index
            .property_declaration(property)
            .expect("resolved property declaration");
        assert!(index
            .declaration_header(declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
        assert!(dispatch_receiver.is_none());
        assert!(extension_receiver.is_none());
    }
    assert_eq!((reads, writes), (2, 1));
}

#[test]
fn classifier_qualified_companion_property_write_has_no_runtime_receiver() {
    let (body, index) = checked_function_body(
        "class C\n\
         companion var C.mutable: String = \"\"\n\
         fun write() { C.mutable = \"OK\" }\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("associated property write")
        .kind
    else {
        panic!("classifier-qualified associated write must become checked property FIR")
    };
    let declaration = index
        .property_declaration(target.module().expect("source associated property"))
        .expect("associated property declaration");
    assert!(index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn companion_block_property_shadows_same_named_companion_object_property() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +CompanionBlocks +CompanionExtensions\n\
         class C {\n\
             companion { val value: String = \"block\" }\n\
             companion object { val value: String = \"object\" }\n\
         }\n\
         fun box(): String = C.value\n",
        "box",
    );
    let FirExprKind::PropertyRead {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(root_expression(&body))
        .expect("companion-block property read")
        .kind
    else {
        panic!("companion-block property must become checked property FIR")
    };
    let declaration = index
        .property_declaration(target.module().expect("associated property target"))
        .expect("associated property declaration");
    assert!(index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION)));
    assert!(dispatch_receiver.is_none());
    assert!(extension_receiver.is_none());
}

#[test]
fn inferred_signature_selects_companion_block_property_without_a_value_facet() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +CompanionBlocks +CompanionExtensions\n\
         interface C { companion { val value get() = \"OK\" } }\n\
         fun box() = C.value\n",
        "box",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("associated property read")
            .ty
            .get(),
        Ty::String,
    );
}

#[test]
fn member_extension_property_write_keeps_both_selected_receivers() {
    let (body, index) = checked_function_body(
        "class Scope {\n    var String.tag: Int\n        get() = length\n        set(value) {}\n    fun write(target: String) { target.tag = 7 }\n}\n",
        "write",
    );
    let FirExprKind::PropertyWrite {
        target,
        dispatch_receiver,
        extension_receiver,
        ..
    } = &body
        .expr(only_block_statement(&body))
        .expect("property write")
        .kind
    else {
        panic!("member extension write must become checked property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
    let dispatch = dispatch_receiver.expect("member extension needs dispatch receiver");
    assert!(matches!(
        body.expr(dispatch.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
    assert!(extension_receiver.is_some());
}

#[test]
fn safe_member_property_read_wraps_the_already_selected_stable_property() {
    let (body, index) = checked_function_body(
        "class Box(val value: Int)\nfun read(box: Box?): Int? = box?.value\n",
        "read",
    );
    let FirExprKind::SafeCall { selector, .. } =
        body.expr(root_expression(&body)).expect("safe read").kind
    else {
        panic!("safe read must retain an explicit null-guarded selector")
    };
    let FirExprKind::PropertyRead { ref target, .. } =
        body.expr(selector).expect("selected property").kind
    else {
        panic!("safe selector must be the selected property FIR")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
}

#[test]
fn safe_member_property_write_wraps_the_already_selected_stable_property() {
    let (body, index) = checked_function_body(
        "class Box(var value: Int)\nfun update(box: Box?) { box?.value = 7 }",
        "update",
    );
    let FirExprKind::SafeCall { selector, .. } =
        body.expr(only_block_statement(&body)).unwrap().kind
    else {
        panic!("safe assignment must retain an explicit null guard")
    };
    let FirExprKind::PropertyWrite { ref target, .. } = body.expr(selector).unwrap().kind else {
        panic!("safe selector must be the selected property write")
    };
    assert!(index
        .property_declaration(target.module().unwrap())
        .is_some());
}
