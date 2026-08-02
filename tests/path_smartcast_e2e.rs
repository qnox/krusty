//! Path-based flow narrowing (smart casts): `==`/`!=` null checks, `is`/`!is` type tests, and
//! contract conclusions apply not only to plain bindings but to STABLE ACCESS PATHS — `this.p`,
//! `a.p`, `a.b.c`, `a?.p` — when every step is immutable (a local `val`/parameter root and `val`
//! properties whose getters cannot be replaced at runtime, per kotlinc's stability rules). A `var`
//! property or a custom getter is never narrowed. A default `val` remains final on an open class;
//! an explicitly open property needs a statically final receiver type. Round-tripped on the JVM
//! under the shared runner.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_ok(src: &str) {
    let stdlib = common::stdlib_jar().expect("stdlib jar");
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(src, &[stdlib], jdk.as_deref());
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(run(src), Some("OK".to_string()));
}

/// Rejection tests must prove that the checker reached the intended assignment/member read. Merely
/// asserting "some diagnostic" would also pass for malformed Kotlin and could hide a parser-only
/// failure in a fixture that never exercised path stability.
fn assert_type_mismatch(src: &str, context: &str) {
    let diagnostics = common::front_end_diagnostics(src, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("type mismatch")),
        "{context}; expected a type mismatch, got {diagnostics:?}"
    );
}

