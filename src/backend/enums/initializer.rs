//! The storage and functions one enum class is realized as.

use crate::ir::{
    Callee, ClassId, ExprId, FunId, IrAccessorLines, IrBinOp, IrCatch, IrConst, IrExpr, IrFile,
    IrFunction, IrIntrinsic, IrStatic, IrStaticAccessors,
};
use crate::types::{EqualityMode, Ty, TypeName, Visibility};

use super::Realized;

/// Build the slots, initializer, `values()` and `valueOf` of the enum `class`.
pub(super) fn build(ir: &mut IrFile, class: ClassId) -> Realized {
    let declaration = &ir.classes[class as usize];
    let classifier = declaration.fq_name_id();
    let owner = declaration.fq_name();
    let names: Vec<String> = declaration
        .enum_entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    let enum_ty = Ty::obj_name(classifier);
    let slots: Vec<u32> = names
        .iter()
        .map(|name| static_slot(ir, format!("{owner}${name}"), Ty::nullable(enum_ty)))
        .collect();
    let flag = static_slot(ir, format!("{owner}$initialized"), Ty::Boolean);
    let entries_ty = entries_type(classifier);
    let entries = static_slot(ir, format!("{owner}$entries"), Ty::nullable(entries_ty));

    let initializer = initializer(ir, class, &slots, flag, entries);
    let values = values(ir, classifier, &owner, &names, &slots, initializer);
    let value_of = value_of(ir, classifier, &owner, &names, &slots, initializer);
    Realized {
        slots,
        entries,
        initializer,
        values,
        value_of,
    }
}

/// `kotlin.enums.EnumEntries<E>`.
pub(super) fn entries_type(classifier: TypeName) -> Ty {
    Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj_name(classifier)])
}

/// A compiler-internal static of this file: no accessor, no initializer of its own.
fn static_slot(ir: &mut IrFile, name: String, ty: Ty) -> u32 {
    let index = u32::try_from(ir.statics.len()).expect("too many statics");
    ir.statics.push(IrStatic {
        name,
        ty,
        init: None,
        is_var: true,
        is_const: false,
        is_lateinit: false,
        owner: None,
        visibility: Visibility::Private,
        getter_jvm_name: None,
        setter_jvm_name: None,
        erased_declared_ty: None,
        accessors: IrStaticAccessors::ABSENT,
        line: 0,
        accessor_lines: IrAccessorLines::default(),
        source_order: u32::MAX,
        property_delegate: false,
    });
    index
}

