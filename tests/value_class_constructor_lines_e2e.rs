//! Where a value class's private `<init>` maps its lines, against kotlinc.
//!
//! kotlinc's JVM inline-class lowering replaces a value class's primary constructor with a new
//! private one whose whole body — the `Object()` call, the store of the underlying field and the
//! `return` — is built at the ORIGINAL constructor's own offset: its `constructor` keyword, or its
//! parameter list's `(` when there is no keyword. Annotations and modifiers in front of either do
//! not move it, and neither does the `value class` keyword: `value class⏎N(val x: Int)` maps to the
//! `N(` line. So the constructor's table is one row, `line <constructor line>: 0`.
//!
//! An ordinary class keeps its statements' own positions: `super()` on the line its declaration
//! starts (annotations included), each property store on its parameter, and the trailing `return`
//! on the same constructor line the value class uses for everything. krusty anchored the value
//! class's `<init>` the ordinary way — the `@JvmInline` line, then the property — and put every
//! `return` on the class header, which is not the constructor once the header wraps before a
//! `constructor` keyword.
//!
//! Every member of every class is compared, so `constructor-impl`, `box-impl` and the other
//! synthesized members are pinned too: none of them gains a row it should not have.
use super::common;

/// The fixture's line numbers are part of the expectations below.
const SOURCE: &str = r#"package vcl

annotation class Mark

@JvmInline
value class OwnLine(val x: Int)

@JvmInline value class SameLine(val x: Int)

inline class Unannotated(val x: Int)

@JvmInline
value class WithInit(val x: Int) {
    init {
        check(x >= 0)
    }
}

@JvmInline
value class Wrapped(
    val x: Int
)

@JvmInline
value class KeywordCtor @Mark
constructor(val x: Int)

@Mark
class PlainAnnotated(val x: Int)

@Mark class PlainSameLine(val x: Int)

@Mark
class PlainKeyword private
constructor(val x: Int) {
    val y = x
}

@JvmInline
value class
KwSplit(val x: Int)

@JvmInline
public
value class ModSplit(val x: Int)

@JvmInline
value class InitTwo(val x: Int) {
    init { println(x) }
    init {
        println(x + 1)
    }
}
"#;

/// Each member with a `LineNumberTable` and its rows, in class-file order.
type MemberLines = Vec<(String, Vec<String>)>;

/// Each member of `class` that has a `LineNumberTable`, with its rows, in class-file order.
fn member_lines(listing: &str) -> MemberLines {
    let mut members: MemberLines = Vec::new();
    let mut current: Option<String> = None;
    for raw in listing.lines() {
        let line = raw.trim();
        if line.ends_with(");") {
            current = Some(line.to_string());
        } else if let (Some(row), Some(member)) = (line.strip_prefix("line "), &current) {
            match members.last_mut() {
                Some((last, rows)) if last == member => rows.push(row.to_string()),
                _ => members.push((member.clone(), vec![row.to_string()])),
            }
        }
    }
    members
}

fn tables(sets: &common::ClassSets, class: &str) -> (MemberLines, MemberLines) {
    let (reference, krusty) = sets.class_listing(class);
    (member_lines(&reference), member_lines(&krusty))
}

fn rows(entries: &[(&str, &[&str])]) -> MemberLines {
    entries
        .iter()
        .map(|(member, rows)| {
            (
                member.to_string(),
                rows.iter().map(|row| row.to_string()).collect(),
            )
        })
        .collect()
}

/// The value-class members with a table: the getter on the property, the private `<init>` on the
/// constructor's own line. `constructor-impl`, `box-impl`, `unbox-impl`, `equals-impl0` and the
/// structural members have none.
fn value_class_rows(class: &str, getter: &str, ctor: &str) -> MemberLines {
    let getter_row = format!("{getter}: 0");
    let ctor_row = format!("{ctor}: 0");
    rows(&[
        ("public final int getX();", &[getter_row.as_str()]),
        (&format!("private vcl.{class}(int);"), &[ctor_row.as_str()]),
    ])
}

#[test]
fn every_member_line_table_matches_kotlinc() {
    let sets = common::classes_against_kotlinc_module(&[("ValueClassCtorLines.kt", SOURCE)]);
    let expected = [
        ("vcl/OwnLine", value_class_rows("OwnLine", "6", "6")),
        ("vcl/SameLine", value_class_rows("SameLine", "8", "8")),
        (
            "vcl/Unannotated",
            value_class_rows("Unannotated", "10", "10"),
        ),
        (
            "vcl/WithInit",
            rows(&[
                ("public final int getX();", &["13: 0"]),
                ("private vcl.WithInit(int);", &["13: 0"]),
                ("public static int constructor-impl(int);", &["15: 2"]),
            ]),
        ),
        ("vcl/Wrapped", value_class_rows("Wrapped", "21", "20")),
        (
            "vcl/KeywordCtor",
            value_class_rows("KeywordCtor", "26", "26"),
        ),
        ("vcl/KwSplit", value_class_rows("KwSplit", "41", "41")),
        ("vcl/ModSplit", value_class_rows("ModSplit", "45", "45")),
        (
            "vcl/InitTwo",
            rows(&[
                ("public final int getX();", &["48: 0"]),
                ("private vcl.InitTwo(int);", &["48: 0"]),
                (
                    "public static int constructor-impl(int);",
                    &["49: 2", "51: 11"],
                ),
            ]),
        ),
        (
            "vcl/PlainAnnotated",
            rows(&[
                ("public vcl.PlainAnnotated(int);", &["28: 0", "29: 4"]),
                ("public final int getX();", &["29: 0"]),
            ]),
        ),
        (
            "vcl/PlainSameLine",
            rows(&[
                ("public vcl.PlainSameLine(int);", &["31: 0"]),
                ("public final int getX();", &["31: 0"]),
            ]),
        ),
        (
            "vcl/PlainKeyword",
            rows(&[
                (
                    "private vcl.PlainKeyword(int);",
                    &["33: 0", "35: 4", "36: 9", "35: 17"],
                ),
                ("public final int getX();", &["35: 0"]),
                ("public final int getY();", &["36: 0"]),
            ]),
        ),
    ];
    for (class, want) in expected {
        let (reference, krusty) = tables(&sets, class);
        assert_eq!(reference, want, "kotlinc's exact tables for {class}");
        assert_eq!(krusty, reference, "{class}'s member line tables");
    }
}
