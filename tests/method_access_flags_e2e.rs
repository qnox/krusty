//! Method access flags kotlinc derives from a declaration's shape. `ACC_VARARGS` marks a method
//! whose LAST physical parameter is its declared `vararg`, so Java can pass it in element form.
//! `ACC_FINAL` follows the member's own modality, not its class's.

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

fn assert_method_flags_match_kotlinc(name: &str, src: &str, classes: &[&str]) {
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
        assert_eq!(
            method_flags(emitted),
            method_flags(&reference),
            "{class}: method access flags"
        );
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
