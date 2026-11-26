//! `StringBuilder.append(CharSequence?)` and its ranged overload follow each Kotlin/Wasm target's own stdlib.
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

/// Run `program`'s `observed()` under each target's reference compiler, then require the product
/// program to observe the same text.
fn matches_each_reference_target(stem: &str, program: &str) {
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

#[test]
fn a_custom_char_sequence_is_appended_as_each_reference_target_appends_it() {
    matches_each_reference_target("wasmCharSequenceAppends", PROGRAM);
}

/// `append(CharSequence?, Int, Int)`: the reads a sequence observes, the order of the copy and the
/// destination it sees, and each invalid range's exception, beside the controls a builder already
/// answers (a `null`, a `String`, and a `StringBuilder` as a `CharSequence`).
const RANGE_PROGRAM: &str = r#"
class Probe(
    private val text: String,
    private val rendered: String = text,
    private val watched: StringBuilder? = null,
    private val failAt: Int = -1,
    private val grows: StringBuilder? = null,
    private val growth: String = "0123456789012345678901234567890123456789",
) : CharSequence {
    private var lengths = 0
    private var gets = 0
    private var renders = 0
    private val seen = StringBuilder()
    override val length: Int
        get() {
            lengths++
            return text.length
        }
    override fun get(index: Int): Char {
        gets++
        if (watched != null) seen.append("" + watched.length + ",")
        if (grows != null) grows.append(growth)
        if (index == failAt) throw IllegalStateException("get")
        return text[index]
    }
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence {
        val range = StringBuilder()
        for (index in startIndex until endIndex) range.append(text[index])
        return range
    }
    override fun toString(): String {
        renders++
        return rendered
    }
    fun reads(): String = "" + lengths + "/" + gets + "/" + renders + "/" + seen
}

fun outcome(target: StringBuilder, block: () -> Unit): String {
    val result = try {
        block()
        "ok"
    } catch (e: IndexOutOfBoundsException) {
        "IndexOutOfBounds"
    } catch (e: IllegalArgumentException) {
        "IllegalArgument"
    } catch (e: IllegalStateException) {
        "IllegalState"
    }
    return result + "[" + target.length + ":" + target + "]"
}

fun observed(): String {
    val direct = Probe("OKay")
    val first = StringBuilder()
    first.append(direct, 0, 2)

    val destination = StringBuilder("<")
    val watching = Probe("abc", watched = destination)
    destination.append(watching, 0, 3)

    val failing = StringBuilder()
    val broken = Probe("xyz", failAt = 1)
    val thrown = outcome(failing) { failing.append(broken, 0, 3) }

    // A full-capacity builder whose sequence's getter grows it: the unit is stored through the
    // backing array taken before the getter, so it does not reach the grown builder.
    val grown = StringBuilder(16)
    grown.append("xxxxxxxxxxxxxx")
    val growing = Probe("abc", grows = grown)
    val grew = outcome(grown) { grown.append(growing, 0, 1) }

    // The same near capacity, over a range of two: the capacity for both units is prepared before
    // the first getter, so neither store is lost to the growth the getters cause.
    val near = StringBuilder(16)
    near.append("xxxxxxxxxxxxxxx")
    val pair = Probe("OK", grows = near, growth = "Z")
    val paired = outcome(near) { near.append(pair, 0, 2) }

    val longer = StringBuilder()
    val wide = Probe("ab", rendered = "abcd")
    val widened = outcome(longer) { longer.append(wide, 0, 3) }

    val shorter = StringBuilder()
    val narrow = Probe("abcd", rendered = "ab")
    val narrowed = outcome(shorter) { shorter.append(narrow, 0, 3) }

    val ranges = StringBuilder()
    val starts = intArrayOf(-1, 0, 2, -1, 3, 1, 0)
    val ends = intArrayOf(1, 9, 1, 9, 2, 1, 3)
    for (pair in 0 until starts.size) {
        val start = starts[pair]
        val end = ends[pair]
        val target = StringBuilder()
        val text: CharSequence = Probe("abc")
        ranges.append("" + start + end + " ")
        ranges.append(outcome(target) { target.append(text, start, end) }).append(" ")
        val plain = StringBuilder()
        val string: CharSequence = "abc"
        ranges.append(outcome(plain) { plain.append(string, start, end) }).append(" ")
    }

    val nulls = StringBuilder()
    val nothing: CharSequence? = null
    nulls.append(nothing, 1, 3)
    val absent = StringBuilder()
    val bad = outcome(absent) { absent.append(nothing, 0, 5) }
    val reversed = StringBuilder()
    val back = outcome(reversed) { reversed.append(nothing, 3, 1) }
    val builder: CharSequence = StringBuilder("BUILD")
    nulls.append(builder, 1, 3)

    return "direct " + first + " " + direct.reads() +
        ", watching " + destination + " " + watching.reads() +
        ", thrown " + thrown + " " + broken.reads() +
        ", grew " + grew + " " + growing.reads() +
        ", paired " + paired + " " + pair.reads() +
        ", longer " + widened + " " + wide.reads() +
        ", shorter " + narrowed + " " + narrow.reads() +
        ", ranges " + ranges +
        ", nulls " + nulls + " " + bad + " " + back
}
"#;

#[test]
fn a_ranged_append_checks_and_copies_as_each_reference_target_does() {
    matches_each_reference_target("wasmCharSequenceRangeAppends", RANGE_PROGRAM);
}
