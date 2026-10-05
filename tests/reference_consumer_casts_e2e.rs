//! A callable reference reaches its use as its carrier class, cast only to what the consumer
//! reads: `Function1` for a function-typed parameter or local, `KFunction` for a reference-typed
//! local or result, and nothing for `Any`. A `KFunctionN` result is signed `KFunction<R>`, its one
//! JVM type argument being the reference's result.

use super::common;

const REFERENCES: &str = "class Holder(val base: Int) { fun shift(x: Int) = base + x }
fun sinkAny(value: Any?) = value
fun sinkFunction(function: (Int) -> Int) = function
fun make() = Holder(10)
fun toAny() = sinkAny(make()::shift)
fun toFunction() = sinkFunction(make()::shift)
fun throughReference(): Int { val shift = make()::shift; return shift(1) }
fun unboundToAny() = sinkAny(Holder::shift)
fun throughFunction(): Any { val shift: (Int) -> Int = make()::shift; return shift }
fun returned() = make()::shift
fun box(): String {
    @Suppress(\"UNCHECKED_CAST\")
    val any = toAny() as (Int) -> Int
    val sum = any(1) + toFunction()(2) + throughReference() + returned()(4)
    return if (sum == 48 && unboundToAny() != null && throughFunction() != null) \"OK\" else \"$sum\"
}
";

const FUNCTIONS: [&str; 6] = [
    "toAny",
    "toFunction",
    "throughReference",
    "unboundToAny",
    "throughFunction",
    "returned",
];

#[test]
fn a_reference_is_cast_only_to_what_its_consumer_reads() {
    let pair = common::ModuleClassPair::compile(&[("References.kt", REFERENCES)], "ReferencesKt");
    for function in FUNCTIONS {
        let (kotlinc, krusty) = pair.method_code("ReferencesKt", function);
        assert_eq!(krusty, kotlinc, "{function}");
    }
    let (kotlinc, krusty) = (declarations(&pair.kotlinc), declarations(&pair.krusty));
    assert_eq!(krusty, kotlinc);
}

#[test]
fn a_reference_cast_for_its_consumer_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(REFERENCES, "References");
}

/// The generic method declarations javap reads from each `Signature`.
fn declarations(bytes: &[u8]) -> String {
    let work = common::scratch_dir().expect("cannot allocate disassembly fixture");
    let path = work.join("ReferencesKt.class");
    std::fs::write(&path, bytes).expect("write class for disassembly");
    let text = common::javap(&["-p", &path.to_string_lossy()]).expect("javap unavailable");
    let _ = std::fs::remove_dir_all(work);
    text.lines()
        .filter(|line| line.contains('('))
        .collect::<Vec<_>>()
        .join("\n")
}
