//! The receiver of a compound member assignment `receiver.x op= value`.
//!
//! kotlinc evaluates the receiver once. A read of an immutable binding (a `val`, a parameter, or a
//! value a lambda lifted to a method received as a parameter) and every `this` receiver are read
//! again for the setter instead of being saved; any other receiver, including a `var`, a property,
//! and a value a local class or a suspend lambda captured into a field, is saved in a temporary.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and each
//! class file is compared byte for byte.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

const LOCAL_RECEIVERS: &str = r#"
class Cell(var x: Float)
interface Task { fun run() }
class Holder(val stored: Cell, var replaced: Cell) {
    fun viaVal() { stored.x += 1f }
    fun viaVar() { replaced.x += 1f }
}
fun viaParameter(cell: Cell) { cell.x += 2f }
fun viaLocalVal(): Float { val cell = Cell(1f); cell.x += 2f; return cell.x }
fun viaLocalVar() { var cell = Cell(1f); cell.x += 2f; cell = Cell(2f) }
fun viaAlias(cell: Cell) { val alias = cell; alias.x *= 2f }
fun Cell.viaExtensionThis() { x += 1f; this.x += 1f }
fun viaLambdaCapture() { val cell = Cell(1f); val bump = { cell.x += 1f }; bump() }
fun viaLocalFunction(cell: Cell) { fun bump() { cell.x += 9f }; bump() }
fun viaObjectCapture(cell: Cell) {
    val task = object : Task { override fun run() { cell.x += 6f } }
    task.run()
}
"#;

#[test]
fn an_immutable_binding_receiver_is_read_again() {
    for class in [
        "LocalReceiversKt",
        "Holder",
        "LocalReceiversKt$viaObjectCapture$task$1",
    ] {
        byte_identical("LocalReceivers", LOCAL_RECEIVERS, class);
    }
}

const THIS_RECEIVERS: &str = r#"
interface Task { fun run() }
class Counter(var y: Int) {
    fun own() { this.y += 1 }
    fun inLambda() = { this.y += 2 }
    fun inObject() = object : Task { override fun run() { this@Counter.y += 3 } }
    inner class Inner { fun outer() { this@Counter.y += 4 } }
}
"#;

#[test]
fn a_this_receiver_is_read_again() {
    for class in ["Counter", "Counter$inObject$1", "Counter$Inner"] {
        byte_identical("ThisReceivers", THIS_RECEIVERS, class);
    }
}

const SUSPEND_CAPTURE: &str = r#"
class Cell(var x: Float)
fun capture(cell: Cell): suspend () -> Unit = { cell.x += 7f }
"#;

/// The suspend lambda's class is compared member by member: its constant pool is not yet in
/// kotlinc's order, which is unrelated to the receiver.
#[test]
fn a_suspend_lambda_saves_its_captured_receiver() {
    common::assert_class_code_matches_kotlinc(
        "SuspendCapture",
        SUSPEND_CAPTURE,
        "SuspendCaptureKt$capture$1",
    );
}
