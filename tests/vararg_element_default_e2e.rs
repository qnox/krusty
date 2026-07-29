//! Element-form VARARG calls against classpath extensions whose remaining parameters are
//! defaulted (`"a.b".trim('.')` → `trim(vararg chars: Char)`, `fq.split('.')` →
//! `split(vararg delimiters: Char, ignoreCase: Boolean = false, limit: Int = 0)`): selection
//! expands the vararg element-wise (exact element type wins over an assignable one), and the
//! lowering PACKS the elements into the array before the `$default` mask machinery. Verified
//! on a real JVM (the packing bug surfaced as a VerifyError, not a diagnostic).
use super::common;
fn run(src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let jdk = std::env::var("JAVA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|jh| std::path::PathBuf::from(format!("{jh}/lib/modules")));
    let cp = std::slice::from_ref(&stdlib);
    let classes = common::compile_in_process(src, "T", cp, jdk.as_deref())?;
    common::run_box(&classes, "TKt", cp)
}
#[test]
fn char_vararg_element_call_packs_and_defaults() {
    let src = "fun box(): String {\n\
        \x20 val parts = \".a.b.c.\".trim('.').split('.')\n\
        \x20 if (parts != listOf(\"a\", \"b\", \"c\")) return \"split: \" + parts\n\
        \x20 if (\"!!x!.\".trimEnd('!', '.') != \"!!x\") return \"trimEnd\"\n\
        \x20 if (parts[1] != \"b\") return \"index\"\n\
        \x20 return \"OK\"\n}\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skip"),
    }
}
