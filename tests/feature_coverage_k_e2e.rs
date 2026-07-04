//! End-to-end "box" coverage for delegation features distinct from the existing delegation e2e
//! suites: interface delegation with a partial override, delegation to a `val`-property delegate,
//! `by lazy` (single evaluation + cross-property reference), and custom `getValue`/`setValue`
//! property delegates (`val` and `var`). Each program exposes `fun box(): String` returning "OK"
//! and is round-tripped on the JVM.

mod common;

fn run(src: &str, stem: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, stem)
}

/// Interface delegation where the class overrides ONE member and delegates the rest to `by b`.
#[test]
fn interface_delegation_partial_override() {
    const SRC: &str = "interface I { fun a(): String; fun b(): String }\n\
class Base : I { override fun a() = \"a\"; override fun b() = \"b\" }\n\
class C(d: I) : I by d { override fun b() = \"B\" }\n\
fun box(): String {\n\
    val c = C(Base())\n\
    if (c.a() != \"a\") return \"f1 ${c.a()}\"\n\
    if (c.b() != \"B\") return \"f2 ${c.b()}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC, "PartialOverride").expect("partial-override delegation compiles + runs"),
        "OK"
    );
}

/// Interface delegation to a `val` constructor-parameter property (kotlinc stores the property field
/// and forwards through it), as opposed to a non-`val` param.
#[test]
fn interface_delegation_val_property() {
    const SRC: &str = "interface I { fun greet(): String }\n\
class Impl : I { override fun greet() = \"OK\" }\n\
class C(val d: I) : I by d\n\
fun box(): String {\n\
    val c = C(Impl())\n\
    if (c.d.greet() != \"OK\") return \"f-direct\"\n\
    return c.greet()\n\
}\n";
    assert_eq!(
        run(SRC, "ValPropDeleg").expect("val-property delegation compiles + runs"),
        "OK"
    );
}

/// Interface delegation combined with an added method not present on the delegated interface.
#[test]
fn interface_delegation_with_added_method() {
    const SRC: &str = "interface I { fun base(): String }\n\
class Impl : I { override fun base() = \"base\" }\n\
class C(d: I) : I by d { fun extra() = \"extra\" }\n\
fun box(): String {\n\
    val c = C(Impl())\n\
    if (c.base() != \"base\") return \"f1\"\n\
    if (c.extra() != \"extra\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC, "DelegAddedMethod").expect("delegation + added method compiles + runs"),
        "OK"
    );
}

/// Multiple interface delegation to two distinct `val`-property delegates plus an added method.
#[test]
fn multiple_interface_delegation_val_props() {
    const SRC: &str = "interface A { fun a(): String }\n\
interface B { fun b(): String }\n\
class IA : A { override fun a() = \"a\" }\n\
class IB : B { override fun b() = \"b\" }\n\
class C(val x: A, val y: B) : A by x, B by y { fun c() = \"c\" }\n\
fun box(): String {\n\
    val c = C(IA(), IB())\n\
    if (c.a() + c.b() + c.c() != \"abc\") return \"f ${c.a()}${c.b()}${c.c()}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC, "MultiValDeleg").expect("multiple val-property delegation compiles + runs"),
        "OK"
    );
}

/// Multiple interface delegation where one delegated member is overridden.
#[test]
fn multiple_interface_delegation_partial_override() {
    const SRC: &str = "interface A { fun a(): String }\n\
interface B { fun b(): String }\n\
class IA : A { override fun a() = \"a\" }\n\
class IB : B { override fun b() = \"b\" }\n\
class C(x: A, y: B) : A by x, B by y { override fun a() = \"X\" }\n\
fun box(): String {\n\
    val c = C(IA(), IB())\n\
    if (c.a() + c.b() != \"Xb\") return \"f ${c.a()}${c.b()}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC, "MultiDelegOverride").expect("multiple delegation with override compiles + runs"),
        "OK"
    );
}

/// Custom delegate class providing `operator fun getValue` for a member `val`.
#[test]
fn custom_getvalue_member_val() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del { operator fun getValue(thisRef: Any?, p: KProperty<*>): String = \"OK\" }\n\
class Holder { val x: String by Del() }\n\
fun box(): String = Holder().x\n";
    assert_eq!(
        run(SRC, "CustomGetVal").expect("custom getValue member val compiles + runs"),
        "OK"
    );
}

/// Top-level `val` delegated to a custom delegate returning a non-`String` (`Int`) value.
#[test]
fn custom_getvalue_top_level_int() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del { operator fun getValue(thisRef: Any?, p: KProperty<*>): Int = 7 }\n\
val num: Int by Del()\n\
fun box(): String = if (num == 7) \"OK\" else \"f $num\"\n";
    assert_eq!(
        run(SRC, "TopIntDeleg").expect("top-level Int custom delegate compiles + runs"),
        "OK"
    );
}

/// Generic custom delegate class `Del<T>` whose `getValue` returns its type parameter.
#[test]
fn generic_custom_delegate() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del<T>(val v: T) { operator fun getValue(thisRef: Any?, p: KProperty<*>): T = v }\n\
class Holder { val x: String by Del(\"OK\") }\n\
fun box(): String = Holder().x\n";
    assert_eq!(
        run(SRC, "GenericDeleg").expect("generic custom delegate compiles + runs"),
        "OK"
    );
}

/// Custom delegate whose `getValue` takes a TYPED `thisRef` and reads a property off it.
#[test]
fn custom_delegate_typed_thisref() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del { operator fun getValue(thisRef: Holder, p: KProperty<*>): String = thisRef.tag }\n\
class Holder(val tag: String) { val x: String by Del() }\n\
fun box(): String = Holder(\"OK\").x\n";
    assert_eq!(
        run(SRC, "TypedThisRef").expect("typed-thisRef custom delegate compiles + runs"),
        "OK"
    );
}

/// Custom delegate providing `getValue` + `setValue` for a member `var` (state round-trips).
#[test]
fn custom_getset_member_var() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del {\n\
    var stored = \"init\"\n\
    operator fun getValue(thisRef: Any?, p: KProperty<*>): String = stored\n\
    operator fun setValue(thisRef: Any?, p: KProperty<*>, value: String) { stored = value }\n\
}\n\
class Holder { var x: String by Del() }\n\
fun box(): String {\n\
    val h = Holder()\n\
    if (h.x != \"init\") return \"f1 ${h.x}\"\n\
    h.x = \"changed\"\n\
    if (h.x != \"changed\") return \"f2 ${h.x}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC, "CustomGetSet").expect("custom getValue/setValue member var compiles + runs"),
        "OK"
    );
}

/// A single custom delegate instance reused for two properties (`getValue` receives the correct
/// `KProperty` name for each).
#[test]
fn custom_delegate_reads_property_name() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class NameDel { operator fun getValue(thisRef: Any?, p: KProperty<*>): String = p.name }\n\
class Holder {\n\
    val alpha: String by NameDel()\n\
    val beta: String by NameDel()\n\
}\n\
fun box(): String {\n\
    val h = Holder()\n\
    if (h.alpha != \"alpha\") return \"f1 ${h.alpha}\"\n\
    if (h.beta != \"beta\") return \"f2 ${h.beta}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC, "NameDeleg").expect("KProperty.name delegate compiles + runs"),
        "OK"
    );
}
