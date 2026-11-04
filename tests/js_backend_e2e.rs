//! The alternate `js` backend (krusty-ir → JavaScript) exercised end-to-end on Node: lower Kotlin to
//! backend-agnostic IR, emit JS, run it, and assert the result. Proves the IR is target-neutral — the
//! same lowering that feeds the JVM backend runs correctly on Node — and covers `src/js/mod.rs`.
//!
//! The frontend uses the target-neutral Kotlin builtin provider; only Node is an external runtime
//! dependency.

use super::common;
use krusty::ir::{Callee, EqualityMode, IrBinOp, IrConst, IrExpr, IrFile, IrFunction, IrIntrinsic};
use krusty::types::Ty;

/// Lower `src`, emit JS, append `console.log(box())`, run on Node. Returns Node's stdout, or `None`
/// to skip only when Node is unavailable. A compiler rejection is a test failure.
fn run(src: &str) -> Option<String> {
    let mut js = common::compile_js_in_process(src, "Main").unwrap_or_else(|diagnostics| {
        panic!(
            "JS production pipeline rejected source:\n{src}\n--- diagnostics ---\n{}",
            diagnostics.join("\n")
        )
    });
    js.push_str("\nconsole.log(box());\n");
    common::run_js(&js)
}

/// Assert Node prints `expected`; skip only if Node is unavailable.
fn check(src: &str, expected: &str) {
    match run(src) {
        Some(out) => assert_eq!(out, expected, "js output mismatch\n--- src ---\n{src}"),
        None => eprintln!("skipping js_backend_e2e: node is unavailable"),
    }
}

fn add_ir_function(ir: &mut IrFile, name: &str, value: u32, ret: Ty) {
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    ir.add_fun(IrFunction {
        name: name.to_string(),
        params: Vec::new(),
        ret,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
}

fn add_ulong_shift<'a>(
    ir: &mut IrFile,
    name: &'a str,
    op: IrBinOp,
    lhs: u64,
    rhs: i32,
    expected: &'a str,
) -> (&'a str, &'a str) {
    let lhs = ir.add_expr(IrExpr::Const(IrConst::ULong(lhs)));
    let rhs = ir.add_expr(IrExpr::Const(IrConst::Int(rhs)));
    let value = ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs });
    ir.set_logical_type(value, Ty::ULong);
    add_ir_function(ir, name, value, Ty::ULong);
    (name, expected)
}

#[test]
fn arithmetic_and_precedence() {
    check("fun box(): Int { return 2 + 3 * 4 - 10 / 2 }", "9");
}

#[test]
fn if_else_expression() {
    check(
        "fun box(): String { val x = 7; return if (x > 5) \"big\" else \"small\" }",
        "big",
    );
}

#[test]
fn checked_equality_mode_distinguishes_boxed_and_ieee_zero() {
    check(
        "fun structural(a: Any?, b: Any?): Boolean = a == b\n\
         fun ieee(a: Double, b: Double): Boolean = a == b\n\
         fun box(): String = if (!structural(-0.0, 0.0) && ieee(-0.0, 0.0)) \"OK\" else \"FAIL\"",
        "OK",
    );
}

#[test]
fn nullable_ulong_equality_preserves_null() {
    check(
        "fun box(): String {\n\
           val absent: ULong? = null\n\
           return if (absent != 0uL && 0uL != absent && absent == null) \"OK\" else \"FAIL\"\n\
         }",
        "OK",
    );
}

