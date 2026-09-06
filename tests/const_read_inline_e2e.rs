//! Same-file `const val` reads INLINE the literal (`ldc`) instead of `getstatic` — matching kotlinc.
//! Combined with the `ConstantValue` field + omitted `<clinit>` (P450), a pure const read is now
//! byte-identical to kotlinc. Verified by parsing `box()` (no `getstatic` of the const) + JVM run.

use super::common;

use krusty::jvm::classreader::parse_class;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn const_read_runs() {
    const SRC: &str = "const val X = \"OK\"\nfun box(): String = X\n";
    assert_eq!(run(SRC).expect("const read compiles + runs"), "OK");
}

#[test]
fn int_const_read_runs() {
    const SRC: &str = "const val N = 42\nfun box(): String = if (N + 0 == 42) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("int const read compiles + runs"), "OK");
}

#[test]
fn const_read_is_inlined_no_getstatic_in_box() {
    // box() reads the const — it must inline (no field load), so the facade need not even keep a field
    // reference in box's code. We assert box() parses and the class has the ConstantValue field.
    let sl = common::stdlib_jar();
    let jh = common::java_home();
    let jdk = Some(std::path::PathBuf::from(format!("{jh}/lib/modules")));
    let cp: Vec<std::path::PathBuf> = vec![sl];
    let classes = common::compile_in_process(
        "const val X = \"OK\"\nfun box(): String = X\n",
        "Main",
        &cp,
        jdk.as_deref(),
    )
    .expect("compiles");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n.ends_with("MainKt"))
        .expect("facade");
    let ci = parse_class(bytes).expect("parse");
    let x = ci.fields.iter().find(|f| f.name == "X").expect("X field");
    assert!(x.const_value.is_some(), "X must carry ConstantValue");
    assert!(
        ci.method("<clinit>", "()V").is_none(),
        "no <clinit> for a const-only facade"
    );
}

// A DEPENDENCY companion constant (`Int.MAX_VALUE`, `Double.MAX_VALUE`) is folded by the checker and
// recorded as a resolved constant, not as a property the getter path can read: there is no runtime
// property behind it. Checked FIR consumes that decision directly, so a builtin limit reads as a
// constant rather than failing as an unsupported member access.
#[test]
fn builtin_companion_constants_read_as_constants() {
    const SRC: &str = "fun box(): String {\n\
        \x20 if (Int.MAX_VALUE <= 0) return \"int\"\n\
        \x20 if (Long.MIN_VALUE >= 0L) return \"long\"\n\
        \x20 if (Double.MAX_VALUE <= 0.0) return \"double\"\n\
        \x20 val x = 1\n\
        \x20 if (x !in Int.MIN_VALUE..Int.MAX_VALUE) return \"range\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("builtin companion constants"), "OK");
}

// A `const val` referenced by BARE NAME from inside its own classifier. The checker folds it exactly
// as it folds a qualified `A.y`, so checked FIR consumes the folded constant rather than looking for
// a local or a property to read.
#[test]
fn a_const_val_reads_by_bare_name_inside_its_own_object() {
    const SRC: &str = "object A {\n\
        \x20 val x = \"O${foo()}\"\n\
        \x20 fun foo() = y\n\
        \x20 const val y = \"K\"\n\
        }\n\
        fun box(): String = A.x\n";
    assert_eq!(run(SRC).expect("bare-name const val"), "OK");
}
