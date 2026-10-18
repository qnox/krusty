//! A reference to a CLASSPATH top-level function — `::greet` where `greet` lives in a jar / dependency
//! module — was rejected ("callable references are not supported"). Now it resolves to a
//! `FunctionReferenceImpl` whose `invoke` calls the real `invokestatic <facade>.greet(args)`. Verified
//! end-to-end on a real JVM (the function is in a separately kotlinc-compiled library).
use super::common;
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}
#[test]
fn classpath_function_reference_compiles_and_runs() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let sl = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some(libout) =
        common::compile_lib("cfr", "package lib\nfun add(a: Int, b: Int): Int = a + b\n")
    else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    // `::add` bound to a val and invoked, and passed to a higher-order function.
    let main = "import lib.add\n\
        fun apply2(f: (Int, Int) -> Int): Int = f(10, 20)\n\
        fun box(): String {\n\
        \x20 val g = ::add\n\
        \x20 return if (g(3, 4) == 7 && apply2(::add) == 30) \"OK\" else \"fail\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a classpath function reference");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn constructor_reference_wins_over_same_named_classpath_function() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let sl = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some(libout) =
        common::compile_lib("ctor_cfr", "package lib\nfun Foo(s: String): String = s\n")
    else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Foo\n\
        class Foo(val i: Int)\n\
        fun box(): String {\n\
        \x20 val make = ::Foo\n\
        \x20 val made = make(7)\n\
        \x20 return if (made.i == 7) \"OK\" else \"fail\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a constructor reference shadowing a classpath function");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn generic_dependency_constructor_reference_keeps_its_selected_parameter_application() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let sl = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some(libout) = common::compile_lib(
        "generic_ctor_cfr",
        "package lib\n\
         class Box<T>(val value: T)\n\
         typealias StringBox = Box<String>\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Box\n\
        import lib.StringBox\n\
        val make: (String) -> Box<String> = ::Box\n\
        val makeAlias: (String) -> StringBox = ::StringBox\n\
        fun box(): String = make(\"O\").value + makeAlias(\"K\").value\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a generic dependency constructor reference");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn java_constructor_formal_shadowing_does_not_replace_the_classifier_result_formal() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let sl = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some((libout, _)) = common::javac_compile(
        &[(
            "ShadowBox.java".to_string(),
            "package lib;\n\
             public final class ShadowBox<T> {\n\
               public final T value;\n\
               public <T> ShadowBox(String value, T ignored) { this.value = null; }\n\
             }\n"
            .to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.ShadowBox\n\
        val make: (String, Int) -> ShadowBox<String> = ::ShadowBox\n\
        fun box(): String { make(\"ignored\", 1); return \"OK\" }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a shadowed generic Java constructor reference");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
