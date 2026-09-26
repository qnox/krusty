//! A primitive read from a Java generic (`cell.read()` on a javac-compiled `Cell<Int>`, typed
//! `Int!`) is unboxed straight from the `Object` the call returns, as kotlinc does: unary operators
//! consume it at their primitive receiver, and the unbox goes through `Number` without an
//! intermediate `checkcast` to the wrapper.

use super::common;

const CELL: &str = "package fixtures;\n\
    public interface Cell<T> { T read(); }\n";

const HOLDER: &str = "package fixtures;\n\
    public final class Holder<T> implements Cell<T> {\n\
        private final T value;\n\
        public Holder(T value) { this.value = value; }\n\
        public T read() { return value; }\n\
    }\n";

const METHODS: [&str; 6] = [
    "negated",
    "unaryPlus",
    "inverted",
    "negatedDouble",
    "incremented",
    "explicitNegation",
];

fn java_fixtures() -> std::path::PathBuf {
    let sources = [
        ("fixtures/Cell.java".to_string(), CELL.to_string()),
        ("fixtures/Holder.java".to_string(), HOLDER.to_string()),
    ];
    common::javac_compile(&sources, &[])
        .expect("javac compiles the Cell fixture")
        .0
}

#[test]
fn platform_primitive_operands_compile_like_kotlinc() {
    let source = "import fixtures.Cell\n\
        fun negated(cell: Cell<Int>): Int = -cell.read()\n\
        fun unaryPlus(cell: Cell<Int>): Int = +cell.read()\n\
        fun inverted(cell: Cell<Boolean>): Boolean = !cell.read()\n\
        fun negatedDouble(cell: Cell<Double>): Double = -cell.read()\n\
        fun incremented(cell: Cell<Int>): Int = cell.read() + 1\n\
        fun explicitNegation(cell: Cell<Long>): Long = cell.read().unaryMinus()\n";
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("Platform.kt", source)],
        &[java_fixtures()],
        "PlatformKt",
    );
    for method in METHODS {
        let (kotlinc, krusty) = pair.method_code("PlatformKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

/// `-cell.read()` used to negate the boxed `Integer` with `ineg`, which the verifier rejects.
#[test]
fn a_negated_java_generic_result_runs() {
    let source = "import fixtures.Holder\n\
        fun box(): String {\n\
            val x = -Holder(1).read()\n\
            if (x != -1) return \"fail negated $x\"\n\
            if (!Holder(false).read() != true) return \"fail inverted\"\n\
            return \"OK\"\n\
        }\n";
    assert_eq!(
        common::java_interop_box(
            "platform_negation",
            &[
                ("fixtures/Cell.java", CELL),
                ("fixtures/Holder.java", HOLDER)
            ],
            source,
        ),
        "OK"
    );
}

/// The same operands read from `java.util.ArrayList`, a JDK declaration that Kotlin also maps.
#[test]
fn mapped_java_list_operands_compile_like_kotlinc() {
    let source = "import java.util.ArrayList\n\
        fun negated(l: ArrayList<Int>): Int = -l[0]\n\
        fun unaryPlus(l: ArrayList<Int>): Int = +l[0]\n\
        fun inverted(l: ArrayList<Boolean>): Boolean = !l[0]\n\
        fun negatedDouble(l: ArrayList<Double>): Double = -l[0]\n\
        fun incremented(l: ArrayList<Int>): Int = l[0] + 1\n\
        fun explicitNegation(l: ArrayList<Long>): Long = l[0].unaryMinus()\n";
    let pair = common::ModuleClassPair::compile(&[("Platform.kt", source)], "PlatformKt");
    for method in METHODS {
        let (kotlinc, krusty) = pair.method_code("PlatformKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}
