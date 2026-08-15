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
