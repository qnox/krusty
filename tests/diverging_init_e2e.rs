//! A property initializer (or init block) that diverges — e.g. `val x: String = TODO()` — must not
//! emit the dead field-store/return after the throw (which produced an inconsistent StackMapTable).
//! `TODO()` throws `kotlin.NotImplementedError`, resolved from the stdlib on the classpath.

use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};

mod common;

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
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping diverging_init_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping diverging_init_e2e: no kotlin-stdlib jar found");
        return;
    };

    // Sanity: the checker accepts it (with the stdlib classpath).
    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let files = vec![parse(SRC, &toks, &mut d)];
    let mut syms = collect_signatures_with_cp(
        &files,
        Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![stdlib.clone()])),
        )),
        &mut d,
    );
    let _ = check_file(&files[0], &mut syms, &mut d);
    assert!(
        !d.has_errors(),
        "krusty errors: {:?}",
        d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
    );

    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "Div", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