#[test]
fn not_null_check_narrows_val_property() {
    const SRC: &str = "data class Box(val label: String?)\n\
fun box(): String {\n\
    val b = Box(\"hi\")\n\
    if (b.label != null) {\n\
        val s: String = b.label\n\
        if (s.length != 2) return \"FAIL len\"\n\
    } else {\n\
        return \"FAIL else\"\n\
    }\n\
    val empty = Box(null)\n\
    if (empty.label != null) return \"FAIL null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn null_check_narrows_val_property_in_else_branch() {
    const SRC: &str = "data class Box(val label: String?)\n\
fun len(b: Box): Int {\n\
    if (b.label == null) return -1\n\
    val s: String = b.label\n\
    return s.length\n\
}\n\
fun box(): String {\n\
    if (len(Box(null)) != -1) return \"FAIL null\"\n\
    if (len(Box(\"abc\")) != 3) return \"FAIL abc\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn is_check_narrows_val_property_type() {
    // `h.value.length` is a DIRECT member read on the `Any`-typed property: it only resolves
    // when the `is` proof narrowed the read to `String` (krusty would not resolve `.length`
    // on `Any`).
    const SRC: &str = "class Holder(val value: Any)\n\
fun box(): String {\n\
    val h = Holder(\"krusty\")\n\
    if (h.value is String) {\n\
        if (h.value.length != 6) return \"FAIL len\"\n\
    } else {\n\
        return \"FAIL else\"\n\
    }\n\
    val n = Holder(42)\n\
    if (n.value is String) return \"FAIL is\"\n\
    if (n.value !is Int) return \"FAIL !is\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn is_check_narrows_nullable_primitive_property() {
    const SRC: &str = "data class Num(val n: Int?)\n\
fun box(): String {\n\
    val a = Num(20)\n\
    if (a.n != null) {\n\
        val i: Int = a.n\n\
        if (i + 1 != 21) return \"FAIL arith\"\n\
    } else {\n\
        return \"FAIL else\"\n\
    }\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn safe_call_not_null_narrows_receiver_and_path() {
    const SRC: &str = "data class Inner(val s: String)\n\
data class Outer(val inner: Inner?)\n\
fun box(): String {\n\
    val o = Outer(Inner(\"ok\"))\n\
    if (o.inner?.s != null) {\n\
        val i: Inner = o.inner\n\
        val s: String = o.inner.s\n\
        if (i.s.length + s.length != 4) return \"FAIL len\"\n\
    } else {\n\
        return \"FAIL else\"\n\
    }\n\
    val none = Outer(null)\n\
    if (none.inner?.s != null) return \"FAIL null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn safe_call_nullable_primitive_property_narrows_path_and_receiver() {
    // The safe-call expression is physically a boxed `Int?`, while the proof narrows its stable
    // property path to primitive `Int`. This covers the generic read-time coercion as well as the
    // prefix fact that a non-null safe-call result proves the nullable root receiver non-null.
    const SRC: &str = "data class Count(val n: Int?)\n\
fun read(count: Count?): Int {\n\
    if (count?.n != null) {\n\
        val receiver: Count = count\n\
        val n: Int = count?.n\n\
        if (receiver.n == null) return -2\n\
        return n + n\n\
    }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (read(Count(2)) != 4) return \"FAIL value\"\n\
    if (read(Count(null)) != -1) return \"FAIL property null\"\n\
    if (read(null) != -1) return \"FAIL receiver null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn nested_property_chain_narrows() {
    const SRC: &str = "data class C(val s: String?)\n\
data class B(val c: C)\n\
data class A(val b: B)\n\
fun box(): String {\n\
    val a = A(B(C(\"deep\")))\n\
    if (a.b.c.s != null) {\n\
        val s: String = a.b.c.s\n\
        if (s.length != 4) return \"FAIL len\"\n\
    } else {\n\
        return \"FAIL else\"\n\
    }\n\
    if (A(B(C(null))).b.c.s != null) return \"FAIL null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn this_property_narrows_in_member() {
    const SRC: &str = "class Repo(val name: String?) {\n\
    fun nameLength(): Int {\n\
        if (this.name != null) {\n\
            val s: String = this.name\n\
            return s.length\n\
        }\n\
        return -1\n\
    }\n\
    fun unqualified(): Int {\n\
        if (name != null) return name.length\n\
        return -1\n\
    }\n\
}\n\
fun box(): String {\n\
    if (Repo(\"abcd\").nameLength() != 4) return \"FAIL this\"\n\
    if (Repo(null).nameLength() != -1) return \"FAIL this null\"\n\
    if (Repo(\"xy\").unqualified() != 2) return \"FAIL unqualified\"\n\
    if (Repo(null).unqualified() != -1) return \"FAIL unqualified null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn early_return_guard_narrows_property_for_rest_of_block() {
    const SRC: &str = "data class Box(val label: String?)\n\
class Holder(val value: Any)\n\
fun len(b: Box): Int {\n\
    if (b.label == null) return -1\n\
    val s: String = b.label\n\
    return s.length\n\
}\n\
fun strLen(h: Holder): Int {\n\
    if (h.value !is String) return -1\n\
    val s: String = h.value\n\
    return s.length\n\
}\n\
fun box(): String {\n\
    if (len(Box(null)) != -1) return \"FAIL null\"\n\
    if (len(Box(\"abcde\")) != 5) return \"FAIL len\"\n\
    if (strLen(Holder(7)) != -1) return \"FAIL is\"\n\
    if (strLen(Holder(\"xy\")) != 2) return \"FAIL strlen\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn and_condition_rhs_sees_property_narrowing() {
    const SRC: &str = "data class Box(val label: String?)\n\
fun box(): String {\n\
    val b = Box(\"hey\")\n\
    if (b.label != null && b.label.length == 3) {\n\
        return \"OK\"\n\
    }\n\
    return \"FAIL\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn while_condition_narrows_property_in_body() {
    const SRC: &str = "class Acc(val s: String?)\n\
fun box(): String {\n\
    val a = Acc(\"ab\")\n\
    var i = 0\n\
    while (a.s != null && i < 10) {\n\
        val s: String = a.s\n\
        i += s.length\n\
    }\n\
    return if (i == 10) \"OK\" else \"FAIL $i\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn when_arm_is_narrows_property_subject() {
    // The arm body reads a String member DIRECTLY on the property — only resolvable when the
    // `is String` arm narrowed the read.
    const SRC: &str = "class Holder(val value: Any)\n\
fun describe(h: Holder): String {\n\
    return when (h.value) {\n\
        is String -> \"str:${h.value.length}\"\n\
        is Int -> \"int\"\n\
        else -> \"other\"\n\
    }\n\
}\n\
fun box(): String {\n\
    if (describe(Holder(\"s\")) != \"str:1\") return \"FAIL string\"\n\
    if (describe(Holder(1)) != \"int\") return \"FAIL int\"\n\
    if (describe(Holder(1.5)) != \"other\") return \"FAIL other\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn when_value_class_property_narrowing_stays_rejected() {
    // A value class selected from an `Any` property needs representation-specific unboxing that the
    // generic member-read coercion does not yet provide. `if` already rejects this narrowing through
    // the shared support gate; `when` must do the same instead of accepting a verifier-risking path.
    const SRC: &str = "@JvmInline value class Token(val raw: String)\n\
class Holder(val value: Any)\n\
fun box(): String {\n\
    val holder = Holder(Token(\"OK\"))\n\
    return when (holder.value) {\n\
        is Token -> holder.value.raw\n\
        else -> \"FAIL\"\n\
    }\n\
}\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unresolved reference 'raw'")),
        "unsupported value-class property narrowing must stay a checked skip; got {diagnostics:?}"
    );
}

