use super::test_support::{checked_function_body, root_expression};
use super::*;
use crate::fir::LocalDelegatedPropertyId;

fn expressions(body: &FirBody) -> impl Iterator<Item = &FirExpr> {
    (0..body.expression_count()).filter_map(|raw| {
        body.expr(FirExprId::from_raw(
            u32::try_from(raw).expect("too many FIR expressions"),
        ))
    })
}

fn assert_production_frontend_accepts(source: &str) {
    let inputs = [crate::source::SourceInput::kotlin(source).with_file_stem("DelegateFir")];
    let stems = ["DelegateFir".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = crate::diag::DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath)
                .expect("JVM provider initialization"),
        ),
        &crate::features::LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);

    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn generic_sam_constructor_infers_the_delegated_property_result() {
    assert_production_frontend_accepts(
        "import kotlin.reflect.KProperty\n\
         fun interface ReadOnlyProperty<in T, out V> {\n\
             operator fun getValue(thisRef: T, property: KProperty<*>): V\n\
         }\n\
         fun box(): String {\n\
             val property by ReadOnlyProperty { _, reference -> reference }\n\
             return property.name\n\
         }\n",
    );
}

#[test]
fn reified_enum_delegate_anonymous_object_uses_the_enclosing_formal_identity() {
    assert_production_frontend_accepts(
        "import kotlin.properties.ReadWriteProperty\n\
         import kotlin.reflect.KProperty\n\
         enum class Enumeration { OK }\n\
         inline fun <reified T : Enum<T>> delegate() =\n\
             object : ReadWriteProperty<Any?, T?> {\n\
                 override fun getValue(thisRef: Any?, property: KProperty<*>): T? =\n\
                     Enumeration.OK as T?\n\
                 override fun setValue(\n\
                     thisRef: Any?, property: KProperty<*>, value: T?\n\
                 ) {}\n\
             }\n\
         class Klass { var enumeration: Enumeration? by delegate() }\n",
    );
}

#[test]
fn inline_delegate_convention_records_its_exact_reified_parameter() {
    let (body, _index) = checked_function_body(
        "class Delegate<T, R>(val value: T)\n\
         inline operator fun <T, reified R> Delegate<T, R>.getValue(\n\
             owner: Any?, property: Any?\n\
         ): T = value\n\
         inline fun <T, reified R> read(value: T): T {\n\
             val local by Delegate<T, R>(value)\n\
             return local\n\
         }\n",
        "read",
    );

    let [plan] = body.local_delegate_plans() else {
        panic!("the local delegated property records one semantic plan")
    };
    assert!(matches!(
        plan.get_value.substitutions.as_ref(),
        [
            FirTypeSubstitution { reified: false, .. },
            FirTypeSubstitution { reified: true, .. }
        ]
    ));
}

#[test]
fn covariant_extension_receiver_widens_from_a_nullable_lambda_result() {
    assert_production_frontend_accepts(
        "class Holder {\n\
             val map: Map<String, String> = mapOf(\"value\" to \"set\")\n\
             val value: String? by map.withDefault { null }\n\
         }\n",
    );
}

#[test]
fn delegate_member_substitution_names_its_receiver_class_parameter() {
    let (body, index) = checked_function_body(
        "class Unrelated<T>\n\
         class Delegate<T>(val value: T) {\n\
             operator fun getValue(owner: Any?, property: Any?): T = value\n\
         }\n\
         fun read(value: String): String {\n\
             val local by Delegate(value)\n\
             return local\n\
         }\n",
        "read",
    );

    let [plan] = body.local_delegate_plans() else {
        panic!("the local delegated property records one semantic plan")
    };
    let callable = index
        .callable(
            plan.get_value
                .target
                .module()
                .expect("module delegate operator"),
        )
        .expect("selected callable header");
    let owner = index
        .declaration_anchor(callable.declaration)
        .and_then(|anchor| anchor.owner)
        .expect("the selected member's receiver declaration");
    assert_eq!(index.declaration_name(owner), Some("Delegate"));
    let parameter = index
        .type_parameter(owner, 0)
        .expect("receiver type parameter");
    assert_eq!(
        plan.get_value.substitutions.as_ref(),
        &[FirTypeSubstitution {
            parameter: parameter.into(),
            reified: false,
            value: ResolvedTy::new(Ty::String).unwrap(),
            reified_runtime: ResolvedTy::new(Ty::String).unwrap(),
            additional_bounds: Box::new([]),
        }],
    );
}

