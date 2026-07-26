//! A generic extension invoked through an implicit receiver must infer its result type from the lambda
//! body before a following generic extension target-types its own lambda parameter.

use super::common;

#[test]
fn chained_grouping_lambda_destructures_map_not_null_result() {
    const SOURCE: &str = r#"
        // LANGUAGE: +NameBasedDestructuring
        class Entry(val enabled: Boolean)
        class Scope
        class Marker(val accepted: Boolean)

        class Registry {
            private fun Entry.resolve(
                scope: Scope? = null,
                predicate: (Marker) -> Boolean = { true },
            ): Marker? = Marker(enabled).takeIf(predicate)

            private fun accepts(marker: Marker): Boolean = marker.accepted

            private fun Collection<Entry>.grouped(scope: Scope): Map<Boolean, List<Pair<Entry, Marker>>> =
                mapNotNull { entry ->
                    entry.takeIf { it.enabled }?.resolve(scope) { accepts(it) }?.let { entry to it }
                }.groupBy { [entry, marker] ->
                    entry.enabled && marker.accepted
                }
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
fn safe_call_ordinary_member_keeps_precedence_over_member_extension() {
    const SOURCE: &str = r#"
        class Entry {
            fun resolve(predicate: (String) -> Boolean): String? = "ok".takeIf(predicate)
        }
        class Marker(val accepted: Boolean)

        class Registry {
            private fun Entry.resolve(predicate: (Marker) -> Boolean): Marker? =
                Marker(true).takeIf(predicate)

            fun check(entry: Entry?): String? = entry?.resolve { it.length > 0 }
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
fn safe_call_member_extensions_run() {
    const SOURCE: &str = r##"
        class Entry(val text: String)

        class Node(val text: String) {
            private fun Node.render(): String = text + "%"

            fun read(other: Node?): String = other?.render() ?: "none"
        }

        class Registry {
            private fun suffix(value: String): String = value

            private fun Entry.resolve(transform: (String) -> String): String =
                transform(text) + suffix("")

            private fun String.decorate(transform: (String) -> String): String =
                transform(this)

            private fun <T> T.adapt(transform: (T) -> T): T =
                transform(this)

            fun read(entry: Entry?): String = entry?.resolve { it + "!" } ?: "none"
            fun direct(entry: Entry): String = entry.resolve { it + "." }
            fun decorate(value: String?): String = value?.decorate { it + "?" } ?: "none"
            fun adapt(value: String?): String = value?.adapt { it + "*" } ?: "none"
        }

        object Formatter {
            private fun String.mark(): String = this + "#"

            fun render(value: String?): String = value?.mark() ?: "none"
        }

        fun box(): String {
            val registry = Registry()
            if (registry.read(Entry("OK")) != "OK!") return "object"
            if (registry.read(null) != "none") return "object-null"
            if (registry.direct(Entry("OK")) != "OK.") return "object-direct"
            if (registry.decorate("OK") != "OK?") return "string"
            if (registry.decorate(null) != "none") return "string-null"
            if (registry.adapt("OK") != "OK*") return "generic"
            if (registry.adapt(null) != "none") return "generic-null"
            if (Formatter.render("OK") != "OK#") return "object-dispatch"
            if (Formatter.render(null) != "none") return "object-dispatch-null"
            val node = Node("OK")
            if (node.read(Node("OK")) != "OK%") return "same-owner"
            if (node.read(null) != "none") return "same-owner-null"
            return "OK"
        }
    "##;

    common::expect_box_ok_with_stdlib(SOURCE, "S");
}
