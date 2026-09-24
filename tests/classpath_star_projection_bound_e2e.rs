//! A star projection of a classpath class whose type parameter is bounded (`Discounted<*>` over
//! `class Discounted<P : Product>`) denotes the same type wherever it is written.
//!
//! The readable upper bound of `*` is the parameter's declared bound (`out Product`). Compact
//! signature resolution read that bound only from classes of the module being compiled, so a
//! declaration header spelled `Discounted<*>` as `Discounted<out Any?>` while the body checker, which
//! reads the class through the classpath provider, spelled it `Discounted<out Product>`. The two
//! "equal" types then disagreed: a smart cast to `Discounted<*>` no longer matched a `Discounted<*>`
//! parameter, so no cast reached the call (a `VerifyError`), and an override taking `Discounted<*>`
//! no longer matched the interface member it overrides, so its bridge was missing
//! (`AbstractMethodError`).
//!
//! A `*` read out of a dependency MEMBER (`holder.frames[0]` from `val frames: List<Frame<*>>`) is
//! the same type too. `@Metadata` records a star without a bound, so the reader's star carries the
//! placeholder `Any?` while a written `Frame<*>` carries `Bound`; containment and equality of star
//! projections never consult that carried bound, so the read star is still the receiver of a
//! `Frame<*>` extension and still fits a `Frame<*>` local, parameter, or invariant argument.
use super::common;

const LIB: &str = "package lib\n\
sealed class Product(val price: Long) {\n\
    class Simple(price: Long) : Product(price)\n\
    class Bundle(val parts: List<Product>) : Product(parts.sumOf { it.price })\n\
}\n\
sealed class Line<out P : Product>(val product: P, val qty: Int) {\n\
    class Standard(product: Product.Simple, qty: Int) : Line<Product.Simple>(product, qty)\n\
    class Discounted<P : Product>(product: P, qty: Int, val percent: Int) : Line<P>(product, qty)\n\
}\n";

const VISITOR: &str = "import lib.Line\n\
import lib.Product\n\
interface Visitor<out R> {\n\
    fun standard(line: Line.Standard): R\n\
    fun discounted(line: Line.Discounted<*>): R\n\
}\n\
fun lines(): List<Line<*>> {\n\
    val simple = Product.Simple(10)\n\
    return listOf(Line.Standard(simple, 1), Line.Discounted(Product.Bundle(listOf(simple)), 2, 5))\n\
}\n";

/// The smart-cast receiver reaches `discounted` through a `checkcast`.
#[test]
fn a_smart_cast_to_a_classpath_star_projection_is_passed_with_its_cast() {
    let main = format!(
        "{VISITOR}\
fun <R> Line<*>.accept(v: Visitor<R>): R = when (this) {{\n\
    is Line.Standard -> v.standard(this)\n\
    is Line.Discounted<*> -> v.discounted(this)\n\
}}\n\
fun box(): String {{\n\
    val counter = object : Visitor<Int> {{\n\
        override fun standard(line: Line.Standard) = line.qty\n\
        override fun discounted(line: Line.Discounted<*>) = line.qty * 2 + line.percent\n\
    }}\n\
    val total = lines().sumOf {{ it.accept(counter) }}\n\
    return if (total == 10) \"OK\" else \"fail: $total\"\n\
}}\n"
    );
    let Some(result) = common::expect_box_run_against("StarBoundSmartCast", LIB, &main) else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    assert_eq!(result, "OK");
}

/// The local anonymous visitor's `discounted` overrides the interface member, so a call through
/// `Visitor<R>` reaches it through the `Object`-returning bridge.
#[test]
fn an_override_taking_a_classpath_star_projection_gets_its_bridge() {
    let main = format!(
        "{VISITOR}\
fun <R> Line<*>.accept(v: Visitor<R>): R = when (this) {{\n\
    is Line.Standard -> v.standard(this)\n\
    is Line.Discounted<*> -> v.discounted(this as Line.Discounted<*>)\n\
}}\n\
fun box(): String {{\n\
    val counter = object : Visitor<Int> {{\n\
        override fun standard(line: Line.Standard) = line.qty\n\
        override fun discounted(line: Line.Discounted<*>) = line.qty * 2 + line.percent\n\
    }}\n\
    val total = lines().sumOf {{ it.accept(counter) }}\n\
    return if (total == 10) \"OK\" else \"fail: $total\"\n\
}}\n"
    );
    let Some(result) = common::expect_box_run_against("StarBoundBridge", LIB, &main) else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    assert_eq!(result, "OK");
}

