//! The expression trees a class's constructors own, and the pre-erasure slot types each runs over.
//!
//! A constructor is not one function body: its `init` block, each secondary constructor's body,
//! delegation and defaults, the superclass constructor arguments, and the primary constructor's
//! default expressions (evaluated in the synthetic `$default` `<init>`) are separate roots. Each
//! runs over its constructor's parameters (slot 0 = `this`, parameters at 1..) plus the locals it
//! declares. Their slot types are captured before expression erasure, since afterwards a local
//! `val tmp: Money` reads as its carrier and hides the box a generic slot needs.

use super::collect_reachable;
use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::Ty;
use std::collections::{HashMap, HashSet};

pub(super) type SlotTypes = HashMap<u32, Ty>;

/// Slot types for code rooted at `roots` running over `params`.
pub(super) fn slot_map(
    exprs: &[IrExpr],
    roots: impl IntoIterator<Item = ExprId>,
    params: &[Ty],
) -> SlotTypes {
    let mut slots: SlotTypes = params
        .iter()
        .enumerate()
        .map(|(index, ty)| (1 + index as u32, *ty))
        .collect();
    let mut reach = HashSet::new();
    for root in roots {
        collect_reachable(exprs, root, &mut reach);
    }
    for id in reach {
        if let IrExpr::Variable { index, ty, .. } = &exprs[id as usize] {
            slots.insert(*index, *ty);
        }
    }
    slots
}

/// Per class (parallel to `ir.classes`), the slot types of every constructor-owned root.
pub(super) struct ConstructorSlots {
    init: Vec<Option<SlotTypes>>,
    secondary: Vec<Vec<SlotTypes>>,
    super_args: Vec<SlotTypes>,
    primary_defaults: Vec<Vec<(ExprId, SlotTypes)>>,
}

impl ConstructorSlots {
    /// `primary` holds each class's primary constructor parameter types and `secondary` each
    /// secondary constructor's, both captured before erasure.
    pub(super) fn capture(ir: &IrFile, primary: &[Vec<Ty>], secondary: &[Vec<Vec<Ty>>]) -> Self {
        let exprs = &ir.exprs;
        let classes = ir.classes.iter().enumerate();
        Self {
            init: classes
                .clone()
                .map(|(class, declaration)| {
                    declaration
                        .init_body
                        .map(|root| slot_map(exprs, [root], &primary[class]))
                })
                .collect(),
            secondary: classes
                .clone()
                .map(|(class, declaration)| {
                    declaration
                        .secondary_ctors
                        .iter()
                        .zip(&secondary[class])
                        .map(|(constructor, params)| {
                            let roots = constructor
                                .delegate_prelude
                                .iter()
                                .chain(&constructor.delegate_args)
                                .chain(constructor.defaults.iter().flatten())
                                .copied()
                                .chain(constructor.body);
                            slot_map(exprs, roots, params)
                        })
                        .collect()
                })
                .collect(),
            super_args: classes
                .clone()
                .map(|(class, declaration)| {
                    let roots = declaration
                        .super_arg_prelude
                        .iter()
                        .chain(&declaration.super_args)
                        .copied();
                    slot_map(exprs, roots, &primary[class])
                })
                .collect(),
            primary_defaults: classes
                .map(|(class, declaration)| {
                    ir.class_ctor_defaults_name(declaration.fq_name)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|&root| (root, slot_map(exprs, [root], &primary[class])))
                        .collect()
                })
                .collect(),
        }
    }

    /// The slot types the superclass constructor arguments of `class` run over.
    pub(super) fn super_args(&self, class: usize) -> &SlotTypes {
        &self.super_args[class]
    }

    /// Every constructor-owned root of `class` with its slot types.
    pub(super) fn push_bodies(
        &self,
        ir: &IrFile,
        class: usize,
        out: &mut Vec<(ExprId, SlotTypes)>,
    ) {
        let declaration = &ir.classes[class];
        if let Some(root) = declaration.init_body {
            out.push((root, self.init[class].clone().unwrap_or_default()));
        }
        for (constructor, slots) in declaration
            .secondary_ctors
            .iter()
            .zip(&self.secondary[class])
        {
            let roots = constructor
                .body
                .into_iter()
                .chain(constructor.delegate_prelude.iter().copied())
                .chain(constructor.delegate_args.iter().copied())
                .chain(constructor.defaults.iter().flatten().copied());
            out.extend(roots.map(|root| (root, slots.clone())));
        }
        out.extend(self.primary_defaults[class].iter().cloned());
        let super_roots = declaration
            .super_arg_prelude
            .iter()
            .chain(&declaration.super_args)
            .copied();
        out.extend(super_roots.map(|root| (root, self.super_args[class].clone())));
    }
}