#[test]
fn contract_condition_narrows_property_in_else_branch() {
    // `isNullOrBlank` carries `returns(false) implies (this != null)`: reaching the `else`
    // proves the RECEIVER path `a.p` non-null (kotlinc smart-casts it there).
    const SRC: &str = "data class A(val p: String?)\n\
fun box(): String {\n\
    val a = A(\"x\")\n\
    if (a.p.isNullOrBlank()) {\n\
        return \"FAIL blank\"\n\
    } else {\n\
        val s: String = a.p\n\
        if (s.length != 1) return \"FAIL len\"\n\
    }\n\
    if (!A(null).p.isNullOrBlank()) return \"FAIL null\"\n\
    if (!A(\"  \").p.isNullOrBlank()) return \"FAIL blank2\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn contract_statement_narrows_property_for_rest_of_block() {
    const SRC: &str = "data class Box(val label: String?)\n\
fun box(): String {\n\
    val b = Box(\"req\")\n\
    require(b.label != null)\n\
    val s: String = b.label\n\
    if (s.length != 3) return \"FAIL len\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn var_property_is_not_narrowed() {
    // kotlinc: a `var` can change between the check and the use — no smart cast.
    const SRC: &str = "class Box(var label: String?)\n\
fun box(): String {\n\
    val b = Box(\"hi\")\n\
    if (b.label != null) {\n\
        val s: String = b.label\n\
        return s\n\
    }\n\
    return \"none\"\n\
}\n";
    assert_type_mismatch(
        SRC,
        "a var property must not smart-cast (assigning String? to String must fail)",
    );
}

#[test]
fn custom_getter_property_is_not_narrowed() {
    // kotlinc: a custom getter can return a different value on every call — no smart cast.
    const SRC: &str = "class Box(val raw: String?) {\n\
    val label: String?\n\
        get() = raw\n\
}\n\
fun box(): String {\n\
    val b = Box(\"hi\")\n\
    if (b.label != null) {\n\
        val s: String = b.label\n\
        return s\n\
    }\n\
    return \"none\"\n\
}\n";
    assert_type_mismatch(
        SRC,
        "a custom-getter property must not smart-cast (assigning String? to String must fail)",
    );

    // The unqualified spelling is the same dispatch-property read, not a stable local slot. It
    // must pass through the identical custom-getter gate instead of being narrowed by name alone.
    const BARE_SRC: &str = "class Box(val raw: String?) {\n\
    val label: String?\n\
        get() = raw\n\
    fun read(): String {\n\
        if (label != null) {\n\
            val s: String = label\n\
            return s\n\
        }\n\
        return \"none\"\n\
    }\n\
}\n";
    assert_type_mismatch(
        BARE_SRC,
        "a bare custom-getter property must use the same stability gate as this.property",
    );
}

#[test]
fn final_property_on_open_class_is_narrowed() {
    // Kotlin members are final by default even when the containing class is open. Stability follows
    // the selected property, so this default `val` can be smart-cast exactly as kotlinc does.
    const SRC: &str = "open class Box(val label: String?)\n\
fun box(): String {\n\
    val b = Box(\"hi\")\n\
    if (b.label != null) {\n\
        val s: String = b.label\n\
        return if (s == \"hi\") \"OK\" else \"FAIL value\"\n\
    }\n\
    return \"FAIL null\"\n\
}\n";
    assert_ok(SRC);

    // The same final property remains stable through its bare spelling inside the open class.
    const BARE_SRC: &str = "open class Box(val label: String?) {\n\
    fun read(): String {\n\
        if (label != null) {\n\
            val s: String = label\n\
            return s\n\
        }\n\
        return \"none\"\n\
    }\n\
}\n\
fun box(): String = Box(\"OK\").read()\n";
    assert_ok(BARE_SRC);
}

