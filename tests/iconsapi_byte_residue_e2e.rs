//! Byte-parity residue found byte-verifying intellij's `icons-api` module against kotlinc 2.4.10.
//! Each test isolates ONE divergence with a minimal fixture whose whole class file (or, for
//! metadata-only gaps, whose `@Metadata` payload) must equal the reference compiler's:
//!
//! * short float/double constants (`fconst_0/1/2`, `dconst_0/1`) — kotlinc emits the short form
//!   for the EXACT bit patterns of 0.0/1.0/2.0 (so not `-0.0`), mirroring `push_int`;
//! * `infix fun` sets `Function.flags` bit 9 (`IS_INFIX`) in `@Metadata`, facade and member alike;
//! * a class's `@Metadata` d2 interns its annotation strings LAST — after nested-class names, the
//!   companion, sealed subclasses, and the module name (measured on kotlinc 2.4.10);
//! * a `$default` stub's LineNumberTable points at the DECLARATION line (`fun …`), not the
//!   expression body's line, which differs whenever the body starts on the next line;
//! * a top-level extension property's accessors carry a LocalVariableTable naming the receiver
//!   `$this$<property>` (icons-api hit the primitive-receiver getters);
//! * an interface emits its members in SOURCE order — property accessors at the property's
//!   declared position — not functions-then-accessors.

use super::common;

fn assert_byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc(name, src, class) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

