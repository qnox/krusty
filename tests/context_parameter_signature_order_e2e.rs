//! Where a context parameter sits in the JVM signature, and what it contributes to inference.
//!
//! Two facts, both probed on kotlinc 2.4.10 and both differential here rather than hardcoded:
//!
//! 1. **Order.** A context parameter precedes the EXTENSION RECEIVER, it does not follow it. For
//!    `context(c: String) fun Src.plain(x: Int)` kotlinc signs `(Ljava/lang/String;Lrepro/Src;I)`:
//!    contexts, then receiver, then value parameters. This is the same layout krusty already models
//!    for a context FUNCTION TYPE (whose receiver sits at `params[context_count]`), so declarations
//!    and function types agree now that the declaration side matches.
//!
//! 2. **Inference.** A context argument is an ordinary inference source, and the context prefix must
//!    not consume the arguments that follow it. In `context(c: T) fun <T> Src.tagged(x: String): T`
//!    the value filling the context slot pins `T`; in `context(c: String) fun <T> Src.valued(x: T): T`
//!    the written argument does.
//!
//! Both facets miscompiled independently. The order divergence made every context extension
//! ABI-incompatible with kotlinc in BOTH directions — krusty could not resolve a context extension
//! read back from a kotlinc-built dependency at all. Separately, a context argument contributed
//! nothing to inference and was not boxed into its erased slot, so a primitive context value reached
//! an `Object` parameter unboxed: a `VerifyError` at class load rather than a wrong answer.
//!
//! What these tests deliberately do NOT use is a MEMBER PROPERTY initialized from a `with { }` block.
//! That shape has its own, context-independent defect — `class H { val v = with(42) { "x".length } }`
//! types `v` as `Object` while its initializer leaves an `int` on the stack — which reproduces with no
//! context parameters anywhere and is not this file's subject.

use super::common;

/// `javap -v -p` of one class, compiled by BOTH compilers: `(kotlinc, krusty)`. `None` when the
/// reference toolchain is unavailable (the caller then skips).
fn javap_both(stem: &str, src: &str, class: &str) -> Option<(String, String)> {
    let dir = common::scratch_dir()?;
    let kref = dir.join("ref");
    let kout = dir.join("out");
    std::fs::create_dir_all(&kref).ok()?;
    std::fs::create_dir_all(&kout).ok()?;
    let src_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&src_path, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        kref.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp(src, stem, &[common::stdlib_jar()])
        .unwrap_or_else(|| panic!("{stem}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == class)
        .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));
    let path = kout.join(format!("{class}.class"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();

    let dump = |root: &std::path::Path| {
        common::javap(&["-v", "-p", "-cp", &root.to_string_lossy(), class])
            .unwrap_or_else(|| panic!("{stem}: javap failed"))
    };
    let both = (dump(&kref), dump(&kout));
    let _ = std::fs::remove_dir_all(&dir);
    Some(both)
}

/// Every member descriptor, in `javap` order.
fn descriptors(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|l| l.trim().strip_prefix("descriptor: "))
        .map(str::to_string)
        .collect()
}

/// Every generic `Signature` attribute value, in `javap` order.
fn signatures(dump: &str) -> Vec<String> {
    dump.lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("Signature: "))
        .map(|l| l.split_once("// ").map_or(l, |(_, s)| s).trim().to_string())
        .collect()
}

/// `(slot, name, type)` of every `LocalVariableTable` row. This is what proves the BODY agrees with
/// the signature: the receiver's `$this$…` entry has to land in the same slot the descriptor puts it
/// in, or the body reads its parameters from the wrong locals. The row's start/length columns are
/// deliberately dropped — they measure the method's code SIZE, which still differs from kotlinc for
/// an unrelated reason (krusty's string concatenation lowers to nested `StringBuilder`s).
fn local_slots(dump: &str) -> Vec<String> {
    dump.lines()
        .map(str::trim)
        .filter_map(|l| {
            let fields = l.split_whitespace().collect::<Vec<_>>();
            let [start, length, slot, name, ty] = fields[..] else {
                return None;
            };
            let numeric = [start, length, slot]
                .iter()
                .all(|f| f.chars().all(|c| c.is_ascii_digit()));
            numeric.then(|| format!("{slot} {name} {ty}"))
        })
        .collect()
}

const ORDER: &str = r#"package repro

class Src

context(c: String) fun Src.plain(x: Int): String = c + x

context(c: String, d: Int) fun Src.two(x: Int): String = c + d + x

context(c: String) fun plainTop(x: Int): String = c + x
"#;

