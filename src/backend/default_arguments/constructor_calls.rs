//! Constructor calls that leave arguments out, rewritten to reach their constructor's
//! default-argument entry with the whole frame: the compiler-supplied prefix as it was, each
//! supplied argument in its parameter's place, an [`IrExpr::AbsentArgument`] in each omitted one,
//! and the omission masks after them.
//!
//! A constructor is called four ways: a construction `C(…)`, a class's `super(…)` (an enum
//! constant with a body included, whose subclass passes the constant's arguments), a secondary
//! constructor's `this(…)` or `super(…)`, and an enum constant. Each already names the exact
//! constructor the checker selected; the rewrite names the entry's place instead.
//!
//! A call this lowering cannot realize keeps its checked form and its omitted ordinals, so the
//! backend's own decline names it: a construction of a class another file declares, and a
//! delegation to a dependency's constructor.

use crate::ir::{ClassId, ExprId, IrConstructorTarget, IrExpr, IrFile};
use crate::types::Ty;

use super::call_sites::{arguments, frame, Frame};
use super::constructor_entries::{declared, DeclaredConstructor};
use super::entries::Entries;

/// Rewrite `site` when it is a construction of a class of this file that leaves arguments out.
pub(super) fn rewrite(ir: &mut IrFile, entries: &mut Entries, site: ExprId) {
    let IrExpr::New {
        internal,
        args,
        ctor_desc: None,
        external_target: None,
        defaults,
        default_prefix_count,
        ..
    } = ir.expr(site)
    else {
        return;
    };
    if defaults.is_empty() {
        return;
    }
    let (internal, default_prefix_count) = (*internal, *default_prefix_count);
    let Some(class) = ir.class_id_by_name(internal) else {
        return;
    };
    let Some(&target) = ir.construction_targets.get(&site) else {
        return;
    };
    let Some(constructor) = declared(ir, class, target.ordinal) else {
        return;
    };
    let prefix = constructor.prefix.len();
    if default_prefix_count as usize != prefix || args.len() < prefix {
        return;
    }
    let (leading, supplied) = args.split_at(prefix);
    let Some(frame) = frame(supplied, defaults, constructor.parameters.len()) else {
        return;
    };
    let mut args = leading.to_vec();
    let Some((entry, operands)) = reach(ir, entries, class, target, &constructor, frame) else {
        return;
    };
    args.extend(operands);
    let parameters = entry_parameters(ir, class, entry);
    ir.exprs[site as usize] = IrExpr::New {
        internal,
        args,
        ctor_params: Some(parameters.clone()),
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count,
    };
    ir.construction_targets.insert(site, entry);
    if let Some(declared) = ir.construction_declared_params.get_mut(&site) {
        *declared = constructor
            .prefix
            .iter()
            .copied()
            .chain(parameters)
            .collect();
    }
}

/// Rewrite every delegation and enum constant of this file's classes that leaves arguments out.
pub(super) fn rewrite_delegations(ir: &mut IrFile, entries: &mut Entries) {
    for class in 0..ir.classes.len() {
        let class = ClassId::try_from(class).expect("too many classes");
        super_call(ir, entries, class);
        for ordinal in 0..ir.classes[class as usize].secondary_ctors.len() {
            delegation(ir, entries, class, ordinal);
        }
        for constant in 0..ir.classes[class as usize].enum_entries.len() {
            enum_constant(ir, entries, class, constant);
        }
    }
}

/// The primary constructor's `super(…)` of `class`. Its operands are the superclass
/// constructor's whole frame, with a placeholder in each omitted parameter.
fn super_call(ir: &mut IrFile, entries: &mut Entries, class: ClassId) {
    let declaration = &ir.classes[class as usize];
    let name = declaration.fq_name_id();
    let Some(omitted) = ir.super_constructor_default_arguments.get(&name) else {
        return;
    };
    if ir.external_super_constructors.contains_key(&name) {
        return;
    }
    let Some(parent) = ir.class_id_by_name(declaration.superclass) else {
        return;
    };
    let target = declaration.super_ctor;
    let Some(constructor) = declared(ir, parent, target.ordinal) else {
        return;
    };
    let prefix = constructor.prefix.len();
    let declaration = &ir.classes[class as usize];
    if declaration.super_ctor_params.len() != declaration.super_args.len() {
        return;
    }
    let Some(frame) = omitting(&declaration.super_args, prefix, &constructor, omitted) else {
        return;
    };
    let Some((entry, operands)) = reach(ir, entries, parent, target, &constructor, frame) else {
        return;
    };
    let parameters = entry_parameters(ir, parent, entry);
    let declaration = &mut ir.classes[class as usize];
    declaration.super_args.truncate(prefix);
    declaration.super_args.extend(operands);
    declaration.super_ctor_params.truncate(prefix);
    declaration.super_ctor_params.extend(parameters);
    declaration.super_ctor = entry;
    ir.super_constructor_default_arguments.remove(&name);
}

