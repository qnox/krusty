//! `ifBlank`/`ifEmpty` with a null-returning lambda.
//!
//! Shape (config plumbing): `controllerUrl.ifBlank { null }` — the stdlib declaration is the
//! multi-bound generic `fun <C, R> C.ifBlank(defaultValue: () -> R): R where C : CharSequence,
//! C : R`, and the lambda's `null` binds `R := Nothing?` — the `C : R` bound check then rejected
//! the only candidate. Inference must JOIN the bottom binding with the constraining side
//! (`R = String?`) in the REAL bindings, not a check-local copy: the return type substitutes from
//! them, so a local fix admits the call but returns `Nothing?` and poisons unannotated signatures.
use super::common;

fn run_box(src: &str) -> String {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    common::expect_box_run(src, "Main", &[sl], Some(jdk.as_path()))
}

#[test]
fn if_blank_nullable_result_emits_a_string_descriptor() {
    const SRC: &str = "fun pick(url: String): String? = url.ifBlank { null }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(SRC, "IfBlankNullSignature", &[stdlib], Some(&jdk))
        .expect("ifBlank signature source compiles with Krusty");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "IfBlankNullSignatureKt")
        .expect("Krusty facade emitted");
    let class = krusty::jvm::classreader::parse_class(bytes).expect("Krusty facade parses");
    assert!(
        class
            .method("pick", "(Ljava/lang/String;)Ljava/lang/String;")
            .is_some(),
        "the completed R = String? binding must reach the emitted JVM descriptor"
    );
}

#[test]
fn if_blank_null_lambda_widens_to_nullable() {
    const SRC: &str = "fun pick(url: String): Pair<String, String?> =\n\
        \x20   \"controller_url\" to url.ifBlank { null }\n\
        fun box(): String {\n\
        \x20   val (k, blank) = pick(\"\")\n\
        \x20   val (_, kept) = pick(\"http://x\")\n\
        \x20   return if (k == \"controller_url\" && blank == null && kept == \"http://x\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn if_blank_null_without_expected_type_keeps_the_string() {
    // No expected type: a local val and a chained safe-call read. The joined binding must reach
    // the RETURN type — `Nothing?` here previously left `length` unresolved. (A TOP-LEVEL
    // property of this shape still falls to the signature pre-pass's untyped-lambda limit —
    // a separate gap.)
    const SRC: &str = "fun box(): String {\n\
        \x20   val x = \"http://x\".ifBlank { null }\n\
        \x20   val blank = \"\".ifBlank { null }\n\
        \x20   return if (x?.length == 8 && blank == null) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn if_empty_default_value_keeps_the_type() {
    // The plain (non-null) shape of the same declaration family.
    const SRC: &str = "fun box(): String {\n\
        \x20   val v = \"\".ifEmpty { \"fallback\" }\n\
        \x20   return if (v == \"fallback\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_box(SRC), "OK");
}
