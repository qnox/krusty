use super::common;

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

    let output = common::run_box_against("classpath_unbound_refs", library, main);
    assert_eq!(
        output.unwrap_or_else(|| {
            panic!(
                "compile and run unbound classpath callable references: {:?}",
                common::checker_diags_against("classpath_unbound_refs_diag", library, main)
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

    if let Some(output) = common::run_box_against("classpath_shared_callable_name", library, main) {
        assert_eq!(output, "OK");
    }
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

        class PrivateSample {
            private fun reveal(): String = "OK"
            fun reference(): () -> String = ::reveal
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

    if let Some(output) =
        common::run_box_against_with_reflect("classpath_extension_reflection", library, main)
    {
        assert_eq!(output, "OK");
    }
}
