//! kotlinc's `SwitchGenerator` materializes every arm of an exhaustive `Unit` `when` that it
//! dispatches through a `tableswitch`, and the statement pops the joined `Unit` once. In a method
//! too large for `PopBackwardPropagation` (its source-interpreter memory gate), each arm keeps its
//! `getstatic kotlin/Unit.INSTANCE` and the join keeps its `pop`. A switch without `else`, and an
//! `if`/`else` chain, discard each branch in the branch instead.

use super::common;
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

/// A function too large for the pop-backward gate: `slots` locals assigned through a loop whose
/// `when` has an arm per local, after a few statement `when`s and `if`s of each shape.
fn large_function(slots: usize) -> String {
    let declarations: String = (0..slots)
        .map(|i| format!("    var p{i}: List<String>? = null\n"))
        .collect();
    let arms: String = (0..slots)
        .map(|i| {
            format!(
                "            {i} -> {{ p{i} = d.next(p{i}); mask = mask or {} }}\n",
                1 << (i % 31)
            )
        })
        .collect();
    let sum = (0..slots)
        .map(|i| format!("p{i}.hashCode()"))
        .collect::<Vec<_>>()
        .join(" + ");
    format!(
        "interface D {{ fun idx(): Int; fun next(o: List<String>?): List<String>?; fun n(): Int }}\n\
         fun big(d: D): Int {{\n\
         {declarations}\
         \x20   var mask = 0\n\
         \x20   var go = true\n\
         \x20   when (d.n()) {{ 1 -> mask = 2; 2 -> mask = 3; 3 -> mask = 5; else -> mask = 9 }}\n\
         \x20   mark()\n\
         \x20   when (d.n()) {{ 1 -> mask = 2; 2 -> mask = 3; 3 -> mask = 5 }}\n\
         \x20   mark()\n\
         \x20   if (d.n() == 1) {{ mask = 2 }} else throw IllegalStateException()\n\
         \x20   mark()\n\
         \x20   when (d.n()) {{ 1 -> mask = 2; 2 -> mask = 3; 3 -> mask = 5; else -> throw IllegalStateException() }}\n\
         \x20   mark()\n\
         \x20   while (go) {{\n\
         \x20       val i = d.idx()\n\
         \x20       when (i) {{\n\
         \x20           -1 -> go = false\n\
         {arms}\
         \x20           else -> throw IllegalStateException(i.toString())\n\
         \x20       }}\n\
         \x20       mark()\n\
         \x20   }}\n\
         \x20   return mask + {sum}\n\
         }}\n\
         fun mark() {{}}\n"
    )
}

#[test]
fn a_large_method_keeps_each_exhaustive_switch_arms_unit() {
    let source = large_function(90);
    let comparison = common::byte_diff_against_kotlinc_cp(
        "ExhaustiveSwitchUnit",
        &source,
        "ExhaustiveSwitchUnitKt",
        &[common::stdlib_jar()],
    )
    .expect("reference class comparison must be available");
    if let Err(e) = comparison {
        panic!("{e}");
    }
}

#[test]
fn a_small_method_drops_the_switch_arms_unit() {
    let source = large_function(3);
    let comparison = common::byte_diff_against_kotlinc_cp(
        "ExhaustiveSwitchUnitSmall",
        &source,
        "ExhaustiveSwitchUnitSmallKt",
        &[common::stdlib_jar()],
    )
    .expect("reference class comparison must be available");
    if let Err(e) = comparison {
        panic!("{e}");
    }
}

/// A `Unit` switch whose `when` is `Unit` only because it ends in an open `else -> if`: its other
/// arms keep their `String` type, so each discards its value and the join gets a `Unit`, with a
/// `box()` that takes such an arm. `slots` sizes the method as [`large_function`] does.
fn mixed_arm_function(slots: usize) -> String {
    let declarations: String = (0..slots)
        .map(|i| format!("    var p{i}: List<String>? = null\n"))
        .collect();
    let arms: String = (0..slots)
        .map(|i| format!("            {i} -> p{i} = d.next(p{i})\n"))
        .collect();
    let sum = (0..slots)
        .map(|i| format!("p{i}.hashCode()"))
        .collect::<Vec<_>>()
        .join(" + ");
    format!(
        "interface D {{ fun idx(): Int; fun next(o: List<String>?): List<String>?; fun n(): Int }}\n\
         fun text(): String = \"ignored\"\n\
         fun big(d: D): Int {{\n\
         {declarations}\
         \x20   var go = true\n\
         \x20   when (d.n()) {{\n\
         \x20       1 -> text()\n\
         \x20       2 -> text()\n\
         \x20       else -> if (d.n() == 5) text()\n\
         \x20   }}\n\
         \x20   mark()\n\
         \x20   while (go) {{\n\
         \x20       val i = d.idx()\n\
         \x20       when (i) {{\n\
         \x20           -1 -> go = false\n\
         {arms}\
         \x20           else -> throw IllegalStateException(i.toString())\n\
         \x20       }}\n\
         \x20       mark()\n\
         \x20   }}\n\
         \x20   return {sum}\n\
         }}\n\
         fun mark() {{}}\n\
         fun box(): String {{\n\
         \x20   val d = object : D {{\n\
         \x20       override fun idx() = -1\n\
         \x20       override fun next(o: List<String>?) = o\n\
         \x20       override fun n() = 1\n\
         \x20   }}\n\
         \x20   big(d)\n\
         \x20   return \"OK\"\n\
         }}\n"
    )
}

fn assert_mixed_arm_switch_matches_kotlinc(slots: usize, stem: &str) {
    let source = mixed_arm_function(slots);
    common::expect_box_ok_with_stdlib(&source, stem);
    let comparison = common::byte_diff_against_kotlinc_cp(
        stem,
        &source,
        &format!("{stem}Kt"),
        &[common::stdlib_jar()],
    )
    .expect("reference class comparison must be available");
    if let Err(e) = comparison {
        panic!("{e}");
    }
}

#[test]
fn a_large_method_discards_a_non_unit_arm_before_its_unit() {
    assert_mixed_arm_switch_matches_kotlinc(90, "ExhaustiveSwitchMixedArms");
}

#[test]
fn a_small_method_discards_a_non_unit_arm() {
    assert_mixed_arm_switch_matches_kotlinc(3, "ExhaustiveSwitchMixedArmsSmall");
}

/// A large `$$serializer.deserialize`: its element `when` is the last statement of the decode
/// loop's body, so the joined `Unit` is popped after that body's locals end.
#[test]
fn a_large_deserializer_keeps_each_element_arms_unit() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let properties = (0..31)
        .map(|i| format!("    val p{i}: List<String>? = null"))
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!(
        "import kotlinx.serialization.Serializable\n@Serializable\nclass Wide(\n{properties}\n)\n"
    );
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "ExhaustiveSwitchUnitDeserializer",
        &source,
        "Wide$$serializer",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "Wide$$serializer must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}