#[test]
fn open_property_on_open_receiver_is_not_narrowed() {
    // An explicitly `open val` can be overridden with a computed getter by a runtime subclass, so
    // re-reading it after the null check is unstable while the receiver's static type remains open.
    const SRC: &str = "open class Box(open val label: String?)\n\
fun box(): String {\n\
    val b: Box = Box(\"hi\")\n\
    if (b.label != null) {\n\
        val s: String = b.label\n\
        return s\n\
    }\n\
    return \"none\"\n\
}\n";
    assert_type_mismatch(
        SRC,
        "an open property on an open receiver must not smart-cast (assigning String? to String must fail)",
    );

    // Inside the declaring class, an unqualified name still dispatches through the open getter and
    // can observe an override on the runtime receiver. It is therefore equally unstable.
    const BARE_SRC: &str = "open class Box(open val label: String?) {\n\
    fun read(): String {\n\
        if (label != null) {\n\
            val s: String = label\n\
            return s\n\
        }\n\
        return \"none\"\n\
    }\n\
}\n";
    assert_type_mismatch(
        BARE_SRC,
        "a bare open property must use the same stability gate as this.property",
    );

    // An override remains open by default in an open class even without a repeated `open` token.
    // The parser records that semantic modality; signature collection must preserve it here.
    const IMPLICITLY_OPEN_OVERRIDE_SRC: &str = "open class Base(open val label: String?)\n\
open class Mid(override val label: String?) : Base(label)\n\
fun read(mid: Mid): String {\n\
    if (mid.label != null) {\n\
        val s: String = mid.label\n\
        return s\n\
    }\n\
    return \"none\"\n\
}\n";
    assert_type_mismatch(
        IMPLICITLY_OPEN_OVERRIDE_SRC,
        "an override in an open class remains overridable unless explicitly final",
    );
}

#[test]
fn open_property_on_final_receiver_is_narrowed() {
    // Both an override and an inherited open property are stable through a final static receiver:
    // the receiver type rules out a runtime subclass with a different getter. Kotlinc therefore
    // permits these smart casts; checking only the property's modifier would be too conservative.
    const SRC: &str = "open class Base(open val label: String?)\n\
class Child(override val label: String?) : Base(label)\n\
class Inherited(label: String?) : Base(label)\n\
fun box(): String {\n\
    val child = Child(\"OK\")\n\
    if (child.label != null) {\n\
        val s: String = child.label\n\
        if (s != \"OK\") return \"FAIL override value\"\n\
    } else {\n\
        return \"FAIL override null\"\n\
    }\n\
    val inherited = Inherited(\"OK\")\n\
    if (inherited.label != null) {\n\
        val s: String = inherited.label\n\
        return s\n\
    }\n\
    return \"FAIL inherited null\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn safe_call_method_result_narrows_root_receiver() {
    // `c?.f() != null`: a non-null safe-call RESULT means the receiver held — kotlinc narrows
    // the root `c` in the branch (and so did krusty before paths; the method call itself is not
    // a property path, so only the root narrows).
    const SRC: &str = "class C(val x: Int) {\n\
    fun f(): String? = \"y\"\n\
}\n\
fun probe(c: C?): Int {\n\
    if (c?.f() != null) {\n\
        val n: C = c\n\
        return n.x\n\
    }\n\
    return -1\n\
}\n\
fun elvis(c: C?): Int {\n\
    val s = c?.f() ?: return -1\n\
    val n: C = c\n\
    return s.length + n.x\n\
}\n\
fun box(): String {\n\
    if (probe(C(1)) != 1) return \"FAIL probe\"\n\
    if (probe(null) != -1) return \"FAIL probe null\"\n\
    if (elvis(C(2)) != 3) return \"FAIL elvis\"\n\
    if (elvis(null) != -1) return \"FAIL elvis null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn redeclared_root_in_same_block_drops_path_narrowing() {
    // The guard narrows the PARAMETER `a`; the later `val a` is a NEW binding in the same block —
    // the narrowing must not transfer to it (its `p` is null here).
    const SRC: &str = "data class A(val p: String?)\n\
fun f(a: A): Int {\n\
    if (a.p == null) return -1\n\
    val a = A(null)\n\
    val s: String = a.p\n\
    return s.length\n\
}\n\
fun box(): String = \"OK\"\n";
    assert_type_mismatch(
        SRC,
        "a redeclared root must not keep the old binding's narrowing (assigning String? to String must fail)",
    );
}

