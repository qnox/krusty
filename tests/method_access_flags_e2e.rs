//! Method access flags kotlinc derives from a declaration's shape. `ACC_VARARGS` marks a method
//! whose LAST physical parameter is its declared `vararg`, so Java can pass it in element form.
//! `ACC_FINAL` follows the member's own modality, not its class's. A reifiable function (one with a
//! `reified` type parameter) is `ACC_SYNTHETIC` and carries no nullability annotations.

use super::common;

/// Every placement of a `vararg`: last or not, behind a receiver or captures, on constructors, and
/// ahead of a suspend function's continuation.
const VARARGS: &str = "class Crate(vararg val slots: Int) {\n\
    constructor(label: String, vararg extra: String) : this(label.length)\n\
    fun pick(vararg picks: Int): Int = picks[0]\n\
    fun pickThen(vararg picks: Int, tail: Int): Int = picks[0] + tail\n\
    fun Crate.nested(vararg picks: Int): Int = picks[0]\n\
}\n\
enum class Tier(vararg val marks: Int) { LOW(1), HIGH(2, 3) }\n\
class Shelf(vararg val rows: Int, val depth: Int = 1)\n\
fun first(vararg values: Int): Int = values[0]\n\
fun Crate.stacked(vararg values: Int): Int = values[0]\n\
fun defaulted(vararg values: Int, bias: Int = 2): Int = values[0] + bias\n\
fun leading(bias: Int = 2, vararg values: Int): Int = values[0] + bias\n\
suspend fun waiting(vararg values: Int): Int = values[0]\n\
fun box(): String {\n\
    val base = 3\n\
    fun offset(vararg values: Int): Int = values[0] + base\n\
    val total = Crate(1).pick(1) + first(1) + defaulted(1) + leading(4, 1) + offset(1)\n\
    return if (total == 14) \"OK\" else \"fail: \" + total\n\
}\n";

/// Final, open, abstract, overriding and delegated-override members in open, final, abstract and
/// enum classes.
const MODALITY: &str = "import kotlin.reflect.KProperty\n\
interface Gauge { fun read(): Int; fun scale(): Int = 1 }\n\
open class Meter : Gauge {\n\
    override fun read(): Int = 1\n\
    final override fun scale(): Int = 2\n\
    fun plain(): Int = 3\n\
    open fun tuned(): Int = 4\n\
    private fun hidden(): Int = 5\n\
    val fixed: Int = 6\n\
    open val loose: Int = 7\n\
}\n\
class Dial : Meter() {\n\
    override fun tuned(): Int = 8\n\
    override val loose: Int = 9\n\
    open fun spare(): Int = 10\n\
}\n\
abstract class Probe { abstract fun sample(): Int; fun steady(): Int = 11 }\n\
open class Relay(val inner: Gauge) : Gauge by inner\n\
class Sealed(val inner: Gauge) : Gauge by inner\n\
interface Level { val depth: Int }\n\
class Cell { operator fun getValue(owner: Any?, property: KProperty<*>): Int = 15 }\n\
class Shaft : Level { override val depth: Int by Cell() }\n\
enum class Mode { SLOW { override fun rate(): Int = 12 }; open fun rate(): Int = 13; fun base(): Int = 14 }\n\
fun box(): String {\n\
    val dial = Dial()\n\
    val total = dial.read() + dial.scale() + dial.plain() + dial.tuned() + dial.fixed + dial.loose + dial.spare() + Mode.SLOW.rate() + Shaft().depth\n\
    return if (total == 66) \"OK\" else \"fail: \" + total\n\
}\n";

