//! The native runtime, run on the host.
//!
//! The runtime under `src/native/runtime/` is freestanding C that `build.rs` compiles for every
//! target and krusty links into a user's program. Compiling it proves only that it compiles; this
//! harness RUNS it. Each driver under `tests/native_runtime/` is a small freestanding program that
//! supplies `kt_program_entry`, calls into the runtime, and prints `OK` when what it checked held.
//! The harness links the driver with every runtime source on the branch — with the host's clang,
//! `-nostdlib -static`, so nothing but the runtime itself answers its symbols — and runs it.
//!
//! A driver reports failure by exiting non-zero with a message on stderr (`KT_SYS_FAIL`), or by
//! crashing; either fails the test with what it printed. A driver that checks the runtime ENDS the
//! program, as it does on exhausted memory, is instead expected to exit with the runtime's failure
//! status and exactly the runtime's message; anything the driver prints itself fails the test.
//!
//! A driver whose expected answers are Kotlin's prints them as a transcript instead of comparing
//! them with values copied into C, and the harness compares that transcript with the one the Kotlin
//! program beside it (`<driver>.kt`) answers under the reference kotlinc
//! (`run_driver_against_kotlin`). Where the native runtime answers differently from the JVM on
//! purpose, the test declares the line (`run_driver_against_kotlin_with`, `Divergence`).
//!
//! The drivers need a C compiler for the host. CI has one and must run them; a local build without
//! clang is told why they did not run rather than failing on a missing tool.

use super::common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// Whether every function the runtime sources on this branch call is defined by them. The runtime
/// lands in tiers, and a tier below the last one calls functions a later tier defines; those links
/// leave the missing symbols unresolved (a driver that reaches one crashes, it does not pass). The
/// tier that completes the runtime turns this on, and from then on a missing definition fails the
/// link.
const RUNTIME_COMPLETE: bool = false;

/// Warnings a tier below the last one cannot help giving. Such a tier DECLARES the internal
/// functions a later tier defines, and defines helpers only a later tier's code calls; the tier that
/// completes the runtime turns `RUNTIME_COMPLETE` on and with it every one of these back into an
/// error.
const INCOMPLETE_RUNTIME_WARNINGS: &[&str] = &[
    "-Wno-undefined-internal",
    "-Wno-unused-function",
    "-Wno-unused-const-variable",
];

fn runtime_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/runtime")
}

fn driver_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_runtime")
}

/// Whether the host is a target the runtime supports and a clang is there to build for it.
fn host_can_run() -> bool {
    let supported = cfg!(target_os = "linux")
        && cfg!(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ));
    let clang = Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if supported && clang {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI must run the native runtime drivers, but this host cannot (supported target: \
         {supported}, clang: {clang})"
    );
    eprintln!("native runtime drivers skipped: this host has no clang or is not a runtime target");
    false
}

