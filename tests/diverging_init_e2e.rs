//! A property initializer (or init block) that diverges — e.g. `val x: String = TODO()` — must not
//! emit the dead field-store/return after the throw (which produced an inconsistent StackMapTable).
//! `TODO()` throws `kotlin.NotImplementedError`, resolved from the stdlib on the classpath.

use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures_with_cp};
use krusty::lexer::lex;
use krusty::parser::parse;

use super::common;

const SRC: &str = r#"
class C {
    val todo: String = TODO()
    val uninitializedVal: String
    var uninitializedVar: String
}
fun box(): String {
    try {
        C()
        return "Fail: no throw"
    } catch (e: NotImplementedError) {
        return "OK"
    }
}
"#;

#[test]
fn diverging_property_initializer_runs() {
    let java_home = common::java_home();
    let stdlib = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    // Sanity: the checker accepts it with the same semantic classpath used for compilation.
    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let files = vec![parse(SRC, &toks, &mut d)];
    let mut syms = collect_signatures_with_cp(
        &files,
        Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
                stdlib.clone(),
                jdk.clone(),
            ])),
        )),
        &mut d,
    );
    let _ = check_file(&files[0], &mut syms, &mut d);
    assert!(
        !d.has_errors(),
        "krusty errors: {:?}",
        d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
    );
    assert_eq!(
        common::expect_box_run(SRC, "Div", &[stdlib], Some(&jdk)),
        "OK"
    );
}

#[test]
fn diverging_nested_class_property_initializer_runs() {
    let src = r#"
interface Action { fun run() }
fun box(): String = try {
    object : Action {
        override fun run() {}
        val unreachable: Int = throw IllegalStateException()
    }.run()
    "FAIL"
} catch (_: IllegalStateException) {
    "OK"
}
"#;
    common::expect_box_ok_with_stdlib(src, "DivergingNestedClassInitializer");
}
