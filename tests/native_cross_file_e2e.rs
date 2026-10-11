//! A top-level function, a package property, a class's member property, or a class's constructor
//! defined in another file of the same module.
//!
//! The call is a direct symbol both files derive from the checked callable identity, a property
//! is reached through the getter and setter entry points both files derive from the property
//! identity, and a construction through the entry point both files derive from the constructor's
//! qualified owner and parameter list. The file that does not contain `box` still has to be linked: its function is
//! not in the entry file.

use crate::common::{expect_box_ok_files_with_stdlib, expect_native_sources};

/// A portable multi-file program: the JVM answer is established first, then Native must agree.
fn expect_portable_sources(sources: &[(&str, &str)]) {
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    expect_box_ok_files_with_stdlib(sources, stem);
    expect_native_sources(sources, "OK");
}

#[test]
fn a_file_calls_a_top_level_function_defined_in_another_file() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun value(): Int = 21
                    fun double(n: Int): Int = n * 2
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = if (double(value()) == 42) "OK" else "FAIL"
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn overloads_defined_in_different_files_stay_distinct() {
    expect_native_sources(
        &[
            (
                "ints",
                r#"
                    package demo
                    fun pick(n: Int): Int = n + 1
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun pick(text: String): Int = text.length
                    fun box(): String =
                        if (pick(1) == 2 && pick("ab") == 2) "OK" else "FAIL"
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_top_level_property_in_another_file_is_initialized_before_the_call() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    val base: Int = 21
                    fun value(): Int = base
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = if (value() == 21) "OK" else "FAIL"
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_file_initializer_runs_once_however_many_calls_arrive() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    var hits: Int = 0
                    fun note(): Int {
                        hits += 1
                        return hits
                    }
                    val marker: Int = note()
                    fun value(): Int = marker
                    fun seen(): Int = hits
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val first = value()
                        val second = value()
                        return if (first == 1 && second == 1 && seen() == 1) "OK" else "FAIL"
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn call_arguments_are_evaluated_before_the_defining_file_is_initialized() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    val initialized: Int = observeInitialization()
                    fun consume(value: Int): Int = value + initialized
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    var order: Int = 0
                    fun argument(): Int {
                        order = order * 10 + 1
                        return 7
                    }
                    fun observeInitialization(): Int {
                        order = order * 10 + 2
                        return 0
                    }
                    fun box(): String {
                        consume(argument())
                        return if (order == 12) "OK" else "FAIL: $order"
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_string_returned_from_another_file_is_that_string() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun label(): String = "OK"
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = label()
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_package_property_is_read_from_the_file_that_stores_it() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    val label: String = "OK"
                    var count: Int = 1
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    fun box(): String {
                        count += 2
                        return if (label == "OK" && count == 3) "OK" else "FAIL"
                    }
                "#,
        ),
    ]);
}

#[test]
fn an_inline_function_updates_a_package_property_in_its_own_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    var order: String = ""
                    inline fun note(piece: String) {
                        order = order + piece
                    }
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    fun box(): String {
                        note("O")
                        note("K")
                        return order
                    }
                "#,
        ),
    ]);
}

#[test]
fn an_assignment_evaluates_its_value_before_the_defining_file_initializes() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    val initialized: Int = observeInitialization()
                    var slot: Int = 0
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    var order: Int = 0
                    fun observeInitialization(): Int {
                        order = order * 10 + 2
                        return 0
                    }
                    fun argument(): Int {
                        order = order * 10 + 1
                        return 7
                    }
                    fun box(): String {
                        slot = argument()
                        return if (order == 12 && slot == 7) "OK" else "FAIL: $order $slot"
                    }
                "#,
        ),
    ]);
}

#[test]
fn a_lateinit_package_property_throws_until_assigned() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    lateinit var label: String
                    fun publish() {
                        label = "OK"
                    }
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    fun box(): String {
                        try {
                            return label
                        } catch (e: kotlin.UninitializedPropertyAccessException) {
                            publish()
                            return if (label == "OK") "OK" else "FAIL"
                        }
                    }
                "#,
        ),
    ]);
}

