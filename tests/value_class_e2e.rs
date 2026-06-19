//! Value/inline-class member synthesis (phase 388). A `@JvmInline value class X(val v: U)` emits
//! kotlinc's unboxed-support members on `X.class`: the `U` field + `<init>(U)` + `getV()` from the
//! ordinary single-field class path, plus the synthesized `box-impl(U):X` / `constructor-impl(U):U`
//! (static) and `unbox-impl():U` (instance). Use-site unboxing isn't wired yet, so the resolver still
//! rejects value-class *files*; this test drives the library directly (ignoring that diagnostic) to
//! verify only the synthesized class shape — the structural half of the differential-vs-kotlinc check.

use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::classreader::parse_class;
use krusty::jvm::ir_emit::emit_all;
use krusty::jvm::names::file_class_name;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const ACC_STATIC: u16 = 0x0008;

#[test]
fn value_class_synthesizes_box_unbox_constructor_impl() {
    let src = "@JvmInline\nvalue class S(val x: Int)\nfun box(): String = \"OK\"\n";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    assert!(!d.has_errors(), "unexpected parse errors");

    // `check_file` rejects value classes (use-site unboxing is a later phase); ignore that
    // diagnostic — this test exercises only the value-class member synthesis.
    let mut d2 = DiagSink::new();
    let syms = collect_signatures(&files, &mut d2);
    let info = check_file(&files[0], &syms, &mut d2);

    let ir = lower_file(&files[0], &info, &syms).expect("value class should lower");
    let facade = file_class_name("S", None);
    let cp = Classpath::new(vec![]);
    let classes = emit_all(&ir, &facade, &cp).expect("emit");

    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "S")
        .expect("S.class emitted");
    let ci = parse_class(bytes).expect("parse S.class");

    // box-impl(I)LS;  — static factory wrapping the underlying value.
    let box_impl = ci.method("box-impl", "(I)LS;").expect("box-impl(I)LS;");
    assert_ne!(
        box_impl.access & ACC_STATIC,
        0,
        "box-impl must be ACC_STATIC"
    );

    // constructor-impl(I)I  — static, returns the (validated) underlying value.
    let ctor_impl = ci
        .method("constructor-impl", "(I)I")
        .expect("constructor-impl(I)I");
    assert_ne!(
        ctor_impl.access & ACC_STATIC,
        0,
        "constructor-impl must be ACC_STATIC"
    );

    // unbox-impl()I  — instance method reading the field.
    let unbox = ci.method("unbox-impl", "()I").expect("unbox-impl()I");
    assert_eq!(
        unbox.access & ACC_STATIC,
        0,
        "unbox-impl is an instance method"
    );

    // The ordinary single-field class path still provides the field's getter.
    assert!(ci.method("getX", "()I").is_some(), "getX()I getter");

    // The static `-impl` members must NOT leak onto the top-level facade.
    if let Some((_, fbytes)) = classes.iter().find(|(n, _)| *n == facade) {
        let fc = parse_class(fbytes).expect("parse facade");
        assert!(
            fc.methods_named("box-impl").is_empty(),
            "box-impl must live on S, not the facade"
        );
    }
}