#[test]
fn ulong_backend_ir_uses_one_node_carrier() {
    let mut ir = IrFile::default();

    let checks = [
        add_ulong_shift(&mut ir, "shl64", IrBinOp::Shl, 1, 64, "1"),
        add_ulong_shift(
            &mut ir,
            "shlNegative",
            IrBinOp::Shl,
            1,
            -1,
            "9223372036854775808",
        ),
        add_ulong_shift(
            &mut ir,
            "shr64",
            IrBinOp::Ushr,
            0x8000_0000_0000_0000,
            64,
            "9223372036854775808",
        ),
        add_ulong_shift(
            &mut ir,
            "shr65",
            IrBinOp::Ushr,
            0x8000_0000_0000_0000,
            65,
            "4611686018427387904",
        ),
    ];
    let present_zero = ir.add_expr(IrExpr::Const(IrConst::ULong(0)));
    ir.set_logical_type(present_zero, Ty::nullable(Ty::ULong));
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let present_is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: present_zero,
        rhs: null,
    });
    add_ir_function(&mut ir, "presentIsNull", present_is_null, Ty::Boolean);
    let absent = ir.add_expr(IrExpr::Const(IrConst::Null));
    ir.set_logical_type(absent, Ty::nullable(Ty::ULong));
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let absent_is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: absent,
        rhs: null,
    });
    add_ir_function(&mut ir, "absentIsNull", absent_is_null, Ty::Boolean);
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let absent = ir.add_expr(IrExpr::Const(IrConst::Null));
    ir.set_logical_type(absent, Ty::nullable(Ty::ULong));
    let null_is_absent = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: null,
        rhs: absent,
    });
    add_ir_function(&mut ir, "nullIsAbsent", null_is_absent, Ty::Boolean);

    let size = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let array = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Intrinsic {
            operation: IrIntrinsic::PrimitiveArrayNew { element: Ty::ULong },
            ret: Ty::array(Ty::ULong),
        }),
        dispatch_receiver: None,
        args: vec![size],
    });
    let index = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let erased_read = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Intrinsic {
            operation: IrIntrinsic::ArrayGet,
            ret: Ty::nullable(Ty::obj("kotlin/Any")),
        }),
        dispatch_receiver: Some(array),
        args: vec![index],
    });
    let zero = ir.add_expr(IrExpr::Const(IrConst::ULong(0)));
    let erased_zero_equals_ulong = ir.add_expr(IrExpr::Equality {
        op: IrBinOp::Eq,
        mode: EqualityMode::Structural,
        lhs: erased_read,
        rhs: zero,
    });
    add_ir_function(
        &mut ir,
        "erasedZeroEqualsULong",
        erased_zero_equals_ulong,
        Ty::Boolean,
    );
    let size = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let array = ir.add_expr(IrExpr::NewArray {
        array_type: Ty::array(Ty::ULong),
        size,
    });
    let index = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let erased_read = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Intrinsic {
            operation: IrIntrinsic::ArrayGet,
            ret: Ty::nullable(Ty::obj("kotlin/Any")),
        }),
        dispatch_receiver: Some(array),
        args: vec![index],
    });
    let zero = ir.add_expr(IrExpr::Const(IrConst::ULong(0)));
    let new_array_zero_equals_ulong = ir.add_expr(IrExpr::Equality {
        op: IrBinOp::Eq,
        mode: EqualityMode::Structural,
        lhs: erased_read,
        rhs: zero,
    });
    add_ir_function(
        &mut ir,
        "newArrayZeroEqualsULong",
        new_array_zero_equals_ulong,
        Ty::Boolean,
    );

    let max = ir.add_expr(IrExpr::Const(IrConst::ULong(u64::MAX)));
    let as_int = ir.add_expr(IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::ImplicitCoercion,
        arg: max,
        type_operand: Ty::Int,
    });
    add_ir_function(&mut ir, "ulongToInt", as_int, Ty::Int);
    let wrapped = ir.add_expr(IrExpr::Const(IrConst::ULong(0x1_0000_0001)));
    let as_uint = ir.add_expr(IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::ImplicitCoercion,
        arg: wrapped,
        type_operand: Ty::UInt,
    });
    add_ir_function(&mut ir, "ulongToUInt", as_uint, Ty::UInt);
    let high = ir.add_expr(IrExpr::Const(IrConst::ULong(0x8000_0000_0000_0000)));
    let as_long = ir.add_expr(IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::ImplicitCoercion,
        arg: high,
        type_operand: Ty::Long,
    });
    let zero = ir.add_expr(IrExpr::Const(IrConst::Long(0)));
    let long_is_negative = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Lt,
        lhs: as_long,
        rhs: zero,
    });
    add_ir_function(
        &mut ir,
        "ulongToLongIsNegative",
        long_is_negative,
        Ty::Boolean,
    );
    let code_point = ir.add_expr(IrExpr::Const(IrConst::ULong(65)));
    let as_char = ir.add_expr(IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::ImplicitCoercion,
        arg: code_point,
        type_operand: Ty::Char,
    });
    add_ir_function(&mut ir, "ulongToChar", as_char, Ty::Char);
    let not_exact_in_float = ir.add_expr(IrExpr::Const(IrConst::ULong(16_777_217)));
    let as_float = ir.add_expr(IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::ImplicitCoercion,
        arg: not_exact_in_float,
        type_operand: Ty::Float,
    });
    add_ir_function(&mut ir, "ulongToFloat", as_float, Ty::Float);
    let five = ir.add_expr(IrExpr::Const(IrConst::Long(5)));
    let as_ulong = ir.add_expr(IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::ImplicitCoercion,
        arg: five,
        type_operand: Ty::ULong,
    });
    add_ir_function(&mut ir, "longToULong", as_ulong, Ty::ULong);

    let mut js = krusty::js::emit_file(&ir);
    let shift_checks = checks
        .into_iter()
        .map(|(name, expected)| format!("String({name}()) === \"{expected}\""))
        .collect::<Vec<_>>()
        .join(" && ");
    js.push_str(&format!(
        "\nconsole.log(({shift_checks} && !presentIsNull() && absentIsNull() && nullIsAbsent() && erasedZeroEqualsULong() && newArrayZeroEqualsULong() && ulongToInt() === -1 && ulongToUInt() === 1 && ulongToLongIsNegative() && ulongToChar() === \"A\" && ulongToFloat() === 16777216 && longToULong() === 5n) ? \"OK\" : \"FAIL\");\n"
    ));
    match common::run_js(&js) {
        Some(out) => assert_eq!(out, "OK", "js output mismatch\n--- js ---\n{js}"),
        None => eprintln!("skipping js_backend_e2e: node is unavailable"),
    }
}

