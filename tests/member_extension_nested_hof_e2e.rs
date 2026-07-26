use super::common;

const GENERIC_MEMBER_LIBRARY: &str = r#"
    package sample
    class Entry(val text: String)

    class Cache<A, B, R> {
        fun getOrPut(first: A, second: B?, compute: (A, B?) -> R?): R? =
            compute(first, second)
    }

    class Scope<R>(private val entry: Entry) {
        fun evaluate(block: Entry.() -> R): R = entry.block()
    }

    class Shadow<T> {
        fun <T> transform(value: T, block: (T) -> T): T = block(value)
    }

    open class Parent<T> {
        fun transform(value: T, block: (T) -> T): T = block(value)
    }

    class Child : Parent<Int>()

    class GenericScope<T>(private val value: T) {
        fun <R> evaluate(block: T.() -> R): R = value.block()
    }

    object Factory {
        fun <T> combine(initial: T, block: (T, T) -> T): T =
            block(initial, initial)
    }

    class Selector {
        fun choose(value: Any, block: (Any) -> Int): Int = block(value)
        fun choose(value: String, block: (String) -> Int): Int = block(value)

        fun chooseDefault(
            value: Any,
            padding: Int = 0,
            block: (Any) -> Int,
        ): Int = block(value) + padding

        fun chooseDefault(
            value: String,
            padding: Int = 0,
            block: (String) -> Int,
        ): Int = block(value) + padding
    }
"#;

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

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
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

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn nested_lambda_keeps_enclosing_member_extension_receiver() {
    const SOURCE: &str = r#"
        import sample.Cache
        import sample.Entry

        class Registry {
            private val cache = Cache<Int, String, String>()

            private fun Entry.selected(): String =
                cache.getOrPut(1, "!") { index, suffix ->
                    render(index, suffix)
                } ?: ""

            private fun Entry.render(index: Int, suffix: String?): String =
                text + index + (suffix ?: "")
        }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "nested_member_extension_receiver",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn classpath_member_hof_uses_receiver_type_arguments() {
    const SOURCE: &str = r#"
        import sample.Cache
        import sample.Scope

        private fun select(cache: Cache<Int, String, String>): String =
            cache.getOrPut(1, "!") { number, text ->
                if (number > 0) text ?: "" else ""
            } ?: ""

        private fun evaluate(scope: Scope<String>): String =
            scope.evaluate { text }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "classpath_member_hof_receiver_arguments",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn classpath_object_member_hof_uses_regular_member_resolution() {
    const SOURCE: &str = r#"
        import sample.Factory

        fun box(): String =
            if (Factory.combine(1) { first, second -> first + second } == 2) "OK" else "fail"
    "#;

    common::expect_box_ok_against(
        "classpath_object_member_hof",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    );
}

#[test]
fn classpath_object_is_not_shadowed_by_same_named_source_object_in_another_package() {
    const SUPPORT: &str = r#"
        package other

        object Factory {
            fun local(): Int = 0
        }
    "#;
    const MAIN: &str = r#"
        package consumer

        import sample.Factory

        fun box(): String =
            if (Factory.combine(1) { first, second -> first + second } == 2) "OK" else "fail"
    "#;

    common::expect_box_ok_files_against(
        "classpath_object_same_simple_source_name",
        GENERIC_MEMBER_LIBRARY,
        &[("Support", SUPPORT), ("Main", MAIN)],
    );
}

#[test]
fn member_method_parameter_shadows_class_parameter() {
    const SOURCE: &str = r#"
        import sample.Shadow

        private fun calculate(shadow: Shadow<String>): Int =
            shadow.transform(1) { value -> value + 1 }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "member_method_parameter_shadow",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn inherited_member_hof_uses_applied_supertype_arguments() {
    const SOURCE: &str = r#"
        import sample.Child

        private fun calculate(child: Child): Int =
            child.transform(1) { value -> value + 1 }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "inherited_member_hof_arguments",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn generic_member_receiver_lambda_uses_implicit_receiver() {
    const SOURCE: &str = r#"
        import sample.GenericScope

        private fun calculate(scope: GenericScope<String>): Int =
            scope.evaluate { length }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "generic_member_receiver_lambda",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn member_hof_target_typing_selects_specific_overload() {
    const SOURCE: &str = r#"
        import sample.Selector

        private fun calculate(selector: Selector): Int =
            selector.choose("value") { text -> text.length }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "member_hof_specific_overload",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn named_member_hof_target_typing_selects_specific_overload() {
    const SOURCE: &str = r#"
        import sample.Selector

        private fun calculate(selector: Selector): Int =
            selector.choose(value = "value") { text -> text.length }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "named_member_hof_specific_overload",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn defaulted_member_hof_target_typing_selects_specific_overload() {
    const SOURCE: &str = r#"
        import sample.Selector

        private fun calculate(selector: Selector): Int =
            selector.chooseDefault("value") { text -> text.length }
    "#;

    let Some(diagnostics) = common::checker_diags_against(
        "defaulted_member_hof_specific_overload",
        GENERIC_MEMBER_LIBRARY,
        SOURCE,
    ) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}