/// The enum's initializer: when the enum is not built yet, allocate and publish every constant,
/// then construct each in declaration order, then build `entries`, then the companion.
///
/// This is Kotlin/Native's order (its `EnumClassLowering` fills `VALUES` with uninitialized
/// instances and then runs `initInstance` per entry): a constructor that reaches back into its
/// enum finds every constant already allocated, the ones not constructed yet with their
/// `kotlin.Enum` storage unset.
fn initializer(ir: &mut IrFile, class: ClassId, slots: &[u32], flag: u32, entries: u32) -> FunId {
    let declaration = &ir.classes[class as usize];
    let classifier = declaration.fq_name_id();
    let name = format!("{}$initEntries", declaration.fq_name());
    let companion = declaration.companion_class;
    let constants = declaration.enum_entries.clone();

    let mut build = vec![write(ir, flag, IrConst::Boolean(true))];
    for (entry, &slot) in constants.iter().zip(slots) {
        let instance = entry.subclass.unwrap_or(classifier);
        let allocation = ir.add_expr(IrExpr::EnumAllocation {
            classifier,
            instance,
        });
        ir.set_logical_type(allocation, Ty::obj_name(instance));
        build.push(ir.add_expr(IrExpr::SetStatic {
            index: slot,
            value: allocation,
        }));
    }
    for (ordinal, entry) in constants.iter().enumerate() {
        // The entry's checked argument evaluation, in source order, before its constructor.
        build.extend(entry.argument_prelude.iter().copied());
        // A constant with a body is an instance of its synthesized subclass, whose bare primary
        // constructor carries the constant's arguments to the enum's. An ordinary constant calls
        // the constructor checked FIR selected for it.
        let construction = match entry.subclass {
            Some(subclass) => IrExpr::New {
                internal: subclass,
                args: Vec::new(),
                ctor_params: None,
                ctor_desc: None,
                external_target: None,
                defaults: Box::default(),
                default_prefix_count: 0,
            },
            None => {
                let mut defaults = entry.default_parameters.clone();
                defaults.sort_unstable();
                IrExpr::New {
                    internal: classifier,
                    args: entry.args.clone(),
                    ctor_params: (!entry.constructor.primary())
                        .then(|| entry.constructor_parameter_types.clone()),
                    ctor_desc: None,
                    external_target: None,
                    defaults: defaults.into(),
                    default_prefix_count: 0,
                }
            }
        };
        let constructed = match &construction {
            IrExpr::New { internal, .. } => Ty::obj_name(*internal),
            _ => unreachable!("a constant is a construction"),
        };
        let construction = ir.add_expr(construction);
        ir.set_logical_type(construction, constructed);
        // The selected constructor travels with the construction, so the default-argument lowering
        // can reach its entry when the constant leaves arguments out.
        if entry.subclass.is_none() {
            ir.construction_targets
                .insert(construction, entry.constructor);
        }
        let constant = ir.add_expr(IrExpr::EnumConstant {
            classifier,
            name: entry.name.as_str().into(),
            ordinal: u32::try_from(ordinal).expect("too many enum constants"),
            construction,
            published: Some(slots[ordinal]),
        });
        ir.set_logical_type(constant, Ty::obj_name(classifier));
        build.push(constant);
    }
    let array = values_array(ir, classifier, slots);
    let list = ir.add_expr(IrExpr::EnumEntriesList {
        classifier,
        values: array,
    });
    build.push(ir.add_expr(IrExpr::SetStatic {
        index: entries,
        value: list,
    }));
    if let Some(companion) = companion {
        let instance = ir.add_expr(IrExpr::SingletonValue {
            classifier: companion,
        });
        build.push(instance);
    }
    let build = unit_block(ir, build);

    // A constant that throws leaves the enum unbuilt, every constant withdrawn: the next access
    // builds it again and throws again, rather than handing out the constants that did get made.
    let caught = next_free_value(ir, build);
    let mut reset = vec![write(ir, flag, IrConst::Boolean(false))];
    for &slot in slots.iter().chain([&entries]) {
        reset.push(write(ir, slot, IrConst::Null));
    }
    let exception = ir.add_expr(IrExpr::GetValue(caught));
    ir.set_logical_type(exception, throwable());
    let rethrow = ir.add_expr(IrExpr::Throw { operand: exception });
    ir.set_logical_type(rethrow, Ty::Nothing);
    reset.push(rethrow);
    let reset = unit_block(ir, reset);
    let guarded = ir.add_expr(IrExpr::Try {
        body: build,
        catches: vec![IrCatch {
            var: caught,
            binding: None,
            ty: throwable(),
            body: reset,
            line: None,
        }],
        finally: None,
        result: Ty::Unit,
    });
    ir.set_logical_type(guarded, Ty::Unit);

    let ready = ir.add_expr(IrExpr::GetStatic(flag));
    ir.set_logical_type(ready, Ty::Boolean);
    let unset = constant(ir, IrConst::Boolean(false), Ty::Boolean);
    let pending = ir.add_expr(IrExpr::Equality {
        op: IrBinOp::Eq,
        mode: EqualityMode::Primitive,
        lhs: ready,
        rhs: unset,
    });
    ir.set_logical_type(pending, Ty::Boolean);
    let when = ir.add_expr(IrExpr::When {
        branches: vec![(Some(pending), guarded)],
    });
    ir.set_logical_type(when, Ty::Unit);
    let body = unit_block(ir, vec![when]);
    function(ir, name, Vec::new(), Ty::Unit, body)
}

