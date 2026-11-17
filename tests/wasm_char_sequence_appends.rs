//! `StringBuilder.append(CharSequence?)` follows each Kotlin/Wasm target's own stdlib.
//!
//! PLATFORM-SPECIFIC: the two Wasm stdlibs append a sequence by different protocols, which a
//! custom `CharSequence` observes. Kotlin/Wasm JS appends `value.toString()`, and its
//! `StringBuilder(content: CharSequence)` renders `content` the same way. Kotlin/Wasm WASI reads
//! `value.length` in `append`, again in `appendRange`'s bounds check, and then each `value[index]`,
//! and its constructor first sizes the builder by `content.length`. One shared program cannot assert
//! both, so this owning test runs the same complete program under the exact Kotlin/Wasm reference
//! compiler for each target and requires the product program to observe what it observes.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty::compilation_target::CompilationTarget;
use krusty_wasm::WasmTarget;

/// Every append path, observed through a sequence that counts its `length`, `get` and `toString`
/// calls, beside the controls a builder already answers: a `null`, a `String` and a
/// `StringBuilder` held as a `CharSequence`, and a builder appended to itself.
const PROGRAM: &str = r#"
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

fun observed(): String {
    val direct = Counted("OK")
    val first = StringBuilder()
    first.append(direct)

    val appended = Counted("OK")
    val second = StringBuilder()
    through(second, appended)

    val joined = Counted("OK")
    val text = arrayOf<CharSequence>(joined).joinToString("")

    val content = Counted("OK")
    val third = StringBuilder(content)

    val nothing: CharSequence? = null
    val string: CharSequence = "S"
    val builder: CharSequence = StringBuilder("B")
    val controls = StringBuilder()
    controls.append(nothing)
    through(controls, nothing)
    controls.append(string)
    through(controls, string)
    controls.append(builder)
    through(controls, builder)
    through(controls, controls)

    return "direct " + first + " " + direct.reads() +
        ", Appendable " + second + " " + appended.reads() +
        ", joined " + text + " " + joined.reads() +
        ", constructed " + third + " " + content.reads() +
        ", controls " + controls
}
"#;

/// `appendRange` of every value form and range the bounds check rejects. WASI reads a custom
/// sequence's `length` and then each `value[index]`; JS reads its `length` and then its `toString`.
const RANGE_PROGRAM: &str = r#"
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

fun rejected(value: CharSequence, start: Int, end: Int): String =
    try {
        StringBuilder("x").appendRange(value, start, end).toString()
    } catch (e: IndexOutOfBoundsException) {
        "IOOBE(" + e.message + ")"
    } catch (e: IllegalArgumentException) {
        "IAE(" + e.message + ")"
    }

fun rejectedChars(value: CharArray, start: Int, end: Int): String =
    try {
        StringBuilder("x").appendRange(value, start, end).toString()
    } catch (e: IndexOutOfBoundsException) {
        "IOOBE(" + e.message + ")"
    } catch (e: IllegalArgumentException) {
        "IAE(" + e.message + ")"
    }

fun observed(): String {
    val counted = Counted("wOKw")
    val builder = StringBuilder("<")
    val returned = builder.appendRange(counted, 1, 3)
    builder.appendRange("sSTRs", 1, 4)
    builder.appendRange(StringBuilder("bBb"), 1, 2)
    builder.appendRange(charArrayOf('c', 'C', 'H', 'c'), 1, 3)
    builder.appendRange("", 0, 0)
    builder.appendRange(counted, 2, 2)
    return builder.toString() + " same " + (returned === builder) + " reads " + counted.reads() +
        ", " + rejected("abc", -1, 2) +
        ", " + rejected("abc", 1, 4) +
        ", " + rejected("abc", 2, 1) +
        ", " + rejected(Counted("abc"), 0, 5) +
        ", " + rejectedChars(charArrayOf('a'), 0, 2) +
        ", " + rejectedChars(charArrayOf('a', 'b'), 2, 1)
}
"#;

#[test]
fn a_custom_char_sequence_is_appended_as_each_reference_target_appends_it() {
    observes_as_each_reference_target("wasmCharSequenceAppends", PROGRAM);
}

#[test]
fn append_range_checks_and_reads_as_each_reference_target_does() {
    observes_as_each_reference_target("wasmAppendRanges", RANGE_PROGRAM);
}

/// Run `program`'s `observed()` under the exact Kotlin/Wasm reference compiler for each target and
/// require the product program to observe the same text.
fn observes_as_each_reference_target(stem: &str, program: &str) {
    for (target, compilation_target) in [
        (WasmTarget::Js, CompilationTarget::WasmJs),
        (WasmTarget::Wasi, CompilationTarget::WasmWasi),
    ] {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "{stem} requires its {} lane",
            target.name()
        );
        let work = std::env::temp_dir().join(format!("{stem}-{}", target.name()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let reference_source = work.join(format!("{stem}.kt"));
        std::fs::write(
            &reference_source,
            format!("{program}\nfun main() {{\n    println(observed())\n}}\n"),
        )
        .unwrap();
        let reference = crate::reference_wasm_program::run_wasm_reference_program(
            compilation_target,
            &work,
            &[reference_source],
        )
        .expect("the exact Kotlin/Wasm reference compiler is provisioned")
        .unwrap_or_else(|error| panic!("{} reference: {error}", target.name()));
        let expected = reference.trim_end();
        let source = format!(
            "// FILE: {stem}.kt\n{program}\nfun box(): String {{\n    val seen = observed()\n    \
             return if (seen == \"{expected}\") \"OK\" else \"FAIL \" + seen\n}}\n"
        );
        let (units, _) = krusty::conformance::split_files(&source)
            .expect("the append program names its one file");
        assert_eq!(
            lane.compile_and_run(&work, stem, &units, &source),
            Outcome::Pass,
            "{stem} on {} against the reference's `{expected}`",
            target.name()
        );
        std::fs::remove_dir_all(work).unwrap();
    }
}