#[test]
fn while_loop_accumulate() {
    check(
        "fun box(): Int { var i = 0; var s = 0; while (i < 5) { s = s + i; i = i + 1 }; return s }",
        "10",
    );
}

#[test]
fn do_while_loop() {
    check(
        "fun box(): Int { var i = 0; do { i = i + 1 } while (i < 3); return i }",
        "3",
    );
}

#[test]
fn for_range_loop() {
    check(
        "fun box(): Int { var s = 0; for (i in 1..4) { s = s + i }; return s }",
        "10",
    );
}

#[test]
fn labeled_break() {
    check(
        "fun box(): Int {\n\
         var s = 0\n\
         loop@ for (i in 0..10) {\n\
           if (i == 4) break@loop\n\
           s = s + i\n\
         }\n\
         return s\n\
         }",
        "6",
    );
}

#[test]
fn continue_in_loop() {
    check(
        "fun box(): Int {\n\
         var s = 0\n\
         for (i in 0..6) {\n\
           if (i % 2 == 1) continue\n\
           s = s + i\n\
         }\n\
         return s\n\
         }",
        "12",
    );
}

#[test]
fn if_elseif_else_statement() {
    check(
        "fun sign(n: Int): String {\n\
         var r = \"\"\n\
         if (n > 0) { r = \"pos\" } else if (n < 0) { r = \"neg\" } else { r = \"zero\" }\n\
         return r\n\
         }\n\
         fun box(): String { return sign(5) + sign(-2) + sign(0) }",
        "posnegzero",
    );
}

#[test]
fn class_field_and_method() {
    check(
        "class Counter(var n: Int) { fun inc(): Int { n = n + 1; return n } }\n\
         fun box(): Int { val c = Counter(10); c.inc(); return c.inc() }",
        "12",
    );
}

#[test]
fn default_arguments_use_semantic_omissions() {
    check(
        "fun join(first: String = \"O\", second: String = \"K\"): String = first + second\n\
         fun box(): String = join(second = \"K\")",
        "OK",
    );
}

#[test]
fn constructor_defaults_use_semantic_omissions() {
    check(
        "class Pair(val first: String = \"O\", val second: String)\n\
         fun box(): String = Pair(second = \"K\").first + Pair(second = \"K\").second",
        "OK",
    );
}

#[test]
fn body_property_jvm_defaults_remain_semantic_initializers() {
    // JVM storage starts as zero/null and may omit these physical stores. JavaScript fields start as
    // `undefined`, so common IR must retain the Kotlin initializers for this backend to realize.
    check(
        "class Defaults {\n\
           val count: Int = 0\n\
           val enabled: Boolean = false\n\
           val text: String? = null\n\
         }\n\
         fun box(): String {\n\
           val d = Defaults()\n\
           return if (d.count == 0 && d.enabled == false && d.text == null) \"OK\" else \"fail\"\n\
         }",
        "OK",
    );
}

