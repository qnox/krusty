//! A call to a CLASSPATH top-level `inline fun <reified T>` compiled, then threw
//! `UnsupportedOperationException: This function has a reified type parameter and thus can only be
//! inlined at compilation time` when run.
//!
//! The splice machinery was fine; its INPUT was missing. `reified_call_subst_for` — which pairs the
//! callee's formal type-parameter names with the call's type arguments — was invoked only on the two
//! EXTENSION lowering paths, so a top-level call recorded no substitution, `splice_unified` refused to
//! specialize a `reifiedOperationMarker` body it could not bind, and the emitter fell back to a direct
//! call. That fallback is a miscompile for a reified callee: kotlinc's compiled body exists only to
//! throw. The guard meant to catch exactly this (`ir_emit`: bail rather than fall back) was keyed on
//! the same absent substitution, so it never fired — the reason a wrong program compiled clean.
//!
//! An extension `<reified T>` splice was already covered (`filterIsInstance<String>()`); this pins the
//! top-level form, which is what `mockk<T>()` and most test-double builders are.
use super::common;

const LIB: &str = r#"
    package lib

    inline fun <reified T : Any> nameOf(): String = T::class.simpleName ?: "?"

    inline fun <reified T : Any> describe(prefix: String = "<", suffix: String = ">"): String =
        prefix + (T::class.simpleName ?: "?") + suffix

    // Same SOURCE name and arity, but a distinct JVM spelling and unrelated generic return. A
    // `$default` synthetic must recover metadata from its exact base, not this same-arity sibling.
    @JvmName("alternateDescribe")
    fun <U> describe(value: Int, marker: Long = 0): U? = null

    inline fun <reified T : Any> isA(value: Any): Boolean = value is T
"#;

/// A reified inline whose DEFAULT is a lambda typed on `T`. kotlinc marks the `$default` body with
/// `Intrinsics.needClassReification` — "regenerate the class this materializes, per call site".
const NEEDS_CLASS_REIFICATION: &str = r#"
    package lib

    inline fun <reified T : Any> configure(block: T.() -> Unit = {}): String =
        (T::class.simpleName ?: "?") + "/configured"
"#;

#[test]
fn top_level_reified_inline_call_splices_and_runs() {
    let Some(libout) = common::compile_lib("reified_inline_top_level", LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    // `nameOf` reifies into `T::class`; `isA` into an `instanceof`; `describe` covers the shapes
    // crossed with defaults — all supplied, all omitted, and one named while an earlier is omitted
    // (the `$default` synthetic, whose body is equally splice-only).
    let main = "import lib.describe\n\
        import lib.isA\n\
        import lib.nameOf\n\
        fun box(): String {\n\
        \x20 if (nameOf<String>() != \"String\") return \"nameOf: ${nameOf<String>()}\"\n\
        \x20 if (describe<String>(\"[\", \"]\") != \"[String]\") return \"supplied: ${describe<String>(\"[\", \"]\")}\"\n\
        \x20 if (describe<String>() != \"<String>\") return \"omitted: ${describe<String>()}\"\n\
        \x20 val named: String = describe<String>(suffix = \"}\")\n\
        \x20 if (named != \"<String}\") return \"named: $named\"\n\
        \x20 if (!isA<String>(\"x\")) return \"isA true\"\n\
        \x20 if (isA<String>(7)) return \"isA false\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out, "OK");
}

/// The omitted `block` is the library's singleton lambda class, whose `$default` arm calls
/// `needClassReification`. The call inlines that lambda's body specialized for `String` instead of
/// calling the bridge, which only throws.
#[test]
fn a_default_lambda_needing_class_reification_is_inlined_and_runs() {
    let Some(libout) = common::compile_lib("reified_needs_reification", NEEDS_CLASS_REIFICATION)
    else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    let main = "import lib.configure\n\
        fun box(): String {\n\
        \x20 val configured = configure<String>()\n\
        \x20 return if (configured == \"String/configured\") \"OK\" else configured\n\
        }\n";
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out, "OK");
}

/// Real kotlinc must be able to INLINE a krusty-built reified inline function: the emitted method
/// body carries kotlinc's own `reifiedOperationMarker` placeholder pattern, which its inliner
/// patches with the call-site class. Self-consumption alone would not prove the bytes are the
/// convention — kotlinc is the arbiter.
#[test]
fn kotlinc_inlines_krusty_reified_method() {
    let root = common::scratch_dir().expect("scratch dir");
    let lib = root.join("lib");
    let lib_src =
        "package demo\ninline fun <reified T : Any> nameOf(): String = T::class.java.simpleName\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    common::compile_to_dir(
        lib_src,
        "Lib",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
        &lib,
    )
    .expect("krusty compiles the reified inline fn");
    std::fs::write(
        root.join("C.kt"),
        "import demo.nameOf\nfun main() { println(nameOf<String>()) }\n",
    )
    .unwrap();
    let cout = root.join("cout");
    let args = vec![
        root.join("C.kt").to_string_lossy().into_owned(),
        "-cp".to_string(),
        lib.to_string_lossy().into_owned(),
        "-d".to_string(),
        cout.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect(
        "provisioned kotlinc server unavailable — run `just kotlinc \"$(just max-version)\"`",
    );
    assert_eq!(
        code, 0,
        "real kotlinc must inline krusty's reified method: {stderr}"
    );
    let driver = "public class M2 { public static void main(String[] a) { CKt.main(); } }";
    std::fs::write(root.join("M2.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        cout.to_string_lossy(),
        lib.to_string_lossy(),
        stdlib.to_string_lossy()
    );
    let out = common::javac_run(
        root.join("M2.java").to_str().unwrap(),
        &cp,
        root.join("m2out").to_string_lossy().as_ref(),
        "M2",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "String");
}