fn selected_name<'i>(
    call: &FirDelegateCall,
    index: &'i crate::fir::ResolvedModuleIndex,
) -> Option<&'i str> {
    call.target
        .module()
        .and_then(|target| index.callable(target))
        .and_then(|callable| index.callable_name(callable.id))
}

fn delegate_accesses(body: &FirBody, write: bool) -> usize {
    expressions(body)
        .filter(|expression| {
            matches!(
                &expression.kind,
                FirExprKind::LocalDelegateAccess { value, .. }
                    if value.is_some() == write
            )
        })
        .count()
}

#[test]
fn local_delegate_read_keeps_selected_module_operator_and_semantic_property_reference() {
    let (body, index) = checked_function_body(
        "class Delegate {\n\
             operator fun getValue(owner: Any?, property: Any?): String = \"OK\"\n\
         }\n\
         fun box(): String { val value by Delegate(); return value }\n",
        "box",
    );

    let [plan] = body.local_delegate_plans() else {
        panic!("a read-only local delegated property records exactly one semantic plan")
    };
    assert_eq!(selected_name(&plan.get_value, &index), Some("getValue"));
    assert!(plan.get_value.dispatch_receiver.is_none());
    assert!(!plan.get_value.extension);
    assert_eq!(plan.get_value.parameters.len(), 2);
    assert!(
        (0..body.statement_count()).all(|raw| !matches!(
            body.statement(FirStatementId::from_raw(raw as u32))
                .map(|statement| &statement.kind),
            Some(FirStatementKind::LocalFunction { .. })
        )),
        "the frontend must not manufacture a target helper"
    );
    assert_eq!(delegate_accesses(&body, false), 1);
    assert!(expressions(&body).any(|expression| {
        matches!(
            &expression.kind,
            FirExprKind::LocalPropertyReference { name, property_type, .. }
                if name.as_ref() == "value" && property_type.get() == Ty::String
        )
    }));
    let FirExprKind::Block { statements, .. } = &body.expr(root_expression(&body)).unwrap().kind
    else {
        panic!("function body must remain a checked block")
    };
    let FirStatementKind::Local { target, .. } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("local delegate storage must be a checked local")
    };
    assert_eq!(body.debug_value_name(*target), Some("value$delegate"));
}

#[test]
fn local_delegate_increments_keep_checked_getter_and_setter_calls() {
    let (body, index) = checked_function_body(
        "class Delegate {\n\
             operator fun getValue(owner: Any?, property: Any?): Int = 0\n\
             operator fun setValue(owner: Any?, property: Any?, value: Int) {}\n\
         }\n\
         fun update(): Int {\n\
             var value by Delegate()\n\
             val old = value++\n\
             val new = ++value\n\
             return old + new\n\
         }\n",
        "update",
    );

    let [plan] = body.local_delegate_plans() else {
        panic!("a mutable local delegated property records one semantic plan")
    };
    assert_eq!(selected_name(&plan.get_value, &index), Some("getValue"));
    assert_eq!(
        selected_name(plan.set_value.as_ref().unwrap(), &index),
        Some("setValue")
    );
    assert_eq!(
        delegate_accesses(&body, false),
        3,
        "postfix reads once and prefix re-reads after its checked write"
    );
    assert_eq!(
        delegate_accesses(&body, true),
        2,
        "each increment must retain one selected delegated write"
    );
}

#[test]
fn lambda_read_of_local_delegate_captures_storage_identity() {
    let (body, _index) = checked_function_body(
        "class Delegate {\n\
             operator fun getValue(owner: Any?, property: Any?): String = \"OK\"\n\
         }\n\
         fun make(): () -> String { val value by Delegate(); return { value } }\n",
        "make",
    );

    let [_plan] = body.local_delegate_plans() else {
        panic!("a read-only local delegated property records exactly one semantic plan")
    };
    let lambda = expressions(&body)
        .find_map(|expression| match &expression.kind {
            FirExprKind::Lambda { body, .. } => Some(body.as_ref()),
            _ => None,
        })
        .expect("delegated read must remain inside a checked lambda body");
    let [capture] = lambda.captures() else {
        panic!("the lambda must capture exactly the delegate storage identity")
    };
    assert_eq!(capture.enclosing_depth, 0);
    assert!(!capture.shared_cell);
    assert_eq!(
        delegate_accesses(lambda, false),
        1,
        "the lambda refers to its enclosing body's semantic delegate plan"
    );
}

