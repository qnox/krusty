//! `StringBuilder.append(CharSequence?)` follows each Kotlin/Wasm target's own stdlib.
//!
//! PLATFORM-SPECIFIC: the two Wasm stdlibs append a sequence by different protocols, which a
//! custom `CharSequence` observes. The pinned Kotlin/Wasm JS `StringBuilder`
//! (`libraries/stdlib/wasm/js/src/kotlin/text/StringBuilder.kt`) appends `value.toString()`, and
//! its `StringBuilder(content: CharSequence)` renders `content` the same way. The pinned
//! Kotlin/Wasm WASI one (`libraries/stdlib/native-wasm/wasi/kotlin/text/StringBuilder.kt`) reads
//! `value.length` in `append`, again in `appendRange`'s bounds check, and then each `value[index]`.
//! Its constructor first sizes the builder by `content.length`. One shared program cannot assert
//! both, so this owning test pins each target's counts of `length`, `get` and `toString` calls,
//! through a direct `StringBuilder.append`, an `Appendable.append`, `joinToString` and the
//! constructor.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty_wasm::WasmTarget;

const PROGRAM: &str = r#"// FILE: wasmCharSequenceAppends.kt
class Counted(private val text: String) : CharSequence {
    private var lengths = 0
    private var gets = 0
    private var renders = 0
    override val length: Int
        get() {
            lengths++
            return text.length
        }
    override fun get(index: Int): Char {
        gets++
        return text[index]
    }
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence {
        val range = StringBuilder()
        for (index in startIndex until endIndex) range.append(text[index])
        return range
    }
    override fun toString(): String {
        renders++
        return text
    }
    fun reads(): String = "" + lengths + "/" + gets + "/" + renders
}

fun through(out: Appendable, value: CharSequence?): Appendable = out.append(value)

fun box(): String {
    val direct = Counted("OK")
    val first = StringBuilder()
    first.append(direct)
    if (first.toString() != "OK") return "FAIL direct: $first"
    if (direct.reads() != "APPEND") return "FAIL direct reads " + direct.reads()

    val appended = Counted("OK")
    val second = StringBuilder()
    through(second, appended)
    if (second.toString() != "OK") return "FAIL Appendable: $second"
    if (appended.reads() != "APPEND") return "FAIL Appendable reads " + appended.reads()

    val joined = Counted("OK")
    val text = arrayOf<CharSequence>(joined).joinToString("")
    if (text != "OK") return "FAIL joined: $text"
    if (joined.reads() != "APPEND") return "FAIL joined reads " + joined.reads()

    val content = Counted("OK")
    val third = StringBuilder(content)
    if (third.toString() != "OK") return "FAIL constructed: $third"
    if (content.reads() != "CONSTRUCT") return "FAIL constructed reads " + content.reads()

    val nothing: CharSequence? = null
    val fourth = StringBuilder()
    through(fourth, nothing)
    fourth.append(nothing)
    if (fourth.toString() != "nullnull") return "FAIL null: $fourth"
    return "OK"
}
"#;

/// The `length`/`get`/`toString` calls one append of `"OK"` makes, then the constructor's.
fn expected_reads(target: WasmTarget) -> (&'static str, &'static str) {
    match target {
        WasmTarget::Js => ("0/0/1", "0/0/1"),
        WasmTarget::Wasi => ("2/2/0", "3/2/0"),
    }
}

#[test]
fn a_custom_char_sequence_is_appended_by_each_targets_own_protocol() {
    let stem = "wasmCharSequenceAppends";
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "{stem} requires its {} lane",
            target.name()
        );
        let (append, construct) = expected_reads(target);
        let source = PROGRAM
            .replace("\"APPEND\"", &format!("\"{append}\""))
            .replace("\"CONSTRUCT\"", &format!("\"{construct}\""));
        let (units, _) = krusty::conformance::split_files(&source)
            .expect("the append program names its one file");
        assert_eq!(
            lane.compile_and_run(&std::env::temp_dir(), stem, &units, &source),
            Outcome::Pass,
            "{stem} on {}",
            target.name()
        );
    }
}