/// A dependency whose members hand out star projections of bounded classifiers: `Frame<*>` over a
/// covariant `Frame<out T : Bound>` and `Pinned<*>` over an invariant `Pinned<T : Bound>`.
const FRAME_LIB: &str = "package lib\n\
open class Bound(val weight: Int)\n\
class Heavy(weight: Int) : Bound(weight)\n\
open class Frame<out T : Bound>(val payload: T)\n\
class Pinned<T : Bound>(val payload: T)\n\
interface FrameVisitor {\n\
    fun visit(frame: Frame<*>): Int\n\
}\n\
class Holder(val frames: List<Frame<*>>, val slots: MutableList<Frame<*>>, val pinned: List<Pinned<*>>)\n";

/// Every place a `*` read out of a dependency member meets a `*` written in this module: an
/// extension receiver, a declared local, an invariant `MutableList<Frame<*>>` argument, and an
/// override's parameter.
const FRAME_USES: &str = "import lib.*\n\
fun Frame<*>.weight(): Int = payload.weight\n\
fun Pinned<*>.weight(): Int = payload.weight\n\
fun count(frames: MutableList<Frame<*>>): Int = frames.size\n\
class Summer : FrameVisitor {\n\
    override fun visit(frame: Frame<*>): Int = frame.weight()\n\
}\n\
fun first(holder: Holder): Int = holder.frames[0].weight()\n\
fun declared(holder: Holder): Int {\n\
    val frame: Frame<*> = holder.frames[1]\n\
    return frame.weight()\n\
}\n\
fun slots(holder: Holder): Int = count(holder.slots)\n\
fun pinned(holder: Holder): Int = holder.pinned[0].weight()\n\
fun visited(holder: Holder): Int = Summer().visit(holder.frames[0])\n";

/// `holder.frames[0]` is a `Frame<*>` read from a dependency's `@Metadata`, which records the star
/// without a bound. It is the same type as the `Frame<*>` this module writes, whose readable bound
/// is `Bound`, so it is the receiver of `Frame<*>.weight()`, initializes a `Frame<*>` local, and is
/// passed where a `Frame<*>` is expected, exactly as kotlinc accepts it.
#[test]
fn a_star_projection_read_from_a_classpath_member_is_the_written_star_projection() {
    let main = format!(
        "{FRAME_USES}\
fun box(): String {{\n\
    val holder = Holder(\n\
        listOf(Frame(Heavy(3)), Frame(Bound(4))),\n\
        mutableListOf(Frame(Heavy(5)), Frame(Heavy(6))),\n\
        listOf(Pinned(Heavy(7))),\n\
    )\n\
    val total = first(holder) + declared(holder) + slots(holder) + pinned(holder) + visited(holder) +\n\
        holder.frames.sumOf {{ it.weight() }}\n\
    return if (total == 26) \"OK\" else \"fail: $total\"\n\
}}\n"
    );
    let Some(result) = common::expect_box_run_against("StarReadFromMember", FRAME_LIB, &main)
    else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    assert_eq!(result, "OK");
}

/// Against a kotlinc-built dependency, the classes using the member-read star projections are
/// byte-identical to kotlinc's.
#[test]
fn classes_using_a_classpath_member_star_projection_match_kotlinc() {
    let Some(lib) = common::kotlinc_library(FRAME_LIB) else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    let reference = dir.join("ref");
    let source = dir.join("StarMemberUses.kt");
    std::fs::write(&source, FRAME_USES).expect("write the fixture");
    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        "-cp".to_string(),
        lib.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp(
        FRAME_USES,
        "StarMemberUses",
        &[lib, common::stdlib_jar()],
    )
    .expect("krusty compiles the fixture against the kotlinc-built dependency");
    for class in ["StarMemberUsesKt", "Summer"] {
        let expected = std::fs::read(reference.join(format!("{class}.class")))
            .unwrap_or_else(|error| panic!("kotlinc did not emit {class}: {error}"));
        let (_, actual) = classes
            .iter()
            .find(|(name, _)| name == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert!(
            *actual == expected,
            "{class}: krusty's class file differs from kotlinc's"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
