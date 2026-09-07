//! A qualified `ClassName.fn(args)` call to a `companion object` FUNCTION where `ClassName` is declared
//! in ANOTHER FILE of the SAME MODULE. Same-file companion calls and classpath companion calls already
//! worked; the same-module-cross-file case recorded no lowering hint (the checker searched only the
//! current file's decls for the `Type$Companion` internal, and `class_internal` assumed the current
//! file's package) → the lowerer bailed "unrecorded qualified call target". The checker now falls back
//! to the module-wide `class_names` for the package-correct internal, and the lowerer emits the same
//! `getstatic Type.Companion; invokevirtual Type$Companion.fn(...)` shape the same-file path uses.
//! Compiled as ONE module (shared signatures) and round-tripped on the JVM.

use super::common;

/// Compile two sources as one module (mirrors `cross_file_ctor_default_e2e`'s harness).
fn compile_two(a: &str, b: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let sl = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_in_process_files(&[("A.kt", a), ("B.kt", b)], &[sl], Some(jdk.as_path()))
}

fn run_two(a: &str, b: &str) -> Option<String> {
    let sl = common::stdlib_jar();
    let classes = compile_two(a, b)?;
    let box_class = common::find_box_class(&classes)?;
    common::run_box(&classes, &box_class, &[sl])
}

#[test]
fn cross_file_companion_function_call_runs() {
    // `Job` (file A) has a `companion object` with functions; file B calls them qualified across the
    // module boundary. Must emit `getstatic Job.Companion; invokevirtual Job$Companion.fn` and run.
    let a = "class Job(val id: String) {\n\
             companion object {\n\
             fun idle(): Job = Job(\"default\")\n\
             fun named(n: String): Job = Job(n)\n\
             }\n\
             }\n";
    let b = "fun box(): String {\n\
             if (Job.idle().id != \"default\") return \"f1\"\n\
             if (Job.named(\"x\").id != \"x\") return \"f2\"\n\
             return \"OK\"\n\
             }\n";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}
