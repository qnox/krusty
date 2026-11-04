use crate::ir::{Callee, ClassId, ExprId, FunId, IrConst, IrExpr, IrFile};
use crate::types::{type_name, Ty};

fn lowered(source: &str) -> IrFile {
    let mut ir = crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(source, "Enums");
    super::realize(&mut ir);
    ir
}

fn class(ir: &IrFile, name: &str) -> ClassId {
    ir.class_id_by_name(type_name(name))
        .expect("the class is lowered")
}

fn function(ir: &IrFile, name: &str) -> FunId {
    ir.functions
        .iter()
        .position(|function| function.name == name)
        .unwrap_or_else(|| panic!("no function `{name}`")) as FunId
}

fn body(ir: &IrFile, function: FunId) -> ExprId {
    ir.functions[function as usize]
        .body
        .expect("a realized function has a body")
}

fn statements(ir: &IrFile, block: ExprId) -> Vec<ExprId> {
    match ir.expr(block) {
        IrExpr::Block { stmts, .. } => stmts.clone(),
        other => panic!("not a block: {other:?}"),
    }
}

/// The emitted expressions of `ir` that are still one of the four enum use nodes.
fn remaining_uses(ir: &IrFile) -> usize {
    crate::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(ir, root))
        .filter(|&expression| {
            matches!(
                ir.expr(expression),
                IrExpr::EnumEntry { .. }
                    | IrExpr::EnumValues { .. }
                    | IrExpr::EnumValueOf { .. }
                    | IrExpr::EnumEntries { .. }
            )
        })
        .count()
}

/// The initializer's guarded build and the handler's statements: `if (!initialized) try { build }
/// catch (t: Throwable) { reset; throw t }`.
fn initializer_parts(ir: &IrFile, initializer: FunId) -> (Vec<ExprId>, Vec<ExprId>, u32) {
    let [when] = statements(ir, body(ir, initializer))[..] else {
        panic!("the initializer is one guarded build");
    };
    let IrExpr::When { branches } = ir.expr(when) else {
        panic!("the build is guarded by the flag");
    };
    let [(Some(_), guarded)] = branches[..] else {
        panic!("one guarded branch and no else");
    };
    let IrExpr::Try { body, catches, .. } = ir.expr(guarded) else {
        panic!("the build is protected");
    };
    let [catch] = &catches[..] else {
        panic!("one handler");
    };
    assert_eq!(catch.ty, Ty::obj_name(type_name("kotlin/Throwable")));
    (statements(ir, *body), statements(ir, catch.body), catch.var)
}

const COLORS: &str = "\
enum class Color(val rgb: Int = 0) {
    RED(1), GREEN, BLUE(3) { override fun bright() = true };
    constructor(text: String) : this(text.length)
    open fun bright() = false
    companion object { val first = RED }
}

enum class Shade { LIGHT, DARK }