/// Link `driver` with every runtime source and run it. `None` when the host cannot run drivers at
/// all.
fn build_and_run(driver: &str) -> Option<Output> {
    if !host_can_run() {
        return None;
    }
    let mut sources: Vec<PathBuf> = std::fs::read_dir(runtime_dir())
        .expect("read the runtime directory")
        .map(|entry| entry.expect("runtime directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    sources.sort();
    let scratch = common::scratch_dir().expect("scratch directory");
    let executable = scratch.join(driver);
    let build = Command::new("clang")
        .args([
            "-std=c11",
            "-ffreestanding",
            "-nostdlib",
            "-static",
            "-fno-pic",
            "-fno-stack-protector",
            "-fno-asynchronous-unwind-tables",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            // Runtime descriptors name the fields they define and intentionally leave the rest
            // zero-initialized. Keep every other warning an error.
            "-Wno-missing-field-initializers",
            // Signed overflow is undefined in C, and Kotlin's `Int` and `Long` wrap or raise; a
            // driver that drives a runtime counter past its maximum must see an overflow the
            // runtime left signed, so every one traps (SIGILL) instead of wrapping quietly.
            "-fsanitize=signed-integer-overflow",
            "-fsanitize-trap=signed-integer-overflow",
        ])
        .args((!RUNTIME_COMPLETE).then_some("-Wl,--unresolved-symbols=ignore-all"))
        .args(if RUNTIME_COMPLETE {
            &[][..]
        } else {
            INCOMPLETE_RUNTIME_WARNINGS
        })
        .arg("-I")
        .arg(runtime_dir())
        .args(&sources)
        .arg(driver_dir().join(format!("{driver}.c")))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run clang");
    assert!(
        build.status.success(),
        "{driver}: the driver and runtime did not build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    Some(Command::new(&executable).output().expect("run the driver"))
}

/// Run `driver`, which must succeed EXACTLY: exit status 0, stdout exactly `OK\n`, and nothing on
/// stderr. A driver checks what it checks and says `OK` once; any other output means it printed
/// something it should not have, which a looser test would let through.
fn run_driver(driver: &str) {
    let Some(output) = build_and_run(driver) else {
        return;
    };
    assert!(
        output.status.success() && output.stdout == b"OK\n" && output.stderr.is_empty(),
        "{driver}: expected status 0, stdout \"OK\\n\" and an empty stderr, got {}\n\
         stdout (first 200 bytes): {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(200)]),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run `driver`, which writes a payload and then `OK\n`: it must exit 0 with nothing on stderr and
/// stdout ending in `OK\n`. Returns the payload before that `OK\n` for the test to check EXACTLY;
/// only a driver whose output is itself the subject uses this. `None` when the host cannot run
/// drivers at all.
fn run_payload_driver(driver: &str) -> Option<Vec<u8>> {
    let output = build_and_run(driver)?;
    let stdout = &output.stdout;
    assert!(
        output.status.success() && stdout.ends_with(b"OK\n") && output.stderr.is_empty(),
        "{driver}: expected status 0, stdout ending in \"OK\\n\" and an empty stderr, got {}\n\
         stdout (last 200 bytes): {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&stdout[stdout.len().saturating_sub(200)..]),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(stdout[..stdout.len() - b"OK\n".len()].to_vec())
}

/// Run `driver` against Kotlin: the program `tests/native_runtime/<driver>.kt` answers, in its
/// `box()`, the transcript of what the driver observes (one observation per line, each ending in a
/// newline), and the driver prints its own transcript before its `OK` (`transcript.h`). The program
/// is compiled by the reference kotlinc and run on the shared JVM through the persistent harness
/// (`common::kotlinc_box_result`); it must compile and answer whole lines, and the driver must
/// succeed exactly as `run_payload_driver` requires. The two transcripts must then be identical, so
/// every answer the driver prints is Kotlin's by execution, not a value copied into C. The driver's
/// own checks of what Kotlin has no counterpart for still end it on failure.
fn run_driver_against_kotlin(driver: &str) {
    run_driver_against_kotlin_with(driver, &[]);
}

/// Which of the native runtime's rules a line the JVM answers differently follows. The runtime
/// BEHAVES as Kotlin/Native does -- which exception type is thrown, a class's identity and names,
/// what an `is` answers, iteration order, a collection's semantics, the order of the calls it makes
/// into the program -- and SAYS what the JVM says -- an exception's message, a diagnostic's wording
/// -- wherever that is cheap. A JVM message is therefore no divergence; these two are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// The runtime behaves as Kotlin/Native does, and the JVM behaves otherwise.
    NativeBehaviour,
    /// The runtime keeps Kotlin/Native's message, because the JVM's is not cheap to reproduce.
    NativeMessage,
}

/// A line of a driver's transcript on which the native runtime answers differently from the JVM,
/// by one of the runtime's rules: kotlinc's program must answer exactly `jvm` there, and the driver
/// exactly `native`. Each declaration cites, beside it, where the Kotlin/Native answer comes from,
/// since no Kotlin/Native compiler runs here.
#[derive(Clone, Copy, Debug)]
struct Divergence {
    rule: Rule,
    jvm: &'static str,
    native: &'static str,
}

impl Divergence {
    const fn native_behaviour(jvm: &'static str, native: &'static str) -> Self {
        Divergence {
            rule: Rule::NativeBehaviour,
            jvm,
            native,
        }
    }

    const fn native_message(jvm: &'static str, native: &'static str) -> Self {
        Divergence {
            rule: Rule::NativeMessage,
            jvm,
            native,
        }
    }
}

/// `run_driver_against_kotlin`, for a driver some of whose lines the native runtime answers
/// differently from the JVM on purpose. Each such line is declared: kotlinc's program must answer
/// exactly the declared JVM line and the driver exactly the declared native line, at the same place
/// in the two transcripts, so the oracle still checks the JVM's side and the driver's side is
/// pinned. Every other line must be identical; an undeclared difference fails, and so does a
/// declared one that no longer occurs, so a declaration cannot outlive the difference it explains.
fn run_driver_against_kotlin_with(driver: &str, divergences: &[Divergence]) {
    for divergence in divergences {
        assert!(
            divergence.jvm != divergence.native && !divergence.jvm.contains('\n'),
            "{driver}: a divergence must be one line that differs: {divergence:?}"
        );
    }
    let Some(native) = run_payload_driver(driver) else {
        return;
    };
    let program = driver_dir().join(format!("{driver}.kt"));
    let source = fs::read_to_string(&program)
        .unwrap_or_else(|error| panic!("{driver}: read {}: {error}", program.display()));
    let kotlin = common::kotlinc_box_result(&source);
    assert!(
        !kotlin.starts_with("ERROR:") && kotlin.ends_with('\n'),
        "{driver}: the Kotlin program must run and answer whole lines, got {kotlin:?}"
    );
    let native = String::from_utf8(native)
        .unwrap_or_else(|error| panic!("{driver}: the native transcript is not UTF-8: {error}"));
    if let Some(difference) = transcript_difference(&kotlin, &native, divergences) {
        panic!(
            "{driver}: the native transcript differs from Kotlin's: {difference}\n\
             Kotlin:\n{kotlin}native:\n{native}"
        );
    }
}

/// Where the native transcript departs from Kotlin's other than as `divergences` declare, or a
/// declared divergence that occurs on no line; `None` when the two agree line for line.
fn transcript_difference(kotlin: &str, native: &str, divergences: &[Divergence]) -> Option<String> {
    let mut kotlin_lines = kotlin.split_inclusive('\n');
    let mut native_lines = native.split_inclusive('\n');
    let mut occurs = vec![false; divergences.len()];
    for line in 1.. {
        match (kotlin_lines.next(), native_lines.next()) {
            (None, None) => break,
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(expected), Some(actual)) => {
                let declared = divergences.iter().position(|divergence| {
                    expected.strip_suffix('\n') == Some(divergence.jvm)
                        && actual.strip_suffix('\n') == Some(divergence.native)
                });
                match declared {
                    Some(at) => occurs[at] = true,
                    None => {
                        return Some(format!(
                            "line {line}: Kotlin {expected:?}, native {actual:?}, and no \
                             divergence declares it"
                        ))
                    }
                }
            }
            (expected, actual) => {
                return Some(format!(
                    "line {line}: Kotlin {:?}, native {:?}",
                    expected.unwrap_or("<end>"),
                    actual.unwrap_or("<end>")
                ));
            }
        }
    }
    let stale = &divergences[occurs.iter().position(|occurs| !occurs)?];
    Some(format!(
        "the declared {:?} divergence, Kotlin {:?} and native {:?}, occurs on no line",
        stale.rule, stale.jvm, stale.native
    ))
}

#[test]
fn a_transcript_differs_only_where_a_divergence_declares_it() {
    assert_eq!(transcript_difference("a\nb\n", "a\nb\n", &[]), None);
    assert_eq!(
        transcript_difference("a\nb\n", "a\nc\n", &[]).as_deref(),
        Some("line 2: Kotlin \"b\\n\", native \"c\\n\", and no divergence declares it")
    );
    assert_eq!(
        transcript_difference("a\n", "a\nb\n", &[]).as_deref(),
        Some("line 2: Kotlin \"<end>\", native \"b\\n\"")
    );
    assert_eq!(
        transcript_difference("a\n", "a", &[]).as_deref(),
        Some("line 1: Kotlin \"a\\n\", native \"a\", and no divergence declares it")
    );
    let declared = [Divergence::native_behaviour("b", "c")];
    assert_eq!(transcript_difference("a\nb\n", "a\nc\n", &declared), None);
    assert_eq!(
        transcript_difference("a\nb\n", "a\nd\n", &declared).as_deref(),
        Some("line 2: Kotlin \"b\\n\", native \"d\\n\", and no divergence declares it")
    );
    assert_eq!(
        transcript_difference("a\nb\n", "a\nb\n", &declared).as_deref(),
        Some(concat!(
            "the declared NativeBehaviour divergence, Kotlin \"b\" and native \"c\", ",
            "occurs on no line"
        ))
    );
    let message = [Divergence::native_message("x: 1", "x: one")];
    assert_eq!(transcript_difference("x: 1\n", "x: one\n", &message), None);
}

/// Run `driver`, which must end the way the runtime ends a program it cannot continue
/// (`kt_sys_fail`: status 134) with exactly `message` on stderr and nothing on stdout.
fn run_driver_expecting_failure(driver: &str, message: &str) {
    let Some(output) = build_and_run(driver) else {
        return;
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() == Some(134) && stderr == message && output.stdout.is_empty(),
        "{driver}: expected status 134 and stderr {message:?}, got {}\nstdout: {:?}\n\
         stderr: {stderr}",
        output.status,
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn a_program_that_returns_ends_with_status_zero() {
    run_driver("program_returns");
}

#[test]
fn a_write_to_a_full_non_blocking_pipe_keeps_writing() {
    let Some(payload) = run_payload_driver("write_nonblocking_stdout") else {
        return;
    };
    assert_eq!(payload.len(), 1 << 20, "every byte of the payload arrives");
    assert!(
        payload
            .iter()
            .enumerate()
            .all(|(index, &byte)| byte == b'a' + (index % 26) as u8),
        "the payload arrives in order"
    );
}

#[test]
fn the_collector_frees_what_is_unreachable_and_reuses_it() {
    run_driver("gc_collects_unreachable");
}

#[test]
fn an_allocation_whose_size_would_wrap_runs_out_of_memory() {
    run_driver_expecting_failure("gc_rejects_wrapping_size", "krusty: out of memory\n");
}

#[test]
fn every_registered_global_root_is_kept_past_four_thousand() {
    run_driver("gc_many_global_roots");
}

#[test]
fn only_an_arrays_end_pointer_keeps_it_alive_and_no_neighbour_is_kept() {
    run_driver("gc_end_pointer_keeps_object");
}

#[test]
fn references_kept_in_typed_fields_elements_and_globals_are_traced() {
    run_driver("gc_typed_reference_slots");
}

#[test]
fn a_collection_started_during_a_collection_fails() {
    run_driver_expecting_failure(
        "gc_reentrant_collection_fails",
        "krusty: a collection started during a collection\n",
    );
}

#[test]
fn a_double_or_float_renders_as_the_jvm_renders_it() {
    run_driver("fp_render_known_answers");
}

#[test]
fn a_floating_remainder_is_exact_and_a_nan_comes_back_quiet() {
    run_driver("fp_remainder_known_answers");
}

#[test]
fn an_array_too_large_for_the_allocator_is_out_of_memory() {
    run_driver_expecting_failure("array_new_overflow", "krusty: out of memory\n");
}

#[test]
fn a_negative_string_index_is_out_of_bounds() {
    // Kotlin/Native's type: its runtime reads a `String` in `KString.cpp`
    // (`boundsCheckedIteratorAt`, `Kotlin_String_subSequence`, JetBrains/kotlin v2.4.10,
    // kotlin-native/runtime/src/main/cpp), which calls `ThrowArrayIndexOutOfBoundsException`, and
    // `RuntimeUtils.kt` throws `ArrayIndexOutOfBoundsException()`. The message is the JVM's.
    run_driver_against_kotlin_with(
        "string_get_negative_index",
        &[
            Divergence::native_behaviour(
                "\"abc\"[-1]: threw StringIndexOutOfBoundsException: Index -1 out of bounds \
                 for length 3",
                "\"abc\"[-1]: threw ArrayIndexOutOfBoundsException: Index -1 out of bounds for \
                 length 3",
            ),
            Divergence::native_behaviour(
                "\"abc\"[-100]: threw StringIndexOutOfBoundsException: Index -100 out of \
                 bounds for length 3",
                "\"abc\"[-100]: threw ArrayIndexOutOfBoundsException: Index -100 out of bounds \
                 for length 3",
            ),
            Divergence::native_behaviour(
                "\"abc\"[-2147483648]: threw StringIndexOutOfBoundsException: Index \
                 -2147483648 out of bounds for length 3",
                "\"abc\"[-2147483648]: threw ArrayIndexOutOfBoundsException: Index -2147483648 \
                 out of bounds for length 3",
            ),
            Divergence::native_behaviour(
                "\"abc\"[3]: threw StringIndexOutOfBoundsException: Index 3 out of bounds for \
                 length 3",
                "\"abc\"[3]: threw ArrayIndexOutOfBoundsException: Index 3 out of bounds for \
                 length 3",
            ),
        ],
    );
}

#[test]
fn a_substring_outside_the_text_is_out_of_bounds() {
    // Kotlin/Native's type: its runtime reads a `String` in `KString.cpp`
    // (`boundsCheckedIteratorAt`, `Kotlin_String_subSequence`, JetBrains/kotlin v2.4.10,
    // kotlin-native/runtime/src/main/cpp), which calls `ThrowArrayIndexOutOfBoundsException`, and
    // `RuntimeUtils.kt` throws `ArrayIndexOutOfBoundsException()`. The message is the JVM's.
    run_driver_against_kotlin_with(
        "string_substring_bounds",
        &[
            Divergence::native_behaviour(
                "substring(-1, 2): threw StringIndexOutOfBoundsException: Range [-1, 2) out of \
                 bounds for length 3",
                "substring(-1, 2): threw ArrayIndexOutOfBoundsException: Range [-1, 2) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(-1): threw StringIndexOutOfBoundsException: Range [-1, 3) out of \
                 bounds for length 3",
                "substring(-1): threw ArrayIndexOutOfBoundsException: Range [-1, 3) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(2, 1): threw StringIndexOutOfBoundsException: Range [2, 1) out of \
                 bounds for length 3",
                "substring(2, 1): threw ArrayIndexOutOfBoundsException: Range [2, 1) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(2, 10): threw StringIndexOutOfBoundsException: Range [2, 10) out of \
                 bounds for length 3",
                "substring(2, 10): threw ArrayIndexOutOfBoundsException: Range [2, 10) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(4): threw StringIndexOutOfBoundsException: Range [4, 3) out of \
                 bounds for length 3",
                "substring(4): threw ArrayIndexOutOfBoundsException: Range [4, 3) out of \
                 bounds for length 3",
            ),
        ],
    );
}

#[test]
fn whitespace_is_the_jvm_set() {
    run_driver("string_whitespace");
}

#[test]
fn a_repeat_too_long_for_memory_is_out_of_memory() {
    run_driver_expecting_failure("string_repeat_overflow", "krusty: out of memory\n");
}

#[test]
fn a_concatenation_too_long_for_memory_is_out_of_memory() {
    run_driver_expecting_failure("string_plus_overflow", "krusty: out of memory\n");
}

#[test]
fn surrogate_halves_concatenate_into_their_character() {
    run_driver("string_plus_surrogates");
}

#[test]
fn a_bound_between_surrogate_halves_cuts_the_pair_and_a_search_finds_either_half() {
    run_driver("string_surrogate_halves");
}

#[test]
fn a_builder_receiver_or_suffix_is_read_as_text_and_sliced_into_a_copy() {
    run_driver("string_builder_receivers");
}

#[test]
fn a_concatenation_stops_at_the_first_throwing_to_string() {
    run_driver("string_plus_throwing_to_string");
}

#[test]
fn unboxing_null_raises_and_comes_back() {
    run_driver("unbox_null_raises");
}

#[test]
fn a_number_conversion_of_null_raises_and_comes_back() {
    run_driver("number_conversion_null_raises");
}

#[test]
fn a_builder_joins_a_surrogate_pair_appended_unit_by_unit() {
    run_driver("builder_joins_surrogate_pair");
}

#[test]
fn a_lazy_whose_initializer_throws_stays_uninitialized() {
    run_driver("lazy_initializer_throws");
}

#[test]
fn an_append_whose_to_string_throws_leaves_the_builder_alone() {
    run_driver("builder_append_throwing_to_string");
}

#[test]
fn an_append_line_whose_to_string_throws_leaves_the_builder_alone() {
    run_driver("builder_append_line_throwing_to_string");
}

#[test]
fn a_negative_builder_capacity_throws_illegal_argument_exception() {
    // Kotlin/Native's type: 2.4.10's stdlib (the distribution's linux_x64 static cache) compiles
    // `StringBuilder(capacity)` to `AllocArrayInstance(CharArray, capacity)`, which calls
    // `ThrowIllegalArgumentException` for a negative size. The message is the JVM's.
    run_driver_against_kotlin_with(
        "builder_negative_capacity",
        &[
            Divergence::native_behaviour(
                "StringBuilder(-1): threw NegativeArraySizeException: -1",
                "StringBuilder(-1): threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "StringBuilder(-42): threw NegativeArraySizeException: -42",
                "StringBuilder(-42): threw IllegalArgumentException: -42",
            ),
            Divergence::native_behaviour(
                "StringBuilder(-2147483648): threw NegativeArraySizeException: -2147483648",
                "StringBuilder(-2147483648): threw IllegalArgumentException: -2147483648",
            ),
        ],
    );
}

#[test]
fn a_builder_made_from_a_program_char_sequence_holds_its_text() {
    run_driver("builder_from_program_char_sequence");
}

#[test]
fn a_builder_made_from_a_program_char_sequence_that_throws_is_not_made() {
    run_driver("builder_from_throwing_char_sequence");
}

#[test]
fn a_pair_stops_at_the_first_component_that_throws() {
    run_driver("pair_component_throws");
}

#[test]
fn a_result_whose_content_throws_renders_no_text() {
    run_driver("result_to_string_throws");
}

#[test]
fn a_builder_grown_past_the_largest_length_is_out_of_memory() {
    run_driver_expecting_failure("builder_length_overflow", "krusty: out of memory\n");
}

#[test]
fn a_class_answers_the_names_its_descriptor_publishes() {
    run_driver("class_names");
}

#[test]
fn a_class_that_publishes_no_names_fails_naming_its_descriptor() {
    run_driver_expecting_failure(
        "class_names_unpublished",
        "krusty: the class pkg.Unpublished publishes no reflection names consistent with its kind\n",
    );
}

#[test]
fn a_result_answers_its_operations_and_get_or_throw_stops_at_the_throw() {
    run_driver("result_operations");
}

#[test]
fn a_property_read_through_an_unknown_delegate_fails_naming_it() {
    run_driver_expecting_failure(
        "rw_property_get_unknown_delegate",
        "krusty: a ReadWriteProperty read of a pkg.CustomDelegate, which is neither \
         Delegates.notNull() nor Delegates.observable()\n",
    );
}

#[test]
fn a_property_write_through_an_unknown_delegate_fails_naming_it() {
    run_driver_expecting_failure(
        "rw_property_set_unknown_delegate",
        "krusty: a ReadWriteProperty write of a pkg.CustomDelegate, which is neither \
         Delegates.notNull() nor Delegates.observable()\n",
    );
}

#[test]
fn each_primitive_boxes_to_its_kind_and_small_values_share_a_box() {
    run_driver("boxing");
}

#[test]
fn a_number_converts_saturating_and_truncating_as_kotlin_does() {
    run_driver("number_conversions");
}

#[test]
fn a_pairs_members_answer_componentwise() {
    run_driver("pair_members");
}

#[test]
fn lazy_observable_and_not_null_delegates_keep_kotlins_order() {
    run_driver("delegates");
}

#[test]
fn a_builder_appends_copies_and_sets_its_length() {
    run_driver("builder_operations");
}

#[test]
fn a_ulong_progression_across_two_to_the_63_contains_its_members() {
    run_driver_against_kotlin("range_contains_unsigned");
}

#[test]
fn a_progression_renders_compares_and_hashes_with_its_step() {
    run_driver_against_kotlin("range_progression_members");
}

#[test]
fn a_range_of_a_program_comparable_orders_by_its_compare_to() {
    run_driver("comparable_range_program_type");
}

#[test]
fn an_empty_unsigned_until_is_the_declared_empty_range() {
    run_driver_against_kotlin("range_unsigned_until_empty");
}

#[test]
fn a_ulong_walk_across_two_to_the_63_steps_without_signed_overflow() {
    run_driver_against_kotlin("range_iterator_ulong_crosses_sign");
}

#[test]
fn a_spread_copy_that_does_not_fit_throws_and_writes_nothing() {
    run_driver_against_kotlin("array_copy_into_bounds");
}

#[test]
fn a_range_a_progression_and_their_iterators_are_kotlins_classes() {
    run_driver_against_kotlin("range_class_identity");
}

#[test]
fn a_comparable_ranges_members_call_the_program_in_kotlins_order_and_stop_at_a_throw() {
    run_driver_against_kotlin("comparable_range_members");
}

#[test]
fn a_floating_point_range_compares_by_ieee_and_answers_its_members_as_kotlin_does() {
    run_driver_against_kotlin("floating_range");
}

#[test]
fn a_spread_of_something_other_than_the_varargs_array_kind_fails() {
    run_driver_expecting_failure(
        "array_copy_into_not_an_array",
        "krusty: a spread of a value that is not an array of the vararg's kind\n",
    );
}

#[test]
fn range_behavior_matches_an_executable_kotlinc_oracle() {
    run_driver_against_kotlin("range_kotlinc_oracle");
}

#[test]
fn a_step_of_something_other_than_a_range_fails() {
    run_driver_expecting_failure(
        "range_step_not_a_range",
        "krusty: a step or reversal of a value that is not a range\n",
    );
}

#[test]
fn a_reversal_of_something_other_than_a_range_fails() {
    run_driver_expecting_failure(
        "range_reversed_not_a_range",
        "krusty: a step or reversal of a value that is not a range\n",
    );
}

#[test]
fn mapping_the_full_long_range_is_too_long_to_collect() {
    run_driver_expecting_failure(
        "range_map_full_span",
        "krusty: a range too long to collect\n",
    );
}

#[test]
fn a_list_write_out_of_bounds_raises_and_leaves_the_list_alone() {
    run_driver("mutable_list_bounds");
}

#[test]
fn a_list_read_out_of_bounds_raises_and_answers_nothing() {
    run_driver("list_read_bounds");
}

#[test]
fn a_list_added_to_itself_doubles() {
    run_driver("list_add_all_self");
}

#[test]
fn a_list_modified_during_for_each_ends_the_walk() {
    run_driver("walk_modified_during_for_each");
}

#[test]
fn a_throwing_lambda_ends_the_walk_that_called_it() {
    run_driver("walk_stops_on_throw");
}

#[test]
fn an_exhausted_iterator_raises_what_kotlin_raises_through_either_protocol() {
    run_driver_against_kotlin("iterator_exhausted");
}

#[test]
fn an_indexed_value_hash_code_wraps() {
    run_driver("indexed_value_hash_overflow");
}

#[test]
fn an_array_list_of_negative_capacity_raises() {
    run_driver("array_list_negative_capacity");
}

#[test]
fn walking_a_string_is_linear_and_yields_its_utf16_units() {
    run_driver("string_iterator_linear");
}

#[test]
fn a_programs_own_text_is_asked_its_length_once_per_step() {
    run_driver("program_text_walk_asks_length_once_per_step");
}

#[test]
fn a_list_grown_past_the_largest_capacity_is_out_of_memory() {
    run_driver_expecting_failure("mutable_list_growth_overflow", "krusty: out of memory\n");
}

#[test]
fn an_element_member_that_throws_ends_the_walk_that_called_it() {
    run_driver("list_stops_on_throwing_element");
}

#[test]
fn a_list_is_the_collection_interfaces_and_equals_a_program_list() {
    run_driver_against_kotlin("list_identity");
}

#[test]
fn a_walk_stops_at_a_throwing_iterator_or_has_next_of_the_program() {
    run_driver_against_kotlin("walk_polls_program_calls");
}

#[test]
fn a_list_modification_count_wraps_and_is_still_noticed() {
    run_driver("list_modification_count_wraps");
}

#[test]
fn a_walk_count_past_the_largest_int_raises_kotlins_overflow() {
    run_driver_against_kotlin("walk_count_overflow");
}

#[test]
fn a_walk_index_past_the_largest_int_raises_kotlins_overflow() {
    run_driver_against_kotlin("walk_index_overflow");
}

#[test]
fn an_indexed_value_or_none_stops_at_the_programs_throwing_call() {
    run_driver("member_stops_at_throw");
}

#[test]
fn a_builder_map_sees_its_length_change_and_a_self_list_renders() {
    run_driver_against_kotlin("builder_map_and_self_list");
}

#[test]
fn the_list_walk_and_array_entry_points_answer_as_kotlin_does() {
    run_driver_against_kotlin("list_api_answers");
}

#[test]
fn a_map_or_for_each_over_a_list_its_lambda_changes_stops_where_kotlins_iterator_does() {
    run_driver_against_kotlin("map_mutated_source");
}

#[test]
fn an_iterator_is_an_iterator_and_an_arrays_is_its_kinds_iterator() {
    run_driver_against_kotlin("iterator_identity");
}

fn compiled_build_script() -> PathBuf {
    static BUILD_SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    BUILD_SCRIPT
        .get_or_init(|| {
            let scratch = common::scratch_dir().expect("scratch directory");
            let executable = scratch.join("native-runtime-build-script");
            let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let output = Command::new(rustc)
                .args(["--edition=2021", "build.rs", "-o"])
                .arg(&executable)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("compile build.rs");
            assert_eq!(
                (output.status.success(), output.stdout, output.stderr),
                (true, Vec::new(), Vec::new()),
                "build.rs compiles as a standalone build-script executable"
            );
            executable
        })
        .clone()
}

fn run_build_script(compiler: &Path, out_dir: &Path) -> Output {
    Command::new(compiled_build_script())
        .env("OUT_DIR", out_dir)
        .env("KRUSTY_RUNTIME_CC", compiler)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run build.rs")
}

#[test]
fn a_missing_runtime_compiler_leaves_the_native_target_unavailable() {
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("missing-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let missing = scratch.join("compiler-that-does-not-exist");

    let output = run_build_script(&missing, &out_dir);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stderr, b"");
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        format!(
            "cargo:rerun-if-changed=src/native/runtime/krusty_rt.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_collections.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_internal.h\n\
             cargo:rerun-if-changed=build.rs\n\
             cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n\
             cargo:warning=native runtime: compiler `{}` was not found; no native target will be available. Install clang, or set KRUSTY_RUNTIME_CC.\n\
             cargo:rerun-if-env-changed=PATH\n",
            missing.display()
        )
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("prebuilt_runtime.rs"))
            .expect("read generated runtime table"),
        "// Generated by build.rs: the native runtime, prebuilt per target.\n\
         pub const PREBUILT: &[(crate::native::Arch, &[(&str, &[u8])])] = &[\n\
         ];\n\
         /// Whether any target's runtime was prebuilt (a C compiler was available when krusty was built).\n\
         pub const AVAILABLE: bool = false;\n"
    );
}

#[test]
fn a_failing_runtime_compiler_fails_the_build() {
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("failing-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let compiler = scratch.join("runtime-compiler-exits-one");
    fs::write(&compiler, "#!/bin/sh\nexit 1\n").expect("write failing compiler");
    let mut permissions = fs::metadata(&compiler)
        .expect("read failing compiler metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&compiler, permissions).expect("make failing compiler executable");

    let output = run_build_script(&compiler, &out_dir);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("build-script stderr is UTF-8"),
        format!(
            "native runtime: `{}` failed compiling `krusty_rt.c` for \
             `x86_64-unknown-linux-gnu` (exit status: 1)\n",
            compiler.display()
        )
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        "cargo:rerun-if-changed=src/native/runtime/krusty_rt.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_collections.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_internal.h\n\
         cargo:rerun-if-changed=build.rs\n\
         cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n"
    );
    assert!(
        !out_dir.join("prebuilt_runtime.rs").exists(),
        "a failed compiler must not publish a partial target table"
    );
}
