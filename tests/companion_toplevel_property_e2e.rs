//! A `companion object` member accesses the FILE's top-level properties exactly like any other
//! member of the file — kotlinc accepts it (companions see the file's top-level declarations).
//! krusty used to hard-reject with the krusty-only "krusty: top-level property access from a
//! companion member is not supported" (a stop-gap from before the `C$Companion` class +
//! facade-access bridge machinery existed). Production hit: intellij's `ActionContextElement`,
//! whose `@JvmStatic` companion functions read a file-private `val ACTION_CONTEXT_ELEMENT_KEY`.
//! Needs the JVM toolchain + kotlin-stdlib; skips otherwise.

use super::common;

/// The intellij repro: a file-private top-level `val` read from a `@JvmStatic` companion function.
#[test]
fn companion_reads_private_toplevel_val() {
    const SRC: &str = "private val key: String = \"OK\"\n\
        class Element {\n\
            companion object {\n\
                @JvmStatic\n\
                fun read(): String = key\n\
            }\n\
        }\n\
        fun box(): String = Element.read()\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// A file-private top-level `var`: read, write, and compound-assign from a companion member.
#[test]
fn companion_reads_and_writes_private_toplevel_var() {
    const SRC: &str = "private var count: Int = 0\n\
        class Counter {\n\
            companion object {\n\
                fun bump() { count += 10 }\n\
                fun get(): Int = count\n\
            }\n\
        }\n\
        fun box(): String {\n\
            Counter.bump()\n\
            Counter.bump()\n\
            return if (Counter.get() == 20) \"OK\" else \"F: \" + Counter.get()\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// Increment/decrement of a top-level `var` from a companion member, statement and expression
/// position (the lowering shadow probe must treat the companion's members as the OUTER class's
/// statics, not as an unregistered `C$Companion` class).
#[test]
fn companion_incdec_toplevel_var() {
    const SRC: &str = "private var n: Int = 41\n\
        class C {\n\
            companion object {\n\
                fun inc() { n++ }\n\
                fun preInc(): Int = ++n\n\
            }\n\
        }\n\
        fun box(): String {\n\
            C.inc()\n\
            return if (C.preInc() == 43 && n == 43) \"OK\" else \"F: \" + n\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// A PUBLIC top-level `val` read from a companion — the plain `getX()` accessor path, no bridge.
#[test]
fn companion_reads_public_toplevel_val() {
    const SRC: &str = "val shared: Int = 7\n\
        class C {\n\
            companion object {\n\
                fun get(): Int = shared\n\
            }\n\
        }\n\
        fun box(): String = if (C.get() == 7) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// A top-level `const val` read from a companion — the literal-inline path.
#[test]
fn companion_reads_toplevel_const() {
    const SRC: &str = "const val ANSWER: Int = 42\n\
        class C {\n\
            companion object {\n\
                fun get(): Int = ANSWER\n\
            }\n\
        }\n\
        fun box(): String = if (C.get() == 42) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// Shadowing: the companion's OWN property wins over a same-named top-level property.
#[test]
fn companion_own_property_shadows_toplevel() {
    const SRC: &str = "private val tag: String = \"outer\"\n\
        class C {\n\
            companion object {\n\
                val tag: String = \"inner\"\n\
                fun get(): String = tag\n\
            }\n\
        }\n\
        fun box(): String = if (C.get() == \"inner\" && tag == \"outer\") \"OK\" else \"F: \" + C.get()\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// A DELEGATED top-level property (`by lazy`) read from a companion — the computed-accessor path.
#[test]
fn companion_reads_delegated_toplevel_val() {
    const SRC: &str = "private val cached: String by lazy { \"OK\" }\n\
        class C {\n\
            companion object {\n\
                fun get(): String = cached\n\
            }\n\
        }\n\
        fun box(): String = C.get()\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// Regression guard: a bare WRITE shadowed by the companion's OWN property must not silently bind
/// the top-level `var` — in Kotlin the companion's member wins. krusty can't emit that write yet,
/// so it must keep rejecting loudly rather than miscompile (reads already bind the companion's).
#[test]
fn companion_shadowed_toplevel_write_is_rejected_not_misbound() {
    const SRC: &str = "private var count: Int = 0\n\
        class C {\n\
            companion object {\n\
                var count: Int = 100\n\
                fun bump() { count += 1 }\n\
            }\n\
        }\n\
        fun box(): String = \"OK\"\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("not supported")),
        "{diagnostics:?}"
    );
}
