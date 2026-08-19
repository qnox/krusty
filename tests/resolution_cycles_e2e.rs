//! A declaration whose type depends on itself declines; it does not hang and it does not invent a
//! type.
//!
//! Resolving declarations on demand makes recursion reachable: asking for `a` asks for `b`, which
//! asks for `a`. Termination is structural — a declaration already being computed is a cycle, and
//! the engine declines instead of recursing — rather than a depth or round budget. kotlinc rejects
//! the same sources ("type checking has run into a recursive problem"), reporting at each
//! implicitly-typed declaration on the loop rather than once at whichever was entered first.
use super::common;

const RECURSIVE_INFERENCE_MESSAGE: &str = "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.";

/// The front end's diagnostics for one source set, with stdlib and JDK symbols available.
fn diagnostics(sources: &[&str]) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics_files(sources, &[stdlib], Some(jdk.as_path()))
}

fn assert_declines(tag: &str, expected_count: usize, sources: &[&str]) {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let reported = diagnostics(sources);
    assert_eq!(reported.len(), expected_count, "{tag}: diagnostic count");
    assert_eq!(
        reported,
        vec![RECURSIVE_INFERENCE_MESSAGE; expected_count],
        "{tag}: exact recursive inference diagnostics"
    );
}

#[test]
fn a_property_initialized_from_itself_declines() {
    assert_declines("self", 1, &["val a = a\n"]);
}

#[test]
fn two_properties_initialized_from_each_other_decline() {
    // kotlinc reports at BOTH declarations, not only at the one entered first, which is why the
    // engine records every declaration on the loop rather than the re-entered one alone.
    assert_declines("mutual", 2, &["val a = b\nval b = a\n"]);
}

#[test]
fn a_three_way_cycle_declines() {
    assert_declines("three-way", 3, &["val a = b\nval b = c\nval c = a\n"]);
}

#[test]
fn a_cycle_spanning_files_declines_in_either_order() {
    // The cycle crosses a file boundary, so it is only reachable at all once resolution is
    // demand-driven. Both argument orders must terminate and decline.
    assert_declines("cross-file", 2, &["val a = b\n", "val b = a\n"]);
    assert_declines("cross-file-reversed", 2, &["val b = a\n", "val a = b\n"]);
}

#[test]
fn a_cycle_through_a_getter_declines() {
    // An expression getter is an executable body and may legally read a declaration written later,
    // so it is the spelling most likely to close a loop by accident.
    assert_declines("getter", 2, &["val a get() = b\nval b get() = a\n"]);
}

#[test]
fn a_cycle_between_a_class_member_and_a_module_property_declines() {
    // The loop crosses the boundary the two old retry passes had between them, so neither could see
    // it whole: one ran to a fixpoint before the other started. One queue makes it an ordinary
    // cycle.
    assert_declines(
        "member-module",
        2,
        &["object Holder { val fromModule = fromMember }\nval fromMember = Holder.fromModule\n"],
    );
}

#[test]
fn a_cycle_between_two_class_members_declines() {
    assert_declines(
        "two-members",
        2,
        &["class Pair { val a get() = b\n val b get() = a }\n"],
    );
}

#[test]
fn a_declaration_reached_twice_without_a_cycle_still_resolves() {
    // Two readers of one declaration is not a loop. Guarding this keeps a cycle check from
    // degenerating into "anything reached more than once declines".
    if !common::stdlib_toolchain_ready() {
        return;
    }
    common::expect_box_ok_files_with_stdlib(
        &[
            ("A.kt", "val shared = listOf(1, 2, 3)\n"),
            ("B.kt", "val left = shared.map { it + 1 }\n"),
            (
                "C.kt",
                "val right = shared.map { it * 2 }\n\
                 fun box(): String = if (left.size == 3 && right.size == 3) \"OK\" else \"fail\"\n",
            ),
        ],
        "a declaration read by two others",
    );
}