/// `values()`: run the initializer, then answer a fresh array of the constants.
///
/// Each constant goes at its current `ordinal`, visiting them in name order: Kotlin/Native's
/// `valuesForEnum` over its name-sorted `VALUES`. Once the enum is built that is declaration
/// order; read from inside a constant's construction, every constant not constructed yet still
/// has ordinal 0, so the last of them by name lands at 0 and their own places stay `null`.
fn values(
    ir: &mut IrFile,
    classifier: TypeName,
    owner: &str,
    names: &[String],
    slots: &[u32],
    initializer: FunId,
) -> FunId {
    let name = format!("{owner}$values");
    let enum_ty = Ty::obj_name(classifier);
    let array_ty = array_type(classifier);
    let build = call(ir, initializer, Vec::new(), Ty::Unit);
    let size = constant(
        ir,
        IrConst::Int(i32::try_from(slots.len()).expect("too many enum constants")),
        Ty::Int,
    );
    let array = ir.add_expr(IrExpr::NewArray {
        array_type: array_ty,
        size,
    });
    ir.set_logical_type(array, array_ty);
    let result = 0;
    let declaration = ir.add_expr(IrExpr::Variable {
        index: result,
        ty: array_ty,
        init: Some(array),
        named: false,
    });
    ir.set_logical_type(declaration, Ty::Unit);
    let mut stmts = vec![build, declaration];
    for index in name_order(names) {
        let constant = read_slot(ir, slots[index], enum_ty);
        let ordinal = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Intrinsic {
                operation: IrIntrinsic::EnumOrdinal,
                ret: Ty::Int,
            }),
            dispatch_receiver: Some(constant),
            args: Vec::new(),
        });
        ir.set_logical_type(ordinal, Ty::Int);
        let target = ir.add_expr(IrExpr::GetValue(result));
        ir.set_logical_type(target, array_ty);
        let element = read_slot(ir, slots[index], enum_ty);
        let store = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Intrinsic {
                operation: IrIntrinsic::ArraySet,
                ret: Ty::Unit,
            }),
            dispatch_receiver: Some(target),
            args: vec![ordinal, element],
        });
        ir.set_logical_type(store, Ty::Unit);
        stmts.push(store);
    }
    let answer = ir.add_expr(IrExpr::GetValue(result));
    ir.set_logical_type(answer, array_ty);
    let answer = ir.add_expr(IrExpr::Return(Some(answer)));
    ir.set_logical_type(answer, Ty::Nothing);
    stmts.push(answer);
    let body = unit_block(ir, stmts);
    function(ir, name, Vec::new(), array_ty, body)
}

/// The constants' declaration indices in Kotlin's name order: `String.compareTo`, which compares
/// UTF-16 units.
fn name_order(names: &[String]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|&a, &b| names[a].encode_utf16().cmp(names[b].encode_utf16()));
    order
}

/// `valueOf(name)`: run the initializer, then answer the first constant whose name is `name`, in
/// declaration order, or fail.
fn value_of(
    ir: &mut IrFile,
    classifier: TypeName,
    owner: &str,
    names: &[String],
    slots: &[u32],
    initializer: FunId,
) -> FunId {
    let name = format!("{owner}$valueOf");
    let enum_ty = Ty::obj_name(classifier);
    let build = call(ir, initializer, Vec::new(), Ty::Unit);
    let mut branches = Vec::with_capacity(names.len() + 1);
    for (constant_name, &slot) in names.iter().zip(slots) {
        let argument = read_parameter(ir);
        let candidate = constant(
            ir,
            IrConst::String(constant_name.as_str().into()),
            Ty::String,
        );
        let same = ir.add_expr(IrExpr::Equality {
            op: IrBinOp::Eq,
            mode: EqualityMode::Structural,
            lhs: argument,
            rhs: candidate,
        });
        ir.set_logical_type(same, Ty::Boolean);
        let found = read_slot(ir, slot, enum_ty);
        branches.push((Some(same), found));
    }
    let argument = read_parameter(ir);
    let missing = ir.add_expr(IrExpr::NoSuchEnumConstant {
        classifier,
        name: argument,
    });
    ir.set_logical_type(missing, Ty::Nothing);
    branches.push((None, missing));
    let selected = ir.add_expr(IrExpr::When { branches });
    ir.set_logical_type(selected, enum_ty);
    let result = ir.add_expr(IrExpr::Return(Some(selected)));
    ir.set_logical_type(result, Ty::Nothing);
    let body = unit_block(ir, vec![build, result]);
    function(ir, name, vec![Ty::String], enum_ty, body)
}

