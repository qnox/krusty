//! `x as? T ?: e` — an unparenthesized safe cast followed by elvis. The cast type is `T` (the trailing
//! `?:` is the elvis operator), but `parse_type` was greedily consuming the `?` of `?:` as a nullable
//! `T?`, leaving a dangling `:`. Now a `?` immediately before `:` is left for the caller (it's elvis, not
//! a nullable-type marker). Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn safe_cast_success_then_elvis() {
    const SRC: &str = "fun box(): String { val x: Any = \"OK\"; return x as? String ?: \"no\" }\n";
    assert_eq!(run(SRC).expect("as? success compiles + runs"), "OK");
}

#[test]
fn safe_cast_failure_then_elvis() {
    const SRC: &str = "fun box(): String { val x: Any = 5; return x as? String ?: \"OK\" }\n";
    assert_eq!(run(SRC).expect("as? failure compiles + runs"), "OK");
}

#[test]
fn nullable_type_still_parses() {
    const SRC: &str = "fun f(x: String?): String = x ?: \"OK\"\nfun box(): String = f(null)\n";
    assert_eq!(run(SRC).expect("nullable type unaffected"), "OK");
}

#[test]
fn safe_cast_to_primitive_then_elvis() {
    // `x as? Int` (safe cast to a PRIMITIVE): the result is the boxed wrapper `Int?` — `instanceof`/
    // `checkcast` against `Integer`, `null` on a mismatch, then the elvis unboxes. Round-tripped.
    const SRC: &str = "fun pick(a: Any): Int = a as? Int ?: 16\n\
fun box(): String {\n\
    if (pick(42) != 42) return \"f1\"\n\
    if (pick(\"x\") != 16) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("as? Int compiles + runs"), "OK");
}

#[test]
fn safe_cast_to_reference_expression_body() {
    // `fun f(a: Any): A? = a as? A` — the `as?` result type `A?` is assignable to the declared `A?`
    // return (nullability-insensitive in a return position). Round-tripped on the JVM.
    const SRC: &str = "open class A\nclass B : A()\nclass C\n\
fun f(a: Any): A? = a as? A\n\
fun box(): String {\n\
    val b = B()\n\
    if (f(b) !== b) return \"f1\"\n\
    if (f(C()) != null) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("as? reference body compiles + runs"), "OK");
}