/// The context prefix comes FIRST and the extension receiver follows it — for one context parameter
/// and for several. A context function with no receiver is the control: it was already correct, so a
/// regression there would mean the fix over-reached.
#[test]
fn context_parameters_precede_the_extension_receiver() {
    let Some((kotlinc, krusty)) = javap_both("Corder", ORDER, "repro/CorderKt") else {
        eprintln!("skip (Corder: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        descriptors(&krusty),
        [
            "(Ljava/lang/String;Lrepro/Src;I)Ljava/lang/String;",
            "(Ljava/lang/String;ILrepro/Src;I)Ljava/lang/String;",
            "(Ljava/lang/String;I)Ljava/lang/String;",
        ]
    );
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
}

/// The BODY's local slots agree with that signature: `$this$two` is local 2, after the two contexts.
/// Registering the parameters in one order and binding the body in another would still produce a
/// well-formed class — it would just read the wrong locals.
#[test]
fn a_context_extension_body_binds_the_receiver_after_the_contexts() {
    let Some((kotlinc, krusty)) = javap_both("Cslots", ORDER, "repro/CslotsKt") else {
        eprintln!("skip (Cslots: reference toolchain unavailable)");
        return;
    };
    assert_eq!(local_slots(&krusty), local_slots(&kotlinc));
    assert!(
        local_slots(&krusty)
            .iter()
            .any(|l| l.contains("2 $this$two")),
        "receiver should occupy slot 2, after both contexts: {:?}",
        local_slots(&krusty)
    );
}

const MEMBER: &str = r#"package repro

class Src(val n: Int)

class Owner {
    context(c: String) fun Src.memExt(x: Int): String = c + n + x

    context(c: String) fun plainMem(x: Int): String = c + x
}
"#;

/// A CLASS-BODY extension signs the same way: its dispatch receiver is `this`, and among the method
/// parameters the context prefix still precedes the extension receiver. This path builds its physical
/// parameter list in a different place from the top-level one, so it stayed receiver-first after the
/// top-level layout was corrected — a half-fixed ABI rather than a consistent one.
#[test]
fn a_member_context_extension_signs_like_a_top_level_one() {
    let Some((kotlinc, krusty)) = javap_both("Cmem", MEMBER, "repro/Owner") else {
        eprintln!("skip (Cmem: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        descriptors(&krusty),
        [
            "()V",
            "(Ljava/lang/String;Lrepro/Src;I)Ljava/lang/String;",
            "(Ljava/lang/String;I)Ljava/lang/String;",
        ]
    );
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
}

/// …and it runs: the body reads its receiver and context from the slots the signature assigns them,
/// and the call site pushes them in that order.
#[test]
fn a_member_context_extension_runs() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        package repro\n\
        class Src(val n: Int)\n\
        class Owner {\n\
        \x20 context(c: String) fun Src.memExt(x: Int): String = c + n + x\n\
        \x20 fun go(): String = with(\"k\") { Src(7).memExt(1) }\n\
        }\n\
        fun box(): String = if (Owner().go() == \"k71\") \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(&[("main.kt", SRC)], "Main");
}

const GENERIC: &str = r#"package repro

class Src

context(c: T) fun <T> Src.tagged(x: String): T = c

context(c: String) fun <T> Src.valued(x: T): T = x
"#;

/// A GENERIC context extension signs identically to kotlinc, in the erased descriptor and in the
/// `Signature` attribute — the attribute is what a consumer's reified-inline splice reads, so the two
/// have to place the receiver in the same slot.
#[test]
fn a_generic_context_extension_signature_matches_kotlinc() {
    let Some((kotlinc, krusty)) = javap_both("Cgen", GENERIC, "repro/CgenKt") else {
        eprintln!("skip (Cgen: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        descriptors(&krusty),
        [
            "(Ljava/lang/Object;Lrepro/Src;Ljava/lang/String;)Ljava/lang/Object;",
            "(Ljava/lang/String;Lrepro/Src;Ljava/lang/Object;)Ljava/lang/Object;",
        ]
    );
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
    assert_eq!(signatures(&krusty), signatures(&kotlinc));
}

/// The value filling the CONTEXT slot pins `T`, and a primitive one boxes into the erased parameter.
/// Without the binding the result stayed `Any`; without the boxing an `int` reached an `Object` slot
/// and the class failed verification at load.
#[test]
fn a_context_argument_pins_the_type_variable() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        package repro\n\
        class Src\n\
        context(c: T) fun <T> Src.tagged(x: String): T = c\n\
        fun box(): String {\n\
        \x20 val v = with(42) { Src().tagged(\"a\") }\n\
        \x20 return if (v == 42) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[SRC]),
        Vec::<String>::new()
    );
    common::expect_box_ok_files_with_stdlib(&[("main.kt", SRC)], "Main");
}

/// The type variable is equally bindable from an ordinary VALUE argument while a context parameter
/// occupies the leading slot: the context prefix must not consume the argument that pins `T`. Zipping
/// the context-inclusive parameter list against the call's arguments shifted every binding by one, so
/// `T` stayed open and its members were unresolvable ("unresolved reference 'length'").
#[test]
fn a_value_argument_pins_the_type_variable_past_the_context_prefix() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        package repro\n\
        class Src\n\
        context(c: String) fun <T> Src.valued(x: T): T = x\n\
        fun box(): String {\n\
        \x20 val w = with(\"ctx\") { Src().valued(\"ab\").length }\n\
        \x20 return if (w == 2) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[SRC]),
        Vec::<String>::new()
    );
    common::expect_box_ok_files_with_stdlib(&[("main.kt", SRC)], "Main");
}

/// The READ side of the same layout: a context extension compiled by kotlinc into a dependency is
/// callable. Decoding the descriptor as receiver-then-contexts read the first context parameter as
/// the receiver, so no candidate matched and the call fell through unresolved.
#[test]
fn a_context_extension_from_a_kotlinc_dependency_is_callable() {
    const LIB: &str = "// LANGUAGE: +ContextParameters\n\
        package lib\n\
        class Src\n\
        context(c: String) fun Src.tag(x: Int): String = c + x\n";
    const MAIN: &str = "// LANGUAGE: +ContextParameters\n\
        import lib.Src\n\
        import lib.tag\n\
        fun box(): String = with(\"O\") { if (Src().tag(1) == \"O1\") \"OK\" else \"fail\" }\n";
    match common::expect_box_run_against_kotlinc(LIB, MAIN) {
        None => eprintln!("skip (Cdep: reference toolchain unavailable)"),
        Some(output) => assert_eq!(output, "OK"),
    }
}

/// A context extension read back from a KRUSTY-built dependency is callable too, with a written
/// argument and with an omitted default. This is the direction that does not cross the reference
/// compiler, so nothing else pins it: krusty's own record omitted the `JvmMethodSignature` handle
/// that kotlinc records here, and the reader's fallback derivation rebuilt the pre-fix
/// `(receiver, contexts…, values…)` order — the call then targeted a descriptor the dependency does
/// not declare, which links but fails at run time rather than at compile time.
#[test]
fn a_context_extension_from_a_krusty_dependency_is_callable() {
    const LIB: &str = "// LANGUAGE: +ContextParameters\n\
        package lib\n\
        class Src\n\
        context(c: String) fun Src.tag(x: Int): String = c + x\n\
        context(c: String) fun Src.tagged(x: Int = 5): String = c + x\n";
    const MAIN: &str = "// LANGUAGE: +ContextParameters\n\
        import lib.Src\n\
        import lib.tag\n\
        import lib.tagged\n\
        fun box(): String = with(\"O\") {\n\
        \x20 if (Src().tag(1) == \"O1\" && Src().tagged() == \"O5\") \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_against("Cself", LIB, MAIN);
}

/// …and the WRITE side of the same boundary: kotlinc itself resolves a context extension out of a
/// krusty-built dependency, at top level and as a class member. This is the strongest available check
/// that the emitted descriptor and the emitted `@Metadata` agree with each other AND with the
/// reference compiler's reader, which a self-consistent (but wrongly ordered) pair of our own would
/// pass. The MEMBER case additionally pins that class metadata records its context parameters as
/// `Function.context_parameter`: published as ordinary value parameters, they are demanded
/// positionally and kotlinc rejects the call with "no value passed for parameter 'x'".
#[test]
fn kotlinc_resolves_a_context_extension_from_a_krusty_dependency() {
    const LIB: &str = "// LANGUAGE: +ContextParameters\n\
        package lib\n\
        class Src\n\
        context(c: String) fun Src.tag(x: Int): String = c + x\n\
        class Owner {\n\
        \x20 context(c: String) fun Src.memExt(x: Int): String = c + x\n\
        }\n";
    const MAIN: &str = "package use\n\
        import lib.Owner\n\
        import lib.Src\n\
        import lib.tag\n\
        fun check(): String = with(\"O\") { Src().tag(1) }\n\
        fun checkMember(): String = with(Owner()) { with(\"O\") { Src().memExt(1) } }\n";
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skip (Cwrite: scratch filesystem unavailable)");
        return;
    };
    let libout = dir.join("lib");
    let useout = dir.join("use");
    std::fs::create_dir_all(&libout).unwrap();
    std::fs::create_dir_all(&useout).unwrap();
    common::compile_to_dir(
        LIB,
        "Lib",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
        &libout,
    )
    .unwrap_or_else(|| panic!("Cwrite: krusty failed to build the dependency"));

    let main_path = dir.join("Use.kt");
    std::fs::write(&main_path, MAIN).unwrap();
    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-cp".to_string(),
        libout.to_string_lossy().into_owned(),
        "-d".to_string(),
        useout.to_string_lossy().into_owned(),
        main_path.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skip (Cwrite: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        code, 0,
        "kotlinc rejected a context extension from a krusty-built dependency: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
