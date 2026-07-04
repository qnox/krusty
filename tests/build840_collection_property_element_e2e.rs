//! build.840 aa1 cascade: a CLASSPATH data-class property of a Kotlin collection with a PRIMITIVE
//! element (`data class Ch(val items: List<Int>)`). Its type was recovered from the getter's generic
//! signature verbatim — `java/util/List<java/lang/Integer>` — so the element typed as the boxed
//! `java/lang/Integer` (not `Int`) and the collection as raw `java/util/List` (not
//! `kotlin/collections/List`): `for (x in c.items) { s += x }` reported "operator cannot be applied to
//! 'Int' and 'java/lang/Integer'", `c.items.sum()` was "unresolved method 'sum' on 'java/util/List'",
//! and iterating gave a `member … on Any`-style cascade. The recovered generic return is now
//! canonicalized to Kotlin form (`kotlin/collections/List<kotlin/Int>`, element as the PRIMITIVE `Int`),
//! mirroring the suspend-return path — so member/`for`/extension resolution works and the element unboxes.
use super::common;

const LIB: &str = "package lib\n\
    data class Ch(val at: Int, val items: List<Int>, val longs: List<Long>)\n\
    object M { fun make(): Ch = Ch(42, listOf(1, 2, 3), listOf(10L, 20L)) }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn for_over_classpath_list_int_property() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20 val c = M.make()\n\
        \x20 var s = 0\n\
        \x20 for (i in c.items) { s += i }\n\
        \x20 return if (s == 6) \"OK\" else \"F:$s\"\n\
        }\n";
    assert_eq!(
        run("b840_forint", MAIN).expect("for over List<Int> property"),
        "OK"
    );
}

#[test]
fn sum_on_classpath_list_int_property() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String = if (M.make().items.sum() == 6) \"OK\" else \"F\"\n";
    assert_eq!(
        run("b840_sum", MAIN).expect("sum() on List<Int> property"),
        "OK"
    );
}

#[test]
fn for_over_classpath_list_long_property() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20 val c = M.make()\n\
        \x20 var s = 0L\n\
        \x20 for (i in c.longs) { s += i }\n\
        \x20 return if (s == 30L) \"OK\" else \"F:$s\"\n\
        }\n";
    assert_eq!(
        run("b840_forlong", MAIN).expect("for over List<Long> property"),
        "OK"
    );
}

#[test]
fn typed_element_binds_primitive() {
    // The element binds the PRIMITIVE `Int` (not the boxed `java/lang/Integer`), so an explicit `Int`
    // annotation on the loop variable type-checks.
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20 for (i in M.make().items) { val n: Int = i; return if (n == 1) \"OK\" else \"F\" }\n\
        \x20 return \"F\"\n\
        }\n";
    assert_eq!(
        run("b840_typed", MAIN).expect("primitive element binding"),
        "OK"
    );
}