/// The `this(…)` or `super(…)` of secondary constructor `ordinal` of `class`. Its operands are the
/// target's whole frame, with a placeholder in each omitted parameter; a `this(…)` leaves the
/// prefix out, passing on its own.
fn delegation(ir: &mut IrFile, entries: &mut Entries, class: ClassId, ordinal: usize) {
    let declaration = &ir.classes[class as usize];
    let secondary = &declaration.secondary_ctors[ordinal];
    if secondary.default_parameters.is_empty() {
        return;
    }
    let (owner, target, passes_prefix) = match &secondary.delegate {
        crate::ir::CtorDelegateTarget::This { target, .. } => (Some(class), *target, false),
        crate::ir::CtorDelegateTarget::Super { owner, target, .. } => {
            let key = (declaration.fq_name_id(), ordinal as u32);
            if ir.external_secondary_super_constructors.contains_key(&key) {
                return;
            }
            (ir.class_id_by_name(*owner), *target, true)
        }
        crate::ir::CtorDelegateTarget::ImplicitEnumBase => return,
    };
    let Some(owner) = owner else { return };
    let Some(constructor) = declared(ir, owner, target.ordinal) else {
        return;
    };
    let prefix = if passes_prefix {
        constructor.prefix.len()
    } else {
        0
    };
    let secondary = &ir.classes[class as usize].secondary_ctors[ordinal];
    let Some(frame) = omitting(
        &secondary.delegate_args,
        prefix,
        &constructor,
        &secondary.default_parameters,
    ) else {
        return;
    };
    let Some((entry, operands)) = reach(ir, entries, owner, target, &constructor, frame) else {
        return;
    };
    let parameters = entry_parameters(ir, owner, entry);
    let secondary = &mut ir.classes[class as usize].secondary_ctors[ordinal];
    let target_params = match &mut secondary.delegate {
        crate::ir::CtorDelegateTarget::This {
            target_params,
            target,
            ..
        }
        | crate::ir::CtorDelegateTarget::Super {
            target_params,
            target,
            ..
        } => {
            *target = entry;
            target_params
        }
        crate::ir::CtorDelegateTarget::ImplicitEnumBase => unreachable!("checked above"),
    };
    target_params.truncate(prefix);
    target_params.extend(parameters);
    secondary.delegate_args.truncate(prefix);
    secondary.delegate_args.extend(operands);
    secondary.default_parameters.clear();
}

/// Enum constant `constant` of `class`, which supplies only the arguments it writes. A constant
/// with a body is constructed through its subclass, whose `super(…)` carries its arguments.
fn enum_constant(ir: &mut IrFile, entries: &mut Entries, class: ClassId, constant: usize) {
    let entry = &ir.classes[class as usize].enum_entries[constant];
    if entry.default_parameters.is_empty() || entry.subclass.is_some() {
        return;
    }
    let target = entry.constructor;
    let mut omitted = entry.default_parameters.clone();
    omitted.sort_unstable();
    let Some(constructor) = declared(ir, class, target.ordinal) else {
        return;
    };
    if !constructor.prefix.is_empty() {
        return;
    }
    let entry = &ir.classes[class as usize].enum_entries[constant];
    let Some(frame) = frame(&entry.args, &omitted, constructor.parameters.len()) else {
        return;
    };
    let Some((reached, operands)) = reach(ir, entries, class, target, &constructor, frame) else {
        return;
    };
    let parameters = entry_parameters(ir, class, reached);
    let entry = &mut ir.classes[class as usize].enum_entries[constant];
    entry.args = operands;
    entry.constructor_parameter_types = parameters;
    entry.constructor = reached;
    entry.default_parameters.clear();
}

/// The frame of a delegation whose `operands` are `prefix` compiler-supplied ones and then one per
/// source parameter, with a placeholder at each of the `omitted` source ordinals.
fn omitting(
    operands: &[ExprId],
    prefix: usize,
    constructor: &DeclaredConstructor,
    omitted: &[u32],
) -> Option<Frame> {
    let supplied = operands.get(prefix..)?;
    if supplied.len() != constructor.parameters.len()
        || omitted
            .iter()
            .any(|&ordinal| ordinal as usize >= supplied.len())
    {
        return None;
    }
    Some(
        supplied
            .iter()
            .enumerate()
            .map(|(ordinal, &operand)| (!omitted.contains(&(ordinal as u32))).then_some(operand))
            .collect(),
    )
}

/// The entry of the constructor `target` of `class` selects, and its operands for `frame` after
/// the prefix, when every parameter `frame` leaves out has a default.
fn reach(
    ir: &mut IrFile,
    entries: &mut Entries,
    class: ClassId,
    target: IrConstructorTarget,
    constructor: &DeclaredConstructor,
    frame: Frame,
) -> Option<(IrConstructorTarget, Vec<ExprId>)> {
    let omits_only_defaults = frame
        .iter()
        .zip(&constructor.defaults)
        .all(|(argument, default)| argument.is_some() || default.is_some());
    if frame.len() != constructor.parameters.len() || !omits_only_defaults {
        return None;
    }
    let entry = entries.constructor_entry(ir, class, target)?;
    Some((entry, arguments(ir, &constructor.parameters, frame)))
}

/// The parameters after the prefix of the entry `entry` reaches in `class`: the source
/// parameters, then the mask words.
fn entry_parameters(ir: &IrFile, class: ClassId, entry: IrConstructorTarget) -> Vec<Ty> {
    ir.classes[class as usize].secondary_ctors[entry.ordinal as usize - 1]
        .params
        .clone()
}