/// A fresh `Array<E>` of the constants' slots as they stand.
fn values_array(ir: &mut IrFile, classifier: TypeName, slots: &[u32]) -> ExprId {
    let enum_ty = Ty::obj_name(classifier);
    let elements: Vec<ExprId> = slots
        .iter()
        .map(|&slot| read_slot(ir, slot, enum_ty))
        .collect();
    let array_type = array_type(classifier);
    let array = ir.add_expr(IrExpr::Vararg {
        array_type,
        spreads: vec![false; elements.len()],
        elements,
    });
    ir.set_logical_type(array, array_type);
    array
}

fn array_type(classifier: TypeName) -> Ty {
    Ty::obj_args("kotlin/Array", &[Ty::obj_name(classifier)])
}

fn throwable() -> Ty {
    Ty::obj_name(crate::types::wk::kotlin_throwable())
}

/// Read the static at `slot` as a value of type `ty`.
pub(super) fn read_slot(ir: &mut IrFile, slot: u32, ty: Ty) -> ExprId {
    let read = ir.add_expr(IrExpr::GetStatic(slot));
    ir.set_logical_type(read, ty);
    read
}

/// Call the function `function` of this file.
pub(super) fn call(ir: &mut IrFile, function: FunId, args: Vec<ExprId>, ty: Ty) -> ExprId {
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(function)),
        dispatch_receiver: None,
        args,
    });
    ir.set_logical_type(call, ty);
    call
}

fn read_parameter(ir: &mut IrFile) -> ExprId {
    let read = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(read, Ty::String);
    read
}

fn constant(ir: &mut IrFile, value: IrConst, ty: Ty) -> ExprId {
    let constant = ir.add_expr(IrExpr::Const(value));
    ir.set_logical_type(constant, ty);
    constant
}

fn write(ir: &mut IrFile, slot: u32, value: IrConst) -> ExprId {
    let ty = match value {
        IrConst::Boolean(_) => Ty::Boolean,
        _ => Ty::Null,
    };
    let value = constant(ir, value, ty);
    let write = ir.add_expr(IrExpr::SetStatic { index: slot, value });
    ir.set_logical_type(write, Ty::Unit);
    write
}

fn unit_block(ir: &mut IrFile, stmts: Vec<ExprId>) -> ExprId {
    let block = ir.add_expr(IrExpr::Block { stmts, value: None });
    ir.set_logical_type(block, Ty::Unit);
    block
}

/// A compiler-generated static function of this file.
fn function(ir: &mut IrFile, name: String, params: Vec<Ty>, ret: Ty, body: ExprId) -> FunId {
    let function = ir.add_fun(IrFunction {
        name,
        params,
        ret,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    // Compiler-generated: no source declaration of its own, and no public name.
    ir.synthetic_methods.insert(function);
    function
}

/// The first value index no expression under `root` declares or reads: the constants' argument
/// preludes declare their own locals in the initializer's frame.
fn next_free_value(ir: &IrFile, root: ExprId) -> u32 {
    crate::ir::value_namespace_expressions(ir, root)
        .into_iter()
        .filter_map(|expression| match ir.expr(expression) {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => Some(*index + 1),
            IrExpr::Try { catches, .. } => catches.iter().map(|catch| catch.var + 1).max(),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}