#[test]
fn member_extension_delegate_keeps_its_independent_dispatch_receiver() {
    let (body, index) = checked_function_body(
        "class Delegate\n\
         class Scope {\n\
             operator fun Delegate.getValue(owner: Any?, property: Any?): String = \"OK\"\n\
             fun read(): String { val value by Delegate(); return value }\n\
         }\n",
        "read",
    );

    let [plan] = body.local_delegate_plans() else {
        panic!("the local delegated property records one semantic plan")
    };
    assert_eq!(selected_name(&plan.get_value, &index), Some("getValue"));
    assert!(plan.get_value.extension);
    assert!(plan.get_value.dispatch_receiver.is_some());
    assert_eq!(
        plan.get_value_dispatch,
        Some(
            crate::fir::FirLocalDelegateDispatchParameter::ImplicitReceiver(
                crate::fir::FirCapturedReceiver::Enclosing,
            )
        ),
        "the accessor plan retains the source role of its selected dispatch receiver"
    );
    let accesses = expressions(&body)
        .filter_map(|expression| match &expression.kind {
            FirExprKind::LocalDelegateAccess {
                dispatch_receiver,
                value: None,
                ..
            } => Some(dispatch_receiver),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [access] = accesses.as_slice() else {
        panic!("one delegated read is expected")
    };
    assert!(
        access.is_some(),
        "the selected dispatch receiver is an explicit operand"
    );
}

#[test]
fn property_reference_delegate_uses_the_normally_selected_conventions() {
    let source = r#"interface I {
        var z: String
    }

    class X {
        var p: String = "Fail"
    }

    class A {
        val x = X()
        val y = object : I {
            override var z: String by x::p
        }
    }"#;
    assert_production_frontend_accepts(source);
}

#[test]
fn local_class_inferred_callable_property_is_visible_to_the_enclosing_body() {
    assert_production_frontend_accepts(
        r#"import kotlin.properties.Delegates.notNull

        fun box(): String {
            val suffix by lazy { "K" }
            class Local(val prefix: String) {
                val read = { prefix + suffix }
            }
            return Local("O").read()
        }"#,
    );
}

#[test]
fn anonymous_inferred_callable_property_can_capture_local_delegate_storage() {
    assert_production_frontend_accepts(
        r#"import kotlin.properties.Delegates.notNull

        fun box(): String {
            var value by notNull<String>()
            val holder = object {
                val read = { value }
            }
            value = "OK"
            return holder.read()
        }"#,
    );
}

#[test]
fn extension_property_delegate_selects_the_extension_receiver_as_this_ref() {
    assert_production_frontend_accepts(
        r#"class Delegate {
            operator fun getValue(owner: A, property: kotlin.reflect.KProperty<*>): Int = 1
        }

        class A

        val A.top: Int by Delegate()

        class Holder {
            val A.member: Int by Delegate()
        }"#,
    );
}

#[test]
fn member_extension_delegate_keeps_both_selected_receivers() {
    assert_production_frontend_accepts(
        r#"class Delegate

        class Host {
            operator fun Delegate.getValue(
                owner: Host,
                property: kotlin.reflect.KProperty<*>,
            ): String = "OK"

            operator fun Delegate.setValue(
                owner: Host,
                property: kotlin.reflect.KProperty<*>,
                value: String,
            ) {}

            var result: String by Delegate()
        }"#,
    );
}

#[test]
fn delegated_property_expected_type_refines_a_generic_factory_call() {
    assert_production_frontend_accepts(
        r#"var result: (() -> String)? by property(null)

        fun <T> property(initial: T): RwProperty<T> = RwProperty(initial)

        class RwProperty<T>(var value: T) {
            operator fun getValue(
                owner: Any?,
                property: kotlin.reflect.KProperty<*>,
            ): T = value

            operator fun setValue(
                owner: Any?,
                property: kotlin.reflect.KProperty<*>,
                value: T,
            ) {
                this.value = value
            }
        }"#,
    );
}

