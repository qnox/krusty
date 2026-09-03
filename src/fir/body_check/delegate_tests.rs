use super::test_support::checked_function_body;
use super::*;

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
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
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
fn covariant_extension_receiver_widens_from_a_nullable_lambda_result() {
    assert_production_frontend_accepts(
        "class Holder {\n\
             val map: Map<String, String> = mapOf(\"value\" to \"set\")\n\
             val value: String? by map.withDefault { null }\n\
         }\n",
    );
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

    let calls = expressions(&body)
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Call(call) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    let get_value = calls
        .iter()
        .find(|call| {
            call.target
                .module()
                .and_then(|target| index.callable(target))
                .and_then(|callable| index.callable_name(callable.id))
                == Some("getValue")
        })
        .expect("delegated read must keep the selected getValue identity");
    assert!(get_value.dispatch_receiver.is_some());
    assert!(get_value.extension_receiver.is_none());
    assert_eq!(get_value.arguments.len(), 2);
    assert!(expressions(&body).any(|expression| {
        matches!(
            &expression.kind,
            FirExprKind::LocalPropertyReference { name, property_type }
                if name.as_ref() == "value" && property_type.get() == Ty::String
        )
    }));
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

    let selected_names = expressions(&body)
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Call(call) => call
                .target
                .module()
                .and_then(|target| index.callable(target))
                .and_then(|callable| index.callable_name(callable.id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected_names
            .iter()
            .filter(|&&name| name == "getValue")
            .count(),
        3,
        "postfix reads once and prefix re-reads after its checked setter: {selected_names:?}"
    );
    assert_eq!(
        selected_names
            .iter()
            .filter(|&&name| name == "setValue")
            .count(),
        2,
        "each increment must retain the selected delegated setter: {selected_names:?}"
    );
}

#[test]
fn lambda_read_of_local_delegate_captures_storage_identity() {
    let (body, _) = checked_function_body(
        "class Delegate {\n\
             operator fun getValue(owner: Any?, property: Any?): String = \"OK\"\n\
         }\n\
         fun make(): () -> String { val value by Delegate(); return { value } }\n",
        "make",
    );

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
    assert!(expressions(lambda).any(|expression| {
        matches!(
            expression.kind,
            FirExprKind::CapturedValueRead {
                enclosing_depth: 0,
                source,
            } if source == capture.source
        )
    }));
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