#[test]
fn this_narrowing_does_not_leak_into_receiver_lambda() {
    // The proof is about the ENCLOSING class's `this`; inside a receiver lambda `this` is a
    // different object (whose `p` may be null), so the narrowing must not apply there.
    const SRC: &str = "class B(val p: String?)\n\
class C(val p: String?) {\n\
    fun leak(b: B): String {\n\
        if (this.p != null) {\n\
            val f: B.() -> String = {\n\
                val s: String = this.p\n\
                s\n\
            }\n\
            return b.f()\n\
        }\n\
        return \"none\"\n\
    }\n\
}\n\
fun box(): String = \"OK\"\n";
    assert_type_mismatch(
        SRC,
        "a this-rooted narrowing must not leak into a receiver lambda (assigning String? to String must fail)",
    );
}

#[test]
fn generic_class_property_narrows() {
    // The declared property type is the type parameter `T`; the read site substitutes the
    // receiver's type arguments, so the narrowing must follow the SUBSTITUTED type.
    const SRC: &str = "class Box<T>(val v: T)\n\
fun box(): String {\n\
    val b = Box<String?>(\"gen\")\n\
    if (b.v != null) {\n\
        val s: String = b.v\n\
        if (s.length != 3) return \"FAIL len\"\n\
    } else {\n\
        return \"FAIL else\"\n\
    }\n\
    if (Box<String?>(null).v != null) return \"FAIL null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn inherited_generic_property_narrows_with_applied_owner_type() {
    // The selected property is declared on `Base<T>`, while the receiver is `Child`. Stability
    // checking, ordinary reads, and read-only probes must all view `Child` through its applied
    // `Base<String?>` supertype instead of substituting the child's unrelated type slots.
    const SRC: &str = "open class Base<T>(val value: T)\n\
class Child(value: String?) : Base<String?>(value)\n\
fun box(): String {\n\
    val child = Child(\"OK\")\n\
    if (child.value != null) {\n\
        val s: String = child.value\n\
        return s\n\
    }\n\
    return \"FAIL null\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn this_property_proof_narrows_bare_read_and_vice_versa() {
    // The bare and `this.`-qualified forms of the same member `val` share one narrowing.
    const SRC: &str = "class Repo(val name: String?) {\n\
    fun qualified(): Int {\n\
        if (this.name != null) {\n\
            val s: String = name\n\
            return s.length\n\
        }\n\
        return -1\n\
    }\n\
    fun bare(): Int {\n\
        if (name != null) {\n\
            val s: String = this.name\n\
            return s.length\n\
        }\n\
        return -1\n\
    }\n\
}\n\
fun box(): String {\n\
    if (Repo(\"abc\").qualified() != 3) return \"FAIL qualified\"\n\
    if (Repo(null).qualified() != -1) return \"FAIL qualified null\"\n\
    if (Repo(\"ab\").bare() != 2) return \"FAIL bare\"\n\
    if (Repo(null).bare() != -1) return \"FAIL bare null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}

#[test]
fn this_narrowing_does_not_leak_into_same_type_receiver_lambda() {
    // Same-type receiver swap: inside the lambda `this` is `c`, a DIFFERENT object of the SAME
    // class — the proof about the enclosing `this.p` must not apply.
    const SRC: &str = "class C(val p: String?) {\n\
    fun h(c: C): Int {\n\
        if (this.p != null) {\n\
            val f: C.() -> Int = {\n\
                val s: String = this.p\n\
                s.length\n\
            }\n\
            return c.f()\n\
        }\n\
        return -1\n\
    }\n\
}\n\
fun box(): String = \"OK\"\n";
    assert_type_mismatch(
        SRC,
        "a this-rooted narrowing must not transfer to a same-type receiver lambda (assigning String? to String must fail)",
    );
}

#[test]
fn this_narrowing_survives_after_receiver_lambda_ends() {
    // Once the receiver lambda ends, `this` is the enclosing receiver again — the narrowing must
    // still apply (the receiver swap is scoped, not permanent).
    const SRC: &str = "class C(val p: String?) {\n\
    fun h(c: C): Int {\n\
        if (this.p != null) {\n\
            c.run { 1 }\n\
            val s: String = this.p\n\
            return s.length\n\
        }\n\
        return -1\n\
    }\n\
}\n\
fun box(): String {\n\
    if (C(\"abc\").h(C(null)) != 3) return \"FAIL narrowed\"\n\
    if (C(null).h(C(\"x\")) != -1) return \"FAIL null\"\n\
    return \"OK\"\n\
}\n";
    assert_ok(SRC);
}
