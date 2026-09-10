//! A classpath `inline fun <reified T>` stopped splicing once the CALLING class's constant pool
//! grew past 255 entries — every call in a large file was rejected with "inline splice failed".
//!
//! The reified repoint rewrites the body's type-bearing instruction to name the concrete type. For
//! `T::class` that instruction is `ldc`, whose pool operand is ONE byte, so as soon as the host
//! class's pool index for the concrete class exceeded 255 the repoint had nowhere to put it and the
//! whole splice was declined. A reified callee cannot fall back to a real call (its compiled body
//! only throws), so the file was dropped.
//!
//! The size dependence is why this hid: the same call in a small file splices fine, and every
//! fixture that covered reified splicing was small. `relocate_insns` already widens `ldc` to the
//! identical-semantics 2-byte `ldc_w` for exactly this reason; the repoint path did not.
//!
//! The filler declarations below are the point of the test — they push the pool past a byte before
//! the reified call is emitted. Keep them, and keep them BEFORE `box()`.
use super::common;

const LIB: &str = r#"
    package lib

    inline fun <reified T : Any> nameOf(): String = T::class.simpleName ?: "?"

    inline fun <reified T : Any> isA(value: Any): Boolean = value is T
"#;

/// Top-level declarations whose distinct string constants push the facade class's constant pool
/// past 255 entries before the reified call sites are emitted.
fn pool_fillers(count: usize) -> String {
    (0..count)
        .map(|i| format!("fun filler{i}(): String = \"pool-filler-{i}\"\n"))
        .collect()
}

#[test]
fn reified_inline_splices_when_the_caller_pool_exceeds_one_byte() {
    let Some(libout) = common::compile_lib("reified_splice_large_pool", LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    let main = format!(
        "import lib.isA\n\
         import lib.nameOf\n\
         {fillers}\
         fun box(): String {{\n\
         \x20 if (nameOf<String>() != \"String\") return \"nameOf: ${{nameOf<String>()}}\"\n\
         \x20 if (!isA<String>(\"x\")) return \"isA true\"\n\
         \x20 if (isA<String>(7)) return \"isA false\"\n\
         \x20 return \"OK\"\n\
         }}\n",
        fillers = pool_fillers(300),
    );
    let Some(out) = common::compile_and_run_box(&main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(&main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out.trim(), "OK", "box() output");
}
