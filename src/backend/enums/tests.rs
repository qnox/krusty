//! The shared enum realization over common IR built by hand: what it makes of each enum, and how
//! it rewrites the uses it finds.

use crate::ir::{
    Callee, ClassId, EnumValueOfDeclaration, ExprId, FnParamInfo, FunId, IrConst,
    IrConstructorTarget, IrEnumEntry, IrExpr, IrFile, IrFunction,
};
use crate::types::{type_name, Ty};

fn entry(name: &str, args: Vec<ExprId>, default_parameters: Vec<u32>) -> IrEnumEntry {
    IrEnumEntry {
        name: name.to_owned(),
        diagnostic_span: None,
        argument_prelude: Vec::new(),
        args,
        constructor_parameter_types: vec![Ty::Int],
        constructor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
        default_parameters,
        decl_line: 0,
        source_order: 0,
        subclass: None,
    }
}

/// An enum class `name` of this file with these constants.
fn enum_class(ir: &mut IrFile, name: &str, entries: Vec<IrEnumEntry>) -> ClassId {
    let mut class = crate::ir::test_support::blank_class(name);
    class.superclass = crate::types::wk::kotlin_enum();
    class.is_enum = true;
    class.enum_entries = entries;
    ir.add_class(class)
}

fn declare(ir: &mut IrFile, name: &str, params: Vec<Ty>, body: Vec<ExprId>) -> FunId {
    let body = ir.add_expr(IrExpr::Block {
        stmts: body,
        value: None,
    });
    ir.add_fun(IrFunction {
        name: name.to_owned(),
        params,
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    })
}

fn read(ir: &mut IrFile, classifier: &str, name: &str) -> ExprId {
    ir.add_expr(IrExpr::EnumEntry {
        classifier: type_name(classifier),
        name: name.into(),
    })
}

/// `enum class Shade { LIGHT, DARK }`.
fn shade(ir: &mut IrFile) -> ClassId {
    let entries = vec![
        entry("LIGHT", Vec::new(), Vec::new()),
        entry("DARK", Vec::new(), Vec::new()),
    ];
    enum_class(ir, "Shade", entries)
}

/// `enum class Color(val rgb: Int = 0)` with `RED(1)`, `GREEN` (its default) and `BLUE(3)` with a
/// body, a companion object, and `enum class Shade { LIGHT, DARK }`, used by `box()` through each
/// of the four use nodes.
fn colors() -> IrFile {
    let mut ir = IrFile::default();
    let red = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let mut blue = entry("BLUE", Vec::new(), Vec::new());
    blue.subclass = Some(type_name("Color$BLUE"));
    let entries = vec![
        entry("RED", vec![red], Vec::new()),
        entry("GREEN", Vec::new(), vec![0]),
        blue,
    ];
    let color = enum_class(&mut ir, "Color", entries);
    let mut body = crate::ir::test_support::blank_class("Color$BLUE");
    body.superclass = type_name("Color");
    ir.add_class(body);
    let mut companion = crate::ir::test_support::blank_class("Color$Companion");
    companion.is_object = true;
    ir.add_class(companion);
    ir.classes[color as usize].companion_class = Some(type_name("Color$Companion"));
    shade(&mut ir);

    let green = read(&mut ir, "Color", "GREEN");
    let values = ir.add_expr(IrExpr::EnumValues {
        classifier: type_name("Color"),
    });
    let name = ir.add_expr(IrExpr::Const(IrConst::String("RED".into())));
    let value_of = ir.add_expr(IrExpr::EnumValueOf {
        classifier: type_name("Color"),
        arg: name,
        declaration: EnumValueOfDeclaration::Member,
    });
    let entries = ir.add_expr(IrExpr::EnumEntries {
        classifier: type_name("Color"),
    });
    declare(
        &mut ir,
        "box",
        Vec::new(),
        vec![green, values, value_of, entries],
    );
    ir
}

