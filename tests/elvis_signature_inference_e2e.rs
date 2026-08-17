//! `a ?: b` in a property initializer, as seen by signature inference.
//!
//! The signature pre-pass types a property before full checking and had arms for `if` and `when` but
//! none for the elvis, so the whole initializer inferred nothing. A property written
//! `val HOST = System.getenv("APP_HOST") ?: DEFAULT_HOST` — the ordinary spelling of a configurable
//! constant — could not be typed, and every later read of it was reported as an unresolved
//! reference, which is what the gap looked like from the outside.
use super::common;

#[test]
fn an_elvis_initializer_types_its_property() {
    // The left side's nullability is what the elvis discharges, so the property is `String`, not
    // `String?` — reporting the nullable type would reject the member reads the source makes on it.
    const MAIN: &str = "package repro\n\
        private const val DEFAULT_HOST = \"https://example.test\"\n\
        private val HOST = System.getenv(\"KRUSTY_TEST_UNSET_HOST\") ?: DEFAULT_HOST\n\
        fun url(): String = HOST + \"/authorize\"\n\
        fun box(): String = if (url() == \"https://example.test/authorize\") \"OK\" else \"fail: \" + url()\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "an elvis initializer");
}

#[test]
fn a_right_side_that_never_returns_leaves_the_left_type() {
    // `?: throw` is the idiom for a REQUIRED configuration value: its right side yields no value,
    // so the type is the left side's without its null. This is read by peeking at the elvis's right
    // side, not by giving `throw` a type — see the throw test below for why. `?: return` is not
    // included: a `return` does not belong in an initializer at all, and it declines.
    const MAIN: &str = "package repro\n\
        fun maybe(): String? = \"present\"\n\
        val REQUIRED = maybe() ?: throw IllegalStateException(\"missing\")\n\
        fun length(): Int = REQUIRED.length\n\
        fun box(): String = if (length() == 7) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "an elvis over a throw");
}

#[test]
fn only_the_left_sides_nullability_is_discharged() {
    // `a ?: b` with a nullable `b` stays nullable — discharging both would type the property
    // non-null and accept member reads the checker must reject.
    const MAIN: &str = "package repro\n\
        fun maybe(): String? = null\n\
        val STILL_NULLABLE = maybe() ?: maybe()\n\
        fun box(): String = if (STILL_NULLABLE == null) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a nullable right side");
}

#[test]
fn sides_that_do_not_agree_decline() {
    // Kotlin's type for a mix is the least upper bound — for `Int` and `Double` that is
    // `Comparable<*> & Number`, which kotlinc emits as `Object`, never as a widened primitive. The
    // nearest helper to hand is arithmetic PROMOTION, and reusing it typed this property `double`:
    // a field descriptor kotlinc never writes and a different printed value (3.0 against 3), with
    // nothing to reveal it at runtime. Declining costs an inference on a shape that erases to
    // `Object` anyway.
    //
    // An earlier version of this test asserted the value through the NULL branch, where both
    // typings agree — it passed while the bug was live. The assertion has to be the descriptor.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for (label, source) in [
        (
            "Int and Double",
            "package repro\n\
             fun maybeInt(): Int? = 3\n\
             val NUMBER = maybeInt() ?: 2.5\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            "Int and Long",
            "package repro\n\
             val n: Int? = null\n\
             val A = n ?: 1L\n\
             fun box(): String = \"OK\"\n",
        ),
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("cannot infer the type of property")),
            "{label}: sides that do not agree must decline rather than widen: {diagnostics:?}"
        );
    }
}

#[test]
fn a_throw_never_gives_an_initializer_a_type_on_its_own() {
    // `Nothing` belongs to the elvis's right side and nowhere else. Typing `throw` itself let it
    // reach the `if`, `when`, block and bare-initializer paths, where the property then inferred
    // `Nothing`, emitted a `Ljava/lang/Void;` field, and compiled a program kotlinc rejects with
    // "property type 'Nothing' needs to be specified explicitly". This is the same defect the
    // `return` case already showed: a pre-pass that answers where it should decline suppresses the
    // diagnostic that would have rejected the source.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for (label, source) in [
        (
            "bare initializer",
            "package repro\n\
             val A = throw RuntimeException()\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            "both if branches",
            "package repro\n\
             val c = true\n\
             val A = if (c) throw RuntimeException() else throw IllegalStateException()\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            "class member",
            "package repro\n\
             class C { val a = throw RuntimeException() }\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            "every when arm throws",
            "package repro\n\
             val c = 1\n\
             val A = when (c) { 1 -> throw RuntimeException() else -> throw IllegalStateException() }\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            "object member",
            "package repro\n\
             object O { val b = throw RuntimeException() }\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            "null left of a throw",
            "package repro\n\
             val A = null ?: throw RuntimeException()\n\
             fun box(): String = \"OK\"\n",
        ),
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("cannot infer the type of property")),
            "{label}: a throw must not give an initializer a type: {diagnostics:?}"
        );
    }
}

#[test]
fn an_untypeable_side_still_declines() {
    // The must-not-touch side. When either side cannot be typed the initializer must keep asking for
    // an explicit type rather than inventing one from the half it does understand.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const SRC: &str = "package repro\n\
        private val UNKNOWN = unresolvedThing() ?: \"fallback\"\n\
        fun box(): String = \"OK\"\n";
    let diagnostics = common::front_end_diagnostics(SRC, std::slice::from_ref(&stdlib), Some(&jdk));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("cannot infer the type of property")),
        "an initializer whose left side does not resolve must not be typed silently: {diagnostics:?}"
    );
}

#[test]
fn a_platform_right_side_keeps_its_flexible_type() {
    // A Java method's return is flexible (`String!`). Discharging its nullability the way a Kotlin
    // nullable is discharged states that the property is non-null — a guarantee the declaration
    // never made — and the field then carried a `@NotNull` while provably holding `null` at runtime.
    // krusty does not yet model `T!` in an inferred property signature (an elvis-free
    // `val A = System.getenv(..)` annotates the same way), so what is pinned here is that the elvis
    // makes no STRONGER claim than the value supports.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const SRC: &str = "package repro\n\
        val A = System.getenv(\"KRUSTY_TEST_UNSET_A\") ?: System.getProperty(\"krusty.test.unset.b\")\n\
        fun box(): String = \"OK\"\n";
    let Some(classes) =
        common::compile_in_process(SRC, "Main", std::slice::from_ref(&stdlib), Some(&jdk))
    else {
        panic!(
            "{:?}",
            common::front_end_diagnostics(SRC, std::slice::from_ref(&stdlib), Some(&jdk))
        );
    };
    let work = std::env::temp_dir().join(format!("krusty_elvis_platform_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    for (internal, bytes) in &classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("scratch dir");
        }
        std::fs::write(&path, bytes).expect("write class");
    }
    let dumped = common::javap(&["-p", "-v", "-cp", &work.to_string_lossy(), "repro.MainKt"])
        .expect("javap unavailable");
    let _ = std::fs::remove_dir_all(&work);
    let field = dumped
        .split("java.lang.String A;")
        .nth(1)
        .unwrap_or_default()
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        !field.contains("NotNull"),
        "a property whose value can be null must not claim non-null: {field}"
    );
}
