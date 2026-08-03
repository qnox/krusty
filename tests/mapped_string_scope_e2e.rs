//! `kotlin/String`'s member scope is its `.kotlin_builtins` declaration, not the method set of the
//! `java.lang.String` it maps to. Taking the Java set put the whole JDK API into the Kotlin scope —
//! `getChars`, `concat`, `replaceAll`, `equalsIgnoreCase`, … — every one of which kotlinc reports as
//! unresolved. Most merely accepted source kotlinc rejects, but `java.lang.String.split(String)` also
//! selected regex/array semantics instead of Kotlin's literal-delimiter/List extension. Each leaked
//! Java member could likewise shadow a `kotlin.text` or user extension that IS the Kotlin API.
//!
//! The three shapes the Java scope had been covering — `substring(Int)`, `substring(Int, Int)` and
//! `indexOf(String)` — reach their `kotlin.text` extensions instead. They must be RUN, not merely
//! compiled: the checker used to type them from a hardcoded `rt == Ty::String` arm that recorded no
//! call target at all, so the front end accepted them and the IR lowerer bailed.

use super::common;

#[test]
fn string_extension_calls_that_the_java_scope_had_covered() {
    // `substring`/`indexOf` are `kotlin.text` extensions (an `@InlineOnly` splice down to the Java
    // member, and `StringsKt.indexOf$default`). They resolved before only because `java.lang.String`'s
    // members leaked in; with the Kotlin scope authoritative they must bind as extensions and EMIT.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun box(): String {
            val s = "abcdef"
            if (s.substring(0, 2) != "ab") return "FAIL substring(Int, Int)"
            if (s.substring(1) != "bcdef") return "FAIL substring(Int)"
            if (s.indexOf("b") != 1) return "FAIL indexOf(String)"
            if (s.indexOf("z") != -1) return "FAIL indexOf(String) absent"
            return "OK"
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "OK");
}

#[test]
fn the_rest_of_the_kotlin_string_api_still_resolves() {
    // The members and extensions the builtins declaration DOES cover, so the scope swap is not a
    // wholesale removal: a member (`get`, `length`), an operator (`plus`, `compareTo`) and a spread of
    // `kotlin.text` extensions.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun box(): String {
            val s = "abcdef"
            if (s.lastIndexOf("c") != 2) return "FAIL lastIndexOf"
            if (s.replace("a", "X") != "Xbcdef") return "FAIL replace"
            if (s.split("c") != listOf("ab", "def")) return "FAIL split"
            if (" a ".trim() != "a") return "FAIL trim"
            if (s.uppercase() != "ABCDEF") return "FAIL uppercase"
            if (!s.startsWith("ab")) return "FAIL startsWith"
            if (!s.contains("cd")) return "FAIL contains"
            if (s.toCharArray().size != 6) return "FAIL toCharArray"
            if (s.get(0) != 'a') return "FAIL get"
            if (s[1] != 'b') return "FAIL indexed get"
            if (s.length != 6) return "FAIL length"
            if (s.plus("g") != "abcdefg") return "FAIL plus"
            if (s.compareTo("abcdef") != 0) return "FAIL compareTo"
            return "OK"
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "OK");
}

