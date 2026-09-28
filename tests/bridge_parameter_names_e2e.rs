//! A bridge names its parameters after the declaration it overrides, not the override it calls, as
//! kotlinc's `BridgeLowering` copies them from the overridden function: `f(y: String)` over
//! `I<T>.f(x: T)` has a bridge `f(Object)` whose parameter is `x`. A builtin supertype's names come
//! from `.kotlin_builtins` (`MutableEntry.setValue(newValue)`, `Comparable.compareTo(other)`), and
//! a function type's from its `FunctionN` (`p1`).

use super::common;

const SRC: &str = "interface Pick<T> { fun pick(first: T, count: Int): Int }\n\
    class Picker : Pick<String> { override fun pick(word: String, times: Int): Int = times }\n\
    class Entry : MutableMap.MutableEntry<String, String> {\n\
    \x20   override val key: String get() = \"k\"\n\
    \x20   override val value: String get() = \"v\"\n\
    \x20   override fun setValue(value: String): String = value\n\
    }\n\
    class Rank(val n: Int) : Comparable<Rank> { override fun compareTo(that: Rank): Int = n - that.n }\n\
    class Length : (String) -> String { override fun invoke(text: String): String = text }\n\
    fun box(): String {\n\
    \x20   val pick: Pick<String> = Picker()\n\
    \x20   if (pick.pick(\"a\", 2) != 2) return \"pick\"\n\
    \x20   val entry: MutableMap.MutableEntry<String, String> = Entry()\n\
    \x20   if (entry.setValue(\"w\") != \"w\") return \"entry\"\n\
    \x20   val rank: Comparable<Rank> = Rank(3)\n\
    \x20   if (rank.compareTo(Rank(1)) != 2) return \"rank\"\n\
    \x20   val length: (String) -> String = Length()\n\
    \x20   if (length(\"OK\") != \"OK\") return \"length\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// The `LocalVariableTable` rows (`slot name signature`) of the method whose declaration line is
/// `member`, in table order.
fn local_variables(disassembly: &str, member: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut inside = false;
    let mut table = false;
    for line in disassembly.lines().map(str::trim) {
        if line == member {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.is_empty() || line == "}" {
            break;
        }
        if line == "LocalVariableTable:" {
            table = true;
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if table && fields.len() == 5 && fields[..3].iter().all(|f| f.parse::<u32>().is_ok()) {
            rows.push(format!("{} {} {}", fields[2], fields[3], fields[4]));
        } else if table && !line.starts_with("Start") {
            break;
        }
    }
    rows
}

fn assert_same_locals(class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "BridgeParameterNames",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = local_variables(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        local_variables(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

#[test]
fn a_bridge_takes_a_source_supertypes_parameter_names() {
    assert_same_locals("Picker", "public int pick(java.lang.Object, int);");
}

#[test]
fn a_bridge_takes_a_builtin_supertypes_parameter_names() {
    assert_same_locals(
        "Entry",
        "public java.lang.Object setValue(java.lang.Object);",
    );
    assert_same_locals("Rank", "public int compareTo(java.lang.Object);");
}

#[test]
fn a_bridge_takes_a_function_types_parameter_names() {
    assert_same_locals(
        "Length",
        "public java.lang.Object invoke(java.lang.Object);",
    );
}

#[test]
fn bridges_named_after_the_overridden_declaration_run() {
    common::expect_box_same_as_kotlinc(SRC, "BridgeParameterNames");
}