#[test]
fn nullable_delegate_uses_an_applicable_extension_convention() {
    assert_production_frontend_accepts(
        r#"operator fun Any?.getValue(owner: Any?, property: Any?): String = "OK"

        val result: String by null"#,
    );
}

#[test]
fn inferred_mutable_delegate_uses_the_specialized_inherited_setter() {
    assert_production_frontend_accepts(
        r#"open class Parent<T>(private var value: T) {
            protected operator fun getValue(owner: Any?, property: Any?): T = value
            protected operator fun setValue(owner: Any?, property: Any?, value: T) {
                this.value = value
            }
        }

        class Child : Parent<Long>(42L) {
            inner class Inner {
                var result by this@Child
            }
        }"#,
    );
}

#[test]
fn delegated_property_keeps_symbolic_super_constructor_arguments() {
    assert_production_frontend_accepts(
        r#"interface ValueDelegate<T> {
            operator fun getValue(owner: Any?, property: kotlin.reflect.KProperty<*>): T
        }

        abstract class Entity<R>(val delegate: ValueDelegate<R>) {
            operator fun provideDelegate(
                owner: Any?,
                property: kotlin.reflect.KProperty<*>,
            ): ValueDelegate<R> = delegate
        }

        abstract class Option<T : Any, R>(delegate: ValueDelegate<R>) : Entity<R>(delegate)

        class NullableValue<T : Any> : ValueDelegate<T?> {
            override fun getValue(owner: Any?, property: kotlin.reflect.KProperty<*>): T? = null
        }

        class NullableOption<T : Any> : Option<T, T?>(NullableValue<T>())

        fun box() {
            val value: String? by NullableOption<String>()
        }"#,
    );
}

#[test]
fn delegated_property_context_refines_nested_generic_constructor_arguments() {
    assert_production_frontend_accepts(
        r#"import kotlin.reflect.KProperty

        class Descriptor<T>
        interface ValueDelegate<T> {
            operator fun getValue(owner: Any?, property: KProperty<*>): T = error("unused")
        }

        abstract class Entity<R>(val delegate: ValueDelegate<R>) {
            operator fun provideDelegate(
                owner: Any?,
                property: KProperty<*>,
            ): ValueDelegate<R> = delegate
        }

        abstract class Option<T : Any, R>(delegate: ValueDelegate<R>) : Entity<R>(delegate)
        class NullableValue<T : Any>(descriptor: Descriptor<T>) : ValueDelegate<T?>
        class NullableOption<T : Any>(descriptor: Descriptor<T>) :
            Option<T, T?>(NullableValue(descriptor))

        fun box() {
            val value: List<Any>? by NullableOption(Descriptor())
        }"#,
    );
}

#[test]
fn delegate_this_ref_argument_applies_a_raw_generic_constructor_receiver() {
    assert_production_frontend_accepts(
        r#"import kotlin.reflect.KProperty

        class Delegate<in R>(val suffix: String) {
            operator fun getValue(owner: R, property: KProperty<*>): String =
                owner.toString() + suffix

            operator fun setValue(owner: R, property: KProperty<*>, value: String?) {}
        }

        var String.result: String by Delegate("K")"#,
    );
}

#[test]
fn delegate_expected_type_adaptation_reaches_a_fixed_point() {
    assert_production_frontend_accepts(
        r#"val x: String.() -> String = { this }

        fun box() {
            val receiverShape: String.() -> String by ::x
            val parameterShape: (String) -> String by ::x
        }"#,
    );
}

#[test]
fn receiver_function_invoke_reads_its_local_delegate_before_invocation() {
    assert_production_frontend_accepts(
        r#"val source: String.() -> String = { this }

        fun box(): String {
            val delegated: String.() -> String by ::source
            return "OK".delegated()
        }"#,
    );
}