/// Reifiable functions at every placement: top level, member, companion, extension and private,
/// beside inline and generic functions that are not reifiable.
const REIFIED: &str = "class Holder {\n\
    inline fun <reified T> holds(value: Any): Boolean = value is T\n\
    inline fun <T> plain(value: T, pick: (T) -> Boolean): Boolean = pick(value)\n\
    companion object {\n\
        inline fun <reified T : Any> pick(value: Any, fallback: T): T = value as? T ?: fallback\n\
    }\n\
}\n\
inline fun <reified T> matches(value: Any, label: String): String = if (value is T) label else \"no\"\n\
inline fun <reified T, U> mixed(value: Any, other: U): U = if (value is T) other else other\n\
inline fun <reified T> Any.asOrNull(): T? = this as? T\n\
private inline fun <reified T> hidden(value: Any): Boolean = value is T\n\
fun <T> generic(value: T): T = value\n\
fun box(): String {\n\
    val holder = Holder()\n\
    val ok = holder.holds<String>(\"a\") && Holder.pick(\"b\", \"c\") == \"b\" && matches<String>(\"d\", \"e\") == \"e\" &&\n\
        mixed<String, Int>(\"f\", 1) == 1 && \"g\".asOrNull<String>() == \"g\" &&\n\
        hidden<String>(\"i\") && generic(2) == 2 && holder.plain(3) { it == 3 }\n\
    return if (ok) \"OK\" else \"fail\"\n\
}\n";

#[test]
fn reifiable_functions_run() {
    common::expect_box_ok_with_stdlib(REIFIED, "Reified");
}

#[test]
fn reifiable_functions_are_synthetic_and_unannotated_like_kotlinc() {
    assert_methods_match_kotlinc(
        "Reified",
        REIFIED,
        &["Holder", "Holder$Companion", "ReifiedKt"],
        method_shape,
    );
}

#[test]
fn modality_members_run() {
    common::expect_box_ok_with_stdlib(MODALITY, "Modality");
}

#[test]
fn final_flag_follows_member_modality_like_kotlinc() {
    assert_method_flags_match_kotlinc(
        "Modality",
        MODALITY,
        &[
            "Gauge", "Meter", "Dial", "Probe", "Relay", "Sealed", "Shaft",
        ],
    );
}

#[test]
fn vararg_methods_run() {
    common::expect_box_ok_with_stdlib(VARARGS, "Varargs");
}

#[test]
fn vararg_method_flags_match_kotlinc() {
    assert_method_flags_match_kotlinc("Varargs", VARARGS, &["Crate", "Tier", "Shelf", "VarargsKt"]);
}

/// Visibility of overriding members: an override with NO visibility modifier keeps the overridden
/// member's visibility (transitively down an override chain, and the most permissive one when it
/// overrides several members), an explicit modifier wins, and a declared `protected abstract`
/// member stays protected. Covers functions, properties, an enum-entry body, and a constructor
/// property parameter.
const OVERRIDE_VISIBILITY: &str = "abstract class Counter {\n\
    protected abstract fun <T> countTime(block: () -> T): T\n\
    protected abstract fun plain(): Int\n\
    protected open fun openMeth(): Int = 1\n\
}\n\
abstract class Middle : Counter() {\n\
    abstract override fun plain(): Int\n\
}\n\
class Simple : Middle() {\n\
    override fun <T> countTime(block: () -> T): T = block()\n\
    override fun plain(): Int = 2\n\
    override fun openMeth(): Int = 3\n\
    fun callAll(): Int = countTime { 1 } + plain() + openMeth()\n\
}\n\
open class Widened {\n\
    protected open fun keep(): Int = 1\n\
}\n\
class Explicit : Widened() {\n\
    public override fun keep(): Int = 4\n\
}\n\
interface Iface { fun m(): Int }\n\
class Impl : Iface { override fun m(): Int = 6 }\n\
open class BothBase { protected open fun f(): Int = 1 }\n\
interface BothIface { fun f(): Int }\n\
class Both : BothBase(), BothIface { override fun f(): Int = 7 }\n\
open class PropBase {\n\
    protected open val p: Int = 1\n\
    protected open var q: Int = 2\n\
}\n\
class PropDerived : PropBase() {\n\
    override val p: Int = 3\n\
    override var q: Int = 4\n\
    fun sum(): Int = p + q\n\
}\n\
enum class Mode {\n\
    SLOW { override fun rate(): Int = 12 };\n\
    protected abstract fun rate(): Int\n\
    fun base(): Int = rate()\n\
}\n\
open class ParamBase { protected open val x: Int = 1 }\n\
class ParamDerived(override val x: Int) : ParamBase() {\n\
    fun read(): Int = x\n\
}\n\
fun box(): String {\n\
    val total = Simple().callAll() + Explicit().keep() + Impl().m() +\n\
        Both().f() + PropDerived().sum() + Mode.SLOW.base() + ParamDerived(8).read()\n\
    return if (total == 50) \"OK\" else \"fail: \" + total\n\
}\n";

