//! A private constructor, reached the way kotlinc reaches it.
//!
//! kotlinc keeps a private constructor private in the class file and calls it directly from its
//! own class. Another class (a nested subclass's `super(…)`, a companion's factory) calls a public
//! synthetic `(…, DefaultConstructorMarker)` accessor, which kotlinc creates only for a constructor
//! that such a call reaches. A call that leaves arguments to their defaults goes through the
//! package-private `$default` overload instead and needs no accessor. kotlinc guards no private
//! constructor's parameters and gives a private all-default primary no no-argument overload.
use super::common;

const SOURCE: &str = r#"open class Plain private constructor(val x: Int, val y: String) {
    private constructor(y: String) : this(1, y)
    private constructor(z: Long, w: String = "w") : this(1, w)

    fun own() = Plain("own")
    fun anon() = object { fun make() = Plain(5L) }.make()

    class Sub : Plain("sub")
    class Sub2 : Plain(3, "s2")

    companion object {
        fun make() = Plain(7L, "c")
        fun other() = Plain("c2")
    }
}

class Defaults private constructor(val a: Int = 1, val b: String = "b") {
    companion object {
        fun make() = Defaults()
        fun given() = Defaults(2, "c")
    }
}

fun box(): String {
    val p = Plain.make()
    if (p.y != "c") return "make"
    if (p.own().y != "own") return "own"
    if (p.anon().y != "w") return "anon"
    if (Plain.other().y != "c2") return "other"
    if (Plain.Sub().y != "sub") return "sub"
    if (Plain.Sub2().y != "s2") return "sub2"
    if (Defaults.make().b != "b") return "defaults"
    return if (Defaults.given().b == "c") "OK" else "given"
}
"#;

#[test]
fn private_constructors_are_reached_from_other_classes() {
    common::expect_box_ok_with_stdlib(SOURCE, "PrivateConstructors");
}

/// A method's instructions, with pool indices masked; `javap` names every call target.
fn method_code(class: &str, bytes: &[u8], method: &str) -> Vec<String> {
    let work = common::scratch_dir().expect("cannot allocate disassembly fixture");
    let path = work.join(format!("{class}.class"));
    std::fs::write(&path, bytes).expect("write class for disassembly");
    let text = common::javap(&["-c", "-p", &path.to_string_lossy()]).expect("javap unavailable");
    let _ = std::fs::remove_dir_all(work);
    let code = common::method_instructions(&text, &format!("{method};"));
    assert!(!code.is_empty(), "{class} has no method {method}");
    code
}

/// The constructors in classfile order, with their access flags.
fn constructors(bytes: &[u8]) -> Vec<(String, String, u16)> {
    krusty::jvm::classreader::parse_class(bytes)
        .expect("class parses")
        .methods
        .into_iter()
        .filter(|method| method.name == "<init>")
        .map(|method| (method.name, method.descriptor, method.access))
        .collect()
}

#[test]
fn private_constructor_accessors_match_kotlinc() {
    let sources = [("Plain.kt", SOURCE)];
    for (class, methods) in [
        (
            "Plain",
            &[
                "private Plain(int, java.lang.String)",
                "private Plain(java.lang.String)",
                "private Plain(long, java.lang.String)",
                "Plain(long, java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker)",
                "public Plain(java.lang.String, kotlin.jvm.internal.DefaultConstructorMarker)",
                "public Plain(int, java.lang.String, kotlin.jvm.internal.DefaultConstructorMarker)",
                "public Plain(long, java.lang.String, kotlin.jvm.internal.DefaultConstructorMarker)",
            ][..],
        ),
        (
            "Defaults",
            &[
                "private Defaults(int, java.lang.String)",
                "Defaults(int, java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker)",
                "public Defaults(int, java.lang.String, kotlin.jvm.internal.DefaultConstructorMarker)",
            ][..],
        ),
        ("Plain$Sub", &["public Plain$Sub()"][..]),
        ("Plain$Sub2", &["public Plain$Sub2()"][..]),
        (
            "Plain$Companion",
            &[
                "public final Plain make()",
                "public final Plain other()",
            ][..],
        ),
        (
            "Defaults$Companion",
            &[
                "public final Defaults make()",
                "public final Defaults given()",
            ][..],
        ),
    ] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert_eq!(constructors(&pair.krusty), constructors(&pair.kotlinc), "{class}");
        for method in methods {
            assert_eq!(
                method_code(class, &pair.krusty, method),
                method_code(class, &pair.kotlinc, method),
                "{class}.{method}",
            );
        }
    }
}