fn assert_metadata_identical(name: &str, src: &str, class: &str) {
    let classpath = [common::stdlib_jar()];
    let Some(result) = common::metadata_diff_against_kotlinc_cp(name, src, class, &classpath)
    else {
        eprintln!("skip ({name}: provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

// ---- F: short float/double constants ---------------------------------------------------------

/// kotlinc (ASM's `InstructionAdapter`) emits `fconst_0/1/2` for floats whose BITS equal 0.0f,
/// 1.0f, 2.0f — a bit-pattern test, so `-0.0f` stays `ldc` — and `dconst_0/1` for doubles the
/// same way. Everything else loads from the pool. (icons-api: `Color$Companion.<clinit>`.)
#[test]
fn short_float_and_double_constants_use_const_ops() {
    assert_byte_identical(
        "iarShortFloat",
        "fun floats(): Float {\n\
         \x20   val a = 0.0f\n\
         \x20   val b = 1.0f\n\
         \x20   val c = 2.0f\n\
         \x20   val d = -0.0f\n\
         \x20   val e = 3.0f\n\
         \x20   return a + b + c + d + e\n\
         }\n\
         \n\
         fun doubles(): Double {\n\
         \x20   val a = 0.0\n\
         \x20   val b = 1.0\n\
         \x20   val c = 2.0\n\
         \x20   val d = -0.0\n\
         \x20   return a + b + c + d\n\
         }\n",
        "IarShortFloatKt",
    );
}

// ---- G: `infix` in Function.flags ------------------------------------------------------------

/// A top-level `infix fun` publishes `Function.flags` bit 9 (`IS_INFIX`) in the facade metadata —
/// kotlinc's flags word is 518 (public visibility | bit 9). Without it a consuming module rejects
/// the infix call form. (icons-api: `IconModifier.then`.)
#[test]
fn facade_infix_function_records_is_infix_flag() {
    assert_metadata_identical(
        "iarInfixFacade",
        "class Modifier(val bits: Int)\n\
         \n\
         infix fun Modifier.then(other: Modifier): Modifier = Modifier(bits or other.bits)\n",
        "IarInfixFacadeKt",
    );
}

/// The same flag on a CLASS member's `Function` record.
#[test]
fn member_infix_function_records_is_infix_flag() {
    assert_metadata_identical(
        "iarInfixMember",
        "class Modifier(val bits: Int) {\n\
         \x20   infix fun and(other: Modifier): Modifier = Modifier(bits or other.bits)\n\
         }\n",
        "Modifier",
    );
}

// ---- H: class annotation strings intern after the structural tail ----------------------------

/// kotlinc's d2 for an annotated class with nested classes interns the ANNOTATION strings last:
/// `[…, "Nested", "LMarker;", "tag", "hello"]`. The same rule puts them after the companion name,
/// sealed subclass ids, and the `-module-name` string (measured on 2.4.10; icons-api's
/// `ModifiersFactory` diverged on annotation-vs-module-name order).
#[test]
fn class_annotation_strings_intern_after_nested_names() {
    assert_metadata_identical(
        "iarAnnOrder",
        "annotation class Marker(val tag: String)\n\
         \n\
         @Marker(\"hello\")\n\
         class Outer {\n\
         \x20   fun make(): Int = 1\n\
         \x20   class Nested\n\
         }\n",
        "Outer",
    );
}

/// The icons-api divergence proper: compiled with `-module-name`, kotlinc interns the MODULE NAME
/// string before the annotation strings (`[…, "Nested", "<module>", "LMarker;", "tag", "hello"]`);
/// krusty interned the annotations first. Both sides compile under an explicit module name here —
/// the default-module test above never exercises the module string at all.
#[test]
fn class_annotation_strings_intern_after_the_module_name() {
    let Some(result) = common::metadata_diff_against_kotlinc_module(
        "iarAnnModOrder",
        "annotation class Marker(val tag: String)\n\
         \n\
         @Marker(\"hello\")\n\
         class Outer {\n\
         \x20   fun make(): Int = 1\n\
         \x20   class Nested\n\
         }\n",
        "Outer",
        &[common::stdlib_jar()],
        "icons.mod.test",
    ) else {
        eprintln!("skip (iarAnnModOrder: provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

// ---- I: `$default` stub line = declaration line ----------------------------------------------

/// When an expression body starts on the line AFTER the signature, the real method's
/// LineNumberTable maps to the expression's line but the synthetic `$default` stub maps to the
/// DECLARATION line. (icons-api: `IconScaleKt.fitArea$default` — 57, body on 58.)
#[test]
fn default_stub_line_is_the_declaration_line() {
    assert_byte_identical(
        "iarStubLine",
        "class Wide(val w: Int)\n\
         \n\
         fun fitArea(width: Int, height: Int, relative: Boolean = true): Wide =\n\
         \x20   Wide(width + height)\n",
        "IarStubLineKt",
    );
}

// ---- J: extension property accessors carry a LocalVariableTable ------------------------------

/// A top-level extension property getter is a static taking the receiver as parameter 0, and
/// kotlinc gives it a LocalVariableTable entry `$this$<property>` covering the whole method.
/// (icons-api: `IconUnitsKt.getDp(int/double/float)` had none.)
#[test]
fn primitive_receiver_extension_property_getter_has_lvt() {
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "iarExtPropLvt",
        "class Foo(val v: Int)\n\
         \n\
         val Int.dp: Foo\n\
         \x20   get() = Foo(this)\n\
         val Double.dp2: Foo\n\
         \x20   get() = Foo(this.toInt())\n\
         val Float.dp3: Foo\n\
         \x20   get() = Foo(this.toInt())\n",
        "IarExtPropLvtKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skip (iarExtPropLvt: reference toolchain unavailable)");
        return;
    };
    result.unwrap_or_else(|e| panic!("{e}"));
}

/// A `var` extension property's SETTER also names the receiver and its `value` parameter. The
/// remaining LNT residue on block-bodied setters (kotlinc's closing-brace entry) is a separate
/// line-mapping slice, so this asserts the LocalVariableTable shape on krusty's own output.
#[test]
fn var_extension_property_setter_has_lvt() {
    let src = "var Int.scaled: Int\n\
               \x20   get() = this * 2\n\
               \x20   set(value) {\n\
               \x20       if (value < 0) return\n\
               \x20   }\n";
    let classes = common::compile_in_process(src, "IarExtPropSet", &[], None)
        .expect("iarExtPropSet: krusty failed to compile");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "IarExtPropSetKt")
        .expect("IarExtPropSetKt was not emitted");
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join("IarExtPropSetKt.class");
    std::fs::write(&class_file, bytes).unwrap();
    let text = common::javap(&["-c", "-l", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    for needle in ["$this$scaled", "value"] {
        assert!(
            text.lines().any(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                fields.len() == 5 && fields[3] == needle && fields[4] == "I"
            }),
            "LVT entry {needle}: I missing from accessors:\n{text}"
        );
    }
}

// ---- B: interface members in source order ----------------------------------------------------

/// kotlinc emits interface members in SOURCE declaration order — a property's accessors sit at
/// the property's position between the functions (getter before setter). krusty grouped all
/// functions first. (icons-api: `DisplayPoint`/`Pixel` placed `getValue()` last.)
#[test]
fn interface_members_emit_in_source_order() {
    assert_byte_identical(
        "iarIfaceOrder",
        "interface Mix {\n\
         \x20   fun alpha(): Int\n\
         \x20   val value: Double\n\
         \x20   fun beta(): Int\n\
         \x20   var w: Int\n\
         \x20   fun gamma(): Int\n\
         }\n",
        "Mix",
    );
}