#[test]
fn override_visibility_members_run() {
    common::expect_box_ok_with_stdlib(OVERRIDE_VISIBILITY, "OverrideVisibility");
}

#[test]
fn override_without_modifier_keeps_overridden_visibility_like_kotlinc() {
    assert_method_flags_match_kotlinc(
        "OverrideVisibility",
        OVERRIDE_VISIBILITY,
        &[
            "Counter",
            "Middle",
            "Simple",
            "Widened",
            "Explicit",
            "Iface",
            "Impl",
            "BothBase",
            "BothIface",
            "Both",
            "PropBase",
            "PropDerived",
            "ParamBase",
            "ParamDerived",
        ],
    );
    // The enum and its entry subclass take the same assertion on every declared member row. Their
    // CONSTRUCTOR rows are left out: kotlinc routes entry construction through a synthetic
    // `DefaultConstructorMarker` constructor krusty does not emit, a pre-existing enum-entry shape
    // gap unrelated to member visibility.
    assert_methods_match_kotlinc(
        "OverrideVisibility",
        OVERRIDE_VISIBILITY,
        &["Mode", "Mode$SLOW"],
        |bytes| {
            method_flags(bytes)
                .into_iter()
                .filter(|(name, _, _)| !name.starts_with('<'))
                .collect()
        },
    );
}

/// A Kotlin subclass in the same package as a Java class whose member is package-private: kotlinc
/// normalizes the inherited package-private visibility to `protected`
/// (`JavaVisibilities.PackageVisibility.normalize()`), on the JVM access flags and in `@Metadata`
/// alike.
const JAVA_PACKAGE_PRIVATE_OVERRIDE: &str = "package j\n\
class Derived : Base() {\n\
    override fun plain() {}\n\
}\n";

