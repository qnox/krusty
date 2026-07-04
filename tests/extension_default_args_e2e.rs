//! Extension-function DEFAULT ARGUMENTS: `fun T.foo(a: Int = 1)` called while omitting the defaulted
//! argument (`x.foo()`) — or naming it (`x.foo(a = 2)`). krusty fills the omitted constant defaults at
//! the call site (the extension lowers to a static `Facade.foo($receiver, args…)`). Verified on a real JVM.
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
fn extension_default_arg_omitted_named_and_supplied() {
    let src = "fun String.tag(a: Int = 1, b: String = \"z\"): String = this + a + b\n\
        fun box(): String {\n\
        \x20 if (\"x\".tag() != \"x1z\") return \"omit\"\n\
        \x20 if (\"x\".tag(5) != \"x5z\") return \"pos\"\n\
        \x20 if (\"x\".tag(b = \"q\") != \"x1q\") return \"named\"\n\
        \x20 if (\"x\".tag(5, \"q\") != \"x5q\") return \"full\"\n\
        \x20 return \"OK\"\n}\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skip"),
    }
}
