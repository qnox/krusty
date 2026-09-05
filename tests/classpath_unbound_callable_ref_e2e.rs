use super::common;

fn run_source_files_against(tag: &str, library: &str, sources: &[(&str, &str)]) -> Option<String> {
    let dependency = common::compile_lib(tag, library)?;
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box_files(sources, &[dependency, stdlib], Some(jdk.as_path()))
}

fn source_file_diagnostics_against(
    tag: &str,
    library: &str,
    sources: &[&str],
) -> Option<Vec<String>> {
    let dependency = common::compile_lib(tag, library)?;
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    Some(common::front_end_diagnostics_files(
        sources,
        &[dependency, stdlib],
        Some(jdk.as_path()),
    ))
}

#[test]
fn unbound_classpath_callables_compile_and_run() {
    let library = r#"
        package fixtures

        class Sample(var text: String) {
            fun startsWith(prefix: String): Boolean = text.startsWith(prefix)
            val accepted: Boolean get() = text.length == 2
        }

        var Sample.isTagged: Boolean
            get() = text.endsWith("K")
            set(value) {
                text = if (value) "OK" else "NO"
            }
        val CharSequence.isWide: Boolean get() = length > 1

        open class Crate<T>(var item: T) {
            fun read(): T = item
        }
        class TextCrate(value: String) : Crate<String>(value)

        class Choice(val selected: String)
    "#;
    let main = r#"
        import fixtures.Choice
        import fixtures.Sample
        import fixtures.TextCrate
        import fixtures.isTagged
        import fixtures.isWide

        val Choice.selected: String get() = "extension"

        fun accepts(value: Sample, predicate: (Sample) -> Boolean): Boolean = predicate(value)

        fun box(): String {
            val value = Sample("OK")

            val memberMutable = Sample::text
            memberMutable.set(value, "NO")
            if (memberMutable.get(value) != "NO") return "mutable member"
            memberMutable.set(value, "OK")

            val boundMutable = value::text
            boundMutable.set("NO")
            if (boundMutable.get() != "NO") return "bound mutable member"
            boundMutable.set("OK")

            val method = Sample::startsWith
            if (!method(value, "O")) return "member function"

            val indexed = String::get
            if (indexed("OK", 1) != 'K') return "mapped member function"

            val memberProperty = Sample::accepted
            if (memberProperty.get(value) != true) return "member property"

            val extensionProperty = Sample::isTagged
            if (extensionProperty.get(value) != true) return "extension property"
            extensionProperty.set(value, false)
            if (extensionProperty.get(value) != false) return "mutable extension"
            extensionProperty.set(value, true)
            if (!accepts(value, Sample::isTagged)) return "extension predicate"

            val inheritedExtension = String::isWide
            if (inheritedExtension.get("OK") != true) return "extension supertype"

            val genericValue = TextCrate("NO")
            val genericMethod: (TextCrate) -> String = TextCrate::read
            val genericProperty = TextCrate::item
            genericProperty.set(genericValue, "OK")
            if (genericMethod(genericValue).length != 2) return "generic method"
            if (genericProperty.get(genericValue) != "OK") return "generic property"

            val preferredMember = Choice::selected
            if (preferredMember.get(Choice("member")) != "member") return "member precedence"

            return "OK"
        }
    "#;

    let output = common::run_box_against_ref("classpath_unbound_refs", library, main);
    assert_eq!(
        output.unwrap_or_else(|| {
            panic!(
                "compile and run unbound classpath callable references: {:?}",
                common::checker_diags_against_ref("classpath_unbound_refs_diag", library, main)
            )
        }),
        "OK"
    );
}

#[test]
fn extension_function_and_property_can_share_a_name() {
    let library = r#"
        package fixtures

        class Sample
        val Sample.tag: String get() = "property"
        fun Sample.tag(): String = "function"
    "#;
    let main = r#"
        import fixtures.Sample
        import fixtures.tag

        fun box(): String {
            val value = Sample()
            if (value.tag() != "function") return "function"
            val property = Sample::tag
            return if (property.get(value) == "property") "OK" else "property"
        }
    "#;

    let Some(output) =
        common::expect_box_run_against("classpath_shared_callable_name", library, main)
    else {
        return; // toolchain not provisioned
    };
    assert_eq!(output, "OK");
}