#[test]
fn java_package_private_override_inherits_protected_like_kotlinc() {
    let Some((java, _)) = common::javac_compile(
        &[(
            "Base.java".to_string(),
            "package j; public class Base { void plain() {} }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let comparison = common::compare_with_kotlinc_plugin_jdk_cp(
        "Derived",
        JAVA_PACKAGE_PRIVATE_OVERRIDE,
        "j/Derived",
        std::slice::from_ref(&java),
        "1.8",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_eq!(
        method_flags(&comparison.krusty_bytes),
        method_flags(&comparison.reference_bytes),
        "j/Derived: methods"
    );
    assert_eq!(
        common::raw_kotlin_metadata(&comparison.krusty_bytes),
        common::raw_kotlin_metadata(&comparison.reference_bytes),
        "j/Derived: @Metadata"
    );
}

/// A protected Java collection parameter is the flexible `(Mutable)X!`. An override written with
/// the mutable face or the read-only face is that member, so a missing visibility modifier inherits
/// `protected` on the JVM access flags and in `@Metadata`. Covers a generic JDK method
/// (`LinkedHashMap.removeEldestEntry`), a generic `List<T>` parameter, its read-only spelling, and
/// a raw `Map.Entry` parameter whose classfile has no `Signature` attribute.
const JAVA_COLLECTION_OVERRIDE: &str = "import java.util.LinkedHashMap\n\
class MutableHost : LinkedHashMap<String, String>() {\n\
    override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, String>?): Boolean = false\n\
}\n\
class ReadOnlyHost : LinkedHashMap<String, String>() {\n\
    override fun removeEldestEntry(eldest: Map.Entry<String, String>?): Boolean = false\n\
}\n";

const JAVA_COLLECTION_FIXTURE_OVERRIDE: &str = "package j\n\
class MutableTake : ListBox<String>() {\n\
    override fun take(values: MutableList<String>?): Boolean = false\n\
}\n\
class ReadOnlyTake : ListBox<String>() {\n\
    override fun take(values: List<String>?): Boolean = false\n\
}\n\
class RawTake : RawBox() {\n\
    override fun take(eldest: MutableMap.MutableEntry<*, *>?): Boolean = false\n\
}\n";

#[test]
fn java_collection_override_inherits_protected_like_kotlinc() {
    assert_flags_and_metadata("MutableHost", JAVA_COLLECTION_OVERRIDE, "MutableHost", &[]);
    assert_flags_and_metadata(
        "ReadOnlyHost",
        JAVA_COLLECTION_OVERRIDE,
        "ReadOnlyHost",
        &[],
    );
    let Some((java, _)) = common::javac_compile(
        &[
            (
                "ListBox.java".to_string(),
                "package j;\nimport java.util.List;\npublic class ListBox<T> {\n    protected boolean take(List<T> values) { return false; }\n}\n".to_string(),
            ),
            (
                "RawBox.java".to_string(),
                "package j;\nimport java.util.Map;\npublic class RawBox {\n    @SuppressWarnings(\"rawtypes\")\n    protected boolean take(Map.Entry eldest) { return false; }\n}\n".to_string(),
            ),
        ],
        &[],
    ) else {
        return;
    };
    let extra = std::slice::from_ref(&java);
    assert_flags_and_metadata(
        "MutableTake",
        JAVA_COLLECTION_FIXTURE_OVERRIDE,
        "j/MutableTake",
        extra,
    );
    assert_flags_and_metadata(
        "ReadOnlyTake",
        JAVA_COLLECTION_FIXTURE_OVERRIDE,
        "j/ReadOnlyTake",
        extra,
    );
    assert_flags_and_metadata(
        "RawTake",
        JAVA_COLLECTION_FIXTURE_OVERRIDE,
        "j/RawTake",
        extra,
    );
}

fn assert_flags_and_metadata(name: &str, src: &str, class: &str, extra: &[std::path::PathBuf]) {
    let comparison =
        common::compare_with_kotlinc_plugin_jdk_cp(name, src, class, extra, "1.8", &[])
            .expect("reference kotlinc and javap are provisioned");
    assert_eq!(
        method_flags(&comparison.krusty_bytes),
        method_flags(&comparison.reference_bytes),
        "{class}: methods"
    );
    assert_eq!(
        common::raw_kotlin_metadata(&comparison.krusty_bytes),
        common::raw_kotlin_metadata(&comparison.reference_bytes),
        "{class}: @Metadata"
    );
}

/// A body-local classifier's override also keeps the overridden member's visibility: the local
/// class's override plan is published when its declaring body is checked, after the members were
/// predeclared, and `finalize_inherited_statuses` re-reads the corrected header once every body
/// has been.
const LOCAL_OVERRIDE_VISIBILITY: &str = "open class Base {\n\
    protected open fun f(): Int = 1\n\
}\n\
fun box(): String {\n\
    class Local : Base() {\n\
        override fun f(): Int = 2\n\
        fun g(): Int = f()\n\
    }\n\
    return if (Local().g() == 2) \"OK\" else \"fail\"\n\
}\n";

#[test]
fn local_override_members_run() {
    common::expect_box_ok_with_stdlib(LOCAL_OVERRIDE_VISIBILITY, "LocalOverrideVisibility");
}

#[test]
fn local_override_keeps_overridden_visibility_like_kotlinc() {
    assert_method_flags_match_kotlinc(
        "LocalOverrideVisibility",
        LOCAL_OVERRIDE_VISIBILITY,
        &[
            "Base",
            "LocalOverrideVisibilityKt",
            "LocalOverrideVisibilityKt$box$Local",
        ],
    );
}

/// `ACC_VARARGS` on every vararg member shape: a declared abstract member, an `abstract override`,
/// the concrete override under it, an interface member, an `open` member with its override, and the
/// jvm-default compatibility surface — a default member's `access$…$jd` bridge and `$DefaultImpls`
/// forwarder, republished too on a sub-interface that inherits the default without redeclaring it.
const ABSTRACT_VARARGS: &str = "abstract class Printer {\n\
    abstract fun println(vararg objects: Any?): Printer\n\
}\n\
abstract class IndentingPrinter : Printer() {\n\
    abstract override fun println(vararg objects: Any?): Printer\n\
}\n\
class ConsolePrinter : IndentingPrinter() {\n\
    var lines: Int = 0\n\
    override fun println(vararg objects: Any?): Printer {\n\
        lines += objects.size\n\
        return this\n\
    }\n\
}\n\
interface Logger {\n\
    fun log(vararg msgs: String): Int\n\
}\n\
interface Greeter {\n\
    fun greet(vararg names: String): Int = names.size\n\
}\n\
interface FriendlyGreeter : Greeter\n\
open class OpenBase {\n\
    open fun write(vararg bytes: Int): Int = bytes.size\n\
}\n\
class OpenDerived : OpenBase() {\n\
    override fun write(vararg bytes: Int): Int = bytes.size + 1\n\
}\n\
fun box(): String {\n\
    val printer = ConsolePrinter()\n\
    printer.println(\"a\", \"b\")\n\
    val logger = object : Logger {\n\
        override fun log(vararg msgs: String): Int = msgs.size\n\
    }\n\
    val greeter = object : Greeter {}\n\
    val friendly = object : FriendlyGreeter {}\n\
    val total = printer.lines + logger.log(\"x\", \"y\", \"z\") + greeter.greet(\"g\") + friendly.greet(\"h\") + OpenDerived().write(1, 2)\n\
    return if (total == 10) \"OK\" else \"fail: \" + total\n\
}\n";

#[test]
fn abstract_vararg_members_run() {
    common::expect_box_ok_with_stdlib(ABSTRACT_VARARGS, "AbstractVarargs");
}

#[test]
fn abstract_vararg_method_flags_match_kotlinc() {
    assert_method_flags_match_kotlinc(
        "AbstractVarargs",
        ABSTRACT_VARARGS,
        &[
            "Printer",
            "IndentingPrinter",
            "ConsolePrinter",
            "Logger",
            "Greeter",
            "Greeter$DefaultImpls",
            "FriendlyGreeter",
            "FriendlyGreeter$DefaultImpls",
            "OpenBase",
            "OpenDerived",
        ],
    );
}

/// An override of an `internal` member keeps `internal` and so stays JVM-public, as kotlinc's
/// does. kotlinc mangles the member's JVM name (`f$main`) and records the mangled name in the
/// `@Metadata` `jvm_signature` extension where krusty keeps the declared name — a pre-existing
/// mangling gap, visible for a declared `internal` member alike — so only each method's descriptor
/// and access flags are compared. The cross-module access rejection is covered by
/// `diagnostics_language_parity_e2e::internal_override_visibility_access_matches_kotlinc_across_modules`.
const INTERNAL_OVERRIDE: &str = "open class Base {\n\
    internal open fun f(): Int = 1\n\
}\n\
class Derived : Base() {\n\
    override fun f(): Int = 2\n\
}\n\
fun box(): String = if (Derived().f() == 2) \"OK\" else \"fail\"\n";

#[test]
fn internal_override_members_run() {
    common::expect_box_ok_with_stdlib(INTERNAL_OVERRIDE, "InternalOverride");
}

#[test]
fn internal_override_stays_jvm_public_like_kotlinc() {
    assert_methods_match_kotlinc(
        "InternalOverride",
        INTERNAL_OVERRIDE,
        &["Base", "Derived", "InternalOverrideKt"],
        |bytes| {
            method_flags(bytes)
                .into_iter()
                .map(|(_, descriptor, flags)| (descriptor, flags))
                .collect()
        },
    );
}

fn assert_method_flags_match_kotlinc(name: &str, src: &str, classes: &[&str]) {
    assert_methods_match_kotlinc(name, src, classes, method_flags);
}

fn assert_methods_match_kotlinc<T: PartialEq + std::fmt::Debug>(
    name: &str,
    src: &str,
    classes: &[&str],
    project: fn(&[u8]) -> Vec<T>,
) {
    let krusty = common::expect_classes_with_stdlib(src, name);
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join(format!("{name}.kt"));
    std::fs::write(&path, src).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    for class in classes {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(project(emitted), project(&reference), "{class}: methods");
    }
}

/// Each method's name, descriptor and access flags, in classfile order.
fn method_flags(bytes: &[u8]) -> Vec<(String, String, u16)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
            )
        })
        .collect()
}

/// Each method's name, descriptor, access flags, generic signature and nullability annotations, in
/// classfile order.
fn method_shape(bytes: &[u8]) -> Vec<String> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            format!(
                "{}{} {:#06x} {:?} returns {:?} parameters {:?}",
                method.name,
                method.descriptor,
                method.access,
                method.signature,
                method.return_nullability,
                method.parameter_nullability,
            )
        })
        .collect()
}