#[test]
fn local_class_delegated_property_publishes_its_checked_signature() {
    assert_production_frontend_accepts(
        r#"inline operator fun String.getValue(
            owner: Any?,
            property: kotlin.reflect.KProperty<*>,
        ): String = property.name

        fun box(): String {
            class Local {
                val OK by ""
            }
            return Local().OK
        }"#,
    );
}

#[test]
fn declared_number_delegate_result_compares_with_its_int_value() {
    assert_production_frontend_accepts(
        r#"class Delegate {
            operator fun getValue(owner: Any?, property: kotlin.reflect.KProperty<*>): Int = 1
        }
        class Owner {
            val value: Number by Delegate()
        }
        fun box(): String = if (Owner().value == 1) "OK" else "fail""#,
    );
}

#[test]
fn convention_constraints_refine_an_unbound_generic_delegate_factory() {
    assert_production_frontend_accepts(
        r#"object Host {
            interface Delegate<D, E, R>

            fun <D, E, R> delegate(): Delegate<D, E, R> =
                object : Delegate<D, E, R> {}

            operator fun <D, E, R> Delegate<D, E, R>.provideDelegate(
                host: D,
                property: Any?,
            ): Delegate<D, E, R> = this

            operator fun <D, E, R> Delegate<D, E, R>.getValue(
                receiver: E,
                property: Any?,
            ): R = "OK" as R

            val Long.result: String by delegate()
        }"#,
    );
}

#[test]
fn top_level_const_property_reference_is_a_stable_delegate_target() {
    assert_production_frontend_accepts(
        r#"const val SOURCE: String = "OK"
        val result: String by ::SOURCE"#,
    );
}

#[test]
fn generic_provide_delegate_anonymous_result_uses_its_public_supertype() {
    assert_production_frontend_accepts(
        r#"
        import kotlin.properties.ReadOnlyProperty
        import kotlin.reflect.KProperty

        operator fun <C, T> T.provideDelegate(thisRef: C, property: KProperty<*>) =
            object : ReadOnlyProperty<C, T> {
                override operator fun getValue(thisRef: C, property: KProperty<*>) =
                    this@provideDelegate
            }

        val number by 42
        val text by "OK"
        fun box(): String = if (number == 42) text else "Fail"
        "#,
    );
}

#[test]
fn inferred_member_delegate_result_applies_its_dispatch_receiver_arguments() {
    assert_production_frontend_accepts(
        r#"class Delegate<T>(private val value: T) {
            operator fun getValue(owner: Any?, property: Any?) = value
        }

        class Owner {
            val value by Delegate(1)
        }

        fun box(): String = if (Owner().value != 1) "Fail" else "OK""#,
    );
}

#[test]
fn delegated_property_constructor_lambda_preserves_its_generic_result_constraint() {
    assert_production_frontend_accepts(
        r#"class Wrapped(val number: Int)

        class Delegate<T>(private val read: () -> T) {
            operator fun getValue(owner: Any?, property: Any?): T = read()
        }

        object Owner {
            val value by Delegate { Wrapped(42) }
        }

        fun box(): String = if (Owner.value.number == 42) "OK" else "Fail""#,
    );
}

/// The declaration named by every local property reference in `body` and the bodies of the
/// lambdas it contains, in checker order.
fn local_property_references(body: &FirBody) -> Vec<LocalDelegatedPropertyId> {
    let mut references = Vec::new();
    for expression in expressions(body) {
        match &expression.kind {
            FirExprKind::LocalPropertyReference { declaration, .. } => {
                references.push(*declaration)
            }
            FirExprKind::Lambda { body, .. } => references.extend(local_property_references(body)),
            _ => {}
        }
    }
    references
}

fn local_delegate_plan_declarations(body: &FirBody) -> Vec<LocalDelegatedPropertyId> {
    body.local_delegate_plans()
        .iter()
        .map(
            |plan| match &body.expr(plan.reference).expect("plan reference").kind {
                FirExprKind::LocalPropertyReference { declaration, .. } => *declaration,
                kind => panic!("local delegate plan has non-reference operand {kind:?}"),
            },
        )
        .collect()
}

fn local_delegate_access_plans(body: &FirBody) -> Vec<LocalDelegatedPropertyId> {
    let mut accesses = Vec::new();
    for expression in expressions(body) {
        match &expression.kind {
            FirExprKind::LocalDelegateAccess { plan, .. } => accesses.push(*plan),
            FirExprKind::Lambda { body, .. } => {
                accesses.extend(local_delegate_access_plans(body));
            }
            _ => {}
        }
    }
    accesses
}