#[test]
fn classpath_callable_references_resolve_reflection_targets() {
    let library = r#"
        package fixtures

        @JvmInline value class Marker(val raw: String)
        class Sample(val text: String) {
            fun decode(marker: Marker): String = marker.raw
        }
        val Sample.isTagged: Boolean get() = text.endsWith("K")
    "#;
    let main = r#"
        import fixtures.Marker
        import fixtures.Sample
        import fixtures.isTagged
        import kotlin.reflect.KFunction0

        class PrivateSample {
            private fun reveal(): String = "OK"
            // `KFunction0`, not `() -> String`: the plain function type has no `returnType` member, so
            // the assertion below would not be valid Kotlin (kotlinc rejects it too).
            fun reference(): KFunction0<String> = ::reveal
        }

        fun box(): String {
            val getter = Sample::isTagged.getter
            if (getter.returnType.toString() != "kotlin.Boolean") return "wrong getter"

            val function = Sample::decode
            if (function.returnType.toString() != "kotlin.String") return "wrong function"

            val privateFunction = PrivateSample().reference()
            if (privateFunction.returnType.toString() != "kotlin.String") return "wrong private function"
            if (privateFunction() != "OK") return "wrong private invocation"
            return "OK"
        }
    "#;

    let Some(output) = common::expect_box_run_against_with_reflect_ref(
        "classpath_extension_reflection",
        library,
        main,
    ) else {
        return; // toolchain not provisioned
    };
    assert_eq!(output, "OK");
}

#[test]
fn source_extension_reference_accepts_a_classpath_receiver() {
    let library = r#"
        package fixtures

        class Record(val enabled: Boolean)
    "#;
    let main = r#"
        import fixtures.*

        private fun Record.isUsable(): Boolean = enabled

        fun box(): String {
            val predicate = Record::isUsable
            return if (predicate(Record(true)) && !predicate(Record(false))) "OK" else "FAIL"
        }
    "#;

    let output = common::run_box_against("source_extension_classpath_receiver", library, main);
    assert_eq!(
        output.unwrap_or_else(|| {
            panic!(
                "compile source extension reference on a classpath receiver: {:?}",
                common::checker_diags_against(
                    "source_extension_classpath_receiver_diag",
                    library,
                    main
                )
            )
        }),
        "OK"
    );
}

#[test]
fn private_source_extension_reference_stays_file_private() {
    let library = r#"
        package fixtures

        class Record(val enabled: Boolean)
    "#;
    let Some(library_output) = common::compile_lib("source_extension_ref_visibility", library)
    else {
        return;
    };
    let declaration = r#"
        import fixtures.*

        private fun Record.isUsable(): Boolean = enabled
    "#;
    let use_site = r#"
        import fixtures.*

        fun expose(): (Record) -> Boolean = Record::isUsable
    "#;

    let diagnostics =
        common::front_end_diagnostics_files(&[declaration, use_site], &[library_output], None);
    assert_eq!(
        diagnostics,
        vec!["cannot access 'isUsable': it is private in its file"]
    );
}

