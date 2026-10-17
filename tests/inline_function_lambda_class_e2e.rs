//! A lambda written in an `inline` function compiles to a class of its own, never to an
//! `invokedynamic`: `inline fun plain() = take { "plain$it" }` writes `InlineScopeKt$plain$1`. The
//! rule reaches through an enclosing lambda (`nested$1$1`, enclosed by `nested$1.invoke`) and an
//! anonymous object's method (`inObject$1$area$1`). Each class is public, and one written directly
//! in the inline function carries the source map of a class generated for inlining. A caller that
//! inlines the function, in the same file or another, constructs the declaring file's class. A
//! generic mutable capture keeps that foreign constructor's erased `Ref$ObjectRef` contract.

use super::common;

const INLINE_SCOPE: &str = r#"package inlinescope

fun interface Shape {
    fun area(): String
}

fun take(f: (Int) -> String): String = f(1)

fun runBlock(block: () -> Unit) = block()

inline fun plain(): String = take { "plain$it" }

inline fun capturing(prefix: String): String = take { prefix + it }

inline fun <T> update(initial: T, crossinline transform: (T) -> T): T {
    var current = initial
    runBlock { current = transform(current) }
    return current
}

inline fun nested(): String = take { outer -> take { "nested$outer$it" } }

inline fun throughParameter(g: (Int, (Int) -> String) -> String): String = g(2) { "param$it" }

private inline fun hidden(): String = take { "hidden$it" }

inline fun inObject(): String = object : Shape {
    override fun area(): String = take { "object$it" }
}.area()

class Holder(val tag: String) {
    inline fun member(): String = take { tag + it }
}

fun notInline(): String = take { "indy$it" }

fun box(): String {
    val results = plain() + capturing("c") + nested() + hidden() + inObject() +
        Holder("m").member() + notInline() + fromAnotherFile()
    return if (results == "plain1c1nested11hidden1object1m1indy1plain1x1nested112") "OK" else results
}
"#;

const CALLER: &str = r#"package inlinescope

fun fromAnotherFile(): String = plain() + capturing("x") + nested() + update(1) { it + 1 }
"#;

/// Every class both files declare except the anonymous object itself, whose own shape is not this
/// rule's.
const CLASSES: &[&str] = &[
    "inlinescope/InlineScopeKt",
    "inlinescope/InlineScopeKt$plain$1",
    "inlinescope/InlineScopeKt$capturing$1",
    "inlinescope/InlineScopeKt$update$1",
    "inlinescope/InlineScopeKt$nested$1",
    "inlinescope/InlineScopeKt$nested$1$1",
    "inlinescope/InlineScopeKt$throughParameter$1",
    "inlinescope/InlineScopeKt$hidden$1",
    "inlinescope/InlineScopeKt$inObject$1$area$1",
    "inlinescope/Holder",
    "inlinescope/Holder$member$1",
    "inlinescope/CallerKt",
    "inlinescope/CallerKt$fromAnotherFile$$inlined$update$1",
];

#[test]
fn lambdas_in_inline_functions_run() {
    common::expect_box_ok_files_with_stdlib(
        &[("InlineScope.kt", INLINE_SCOPE), ("Caller.kt", CALLER)],
        "InlineScope",
    );
}

#[test]
fn lambdas_in_inline_functions_are_classes_like_kotlinc() {
    let sources = [("InlineScope.kt", INLINE_SCOPE), ("Caller.kt", CALLER)];
    let jdk = common::jdk_modules();
    let krusty =
        common::compile_in_process_files(&sources, &[common::stdlib_jar()], Some(jdk.as_path()))
            .expect("krusty compiles the fixture");
    let dir = common::scratch_dir().expect("scratch directory");
    let mut arguments = vec![
        "-d".to_string(),
        dir.join("ref").to_string_lossy().into_owned(),
    ];
    for (name, source) in sources {
        let path = dir.join(name);
        std::fs::write(&path, source).expect("write source");
        arguments.push(path.to_string_lossy().into_owned());
    }
    let (code, stderr) =
        common::kotlinc_compile(&arguments).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for &class in CLASSES {
        let reference = std::fs::read(dir.join("ref").join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(
            common::member_table(emitted),
            common::member_table(&reference),
            "{class}: members"
        );
        if *emitted != reference {
            panic!(
                "{}",
                common::exact_class_difference(class, &reference, emitted)
            );
        }
    }
    let lifted = krusty
        .iter()
        .filter(|(name, _)| name == "inlinescope/InlineScopeKt")
        .flat_map(|(_, bytes)| common::member_table(bytes))
        .filter(|member| member.contains("$lambda$"))
        .collect::<Vec<_>>();
    assert_eq!(
        lifted.len(),
        1,
        "only the non-inline lambda is lifted: {lifted:?}"
    );
}