fun box(): String {
    val c = Color.GREEN
    val all = Color.values()
    val found = Color.valueOf(\"RED\")
    val list = Color.entries
    return if (c == Color.GREEN && all.size == 3 && found == Color.RED && list.size == 3) \"OK\" else \"fail\"
}
";

#[test]
fn every_use_of_an_enum_of_this_file_is_realized() {
    let ir = lowered(COLORS);
    assert_eq!(remaining_uses(&ir), 0);
    let color = class(&ir, "Color");
    let shade = class(&ir, "Shade");
    assert_eq!(
        ir.enum_initializers.get(&color),
        Some(&function(&ir, "Color$initEntries"))
    );
    assert_eq!(
        ir.enum_initializers.get(&shade),
        Some(&function(&ir, "Shade$initEntries"))
    );
}

#[test]
fn the_initializer_builds_every_constant_in_order_then_entries_then_the_companion() {
    let ir = lowered(COLORS);
    let initializer = function(&ir, "Color$initEntries");
    let (build, reset, caught) = initializer_parts(&ir, initializer);

    // The flag first, so a constructor reaching back into the enum does not start it again.
    let IrExpr::SetStatic { index: flag, value } = ir.expr(build[0]) else {
        panic!("the flag is set first");
    };
    assert!(matches!(
        ir.expr(*value),
        IrExpr::Const(IrConst::Boolean(true))
    ));

    let constants: Vec<(u32, String, u32, ExprId)> = build
        .iter()
        .filter_map(|&statement| match ir.expr(statement) {
            IrExpr::SetStatic { index, value } => match ir.expr(*value) {
                IrExpr::EnumConstant {
                    name,
                    ordinal,
                    construction,
                    ..
                } => Some((*index, name.to_string(), *ordinal, *construction)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let names: Vec<(&str, u32)> = constants
        .iter()
        .map(|(_, name, ordinal, _)| (name.as_str(), *ordinal))
        .collect();
    assert_eq!(names, [("RED", 0), ("GREEN", 1), ("BLUE", 2)]);
    let slots: Vec<u32> = constants.iter().map(|(slot, ..)| *slot).collect();

    // `RED(1)` supplies its argument, `GREEN` leaves it to its default, and `BLUE` is built
    // through its body's subclass, which carries the arguments itself.
    let IrExpr::New {
        internal,
        args,
        ctor_params,
        defaults,
        ..
    } = ir.expr(constants[0].3)
    else {
        panic!("a constant is a construction");
    };
    assert_eq!(
        (*internal, args.len(), ctor_params, &defaults[..]),
        (type_name("Color"), 1, &None, &[][..])
    );
    let IrExpr::New { args, defaults, .. } = ir.expr(constants[1].3) else {
        panic!("a constant is a construction");
    };
    assert_eq!((args.len(), &defaults[..]), (0, &[0][..]));
    let IrExpr::New { internal, args, .. } = ir.expr(constants[2].3) else {
        panic!("a constant is a construction");
    };
    let subclass = ir.classes[class(&ir, "Color") as usize].enum_entries[2]
        .subclass
        .expect("BLUE has a body");
    assert_eq!((*internal, args.len()), (subclass, 0));

    // After the constants: the entries list over their slots, then the companion.
    let tail = &build[build.len() - 2..];
    let IrExpr::SetStatic {
        index: entries,
        value,
    } = ir.expr(tail[0])
    else {
        panic!("entries follow the constants");
    };
    let IrExpr::EnumEntriesList { values, .. } = ir.expr(*value) else {
        panic!("the entries list");
    };
    assert_eq!(array_slots(&ir, *values), slots);
    assert!(matches!(
        ir.expr(tail[1]),
        IrExpr::SingletonValue { classifier }
            if ir.classes[class(&ir, "Color") as usize].companion_class == Some(*classifier)
    ));

    // A failure withdraws everything and rethrows what it caught.
    let cleared: Vec<(u32, bool)> = reset[..reset.len() - 1]
        .iter()
        .map(|&statement| match ir.expr(statement) {
            IrExpr::SetStatic { index, value } => (
                *index,
                matches!(ir.expr(*value), IrExpr::Const(IrConst::Null)),
            ),
            other => panic!("the reset only writes slots: {other:?}"),
        })
        .collect();
    let mut expected = vec![(*flag, false)];
    expected.extend(slots.iter().map(|&slot| (slot, true)));
    expected.push((*entries, true));
    assert_eq!(cleared, expected);
    let IrExpr::Throw { operand } = ir.expr(reset[reset.len() - 1]) else {
        panic!("the reset rethrows");
    };
    assert!(matches!(ir.expr(*operand), IrExpr::GetValue(var) if *var == caught));
}

/// The slots a `values` array reads, in order.
fn array_slots(ir: &IrFile, array: ExprId) -> Vec<u32> {
    let IrExpr::Vararg {
        elements, spreads, ..
    } = ir.expr(array)
    else {
        panic!("a fresh array literal");
    };
    assert!(spreads.iter().all(|spread| !spread));
    elements
        .iter()
        .map(|&element| match ir.expr(element) {
            IrExpr::GetStatic(slot) => *slot,
            other => panic!("an element reads a slot: {other:?}"),
        })
        .collect()
}

fn calls_initializer(ir: &IrFile, statement: ExprId, initializer: FunId) -> bool {
    matches!(
        ir.expr(statement),
        IrExpr::Call { callee, args, .. }
            if matches!(**callee, Callee::Local(function) if function == initializer)
                && args.is_empty()
    )
}

#[test]
fn values_and_value_of_build_the_enum_first() {
    let ir = lowered(COLORS);
    let initializer = function(&ir, "Shade$initEntries");

    let values = statements(&ir, body(&ir, function(&ir, "Shade$values")));
    assert!(calls_initializer(&ir, values[0], initializer));
    let IrExpr::Return(Some(array)) = ir.expr(values[1]) else {
        panic!("values() answers the array");
    };
    assert_eq!(array_slots(&ir, *array).len(), 2);

    let value_of = function(&ir, "Shade$valueOf");
    assert_eq!(ir.functions[value_of as usize].params, [Ty::String]);
    let statements = statements(&ir, body(&ir, value_of));
    assert!(calls_initializer(&ir, statements[0], initializer));
    let IrExpr::Return(Some(selected)) = ir.expr(statements[1]) else {
        panic!("valueOf answers the selected constant");
    };
    let IrExpr::When { branches } = ir.expr(*selected) else {
        panic!("a comparison per constant");
    };
    let compared: Vec<String> = branches
        .iter()
        .filter_map(|(condition, _)| *condition)
        .map(|condition| match ir.expr(condition) {
            IrExpr::Equality { lhs, rhs, .. } => {
                assert!(matches!(ir.expr(*lhs), IrExpr::GetValue(0)));
                match ir.expr(*rhs) {
                    IrExpr::Const(IrConst::String(text)) => text.to_lossy(),
                    other => panic!("compared with a name: {other:?}"),
                }
            }
            other => panic!("an equality: {other:?}"),
        })
        .collect();
    assert_eq!(compared, ["LIGHT", "DARK"]);
    let (None, missing) = branches[branches.len() - 1] else {
        panic!("the last branch is the failure");
    };
    assert!(matches!(
        ir.expr(missing),
        IrExpr::NoSuchEnumConstant { name, .. } if matches!(ir.expr(*name), IrExpr::GetValue(0))
    ));
}

#[test]
fn a_constant_read_builds_the_enum_and_reads_its_slot() {
    let ir = lowered(
        "enum class Shade { LIGHT, DARK }\n\
         fun box(): String = if (Shade.DARK.ordinal == 1) \"OK\" else \"fail\"\n",
    );
    let initializer = function(&ir, "Shade$initEntries");
    let reads: Vec<(Vec<ExprId>, ExprId)> = crate::ir::emitted_body_roots(&ir)
        .into_iter()
        .filter(|&root| root == body(&ir, function(&ir, "box")))
        .flat_map(|root| crate::ir::value_namespace_expressions(&ir, root))
        .filter_map(|expression| match ir.expr(expression) {
            IrExpr::Block {
                stmts,
                value: Some(value),
            } if matches!(ir.expr(*value), IrExpr::GetStatic(_)) => Some((stmts.clone(), *value)),
            _ => None,
        })
        .collect();
    let [(stmts, value)] = &reads[..] else {
        panic!("one constant read: {reads:?}");
    };
    assert!(calls_initializer(&ir, stmts[0], initializer));
    let (build, ..) = initializer_parts(&ir, initializer);
    let dark = build
        .iter()
        .find_map(|&statement| match ir.expr(statement) {
            IrExpr::SetStatic { index, value } => match ir.expr(*value) {
                IrExpr::EnumConstant { name, .. } if &**name == "DARK" => Some(*index),
                _ => None,
            },
            _ => None,
        })
        .expect("DARK has a slot");
    assert!(matches!(ir.expr(*value), IrExpr::GetStatic(slot) if *slot == dark));
}

#[test]
fn an_enum_another_file_declares_keeps_its_use_nodes() {
    let mut ir = crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(
        "enum class Shade { LIGHT }\nfun box(): String = \"OK\"\n",
        "Enums",
    );
    let elsewhere = ir.add_expr(IrExpr::EnumEntry {
        classifier: type_name("other/Elsewhere"),
        name: "ONE".into(),
    });
    let root = body(&ir, function(&ir, "box"));
    let IrExpr::Block { stmts, .. } = &mut ir.exprs[root as usize] else {
        panic!("box has a block body");
    };
    stmts.push(elsewhere);
    super::realize(&mut ir);
    assert!(matches!(ir.expr(elsewhere), IrExpr::EnumEntry { .. }));
}

/// `enumEntries<E>()`, once inline specialization has named the enum, reads the same slot as
/// `E.entries`.
#[test]
fn the_enum_entries_intrinsic_reads_the_entries_slot() {
    let mut ir = crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(
        "enum class Shade { LIGHT, DARK }\nfun box(): String = \"OK\"\n",
        "Enums",
    );
    let shade = type_name("Shade");
    let intrinsic = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Intrinsic {
            operation: crate::ir::IrIntrinsic::EnumEntries {
                classifier: Ty::obj_name(shade),
            },
            ret: super::initializer::entries_type(shade),
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let root = body(&ir, function(&ir, "box"));
    let IrExpr::Block { stmts, .. } = &mut ir.exprs[root as usize] else {
        panic!("box has a block body");
    };
    stmts.push(intrinsic);
    super::realize(&mut ir);

    let initializer = function(&ir, "Shade$initEntries");
    let (build, ..) = initializer_parts(&ir, initializer);
    let entries = build
        .iter()
        .find_map(|&statement| match ir.expr(statement) {
            IrExpr::SetStatic { index, value }
                if matches!(ir.expr(*value), IrExpr::EnumEntriesList { .. }) =>
            {
                Some(*index)
            }
            _ => None,
        })
        .expect("the entries are stored");
    let IrExpr::Block {
        stmts,
        value: Some(value),
    } = ir.expr(intrinsic)
    else {
        panic!(
            "the intrinsic is realized in place: {:?}",
            ir.expr(intrinsic)
        );
    };
    assert!(calls_initializer(&ir, stmts[0], initializer));
    assert!(matches!(ir.expr(*value), IrExpr::GetStatic(slot) if *slot == entries));
}

/// A default is evaluated by the target, in an entry or a constructor's frame, so an enum constant
/// it reads is realized like one in a body.
#[test]
fn defaults_reading_a_constant_are_realized() {
    let ir = lowered(
        "enum class Shade { LIGHT, DARK }\n\
         fun pick(shade: Shade = Shade.DARK) = shade\n\
         class Holder(val shade: Shade = Shade.LIGHT)\n\
         annotation class Tagged(val shade: Shade = Shade.DARK)\n\
         fun box(): String = if (pick() == Holder().shade) \"fail\" else \"OK\"\n",
    );
    let mut defaults = super::uses::function_defaults(&ir);
    defaults.extend(super::uses::constructor_defaults(&ir));
    assert_eq!(defaults.len(), 3);
    let reads: Vec<bool> = defaults
        .iter()
        .map(|&default| {
            crate::ir::value_namespace_expressions(&ir, default)
                .into_iter()
                .any(|expression| matches!(ir.expr(expression), IrExpr::EnumEntry { .. }))
        })
        .collect();
    assert_eq!(reads, [false, false, false]);
}
