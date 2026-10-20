//! A local class reads a captured value from its constructor parameter while it is being
//! constructed, and stores it in a field only when code that runs later reads it.
//!
//! kotlinc's constructor code is the primary constructor's defaults and super-constructor
//! arguments, the property initializers and `init` blocks, and each secondary constructor's
//! delegation arguments and body. A read there uses the parameter, even when the value also has a
//! field. A value no later code reads gets no field at all.
use super::common;

fn assert_same_class(name: &str, source: &str, class: &str) {
    common::byte_diff_against_kotlinc_cp(name, source, class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|why| panic!("{why}"));
}

/// `a` is read only by an initializer and an `init` block, so it has no field; `b` is read by a
/// member and keeps one.
#[test]
fn a_capture_read_only_during_construction_gets_no_field() {
    let source = "\
fun initializer(a: Int, b: Int): Int {
    class L {
        val x = a + 1
        fun m() = b
    }
    return L().x + L().m()
}

fun block(a: Int): Int {
    class M {
        val y: Int
        init { y = a * 2 }
    }
    return M().y
}
";
    assert_same_class(
        "ConstructionOnly",
        source,
        "ConstructionOnlyKt$initializer$L",
    );
    assert_same_class("ConstructionOnly", source, "ConstructionOnlyKt$block$M");
    assert_same_class("ConstructionOnly", source, "ConstructionOnlyKt");
}

/// `a` is read by a member, so it has a field, but the initializer still reads the parameter.
#[test]
fn a_construction_read_uses_the_parameter_even_with_a_field() {
    let source = "\
fun both(a: Int): Int {
    class B {
        val x = a
        fun m() = a
    }
    return B().x + B().m()
}
";
    assert_same_class(
        "ConstructionAndMember",
        source,
        "ConstructionAndMemberKt$both$B",
    );
}

/// Secondary constructor bodies are constructor code too, with or without a primary constructor.
#[test]
fn a_secondary_constructor_body_reads_the_parameter() {
    let source = "\
fun noPrimary(a: Int, b: Int): Int {
    class S {
        val q: Int
        constructor() { q = a }
        init { println(b) }
    }
    return S().q
}

fun delegating(a: Int): Int {
    class D(x: Int) {
        constructor() : this(1) { println(a) }
    }
    D()
    return 0
}
";
    assert_same_class(
        "SecondaryConstruction",
        source,
        "SecondaryConstructionKt$noPrimary$S",
    );
    assert_same_class(
        "SecondaryConstruction",
        source,
        "SecondaryConstructionKt$delegating$D",
    );
}

/// Invoking a captured function value in an initializer reads the parameter too.
#[test]
fn an_invoked_capture_is_read_from_the_parameter() {
    let source = "\
fun invoked(f: () -> Int): Int {
    class I {
        val x = f()
    }
    return I().x
}
";
    assert_same_class(
        "InvokedConstruction",
        source,
        "InvokedConstructionKt$invoked$I",
    );
}

/// The stored captures keep the first-use order the constructor takes them in: the constructor is
/// `(c, b, a)`, `c` has no field, and the fields are `$b`, `$a`.
#[test]
fn stored_captures_follow_the_first_use_order() {
    let source = "\
fun mixed(a: Int, b: Int, c: Int): Int {
    class X {
        val p = c
        fun m() = b + a
    }
    return X().p + X().m()
}
";
    assert_same_class("MixedConstruction", source, "MixedConstructionKt$mixed$X");
    assert_same_class("MixedConstruction", source, "MixedConstructionKt");
}

/// A super-constructor argument is constructor code: `a` is read from the parameter there and
/// gets no field.
#[test]
fn a_super_argument_reads_the_parameter() {
    let source = "\
fun superArgument(a: Int): Int {
    open class Base(val y: Int)
    class Q : Base(a)
    return Q().y
}
";
    assert_same_class(
        "ConstructionSuperArgument",
        source,
        "ConstructionSuperArgumentKt$superArgument$Q",
    );
}

/// A data class's `copy` constructs it again from its storage, so a capture only its initializer
/// reads keeps its field: the copy reads it back. (Its class is not compared with kotlinc: krusty
/// does not yet write a local data class's debug tables and metadata, or its source `toString`
/// name.)
#[test]
fn a_data_class_keeps_its_construction_only_captures() {
    let source = "\
fun dataClass(a: Int): String {
    data class D(val p: Int) {
        val x = a
    }
    val copied = D(1).copy(p = 2)
    return \"${copied.p}:${copied.x}\"
}

fun box(): String = if (dataClass(7) == \"2:7\") \"OK\" else dataClass(7)
";
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "DataConstruction"),
        "OK"
    );
}

