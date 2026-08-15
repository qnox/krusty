use super::common;

#[test]
fn nested_hof_keeps_member_extension_element_type() {
    const SOURCE: &str = r#"
        class Entry(val enabled: Boolean)

        class Registry {
            private fun Entry.accepted(): Boolean = enabled

            private fun Collection<Entry>.selected(): List<Entry> =
                mapNotNull { entry ->
                    entry.takeIf { it.accepted() }
                }

            fun result(): String =
                if (listOf(Entry(true), Entry(false)).selected().size == 1) "OK" else "FAIL"
        }

        fun useRegistry(): String = Registry().result()
    "#;

    common::expect_true_e2e(
        "nested_hof_keeps_member_extension_element_type",
        SOURCE,
        &[],
    );
}

#[test]
fn nested_receiver_lambda_keeps_its_member_extension_dispatch_capture() {
    const SOURCE: &str = r#"
        class NestedEntry

        fun <T, R> withReceiver(receiver: T, block: T.() -> R): R = receiver.block()

        class NestedRegistry(private val suffix: String) {
            private fun NestedEntry.accepted(): String = suffix

            fun result(): String {
                val value = listOf(NestedRegistry("K")).map { registry ->
                    val prefix = "O"
                    withReceiver(registry) {
                        prefix + NestedEntry().accepted()
                    }
                }.single()
                return if (value == "OK") "OK" else "FAIL: $value"
            }
        }

        fun box(): String = NestedRegistry("FAIL").result()
    "#;

    common::expect_true_e2e(
        "nested_receiver_lambda_keeps_its_member_extension_dispatch_capture",
        SOURCE,
        &[],
    );
}

#[test]
fn nested_lambda_keeps_context_receiver_checker_coordinate() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class ContextRegistry(val suffix: String) {
            fun Collection<Int>.render(): String =
                map { value ->
                    listOf(value.toString()).map { contextSuffix() }.single()
                }.single()

            fun result(): String = listOf(1).render()
        }

        context(registry: ContextRegistry)
        fun contextSuffix(): String = registry.suffix

        fun box(): String = ContextRegistry("OK").result()
    "#;

    common::expect_true_e2e(
        "nested_lambda_keeps_context_receiver_checker_coordinate",
        SOURCE,
        &[],
    );
}

#[test]
fn ordinary_lambda_captures_current_enclosing_context_receiver() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class CurrentContextScope(val value: String) {
            fun result(): String = listOf(1).map { contextualValue() }.single()
        }

        context(scope: CurrentContextScope)
        fun contextualValue(): String = scope.value

        fun box(): String = CurrentContextScope("OK").result()
    "#;

    common::expect_true_e2e(
        "ordinary_lambda_captures_current_enclosing_context_receiver",
        SOURCE,
        &[],
    );
}

#[test]
fn ordinary_lambda_captures_named_context_parameter() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class NamedContextRegistry(val value: String)

        context(registry: NamedContextRegistry)
        fun namedContextValue(): String = registry.value

        context(registry: NamedContextRegistry)
        fun renderNamedContext(): String =
            listOf(1).map { namedContextValue() }.single()

        fun box(): String = with(NamedContextRegistry("OK")) { renderNamedContext() }
    "#;

    common::expect_true_e2e(
        "ordinary_lambda_captures_named_context_parameter",
        SOURCE,
        &[],
    );
}

#[test]
fn nested_lambdas_thread_named_context_parameter() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class TransitiveContextRegistry(val value: String)

        fun <T, R> transformOne(value: T, block: (T) -> R): R = block(value)

        context(registry: TransitiveContextRegistry)
        fun transitiveContextValue(): String = registry.value

        context(registry: TransitiveContextRegistry)
        fun renderTransitiveContext(): String =
            transformOne(1) {
                transformOne("unused") { transitiveContextValue() }
            }

        fun box(): String =
            with(TransitiveContextRegistry("OK")) { renderTransitiveContext() }
    "#;

    common::expect_true_e2e("nested_lambdas_thread_named_context_parameter", SOURCE, &[]);
}

#[test]
fn lambda_parameter_shadow_keeps_outer_named_context_parameter() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class ShadowContextRegistry(val value: String)

        context(registry: ShadowContextRegistry)
        fun shadowContextValue(): String = registry.value

        context(registry: ShadowContextRegistry)
        fun renderShadowedContext(): String =
            listOf(1).map { registry: Int -> shadowContextValue() }.single()

        fun box(): String =
            with(ShadowContextRegistry("OK")) { renderShadowedContext() }
    "#;

    common::expect_true_e2e(
        "lambda_parameter_shadow_keeps_outer_named_context_parameter",
        SOURCE,
        &[],
    );
}