/// Native only for now: krusty's JVM backend reads a cross-file value-class package property as
/// its box (the getter returns `int`, the caller boxes it as `Integer` and casts that to `Meters`),
/// so it cannot be this program's oracle until that is fixed.
#[test]
fn a_package_value_class_property_is_stored_as_its_value() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                package demo
                @JvmInline value class Meters(val value: Int)
                @JvmInline value class Name(val text: String)
                var distance: Meters = Meters(40)
                val owner: Name = Name("OK")
                fun meters(value: Int): Meters = Meters(value)
                fun raw(distance: Meters): Int = distance.value
                fun text(name: Name): String = name.text
            "#,
            ),
            (
                "box",
                r#"
                package demo
                fun box(): String {
                    distance = meters(raw(distance) + 1)
                    if (raw(distance) != 41) return "FAIL: ${raw(distance)}"
                    return text(owner)
                }
            "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn an_explicit_backing_field_is_read_at_the_public_type() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                val total: Number
                    field = 41
                fun next(): Int = total + 1
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val seen: Number = total
                    if (seen !is Int) return "FAIL: not an Int"
                    return if (seen.toInt() == 41 && next() == 42) "OK" else "FAIL: $seen"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_source_written_package_accessor_runs_in_its_own_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                var backing: String = "O"
                var greeting: String
                    get() = backing + "K"
                    set(value) {
                        backing = value.substring(0, 1)
                    }
                val length: Int
                    get() = backing.length
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    greeting = "OX"
                    return if (length == 1) greeting else "FAIL: $length"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_member_val_is_read_from_the_file_that_declares_the_class() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Named(val n: Int, val label: String)
                fun named(): Named = Named(2, "OK")
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val value = named()
                    return if (value.n == 2 && value.label == "OK") "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_member_var_is_updated_from_another_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Counter(var n: Int)
                fun counter(): Counter = Counter(1)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val count = counter()
                    count.n += 2
                    return if (count.n == 3) "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn an_initializer_in_another_file_reads_a_member() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class X(val n: Int)
                val x: X = X(3)
            "#,
        ),
        (
            "more",
            r#"
                package demo
                class Z(val n: Int)
                val z: Z = Z(x.n)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String = if (z.n == 3) "OK" else "FAIL"
            "#,
        ),
    ]);
}

#[test]
fn a_member_assignment_evaluates_the_receiver_before_the_value() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Cell(var n: Int)
                fun cell(n: Int): Cell = Cell(n)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                var log: String = ""
                lateinit var held: Cell
                fun receiver(): Cell {
                    log = log + "r"
                    return held
                }
                fun value(): Int {
                    log = log + "v"
                    return 4
                }
                fun box(): String {
                    held = cell(1)
                    log = ""
                    receiver().n = value()
                    return if (log == "rv" && held.n == 4) "OK" else "FAIL: $log ${held.n}"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_lateinit_member_throws_until_assigned() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Holder {
                    lateinit var label: String
                }
                fun holder(): Holder = Holder()
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val held = holder()
                    try {
                        return held.label
                    } catch (e: kotlin.UninitializedPropertyAccessException) {
                        held.label = "OK"
                        return if (held.label == "OK") "OK" else "FAIL"
                    }
                }
            "#,
        ),
    ]);
}

#[test]
fn a_value_class_typed_member_is_carried_as_its_value() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                @JvmInline value class Meters(val value: Int)
                @JvmInline value class Name(val text: String)
                class Track(val length: Meters, var owner: Name)
                fun track(): Track = Track(Meters(41), Name("FAIL: unassigned"))
                fun name(text: String): Name = Name(text)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val track = track()
                    if (track.length.value != 41) return "FAIL: ${track.length.value}"
                    track.owner = name("OK")
                    return track.owner.text
                }
            "#,
        ),
    ]);
}

#[test]
fn a_generic_member_is_read_at_each_instantiation() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Box<T>(val value: T)
                class Slot<T>(var value: T)
                fun ints(): Box<Int> = Box(41)
                fun names(): Box<String> = Box("O")
                fun slot(): Slot<Int> = Slot(1)
                fun labels(): Slot<String> = Slot("")
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val count: Int = ints().value + 1
                    val slot = slot()
                    slot.value = slot.value + 1
                    val labels = labels()
                    labels.value = names().value + "K"
                    if (count != 42 || slot.value != 2) return "FAIL: $count ${slot.value}"
                    return labels.value
                }
            "#,
        ),
    ]);
}

#[test]
fn an_open_member_and_a_source_written_getter_run_in_the_declaring_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                open class Base {
                    open val label: String = "base"
                }
                class Derived : Base() {
                    override val label: String = "O"
                }
                class Twice(val n: Int) {
                    val doubled: Int
                        get() = n * 2
                }
                fun base(): Base = Derived()
                fun twice(): Twice = Twice(21)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    if (twice().doubled != 42) return "FAIL: ${twice().doubled}"
                    return base().label + "K"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_class_is_constructed_from_the_file_that_does_not_declare_it() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Named(val n: Int, val label: String)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val value = Named(2, "OK")
                    return if (value.n == 2 && value.label == "OK") "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_constructed_member_var_is_updated_from_the_calling_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Counter(var n: Int)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val count = Counter(1)
                    count.n += 2
                    return if (count.n == 3) "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn an_empty_constructor_runs_the_property_initializer() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Empty {
                    val n: Int = 1
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String = if (Empty().n == 1) "OK" else "FAIL"
            "#,
        ),
    ]);
}