/// Require every member of `class`, with its code and debug tables, to be kotlinc's except for the
/// one `$default` mask constant `mask` names: kotlinc numbers a local class's captures in its
/// constructor's default mask, and krusty does not yet (`(kotlinc's line, krusty's line)`).
/// Returns krusty's field declarations.
fn assert_same_members_but_mask(
    stem: &str,
    source: &str,
    class: &str,
    mask: (&str, &str),
) -> Vec<String> {
    let comparison = common::compare_with_kotlinc_plugin(
        stem,
        source,
        class,
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("the reference kotlinc and javap are provisioned");
    let mut reference = common::member_blocks(&comparison.reference);
    let krusty = common::member_blocks(&comparison.krusty);
    let masks = reference
        .iter()
        .filter(|line| line.as_str() == mask.0)
        .count();
    assert_eq!(masks, 1, "{class}: kotlinc's mask `{}` occurs once", mask.0);
    for line in &mut reference {
        if line.as_str() == mask.0 {
            *line = mask.1.to_string();
        }
    }
    assert_eq!(krusty, reference, "{class}: kotlinc's members");
    krusty
        .into_iter()
        .filter(|line| {
            line.ends_with(';')
                && !line.contains('(')
                && !line.starts_with(|first: char| first.is_ascii_digit())
        })
        .collect()
}

/// A capture only a primary-constructor default reads is read in the `$default` constructor, from
/// the capture parameter in slot 1, and gets no field.
#[test]
fn a_capture_read_only_by_a_primary_constructor_default_gets_no_field() {
    let source = "\
fun primaryDefault(a: Int): Int {
    class P(val p: Int = a)
    return P().p * 10 + P(1).p
}

fun box(): String = primaryDefault(3).toString()
";
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "PrimaryDefaultCapture"),
        "31"
    );
    let class = "PrimaryDefaultCaptureKt$primaryDefault$P";
    // `$default`: `if (mask & bit) p = $a` is `iload_3; iconst_N; iand; ifeq 8; iload_1; istore_2`.
    let fields = assert_same_members_but_mask(
        "PrimaryDefaultCapture",
        source,
        class,
        ("1: iconst_2", "1: iconst_1"),
    );
    assert_eq!(fields, ["private final int p;"]);
}

/// A capture only a secondary constructor's parameter default reads is read in that constructor's
/// `$default` overload, from the capture parameter in slot 1, and gets no field.
#[test]
fn a_capture_read_only_by_a_secondary_constructor_default_gets_no_field() {
    let source = "\
fun secondaryDefault(a: Int): Int {
    class S(val x: Int) {
        constructor(y: String, z: Int = a) : this(y.length + z)
    }
    return S(\"s\").x * 10 + S(\"s\", 5).x
}

fun box(): String = secondaryDefault(4).toString()
";
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SecondaryDefaultCapture"),
        "56"
    );
    let class = "SecondaryDefaultCaptureKt$secondaryDefault$S";
    // `$default`: `if (mask & bit) z = $a` is `iload 4; iconst_N; iand; ifeq 9; iload_1; istore_3`.
    let fields = assert_same_members_but_mask(
        "SecondaryDefaultCapture",
        source,
        class,
        ("2: iconst_4", "2: iconst_2"),
    );
    assert_eq!(fields, ["private final int x;"]);
}

/// A capture only a computed `this(…)` or `super(…)` delegation argument reads is read from the
/// secondary constructor's own capture parameter, and gets no field; `this(…)` also forwards it.
#[test]
fn a_capture_read_only_by_a_delegation_argument_gets_no_field() {
    let source = "\
fun thisArgument(a: Int): Int {
    class T(val x: Int) {
        constructor() : this(a + 1)
    }
    return T().x
}

open class Base(val y: Int)

fun superArgument(a: Int): Int {
    class U : Base {
        constructor() : super(a * 2)
    }
    return U().y
}

fun box(): String = \"${thisArgument(5)}:${superArgument(6)}\"
";
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "DelegationCapture"),
        "6:12"
    );
    assert_same_class(
        "DelegationCapture",
        source,
        "DelegationCaptureKt$thisArgument$T",
    );
    assert_same_class(
        "DelegationCapture",
        source,
        "DelegationCaptureKt$superArgument$U",
    );
}

/// A property delegate expression runs in the constructor: a capture only it reads comes from the
/// parameter, and only the delegate itself is stored.
#[test]
fn a_capture_read_only_by_a_property_delegate_gets_no_field() {
    let source = "\
class Box(val v: Int) {
    operator fun getValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>): Int = v
}

fun boxOf(a: Int) = Box(a)

fun delegated(a: Int): Int {
    class D {
        val x by boxOf(a)
    }
    return D().x
}

fun box(): String = delegated(7).toString()
";
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "DelegateCapture"),
        "7"
    );
    assert_same_class("DelegateCapture", source, "DelegateCaptureKt$delegated$D");
}