fn lowered(ir: IrFile) -> IrFile {
    lowered_for(ir, super::Contract::KotlinNative)
}

fn lowered_for(mut ir: IrFile, contract: super::Contract) -> IrFile {
    super::realize(
        &mut ir,
        contract,
        Some(super::EntriesFactory::Selected(factory())),
        &mut super::super::DependencyExportSignatures::default(),
    )
    .expect("the enums are realized");
    ir
}

/// The `enumEntries(Array)` resolution selected, as a provider identity.
fn factory() -> crate::fir::EnumEntriesFactory {
    crate::fir::EnumEntriesFactory {
        function: crate::fir::ExternalCallableId::from_raw(41),
    }
}

/// The function a call of this file's own function calls, if `expression` is one.
fn local_callee(ir: &IrFile, expression: ExprId) -> Option<FunId> {
    match ir.expr(expression) {
        IrExpr::Call { callee, .. } => match **callee {
            Callee::Local(function) => Some(function),
            _ => None,
        },
        _ => None,
    }
}

/// The slot the Native contract's initializer stores the `entries` list of `name` in.
fn runtime_entries_slot(ir: &IrFile, name: &str) -> u32 {
    let (build, ..) = initializer_parts(ir, function(ir, &format!("{name}$initEntries")));
    build
        .iter()
        .find_map(|&statement| match ir.expr(statement) {
            IrExpr::SetStatic { index, value }
                if matches!(ir.expr(*value), IrExpr::EnumEntriesList { .. }) =>
            {
                Some(*index)
            }
            _ => None,
        })
        .expect("the initializer stores the entries")
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

#[test]
fn every_use_of_an_enum_of_this_file_is_realized() {
    let ir = lowered(colors());
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

/// The constants an initializer's build constructs, in order: each one's published slot, name,
/// ordinal and construction.
fn constructed(ir: &IrFile, build: &[ExprId]) -> Vec<(u32, String, u32, ExprId)> {
    build
        .iter()
        .filter_map(|&statement| match ir.expr(statement) {
            IrExpr::EnumConstant {
                name,
                ordinal,
                construction,
                published,
                ..
            } => Some((
                published.expect("a constant is published before it is constructed"),
                name.to_string(),
                *ordinal,
                *construction,
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn the_initializer_publishes_every_constant_then_constructs_each_then_entries_then_the_companion() {
    let ir = lowered(colors());
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

    // Then every constant allocated and published, before any constructor runs: an instance of
    // the enum, or of the constant's body subclass.
    let subclass = ir.classes[class(&ir, "Color") as usize].enum_entries[2]
        .subclass
        .expect("BLUE has a body");
    let published: Vec<(u32, crate::types::TypeName)> = build[1..4]
        .iter()
        .map(|&statement| match ir.expr(statement) {
            IrExpr::SetStatic { index, value } => match ir.expr(*value) {
                IrExpr::EnumAllocation { instance, .. } => (*index, *instance),
                other => panic!("a constant's allocation: {other:?}"),
            },
            other => panic!("a constant's publication: {other:?}"),
        })
        .collect();
    let instances: Vec<_> = published.iter().map(|(_, instance)| *instance).collect();
    assert_eq!(
        instances,
        [type_name("Color"), type_name("Color"), subclass]
    );

    // Then each constructed in place, in declaration order.
    let constants = constructed(&ir, &build[4..]);
    let names: Vec<(&str, u32)> = constants
        .iter()
        .map(|(_, name, ordinal, _)| (name.as_str(), *ordinal))
        .collect();
    assert_eq!(names, [("RED", 0), ("GREEN", 1), ("BLUE", 2)]);
    let slots: Vec<u32> = constants.iter().map(|(slot, ..)| *slot).collect();
    let publications: Vec<u32> = published.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(slots, publications);

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
    let ir = lowered(colors());
    let initializer = function(&ir, "Shade$initEntries");

    let (build, ..) = initializer_parts(&ir, initializer);
    let slots: Vec<u32> = constructed(&ir, &build)
        .iter()
        .map(|(slot, ..)| *slot)
        .collect();

    // A fresh array, each constant stored at its own ordinal, visiting them in name order, as
    // Kotlin/Native's `valuesForEnum` does over its name-sorted `VALUES`.
    let values = statements(&ir, body(&ir, function(&ir, "Shade$values")));
    assert!(calls_initializer(&ir, values[0], initializer));
    let IrExpr::Variable {
        index: array,
        init: Some(fresh),
        ..
    } = ir.expr(values[1])
    else {
        panic!("values() declares its array");
    };
    assert!(matches!(
        ir.expr(*fresh),
        IrExpr::NewArray { size, .. } if matches!(ir.expr(*size), IrExpr::Const(IrConst::Int(2)))
    ));
    let stored: Vec<u32> = values[2..values.len() - 1]
        .iter()
        .map(|&statement| match ir.expr(statement) {
            IrExpr::Call {
                callee,
                dispatch_receiver: Some(target),
                args,
            } => {
                assert!(matches!(
                    **callee,
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::ArraySet,
                        ..
                    }
                ));
                assert!(matches!(ir.expr(*target), IrExpr::GetValue(index) if index == array));
                let [ordinal, element] = args[..] else {
                    panic!("a store of one element at an index");
                };
                let IrExpr::Call {
                    callee,
                    dispatch_receiver: Some(constant),
                    ..
                } = ir.expr(ordinal)
                else {
                    panic!("the index is the constant's ordinal");
                };
                assert!(matches!(
                    **callee,
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::EnumOrdinal,
                        ..
                    }
                ));
                let (IrExpr::GetStatic(read), IrExpr::GetStatic(slot)) =
                    (ir.expr(*constant), ir.expr(element))
                else {
                    panic!("the constant is read from its slot");
                };
                assert_eq!(read, slot);
                *slot
            }
            other => panic!("an element store: {other:?}"),
        })
        .collect();
    assert_eq!(stored, [slots[1], slots[0]], "DARK, then LIGHT");
    assert!(matches!(
        ir.expr(values[values.len() - 1]),
        IrExpr::Return(Some(answer)) if matches!(ir.expr(*answer), IrExpr::GetValue(index) if index == array)
    ));

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
    let mut ir = IrFile::default();
    shade(&mut ir);
    let dark_read = read(&mut ir, "Shade", "DARK");
    declare(&mut ir, "box", Vec::new(), vec![dark_read]);
    let ir = lowered(ir);

    let initializer = function(&ir, "Shade$initEntries");
    let IrExpr::Block {
        stmts,
        value: Some(value),
    } = ir.expr(dark_read)
    else {
        panic!("the read is realized in place: {:?}", ir.expr(dark_read));
    };
    assert!(calls_initializer(&ir, stmts[0], initializer));
    let (build, ..) = initializer_parts(&ir, initializer);
    let dark = constructed(&ir, &build)
        .iter()
        .find_map(|(slot, name, ..)| (name == "DARK").then_some(*slot))
        .expect("DARK has a slot");
    assert!(matches!(ir.expr(*value), IrExpr::GetStatic(slot) if *slot == dark));
}

#[test]
fn an_enum_another_file_declares_keeps_its_use_nodes() {
    let mut ir = IrFile::default();
    shade(&mut ir);
    let elsewhere = read(&mut ir, "other/Elsewhere", "ONE");
    declare(&mut ir, "box", Vec::new(), vec![elsewhere]);
    let ir = lowered(ir);
    assert!(matches!(ir.expr(elsewhere), IrExpr::EnumEntry { .. }));
}

/// Another file reads an enum's constant through an entry of this file, which reads the
/// one constant this file holds.
#[test]
fn an_enum_publishes_an_entry_answering_each_constant() {
    let mut ir = IrFile::default();
    let class = shade(&mut ir);
    ir.record_declared_class_self_type(type_name("Shade"), Ty::obj("Shade"));
    let mut exports = super::super::DependencyExportSignatures::default();
    super::publish_constants(&mut ir, &mut exports);
    let ir = lowered(ir);

    let published = exports
        .instance_entries()
        .iter()
        .map(|(instance, entry)| {
            let constant = instance
                .enum_constant()
                .unwrap_or_else(|| panic!("an enum publishes only constant entries: {instance:?}"));
            (constant.classifier, &*constant.name, *entry)
        })
        .collect::<Vec<_>>();
    let [(light_owner, "LIGHT", light), (dark_owner, "DARK", dark)] = published.as_slice() else {
        panic!("one entry per constant, in declaration order: {published:?}");
    };
    assert_eq!([*light_owner, *dark_owner], [type_name("Shade"); 2]);
    assert_eq!(ir.construction_entries.get(dark), Some(&class));
    let (build, ..) = initializer_parts(&ir, function(&ir, "Shade$initEntries"));
    let slots = constructed(&ir, &build);
    for (entry, name) in [(*light, "LIGHT"), (*dark, "DARK")] {
        let function = &ir.functions[entry as usize];
        assert!(function.is_static && function.params.is_empty());
        assert_eq!(function.ret, Ty::obj("Shade"));
        let statements = statements(&ir, body(&ir, entry));
        let [returned] = statements.as_slice() else {
            panic!("an entry only returns the constant");
        };
        let IrExpr::Return(Some(value)) = ir.expr(*returned) else {
            panic!("an entry returns the constant");
        };
        let IrExpr::Block {
            value: Some(read), ..
        } = ir.expr(*value)
        else {
            panic!("the entry's read is realized: {:?}", ir.expr(*value));
        };
        let slot = slots
            .iter()
            .find_map(|(slot, constant, ..)| (constant == name).then_some(*slot))
            .expect("the constant has a slot");
        assert!(matches!(ir.expr(*read), IrExpr::GetStatic(read) if *read == slot));
    }
}

/// `enumEntries<E>()`, once inline specialization has named the enum, calls the same `entries`
/// getter as `E.entries`, which runs the initializer and answers the list it stored.
#[test]
fn the_enum_entries_intrinsic_calls_the_entries_getter() {
    let mut ir = IrFile::default();
    shade(&mut ir);
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
    declare(&mut ir, "box", Vec::new(), vec![intrinsic]);
    let ir = lowered(ir);

    let getter = function(&ir, "Shade$entries");
    assert_eq!(local_callee(&ir, intrinsic), Some(getter));
    let initializer = function(&ir, "Shade$initEntries");
    let entries = runtime_entries_slot(&ir, "Shade");
    let [build, answer] = statements(&ir, body(&ir, getter))[..] else {
        panic!("the getter builds the enum, then answers the list");
    };
    assert!(calls_initializer(&ir, build, initializer));
    let IrExpr::Return(Some(value)) = ir.expr(answer) else {
        panic!("the getter answers the list");
    };
    assert!(matches!(ir.expr(*value), IrExpr::GetStatic(slot) if *slot == entries));
}

/// An enum another file declares is not realized here: `enumEntries<E>()` of it becomes the same
/// node as `E.entries`, which a target links to the declaring file's getter.
#[test]
fn another_files_enum_entries_keep_one_use_node() {
    let mut ir = IrFile::default();
    shade(&mut ir);
    let elsewhere = type_name("other/Elsewhere");
    let intrinsic = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Intrinsic {
            operation: crate::ir::IrIntrinsic::EnumEntries {
                classifier: Ty::obj_name(elsewhere),
            },
            ret: super::initializer::entries_type(elsewhere),
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let read = ir.add_expr(IrExpr::EnumEntries {
        classifier: elsewhere,
    });
    declare(&mut ir, "box", Vec::new(), vec![intrinsic, read]);
    let ir = lowered(ir);
    for expression in [intrinsic, read] {
        assert!(
            matches!(ir.expr(expression), IrExpr::EnumEntries { classifier } if *classifier == elsewhere),
            "{:?}",
            ir.expr(expression)
        );
        assert_eq!(
            super::StaticMember::of(ir.expr(expression)),
            Some((elsewhere, super::StaticMember::Entries))
        );
    }
}

/// Another file calls an enum's `values()`, `valueOf` and `entries` through entries of this file:
/// the very functions this file's own uses are rewritten to, published by the enum and the member.
#[test]
fn an_enum_publishes_its_values_value_of_and_entries() {
    let mut ir = IrFile::default();
    shade(&mut ir);
    let mut exports = super::super::DependencyExportSignatures::default();
    super::realize(&mut ir, super::Contract::KotlinNative, None, &mut exports)
        .expect("the Native contract builds its own list");

    let shade = type_name("Shade");
    let published = exports
        .instance_entries()
        .iter()
        .map(|(instance, entry)| (instance.clone(), *entry))
        .collect::<Vec<_>>();
    let values = function(&ir, "Shade$values");
    let value_of = function(&ir, "Shade$valueOf");
    let entries = function(&ir, "Shade$entries");
    assert_eq!(
        published,
        [
            (
                super::super::instance_links::InstanceEntry::EnumMember {
                    classifier: shade,
                    member: super::StaticMember::Values,
                },
                values,
            ),
            (
                super::super::instance_links::InstanceEntry::EnumMember {
                    classifier: shade,
                    member: super::StaticMember::ValueOf,
                },
                value_of,
            ),
            (
                super::super::instance_links::InstanceEntry::EnumMember {
                    classifier: shade,
                    member: super::StaticMember::Entries,
                },
                entries,
            ),
        ]
    );
}

/// A default is evaluated by the target, in an entry or a constructor's frame, so an enum constant
/// it reads is realized like one in a body.
#[test]
fn defaults_reading_a_constant_are_realized() {
    let mut ir = IrFile::default();
    shade(&mut ir);
    // `fun pick(shade: Shade = Shade.DARK)`.
    let dark = read(&mut ir, "Shade", "DARK");
    let pick = declare(
        &mut ir,
        "pick",
        vec![Ty::obj_name(type_name("Shade"))],
        Vec::new(),
    );
    ir.fn_params.insert(
        pick,
        FnParamInfo::source_defaults(vec!["shade".to_owned()], vec![Some(dark)]),
    );
    // `class Holder(val shade: Shade = Shade.LIGHT)`.
    let light = read(&mut ir, "Shade", "LIGHT");
    ir.add_class(crate::ir::test_support::blank_class("Holder"));
    ir.insert_class_ctor_defaults_name(type_name("Holder"), vec![Some(light)]);
    // `annotation class Tagged(val shade: Shade = Shade.DARK)`.
    let tagged = read(&mut ir, "Shade", "DARK");
    let mut annotation = crate::ir::test_support::blank_class("Tagged");
    annotation.is_annotation = true;
    ir.add_class(annotation);
    ir.insert_class_ctor_defaults_name(type_name("Tagged"), vec![Some(tagged)]);
    let ir = lowered(ir);

    let mut defaults = super::uses::function_defaults(&ir);
    defaults.extend(super::uses::constructor_defaults(&ir));
    assert_eq!(defaults, [dark, light, tagged]);
    for default in defaults {
        assert!(
            matches!(ir.expr(default), IrExpr::Block { value: Some(value), .. } if matches!(ir.expr(*value), IrExpr::GetStatic(_))),
            "{:?}",
            ir.expr(default)
        );
    }
}

/// Every enum builds its `entries` list, whether or not a use of this file reads it: another file
/// may read it through the getter, and what a program observes must not depend on which file is
/// lowered first. `Shade` has no use here, and its initializer still builds and clears the list.
#[test]
fn every_enum_builds_its_entries() {
    let ir = lowered(colors());
    let lists = |initializer: &str| {
        let (build, reset, _) = initializer_parts(&ir, function(&ir, initializer));
        let built = build
            .iter()
            .filter(|&&statement| {
                matches!(
                    ir.expr(statement),
                    IrExpr::SetStatic { value, .. }
                        if matches!(ir.expr(*value), IrExpr::EnumEntriesList { .. })
                )
            })
            .count();
        (built, reset.len())
    };
    // Color: flag, three constants and entries cleared, then the rethrow.
    assert_eq!(lists("Color$initEntries"), (1, 6));
    // Shade: flag, two constants and entries cleared, then the rethrow.
    assert_eq!(lists("Shade$initEntries"), (1, 5));
    function(&ir, "Shade$entries");
}

/// A constant's argument is the only read of another enum's `entries`
/// (`enum class Kind(val n: Int) { A(Shade.entries.size) }`): it calls that enum's getter, under
/// either contract.
#[test]
fn entries_read_by_a_constant_argument_call_the_getter() {
    for contract in [super::Contract::KotlinNative, super::Contract::KotlinWasm] {
        let mut ir = IrFile::default();
        shade(&mut ir);
        let entries = ir.add_expr(IrExpr::EnumEntries {
            classifier: type_name("Shade"),
        });
        enum_class(&mut ir, "Kind", vec![entry("A", vec![entries], Vec::new())]);
        let ir = lowered_for(ir, contract);
        assert_eq!(
            local_callee(&ir, entries),
            Some(function(&ir, "Shade$entries")),
            "{contract:?}: {:?}",
            ir.expr(entries)
        );
    }
}

/// The Wasm contract builds `entries` through the stdlib's `enumEntries(Array)`. A platform that
/// declares none has nothing to build it by, which only an enum needs.
#[test]
fn the_wasm_contract_needs_the_selected_factory_for_an_enum() {
    let mut exports = super::super::DependencyExportSignatures::default();
    let mut ir = colors();
    assert_eq!(
        super::realize(&mut ir, super::Contract::KotlinWasm, None, &mut exports),
        Err(
            "the platform declares no `kotlin.enums.enumEntries(Array)` to build `entries` by"
                .to_owned()
        )
    );
    let mut ir = IrFile::default();
    declare(&mut ir, "box", Vec::new(), Vec::new());
    assert_eq!(
        super::realize(&mut ir, super::Contract::KotlinWasm, None, &mut exports),
        Ok(())
    );
}

/// kotlinc-js/wasm publish each constant once it is constructed, a constant with a body from its
/// subclass's constructor right after the super call, and build `entries` in its getter on its
/// first read, by calling the stdlib's `enumEntries` with the constants.
#[test]
fn the_wasm_contract_publishes_each_constant_once_constructed() {
    let ir = lowered_for(colors(), super::Contract::KotlinWasm);
    let (build, ..) = initializer_parts(&ir, function(&ir, "Color$initEntries"));
    assert!(build
        .iter()
        .all(|&statement| !matches!(ir.expr(statement), IrExpr::SetStatic { value, .. }
            if matches!(ir.expr(*value), IrExpr::EnumAllocation { .. } | IrExpr::EnumEntriesList { .. }))));
    let published: Vec<(u32, String)> = build
        .iter()
        .filter_map(|&statement| match ir.expr(statement) {
            IrExpr::SetStatic { index, value } => match ir.expr(*value) {
                IrExpr::EnumConstant {
                    name,
                    published: None,
                    ..
                } => Some((*index, name.to_string())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let names: Vec<&str> = published.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(names, ["RED", "GREEN", "BLUE"]);

    // BLUE's subclass publishes it first thing after its super call.
    let blue = class(&ir, "Color$BLUE");
    let body = ir.classes[blue as usize]
        .init_body
        .expect("the subclass publishes the constant");
    let first = statements(&ir, body)[0];
    assert!(matches!(
        ir.expr(first),
        IrExpr::SetStatic { index, value }
            if *index == published[2].0 && matches!(ir.expr(*value), IrExpr::GetValue(0))
    ));

    // `values()` answers the slots as they stand.
    let values = statements(&ir, body_of(&ir, "Color$values"));
    let IrExpr::Return(Some(array)) = ir.expr(values[1]) else {
        panic!("values() answers the array");
    };
    let slots: Vec<u32> = published.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(array_slots(&ir, *array), slots);

    // `Color.entries` calls the getter, which fills the list on its first read.
    let getter = function(&ir, "Color$entries");
    let read = crate::ir::emitted_body_roots(&ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(&ir, root))
        .find(|&expression| local_callee(&ir, expression) == Some(getter));
    assert!(read.is_some(), "the entries read calls the getter");
    let [build, fill, answer] = statements(&ir, body_of(&ir, "Color$entries"))[..] else {
        panic!("the getter builds the enum, fills the list, then answers it");
    };
    assert!(calls_initializer(
        &ir,
        build,
        function(&ir, "Color$initEntries")
    ));
    let IrExpr::When { branches } = ir.expr(fill) else {
        panic!("the list is built when missing");
    };
    let [(Some(_), store)] = branches[..] else {
        panic!("one branch");
    };
    let IrExpr::SetStatic { index: list, value } = ir.expr(store) else {
        panic!("the list is stored");
    };
    let IrExpr::Call {
        callee,
        dispatch_receiver: None,
        args,
    } = ir.expr(*value)
    else {
        panic!("the list is a call");
    };
    let Callee::External {
        target,
        substitutions,
        ..
    } = &**callee
    else {
        panic!("of the selected factory: {callee:?}");
    };
    assert_eq!(*target, factory().function);
    let [substitution] = &substitutions[..] else {
        panic!("one type argument");
    };
    assert_eq!(substitution.value, Ty::obj_name(type_name("Color")));
    let [array] = args[..] else {
        panic!("one argument");
    };
    assert_eq!(array_slots(&ir, array), slots);
    let IrExpr::Return(Some(value)) = ir.expr(answer) else {
        panic!("the getter answers the list");
    };
    assert!(matches!(ir.expr(*value), IrExpr::GetStatic(slot) if slot == list));
}

/// A support unit that lowers `enumEntries` itself builds a library enum's list with its own
/// function, which no earlier unit exports.
#[test]
fn a_unit_lowering_the_factory_calls_its_own_function() {
    let mut ir = colors();
    let own: FunId = 0;
    super::realize(
        &mut ir,
        super::Contract::KotlinWasm,
        Some(super::EntriesFactory::Own(own)),
        &mut super::super::DependencyExportSignatures::default(),
    )
    .expect("the enums are realized");
    let [_, fill, _] = statements(&ir, body_of(&ir, "Color$entries"))[..] else {
        panic!("the getter builds the enum, fills the list, then answers it");
    };
    let IrExpr::When { branches } = ir.expr(fill) else {
        panic!("the list is built when missing");
    };
    let [(Some(_), store)] = branches[..] else {
        panic!("one branch");
    };
    let IrExpr::SetStatic { value, .. } = ir.expr(store) else {
        panic!("the list is stored");
    };
    assert_eq!(local_callee(&ir, *value), Some(own));
}

fn body_of(ir: &IrFile, name: &str) -> ExprId {
    body(ir, function(ir, name))
}

/// `name` and `ordinal` are `kotlin.Enum`'s only when the getter's checked semantic owner is
/// `kotlin.Enum`; a package, another classifier, or an unpublished owner is not, whatever class
/// realizes the getter.
#[test]
fn base_members_are_recognized_by_their_semantic_owner() {
    use super::BaseMember;
    use crate::types::SemanticCallableOwner::{Classifier, Package};
    let enum_owner = Some(Classifier(crate::types::wk::kotlin_enum()));
    assert_eq!(
        super::declared_base_member(enum_owner, "name"),
        Some(BaseMember::Name)
    );
    assert_eq!(
        super::declared_base_member(enum_owner, "ordinal"),
        Some(BaseMember::Ordinal)
    );
    assert_eq!(super::declared_base_member(enum_owner, "hashCode"), None);
    assert_eq!(super::declared_base_member(None, "name"), None);
    assert_eq!(
        super::declared_base_member(Some(Package(type_name("kotlin"))), "name"),
        None
    );
    assert_eq!(
        super::declared_base_member(Some(Classifier(type_name("java/lang/Enum"))), "ordinal"),
        None
    );
}

/// Realize `ir` for Kotlin/Wasm and withdraw the entries getters nothing reads.
fn withdrawn(mut ir: IrFile) -> (IrFile, super::super::DependencyExportSignatures) {
    let mut exports = super::super::DependencyExportSignatures::default();
    super::realize(
        &mut ir,
        super::Contract::KotlinWasm,
        Some(super::EntriesFactory::Selected(factory())),
        &mut exports,
    )
    .expect("the enums are realized");
    super::withdraw_unread_entries(&mut [(&mut ir, &mut exports)]);
    (ir, exports)
}

fn publishes_entries(exports: &super::super::DependencyExportSignatures) -> bool {
    exports.instance_entries().iter().any(|(instance, _)| {
        matches!(
            instance,
            super::super::instance_links::InstanceEntry::EnumMember {
                member: super::StaticMember::Entries,
                ..
            }
        )
    })
}

#[test]
fn an_entries_getter_nothing_reads_is_withdrawn_so_its_factory_call_is_no_emitted_use() {
    let mut ir = IrFile::default();
    shade(&mut ir);
    declare(&mut ir, "box", Vec::new(), Vec::new());
    let (ir, exports) = withdrawn(ir);

    assert!(ir.inline_only_fns.contains(&function(&ir, "Shade$entries")));
    assert!(!publishes_entries(&exports));
}

#[test]
fn an_entries_getter_a_body_reads_is_kept_and_published() {
    let mut ir = IrFile::default();
    shade(&mut ir);
    let shade = type_name("Shade");
    let entries = ir.add_expr(IrExpr::EnumEntries { classifier: shade });
    declare(&mut ir, "box", Vec::new(), vec![entries]);
    let (ir, exports) = withdrawn(ir);

    assert!(!ir.inline_only_fns.contains(&function(&ir, "Shade$entries")));
    assert!(publishes_entries(&exports));
}

#[test]
fn an_entries_read_in_another_file_of_the_program_keeps_the_getter() {
    let mut declaring = IrFile::default();
    shade(&mut declaring);
    declare(&mut declaring, "box", Vec::new(), Vec::new());
    let mut reading = IrFile::default();
    let entries = reading.add_expr(IrExpr::EnumEntries {
        classifier: type_name("Shade"),
    });
    declare(&mut reading, "read", Vec::new(), vec![entries]);

    let mut declaring_exports = super::super::DependencyExportSignatures::default();
    super::realize(
        &mut declaring,
        super::Contract::KotlinWasm,
        Some(super::EntriesFactory::Selected(factory())),
        &mut declaring_exports,
    )
    .expect("the enums are realized");
    let mut reading_exports = super::super::DependencyExportSignatures::default();
    super::withdraw_unread_entries(&mut [
        (&mut declaring, &mut declaring_exports),
        (&mut reading, &mut reading_exports),
    ]);

    assert!(!declaring
        .inline_only_fns
        .contains(&function(&declaring, "Shade$entries")));
    assert!(publishes_entries(&declaring_exports));
}