const DELEGATE: &str = "class Delegate(val value: String) {\n\
                            operator fun getValue(owner: Any?, property: Any?): String = value\n\
                        }\n";

#[test]
fn same_named_local_delegates_in_sibling_scopes_are_different_properties() {
    let (body, _) = checked_function_body(
        &format!(
            "{DELEGATE}fun sibling(flag: Boolean): String {{\n\
                 if (flag) {{ val x by Delegate(\"a\"); return x }}\n\
                 else {{ val x by Delegate(\"b\"); return x }}\n\
             }}\n"
        ),
        "sibling",
    );
    let declarations = local_delegate_plan_declarations(&body);
    let [first, second] = declarations[..] else {
        panic!("two local delegate plans, found {declarations:?}")
    };
    assert_ne!(first, second);
    assert_eq!(first.owner(), second.owner());
    assert_eq!((first.ordinal(), second.ordinal()), (0, 1));
}

#[test]
fn a_shadowing_local_delegate_is_a_different_property_from_the_one_it_shadows() {
    let (body, _) = checked_function_body(
        &format!(
            "{DELEGATE}fun nested(): String {{\n\
                 val x by Delegate(\"outer\")\n\
                 val inner = if (x.length > 0) {{ val x by Delegate(\"inner\"); x }} else \"\"\n\
                 return inner + x\n\
             }}\n"
        ),
        "nested",
    );
    let declarations = local_delegate_plan_declarations(&body);
    let [outer, inner] = declarations[..] else {
        panic!("outer and inner plans, found {declarations:?}")
    };
    assert_ne!(outer, inner);
    assert_eq!(outer.owner(), inner.owner());
    assert_eq!((outer.ordinal(), inner.ordinal()), (0, 1));
    let accesses = local_delegate_access_plans(&body);
    assert_eq!(accesses.len(), 3, "outer is read twice and inner once");
    assert_eq!(accesses.iter().filter(|&&plan| plan == outer).count(), 2);
    assert_eq!(accesses.iter().filter(|&&plan| plan == inner).count(), 1);
}

#[test]
fn a_captured_local_delegate_keeps_its_declaration_identity_inside_a_lambda() {
    let (body, _) = checked_function_body(
        &format!(
            "{DELEGATE}fun invoke(block: () -> String): String = block()\n\
             fun captured(): String {{\n\
                 val x by Delegate(\"c\")\n\
                 return invoke {{ x }} + x\n\
             }}\n"
        ),
        "captured",
    );
    let declarations = local_delegate_plan_declarations(&body);
    let [declaration] = declarations[..] else {
        panic!("one local delegated declaration, found {declarations:?}")
    };
    assert_eq!(declaration.ordinal(), 0);
    assert_eq!(
        local_delegate_access_plans(&body),
        [declaration, declaration],
        "the lambda and enclosing read must both address the same semantic plan"
    );
}

#[test]
fn provide_read_write_and_lambda_name_one_local_property() {
    let (body, _) = checked_function_body(
        "class Delegate(var value: String) {\n\
             operator fun provideDelegate(thisRef: Any?, property: Any?): Delegate = this\n\
             operator fun getValue(thisRef: Any?, property: Any?): String = value\n\
             operator fun setValue(thisRef: Any?, property: Any?, value: String) {\n\
                 this.value = value\n\
             }\n\
         }\n\
         fun observe(block: () -> String): String = block()\n\
         fun use(): String {\n\
             var value by Delegate(\"v\")\n\
             value = value\n\
             return observe { value }\n\
         }\n",
        "use",
    );
    let references = local_property_references(&body);
    let [provided, accessor_template] = references[..] else {
        panic!(
            "provideDelegate and the accessor template must name the property, found {references:?}"
        )
    };
    assert_eq!(provided, accessor_template);
    assert_eq!(local_delegate_plan_declarations(&body), [provided]);
    assert_eq!(
        local_delegate_access_plans(&body),
        [provided, provided, provided],
        "read, write and lambda read must all address the same semantic plan"
    );
}
