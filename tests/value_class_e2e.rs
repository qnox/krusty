//! Value/inline-class member synthesis (phase 388). A `@JvmInline value class X(val v: U)` emits
//! kotlinc's unboxed-support members on `X.class`: the `U` field + `<init>(U)` + `getV()` from the
//! ordinary single-field class path, plus the synthesized `box-impl(U):X` / `constructor-impl(U):U`
//! (static) and `unbox-impl():U` (instance). Use-site unboxing is wired (value-class params/fields/
//! construction lower to the unboxed underlying type — see tests/session_subsystems_e2e.rs::
//! value_class_unboxed_arith), so `check_file` accepts value-class files; this test drives the library
//! directly to verify the synthesized class shape — the structural half of the differential-vs-kotlinc
//! check.

use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::classreader::parse_class;
use krusty::jvm::ir_emit::emit_all;
use krusty::jvm::names::file_class_name;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

use super::common;

const ACC_STATIC: u16 = 0x0008;

#[test]
fn value_class_synthesizes_box_unbox_constructor_impl() {
    let src = "@JvmInline\nvalue class S(val x: Int)\nfun box(): String = \"OK\"\n";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    assert!(!d.has_errors(), "unexpected parse errors");

    // `check_file` accepts value-class files (use-site unboxing is wired); the file resolves clean.
    let mut syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &mut syms, &mut d);
    assert!(!d.has_errors(), "value-class file should check clean");

    let runtime = krusty::libraries::EmptySymbolSource;
    let mut ir = lower_file(&files[0], &info, &syms, &runtime).expect("value class should lower");
    let facade = file_class_name("S", None);
    // The value-class `-impl` members are synthesized by the JVM passes (not `ir_lower`).
    krusty::jvm::backend::run_backend_passes(&mut ir, &files[0], &facade, &syms)
        .expect("backend passes should accept this value class");
    let cp = Classpath::new(vec![]);
    let classes = emit_all(&ir, &facade, &cp, None).expect("emit");

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

#[test]
fn value_class_is_property_uses_javabean_getter_name() {
    let src = "@JvmInline\nvalue class Flag(val isOpen: Boolean)\nfun box(): String = \"OK\"\n";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    assert!(!d.has_errors(), "unexpected parse errors");

    let mut syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &mut syms, &mut d);
    assert!(!d.has_errors(), "value-class file should check clean");

    let runtime = krusty::libraries::EmptySymbolSource;
    let mut ir = lower_file(&files[0], &info, &syms, &runtime).expect("value class should lower");
    let facade = file_class_name("Flag", None);
    krusty::jvm::backend::run_backend_passes(&mut ir, &files[0], &facade, &syms)
        .expect("backend passes should accept this value class");
    let cp = Classpath::new(vec![]);
    let classes = emit_all(&ir, &facade, &cp, None).expect("emit");

    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "Flag")
        .expect("Flag.class emitted");
    let ci = parse_class(bytes).expect("parse Flag.class");

    assert!(
        ci.method("isOpen", "()Z").is_some(),
        "isOpen boolean property getter"
    );
    assert!(
        ci.method("getIsOpen", "()Z").is_none(),
        "value-class is-property must not emit getIsOpen"
    );
}

#[test]
fn value_class_reference_underlying_eq_hash_to_string_runs() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(java_home) = common::java_home() else {
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
@JvmInline
value class Id(val raw: String)

fun box(): String {
    val a = Id("x")
    if (a != Id("x")) return "f1"
    if (a == Id("y")) return "f2"
    if (a.hashCode() != Id("x").hashCode()) return "f3"
    if (a.toString() != "Id(raw=x)") return "f4:$a"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "IdBox", &[stdlib], Some(&jdk)).as_deref(),
        Some("OK")
    );
}

#[test]
fn nullable_reference_underlying_value_class_extension_to_string_is_null_safe() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(java_home) = common::java_home() else {
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
@JvmInline
value class Id(val raw: String)

fun Id?.show(): String = toString()

fun box(): String {
    val n = (null as Id?).show()
    if (n != "null") return "n:$n"
    val x = Id("x").show()
    if (x != "Id(raw=x)") return "x:$x"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "NullableValueClassToString", &[stdlib], Some(&jdk))
            .as_deref(),
        Some("OK")
    );
}

#[test]
fn sized_array_of_value_class_uses_provider_value_underlying() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(java_home) = common::java_home() else {
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
@JvmInline
value class Vc(val v: Int)

fun box(): String {
    val arr = Array(3) { Vc(it + 1) }
    var sum = 0
    for (x in arr) sum += x.v
    if (sum != 6) return "f1:$sum"
    if (arr[2].v != 3) return "f2"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "SizedValueClassArray", &[stdlib], Some(&jdk)).as_deref(),
        Some("OK")
    );
}