#[test]
fn java_only_members_are_not_in_the_kotlin_string_scope() {
    // The methods `java.lang.String` declares and Kotlin's `String` does not. Each of these compiled
    // before the scope came from the builtins declaration; kotlinc reports every one as unresolved.
    //
    // Every entry must be declared on `java.lang.String` ALONE. A method that `java.lang.CharSequence`
    // also declares is NOT a valid probe: `kotlin/CharSequence` keeps its joined scope on purpose (that
    // is what preserves `chars`/`codePoints`), so such a method still reaches `String` one rung up —
    // and whether it does is JDK-DEPENDENT. `getChars` is the trap: `String`-only through JDK 24, but a
    // `CharSequence` default method as of JDK 25, so probing it passes on a JDK 21 developer machine
    // and fails on a JDK 25 CI runner. See the residual-leak note in docs/SPEC.md.
    let Some(stdlib) = common::stdlib_jar() else {
        panic!("no kotlin-stdlib jar");
    };
    let Some(jdk) = common::jdk_modules() else {
        panic!("no jdk modules");
    };
    for (member, source) in [
        ("concat", "fun f(s: String) { s.concat(\"z\") }"),
        (
            "replaceAll",
            "fun f(s: String) { s.replaceAll(\"a\", \"b\") }",
        ),
        (
            "equalsIgnoreCase",
            "fun f(s: String) { s.equalsIgnoreCase(\"A\") }",
        ),
        (
            "compareToIgnoreCase",
            "fun f(s: String) { s.compareToIgnoreCase(\"A\") }",
        ),
        ("getBytes", "fun f(s: String) { s.getBytes() }"),
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains(&format!("unresolved reference '{member}'"))),
            "{member} should not be a member of the Kotlin String scope: {diagnostics:?}"
        );
    }
}

#[test]
fn a_same_module_extension_shadows_the_java_member() {
    // The most user-visible consequence of the swap. While `java.lang.String.substring` was a MEMBER
    // it beat a user-declared extension of the same name (Kotlin: members always win), so this
    // silently ran the JDK method. kotlinc runs the extension, because Kotlin's `String` has no
    // `substring` member for it to lose to.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun String.substring(n: Int): String = "shadow"
        fun box(): String = "abcdef".substring(1)
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "shadow");
}

#[test]
fn a_selected_source_extension_is_not_replaced_by_a_name_based_fallback() {
    // The checker and lowerer must consume the same selected origin. `trimIndent` has a deliberately
    // classpath-less constant-fold fallback, but a source extension wins overload resolution; lowering
    // must not see the familiar name and fold the literal as though no callable had been selected.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun String.trimIndent(): String = "shadow"
        fun box(): String = "  original  ".trimIndent()
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "shadow");
}

#[test]
fn string_keeps_java_io_serializable() {
    // `java/io/Serializable` is not a Kotlin type, so no `.kotlin_builtins` declaration lists it — but
    // kotlinc still reports a mapped builtin as implementing it when the Java class does
    // (`JvmBuiltInsCustomizer.getSupertypes`, `isSerializableInJava`). Replacing the JVM supertypes
    // wholesale dropped it and made this an error against a kotlinc that accepts it.
    let Some(stdlib) = common::stdlib_jar() else {
        panic!("no kotlin-stdlib jar");
    };
    let Some(jdk) = common::jdk_modules() else {
        panic!("no jdk modules");
    };
    for source in [
        "fun f(s: String): java.io.Serializable = s",
        "val v: java.io.Serializable = \"abc\"",
        "fun t(x: java.io.Serializable) {}\nfun f() { t(\"abc\") }",
        "fun <T : java.io.Serializable> box(t: T): T = t\nfun f() { box(\"abc\") }",
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics.is_empty(),
            "String is Serializable for kotlinc: {source}: {diagnostics:?}"
        );
    }
}

#[test]
fn charsequence_members_kotlinc_keeps_stay_in_scope() {
    // kotlinc does NOT hide every Java method on a mapped builtin: `JvmBuiltInsSignatures` re-admits
    // `java.lang.CharSequence.chars`/`codePoints`, which kotlinc resolves and emits as
    // `invokevirtual java/lang/String.chars()`. `kotlin/CharSequence` therefore keeps its JOINED scope —
    // this pins that the widening stopped at `kotlin/String` and did not take these with it.
    let Some(stdlib) = common::stdlib_jar() else {
        panic!("no kotlin-stdlib jar");
    };
    let Some(jdk) = common::jdk_modules() else {
        panic!("no jdk modules");
    };
    for source in [
        "fun f(s: String) { s.chars() }",
        "fun f(s: String) { s.codePoints() }",
        "fun f(s: CharSequence) { s.chars() }",
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            !diagnostics
                .iter()
                .any(|message| message.contains("unresolved reference")),
            "kotlinc keeps this in scope: {source}: {diagnostics:?}"
        );
    }
}
