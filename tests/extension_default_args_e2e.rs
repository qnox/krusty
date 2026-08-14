//! Extension-function DEFAULT ARGUMENTS: `fun T.foo(a: Int = 1)` called while omitting the defaulted
//! argument (`x.foo()`) — or naming it (`x.foo(a = 2)`). krusty fills the omitted constant defaults at
//! the call site (the extension lowers to a static `Facade.foo($receiver, args…)`). Verified on a real JVM.
use super::common;
use krusty::jvm::classpath::Classpath;
fn run(src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar();
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

/// The extension receiver is physical parameter zero but is not part of Kotlin's default-mask
/// indexing. Exactly 32 declared value parameters therefore require one mask word, not two.
#[test]
fn thirty_two_logical_extension_parameters_use_one_default_mask() {
    let params = (0..32)
        .map(|index| format!("p{index}: Int = {index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let src = format!(
        "fun String.pick({params}): String = this + p31\n\
         fun box(): String = if (\"O\".pick() == \"O31\") \"OK\" else \"FAIL\"\n"
    );
    let abi_src = format!("package masks\n{src}");
    if common::compile_lib_ref("ExtensionDefaultMask32", &abi_src).is_none() {
        eprintln!("skip kotlinc comparison: reference compiler unavailable");
    }
    let library = common::compile_lib("ExtensionDefaultMask32Abi", &abi_src)
        .expect("compile extension-mask ABI fixture");
    let classpath = Classpath::new(vec![library]);
    let facade = classpath.find("masks/LibKt").expect("extension facade");
    let default = facade
        .methods
        .iter()
        .find(|method| method.name == "pick$default")
        .expect("extension default stub");
    let expected = format!(
        "(Ljava/lang/String;{}ILjava/lang/Object;)Ljava/lang/String;",
        "I".repeat(32)
    );
    assert_eq!(default.descriptor, expected);
    assert_eq!(run(&src).as_deref().map(str::trim), Some("OK"));
}
