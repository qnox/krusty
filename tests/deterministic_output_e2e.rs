//! The same source compiles to the same bytes every time.
//!
//! Every hash map the compiler builds is seeded differently, so a compile whose output follows a
//! map's iteration order changes between compiles. Each declaration below exercises one place that
//! used to: bridges, secondary constructors, locals of an inlined body, default-argument lambdas,
//! and the result type of a public anonymous object whose members declare their own results.

use super::common;

const SOURCE: &str = r#"
interface Source<T> {
    fun first(value: T): T
    fun second(value: T): T
    fun third(value: T): T
    fun fourth(value: T): T
    val fifth: T
    val sixth: T
}

class Impl : Source<String> {
    override fun first(value: String) = value
    override fun second(value: String) = value
    override fun third(value: String) = value
    override fun fourth(value: String) = value
    override val fifth: String get() = ""
    override val sixth: String get() = ""
}

class Many(val a: Int) {
    constructor(a: Long) : this(a.toInt())
    constructor(a: String) : this(a.length)
    constructor(a: Char) : this(a.code)
    constructor(a: Double) : this(a.toInt())
    constructor(a: Boolean) : this(if (a) 1 else 0)
}

inline fun spread(block: (Int) -> Int): Int {
    val one = block(1)
    val two = block(2)
    val three = block(3)
    val four = block(4)
    return one + two + three + four
}

fun inlined(): Int = spread { value -> val doubled = value * 2; val tripled = value * 3; doubled + tripled }

fun defaults(
    a: () -> Int = { 1 },
    b: () -> Int = { 2 },
    c: () -> Int = { 3 },
    d: () -> Int = { 4 },
) = a() + b() + c() + d()

fun defaultsAgain(e: () -> Int = { 5 }, f: () -> Int = { 6 }) = e() + f()

abstract class Base {
    abstract fun value(): Any
}

val chain = object : Base() {
    private fun hidden() = object { val x = 1 }
    fun visible() = object : Base() { override fun value() = 2 }
    private fun alsoHidden() = object { val y = 3 }
    override fun value() = hidden().x + alsoHidden().y
}
"#;

#[test]
fn repeated_compiles_emit_identical_classes() {
    let compile = || {
        let mut classes =
            common::compile_in_process(SOURCE, "deterministic", &[common::stdlib_jar()], None)
                .expect("the fixture compiles");
        classes.sort();
        classes
    };
    let first = compile();
    for attempt in 1..16 {
        let again = compile();
        let names = |classes: &[(String, Vec<u8>)]| {
            classes
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&again),
            names(&first),
            "compile {attempt} emitted other classes"
        );
        for ((name, bytes), (_, expected)) in again.iter().zip(&first) {
            assert!(
                bytes == expected,
                "compile {attempt} emitted different bytes for {name}"
            );
        }
    }
}