#[test]
fn class_property_custom_accessors() {
    // A semantic PropertyRead/PropertyWrite is not necessarily a direct JavaScript field operation:
    // source-written accessor bodies remain real methods in the shared IR. The JS realization must call
    // those methods, just as the JVM realization calls the emitted accessor, or it silently bypasses the
    // user's getter/setter logic.
    check(
        "class Counter {\n\
           var raw: Int = 2\n\
           var scaled: Int = 0\n\
             get() = raw * 3\n\
             set(value) { raw = value + 1 }\n\
         }\n\
         fun box(): Int { val c = Counter(); c.scaled = 4; return c.scaled }",
        "15",
    );
}

#[test]
fn instanceof_and_cast() {
    check(
        "open class Animal\nclass Dog : Animal()\n\
         fun box(): String { val a: Animal = Dog(); return if (a is Dog) \"dog\" else \"no\" }",
        "dog",
    );
}

#[test]
fn unit_is_the_target_s_own_representation_of_it() {
    // `Unit` is `undefined` here rather than a class, so `is Unit` is an identity test against that
    // value. It answered `false` for everything while the target's own representation rule was
    // missing, and reaching the class path instead would be a ReferenceError: there is no `Unit`
    // class in the emitted JavaScript for `instanceof` to name.
    //
    // The value comes from a `Unit`-returning call rather than the `Unit` object, because this
    // backend's target-neutral builtin provider does not resolve that name — a separate gap, and
    // not one this rule needs in order to be stated.
    check(
        "fun record() {}\n\
         fun box(): String {\n\
         \x20   val unit: Any = record()\n\
         \x20   val other: Any = \"a\"\n\
         \x20   if (unit !is Unit) return \"fail: Unit is not Unit\"\n\
         \x20   if (other is Unit) return \"fail: a String is Unit\"\n\
         \x20   return \"OK\"\n\
         }",
        "OK",
    );
}

#[test]
fn no_value_is_an_instance_of_nothing_in_js() {
    // The `Nothing` rule is settled in common lowering, so this target gets it without a rule of
    // its own — which is the point of settling it there. The test is what makes that a claim.
    check(
        "fun box(): String {\n\
         \x20   val present: Any? = \"a\"\n\
         \x20   val absent: Any? = null\n\
         \x20   if (present is Nothing) return \"fail: a value is Nothing\"\n\
         \x20   if (present is Nothing?) return \"fail: a value is Nothing?\"\n\
         \x20   if (absent !is Nothing?) return \"fail: null is not Nothing?\"\n\
         \x20   return \"OK\"\n\
         }",
        "OK",
    );
}

#[test]
fn string_concat_and_tostring() {
    check(
        "fun box(): String { val n = 42; val s = \"ab\" + \"cd\"; return s + \"=\" + n.toString() }",
        "abcd=42",
    );
}

#[test]
fn nullable_string_plus_uses_kotlin_string_conversion() {
    check(
        "fun box(): String { val left: String? = null; val right: String? = null; return left + right }",
        "nullnull",
    );
}

#[test]
fn boolean_short_circuit_and_when() {
    check(
        "fun grade(n: Int): String = when {\n\
           n >= 90 -> \"A\"\n\
           n >= 80 -> \"B\"\n\
           else -> \"C\"\n\
         }\n\
         fun box(): String { return grade(85) + grade(95) + grade(10) }",
        "BAC",
    );
}

#[test]
fn int_array_sum() {
    check(
        "fun box(): Int {\n\
         val a = IntArray(4)\n\
         var i = 0\n\
         while (i < 4) { a[i] = i * i; i = i + 1 }\n\
         var s = 0\n\
         for (j in 0..3) { s = s + a[j] }\n\
         return s\n\
         }",
        "14",
    );
}

#[test]
fn recursion() {
    check(
        "fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)\n\
         fun box(): Int { return fib(10) }",
        "55",
    );
}

#[test]
fn member_recursion_with_checked_block_arguments() {
    check(
        "class C { tailrec fun f(n: Int): Int = if (n == 0) 0 else f(n - 1) }\n\
         fun box(): Int = C().f(3)",
        "0",
    );
}

#[test]
fn inherited_member_default_is_materialized_before_override_dispatch() {
    check(
        "open class A { open fun value(x: String = \"OK\"): String = x }\n\
         class B : A() { override fun value(x: String): String = x }\n\
         fun box(): String = B().value()",
        "OK",
    );
}

#[test]
fn top_level_property_and_null() {
    check(
        "val base: Int = 100\n\
         fun box(): String { val x: String? = null; return if (x == null) base.toString() else \"no\" }",
        "100",
    );
}