#[test]
fn nested_receiver_lambda_threads_outer_context_receiver() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        fun <T, R> transformReceiver(value: T, block: T.() -> R): R = value.block()
        fun <T, R> transformValue(value: T, block: (T) -> R): R = block(value)

        class ReceiverOffsetRegistry(val value: String) {
            fun result(): String =
                transformValue(1) {
                    transformReceiver("unused") { receiverOffsetValue() }
                }
        }

        context(registry: ReceiverOffsetRegistry)
        fun receiverOffsetValue(): String = registry.value

        fun box(): String = ReceiverOffsetRegistry("OK").result()
    "#;

    common::expect_true_e2e(
        "nested_receiver_lambda_threads_outer_context_receiver",
        SOURCE,
        &[],
    );
}

#[test]
fn receiver_lambda_threads_outer_context_through_nested_lambda() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        fun <T, R> withReceiver(value: T, block: T.() -> R): R = value.block()

        class Outer(val value: String)
        class Inner

        context(outer: Outer)
        fun outerContextValue(): String = outer.value

        fun box(): String =
            withReceiver(Outer("OK")) {
                withReceiver(Inner()) {
                    listOf(1).map { outerContextValue() }.single()
                }
            }
    "#;

    common::expect_true_e2e(
        "receiver_lambda_threads_outer_context_through_nested_lambda",
        SOURCE,
        &[],
    );
}

#[test]
fn lambda_body_local_shadow_keeps_outer_named_context_parameter() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class LocalShadowContextRegistry(val value: String)

        fun <T, R> transformLocalShadow(value: T, block: (T) -> R): R = block(value)

        context(registry: LocalShadowContextRegistry)
        fun localShadowContextValue(): String = registry.value

        context(registry: LocalShadowContextRegistry)
        fun renderLocalShadowContext(): String =
            transformLocalShadow(1) {
                val registry = 0
                localShadowContextValue()
            }

        fun box(): String =
            with(LocalShadowContextRegistry("OK")) { renderLocalShadowContext() }
    "#;

    common::expect_true_e2e(
        "lambda_body_local_shadow_keeps_outer_named_context_parameter",
        SOURCE,
        &[],
    );
}

#[test]
fn named_context_lambda_preserves_outer_dispatch_receiver_depth() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class NamedContextMarker
        class NamedContextToken

        class NamedContextHost {
            private fun NamedContextToken.render(): String = "OK"

            val action get() = context(marker: NamedContextMarker) fun(): String = NamedContextToken().render()

            fun result(): String = action(NamedContextMarker())
        }

        fun box(): String = NamedContextHost().result()
    "#;

    common::expect_true_e2e(
        "named_context_lambda_preserves_outer_dispatch_receiver_depth",
        SOURCE,
        &[],
    );
}

#[test]
fn lambda_in_named_context_member_preserves_dispatch_receiver_depth() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class Marker

        class Host(val value: String) {
            context(marker: Marker)
            fun result(): String = listOf(1).map { hostValue() }.single()
        }

        context(host: Host)
        fun hostValue(): String = host.value

        fun box(): String = with(Marker()) { Host("OK").result() }
    "#;

    common::expect_true_e2e(
        "lambda_in_named_context_member_preserves_dispatch_receiver_depth",
        SOURCE,
        &[],
    );
}

#[test]
fn local_function_inside_lambda_captures_implicit_context_receiver() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class Registry(val value: String)

        context(registry: Registry)
        fun contextualValue(): String = registry.value

        context(registry: Registry)
        fun render(): String = listOf(1).map {
            fun local(): String = contextualValue()
            local()
        }.single()

        fun box(): String = with(Registry("OK")) { render() }
    "#;

    common::expect_true_e2e(
        "local_function_inside_lambda_captures_implicit_context_receiver",
        SOURCE,
        &[],
    );
}

#[test]
fn generic_extension_property_keeps_receiver_element_type() {
    const SOURCE: &str = r#"
        class Record(val enabled: Boolean)

        class Store {
            private val Collection<Record>.enabledCount: Int
                get() = count { it.enabled }

            fun result(records: Collection<Record>): Int = records.enabledCount
        }
    "#;

    common::expect_true_e2e(
        "generic_extension_property_keeps_receiver_element_type",
        SOURCE,
        &[],
    );
}
