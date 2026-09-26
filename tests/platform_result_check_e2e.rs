//! A Java value committed to a declared non-null type is checked where it enters Kotlin, as kotlinc
//! does: a generic result is checked in the `Object` slot the call returns before its `checkcast`,
//! and a value unboxed into a primitive is checked too, then unboxed through `Number`.

use super::common;

const CELL: &str = "package fixtures;\n\
    public interface Cell<T> { T read(); }\n";

const BOX: &str = "package fixtures;\n\
    public interface Box {\n\
        Integer boxed();\n\
        Boolean flag();\n\
        Character letter();\n\
        Long wide();\n\
        Double real();\n\
    }\n";

const HOLDER: &str = "package fixtures;\n\
    public final class Holder<T> implements Cell<T> {\n\
        private final T value;\n\
        public Holder(T value) { this.value = value; }\n\
        public T read() { return value; }\n\
    }\n";

fn java_fixtures() -> std::path::PathBuf {
    let sources = [
        ("fixtures/Cell.java".to_string(), CELL.to_string()),
        ("fixtures/Box.java".to_string(), BOX.to_string()),
    ];
    common::javac_compile(&sources, &[])
        .expect("javac compiles the platform fixtures")
        .0
}

#[test]
fn platform_results_are_checked_like_kotlinc() {
    let source = "import fixtures.Box\n\
        import fixtures.Cell\n\
        fun generic(cell: Cell<String>): String = cell.read()\n\
        fun genericLocal(cell: Cell<String>): String { val x: String = cell.read(); return x }\n\
        fun asserted(cell: Cell<String?>): String = cell.read()!!\n\
        fun genericInt(cell: Cell<Int>): Int = cell.read()\n\
        fun genericIntLocal(cell: Cell<Int>): Int { val x: Int = cell.read(); return x }\n\
        fun boxed(box: Box): Int = box.boxed()\n\
        fun flag(box: Box): Boolean = box.flag()\n\
        fun letter(box: Box): Char = box.letter()\n\
        fun wide(box: Box): Long = box.wide()\n\
        fun real(box: Box): Double { val d: Double = box.real(); return d }\n\
        fun argument(box: Box): Int = take(box.boxed())\n\
        fun take(i: Int): Int = i\n\
        fun operand(box: Box): Int = box.boxed() + 1\n";
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("Platform.kt", source)],
        &[java_fixtures()],
        "PlatformKt",
    );
    for method in [
        "generic",
        "genericLocal",
        "asserted",
        "genericInt",
        "genericIntLocal",
        "boxed",
        "flag",
        "letter",
        "wide",
        "real",
        "argument",
        "operand",
    ] {
        let (kotlinc, krusty) = pair.method_code("PlatformKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

/// A null from a generic Java result fails at the declared `Int`, naming the call.
#[test]
fn a_null_generic_result_fails_at_its_declaration() {
    let source = "import fixtures.Holder\n\
        fun read(cell: Holder<Int>): Int { val x: Int = cell.read(); return x }\n\
        fun box(): String {\n\
            if (read(Holder(4)) != 4) return \"fail value\"\n\
            try {\n\
                read(Holder<Int>(null))\n\
                return \"fail no check\"\n\
            } catch (e: NullPointerException) {\n\
                if (e.message != \"read(...) must not be null\") return \"fail message ${e.message}\"\n\
            }\n\
            return \"OK\"\n\
        }\n";
    assert_eq!(
        common::java_interop_box(
            "platform_result_check",
            &[
                ("fixtures/Cell.java", CELL),
                ("fixtures/Holder.java", HOLDER)
            ],
            source,
        ),
        "OK"
    );
}
