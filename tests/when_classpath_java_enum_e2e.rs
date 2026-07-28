//! Classpath enum constants participate in `when` exhaustiveness.
use super::common;
use std::path::PathBuf;

const JAVA_ENUM: &str = "package p;\npublic enum Color { RED, GREEN }\n";

fn compile_java_enum() -> Option<(PathBuf, Vec<PathBuf>, common::JavacOutput)> {
    let jdk = common::jdk_modules()?;
    let jars = common::classpath_jars_for("");
    let output =
        common::javac_compile(&[("Color.java".to_string(), JAVA_ENUM.to_string())], &jars)?;
    Some((jdk, jars, output))
}

fn remove_java_output(javadir: &std::path::Path) {
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn exhaustive_when_over_classpath_java_enum_as_value() {
    let Some((jdk, jars, (javadir, mut classes))) = compile_java_enum() else {
        return;
    };
    let mut cp = jars.clone();
    cp.push(javadir.clone());

    const MAIN: &str = "\
        fun f(c: p.Color): Int = when (c) { p.Color.RED -> 1; p.Color.GREEN -> 2 }\n\
        fun box(): String {\n\
        \x20 val r = f(p.Color.RED) + f(p.Color.GREEN)\n\
        \x20 return if (r == 3) \"OK\" else \"fail: $r\"\n\
        }\n";
    let compiled = common::compile_in_process(MAIN, "MainKt", &cp, Some(jdk.as_path()));
    remove_java_output(&javadir);
    let main_classes =
        compiled.expect("exhaustive `when` over a classpath Java enum should compile");

    classes.extend(main_classes);
    let box_class = common::find_box_class(&classes).expect("box() class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

#[test]
fn non_exhaustive_when_over_classpath_java_enum_still_rejected() {
    let Some((jdk, jars, (javadir, _))) = compile_java_enum() else {
        return;
    };
    let mut cp = jars.clone();
    cp.push(javadir.clone());

    const MAIN: &str =
        "fun f(c: p.Color): Int = when (c) { p.Color.RED -> 1 }\nfun box(): String = \"OK\"\n";
    let diags = common::front_end_diagnostics(MAIN, &cp, Some(jdk.as_path()));
    remove_java_output(&javadir);
    assert_eq!(
        diags,
        vec![
            "'when' expression must be exhaustive. Add the 'GREEN' branch or an 'else' branch."
                .to_string()
        ]
    );
}