#[test]
fn sibling_source_extension_reference_uses_its_declaring_facade() {
    let library = r#"
        package fixtures

        class Record(val enabled: Boolean)
    "#;
    let declaration = r#"
        package app

        import fixtures.Record

        fun Record.isUsable(): Boolean = enabled
    "#;
    let use_site = r#"
        package app

        import fixtures.Record

        fun box(): String {
            val predicate = Record::isUsable
            return if (predicate(Record(true)) && !predicate(Record(false))) "OK" else "FAIL"
        }
    "#;

    let output = run_source_files_against(
        "source_extension_sibling_facade",
        library,
        &[("Extensions.kt", declaration), ("Use.kt", use_site)],
    );
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_internal_bound_extension_keeps_facade_and_declared_supertype() {
    let library = r#"
        package fixtures

        open class Base(val text: String)
        class Derived(text: String) : Base(text)
    "#;
    let declaration = r#"
        package app

        import fixtures.Base

        internal fun Base.label(): String = text
    "#;
    let use_site = r#"
        package app

        import fixtures.Derived

        fun box(): String {
            val label = Derived("OK")::label
            return label()
        }
    "#;

    let output = run_source_files_against(
        "bound_internal_extension_sibling_facade",
        library,
        &[("Extensions.kt", declaration), ("Use.kt", use_site)],
    );
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_primitive_bound_extension_boxes_and_unboxes_the_receiver() {
    let library = r#"
        package fixtures

        class Marker
    "#;
    let declaration = r#"
        package app

        internal fun Int.plusFour(): Int = this + 4
    "#;
    let use_site = r#"
        package app

        fun box(): String {
            val plusFour: () -> Int = 1::plusFour
            return if (plusFour() == 5) "OK" else "FAIL"
        }
    "#;

    let output = run_source_files_against(
        "bound_primitive_extension_sibling_facade",
        library,
        &[("Extensions.kt", declaration), ("Use.kt", use_site)],
    );
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_callable_references_coerce_value_returns_to_unit() {
    let library = r#"
        package fixtures

        class Record(var text: String)
    "#;
    let declaration = r#"
        package app

        import fixtures.Record

        internal fun Record.update(value: String): String {
            text = value
            return value
        }
        internal fun updateTop(record: Record, value: String): String {
            record.text = value
            return value
        }
    "#;
    let use_site = r#"
        package app

        import fixtures.Record

        fun box(): String {
            val first = Record("FAIL")
            val bound: (String) -> Unit = first::update
            bound("O")

            val second = Record("FAIL")
            val topLevel: (Record, String) -> Unit = ::updateTop
            topLevel(second, "K")
            return first.text + second.text
        }
    "#;

    let output = run_source_files_against(
        "cross_file_callable_ref_unit_coercion",
        library,
        &[("Functions.kt", declaration), ("Use.kt", use_site)],
    );
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn imported_public_cross_file_bound_extension_uses_declaring_facade() {
    let library = r#"
        package fixtures

        class Record(val text: String)
    "#;
    let declaration = r#"
        package extensions

        import fixtures.Record

        fun Record.label(): String = text
    "#;
    let use_site = r#"
        package use

        import extensions.label
        import fixtures.Record

        fun box(): String {
            val label = Record("OK")::label
            return label()
        }
    "#;

    let output = run_source_files_against(
        "bound_public_extension_sibling_facade",
        library,
        &[("Extensions.kt", declaration), ("Use.kt", use_site)],
    );
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_internal_toplevel_overload_uses_expected_variance_and_facade() {
    let library = r#"
        package fixtures

        class Marker
    "#;
    let declaration = r#"
        package app

        internal fun convert(value: Any): String = value as String
        internal fun convert(value: Int): Int = value
    "#;
    let use_site = r#"
        package app

        fun box(): String {
            val convert: (String) -> Any = ::convert
            return convert("OK") as String
        }
    "#;

    let output = run_source_files_against(
        "internal_toplevel_ref_sibling_facade",
        library,
        &[("Functions.kt", declaration), ("Use.kt", use_site)],
    );
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn private_cross_file_bound_extension_reports_inaccessible() {
    let library = r#"
        package fixtures

        class Record
    "#;
    let declaration = r#"
        package app

        import fixtures.Record

        private fun Record.label(): String = "private"
    "#;
    let use_site = r#"
        package app

        import fixtures.Record

        fun expose(record: Record): () -> String = record::label
    "#;
    let Some(diagnostics) = source_file_diagnostics_against(
        "bound_private_extension_visibility",
        library,
        &[declaration, use_site],
    ) else {
        return;
    };
    assert_eq!(
        diagnostics,
        vec!["cannot access 'label': it is private in its file"]
    );
}

#[test]
fn private_cross_file_toplevel_reference_reports_inaccessible() {
    let library = r#"
        package fixtures

        class Marker
    "#;
    let declaration = r#"
        package app

        private fun hidden(value: String): String = value
    "#;
    let use_site = r#"
        package app

        fun expose(): (String) -> String = ::hidden
    "#;
    let Some(diagnostics) = source_file_diagnostics_against(
        "private_toplevel_ref_visibility",
        library,
        &[declaration, use_site],
    ) else {
        return;
    };
    assert_eq!(
        diagnostics,
        vec!["cannot access 'hidden': it is private in its file"]
    );
}

#[test]
fn unimported_source_extension_reference_is_not_visible() {
    let library = r#"
        package fixtures

        class Record
    "#;
    let declaration = r#"
        package extensions

        import fixtures.Record

        fun Record.label(): String = "extension"
    "#;
    let use_site = r#"
        package use

        import fixtures.Record

        fun expose() = Record::label
    "#;
    let Some(diagnostics) = source_file_diagnostics_against(
        "source_extension_import_scope",
        library,
        &[declaration, use_site],
    ) else {
        return;
    };
    assert_eq!(diagnostics, vec!["unresolved reference 'label'."]);
}

#[test]
fn source_extension_reference_accepts_a_supertype_receiver() {
    let library = r#"
        package fixtures

        open class Base(val text: String)
        class Derived(text: String) : Base(text)
    "#;
    let main = r#"
        import fixtures.Base
        import fixtures.Derived

        fun Base.label(): String = text

        fun box(): String {
            val label = Derived::label
            return label(Derived("OK"))
        }
    "#;
    let output = common::run_box_against("source_extension_supertype_receiver", library, main);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn overloaded_source_extension_reference_is_ambiguous() {
    let library = r#"
        package fixtures

        class Record
    "#;
    let main = r#"
        import fixtures.Record

        fun Record.select(): String = "zero"
        fun Record.select(value: Int): String = value.toString()

        fun expose() = Record::select
    "#;
    let Some(diagnostics) =
        common::checker_diags_against("source_extension_reference_ambiguity", library, main)
    else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ambigu")),
        "{diagnostics:?}"
    );
}

#[test]
fn classpath_member_still_precedes_a_source_extension_reference() {
    let library = r#"
        package fixtures

        class Record {
            fun select(): String = "member"
        }
    "#;
    let main = r#"
        import fixtures.Record

        fun Record.select(): String = "extension"

        fun box(): String {
            val select = Record::select
            return select(Record())
        }
    "#;
    let output = common::run_box_against("source_extension_member_precedence", library, main);
    assert_eq!(output.as_deref(), Some("member"));
}
