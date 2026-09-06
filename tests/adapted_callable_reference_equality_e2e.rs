mod common;

#[test]
fn adapted_member_references_keep_cross_file_equality_identity() {
    let definitions = r#"
        class V {
            fun target(x: String = "x", y: String = "y", z: String = "z") = x + y + z
        }

        private fun unboundOneDefault(): Any {
            val reference: (V, String, String) -> String = V::target
            return reference
        }

        private fun boundOneDefault(value: V): Any {
            val reference: (String, String) -> String = value::target
            return reference
        }

        private fun unboundAllDefaults(): Any {
            val reference: (V) -> String = V::target
            return reference
        }

        fun box(): String {
            val first = V()
            val second = V()
            if (unboundOneDefault() != otherUnboundOneDefault()) return "unbound"
            if (boundOneDefault(first) != otherBoundOneDefault(first)) return "bound"
            if (boundOneDefault(first) == otherBoundOneDefault(second)) return "receiver"
            if (unboundOneDefault() == unboundAllDefaults()) return "arity"
            return "OK"
        }
    "#;
    let other = r#"
        fun otherUnboundOneDefault(): Any {
            val reference: (V, String, String) -> String = V::target
            return reference
        }

        fun otherBoundOneDefault(value: V): Any {
            val reference: (String, String) -> String = value::target
            return reference
        }
    "#;

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[("Definitions", definitions), ("Other", other)],
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("compile fun-interface adapters");
    let box_class = common::find_box_class(&classes).expect("box class");
    let result = common::run_box(&classes, &box_class, std::slice::from_ref(&stdlib));
    assert_eq!(
        result.as_deref(),
        Some("OK"),
        "classes={:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
}

#[test]
fn fun_interface_wrappers_delegate_adapted_reference_equality() {
    let definitions = r#"
        fun interface Action { fun invoke() }
        class C {
            fun adapted(value: String = "OK"): String = value
        }
        private fun identity(value: Action): Any = value
        private fun local(value: C): Any = identity(value::adapted)

        fun box(): String {
            val first = C()
            val second = C()
            if (local(first) != other(first)) return "equal"
            if (local(first) == other(second)) return "receiver"
            return "OK"
        }
    "#;
    let other = r#"
        private fun identity(value: Action): Any = value
        fun other(value: C): Any = identity(value::adapted)
    "#;

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[("Definitions", definitions), ("Other", other)],
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("compile fun-interface adapters");
    let box_class = common::find_box_class(&classes).expect("box class");
    let result = common::run_box(&classes, &box_class, std::slice::from_ref(&stdlib));
    assert_eq!(
        result.as_deref(),
        Some("OK"),
        "classes={:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
}