#[test]
fn a_secondary_constructor_in_another_file_delegates() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Pair(val a: Int, val b: Int) {
                    constructor(n: Int) : this(n, n)
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val pair = Pair(2)
                    return if (pair.a == 2 && pair.b == 2) "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

/// Two constructors of one class with the same arity are two entry points: each is named from its
/// complete parameter list, not from its position among the class's constructors.
#[test]
fn overloaded_constructors_in_another_file_stay_distinct() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Pick {
                    val label: String
                    constructor(n: Int) { label = "int $n" }
                    constructor(text: String) { label = "text $text" }
                    constructor(n: Int?, text: String) { label = "both $n $text" }
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val labels = Pick(1).label + "|" + Pick("a").label + "|" + Pick(null, "b").label
                    return if (labels == "int 1|text a|both null b") "OK" else "FAIL: $labels"
                }
            "#,
        ),
    ]);
}

/// The arguments are evaluated in source order in the constructing file before the constructor
/// runs, and the constructor reads a package property of its own file at its initialized value.
#[test]
fn a_constructor_reads_its_own_file_after_the_arguments_are_evaluated() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                val prefix: String = "O"
                class Mark(val first: Int, val second: Int) {
                    val label: String = prefix + "K"
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                var order: String = ""
                fun argument(name: String, value: Int): Int {
                    order = order + name
                    return value
                }
                fun box(): String {
                    val mark = Mark(argument("a", 1), argument("b", 2))
                    if (order != "ab" || mark.first != 1 || mark.second != 2) return "FAIL: $order"
                    return mark.label
                }
            "#,
        ),
    ]);
}

#[test]
fn a_generic_class_is_constructed_at_each_instantiation_from_another_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Box<T>(val value: T)
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val count: Box<Int> = Box(41)
                    val held: Box<String> = Box("OK")
                    if (count.value + 1 != 42) return "FAIL: ${count.value}"
                    return held.value
                }
            "#,
        ),
    ]);
}

#[test]
fn an_inner_class_is_constructed_with_its_outer_instance() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                class Outer {
                    val label: String = "O"
                    inner class Inner(val n: Int) {
                        val label: String = this@Outer.label
                    }
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val inner = Outer().Inner(3)
                    return if (inner.n == 3 && inner.label == "O") "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_value_class_constructed_in_another_file_is_its_value() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                @JvmInline
                value class Name(val text: String)
                class Tag(val name: Name)
                fun text(name: Name): String = name.text
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String = text(Tag(Name("O")).name) + text(Name("K"))
            "#,
        ),
    ]);
}

#[test]
fn a_call_to_another_file_leaves_defaulted_arguments_out() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    var calls = 0
                    fun next(): Int { calls += 1; return calls }
                    fun count(): Int = calls
                    fun join(a: String, b: Int = a.length + 1, c: String = "c$b", d: Int = next()): String =
                        "$a/$b/$c/$d"
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val all = join("x", 5, "y", 9)
                        val trailing = join("ab")
                        val middle = join("q", c = "z")
                        val named = join(d = 7, a = "n")
                        val result = "$all $trailing $middle $named ${count()}"
                        return if (result == "x/5/y/9 ab/3/c3/1 q/2/z/2 n/2/c2/7 2") "OK" else result
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn an_extension_in_another_file_fills_its_defaults_from_the_receiver() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun String.pad(width: Int = length + 2, fill: Char = '.'): String {
                        var out = this
                        while (out.length < width) out += fill
                        return out
                    }
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val result = "ab".pad() + "|" + "c".pad(fill = '-') + "|" + "d".pad(4)
                        return if (result == "ab..|c--|d...") "OK" else result
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_default_reads_the_defining_files_top_level_state_after_its_initializer_ran() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun spell(): String = "O" + "K"
                    val greeting: String = spell()
                    fun greet(text: String = greeting): String = text
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = greet()
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_cross_file_defaulted_call_past_thirty_two_parameters_uses_the_next_mask_word() {
    let parameters: Vec<String> = (0..34).map(|k| format!("p{k}: Int = {k}")).collect();
    let sum: Vec<String> = (0..34).map(|k| format!("p{k}")).collect();
    let defs = format!(
        "package demo\nfun wide({}): Int = {}\n",
        parameters.join(", "),
        sum.join(" + ")
    );
    // 0 + 1 + … + 33 = 561; overriding p0 with 100 and p33 with 0 gives 561 + 100 - 33.
    expect_native_sources(
        &[
            ("defs", &defs),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val all = wide()
                        val some = wide(100, p33 = 0)
                        return if (all == 561 && some == 628) "OK" else "FAIL: $all $some"
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_number_returned_from_another_file_is_rendered_into_a_string_template() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun count(): Int = 2
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val result = "n ${count()}"
                        return if (result == "n 2") "OK" else result
                    }
                "#,
            ),
        ],
        "OK",
    );
}
