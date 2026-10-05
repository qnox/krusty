//! JVM realization of shared mutable capture cells.
//!
//! Checked FIR and common IR keep the captured element's Kotlin type. A sparse identity edge marks
//! declaration slots that physically carry the shared cell; this pass realizes class fields and
//! constructor parameters as `kotlin.jvm.internal.Ref` holders without making that JVM choice part
//! of frontend semantics.

use std::collections::{HashMap, HashSet};

use crate::ir::{ClassId, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

const OBJECT_REF: &str = "kotlin/jvm/internal/Ref$ObjectRef";

/// The JVM holder class and its `element` field descriptor for one captured element type.
pub(super) fn holder_class(elem: &Ty) -> (&'static str, &'static str) {
    if elem.is_nullable() {
        return ("kotlin/jvm/internal/Ref$ObjectRef", "Ljava/lang/Object;");
    }
    match elem.non_null() {
        Ty::Int | Ty::UInt => ("kotlin/jvm/internal/Ref$IntRef", "I"),
        Ty::Long | Ty::ULong => ("kotlin/jvm/internal/Ref$LongRef", "J"),
        Ty::Float => ("kotlin/jvm/internal/Ref$FloatRef", "F"),
        Ty::Double => ("kotlin/jvm/internal/Ref$DoubleRef", "D"),
        Ty::Boolean => ("kotlin/jvm/internal/Ref$BooleanRef", "Z"),
        Ty::Char => ("kotlin/jvm/internal/Ref$CharRef", "C"),
        Ty::Byte | Ty::UByte => ("kotlin/jvm/internal/Ref$ByteRef", "B"),
        Ty::Short | Ty::UShort => ("kotlin/jvm/internal/Ref$ShortRef", "S"),
        Ty::Obj(name, _) if name.matches("kotlin/Int") || name.matches("kotlin/UInt") => {
            ("kotlin/jvm/internal/Ref$IntRef", "I")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Long") || name.matches("kotlin/ULong") => {
            ("kotlin/jvm/internal/Ref$LongRef", "J")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Float") => {
            ("kotlin/jvm/internal/Ref$FloatRef", "F")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Double") => {
            ("kotlin/jvm/internal/Ref$DoubleRef", "D")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Boolean") => {
            ("kotlin/jvm/internal/Ref$BooleanRef", "Z")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Char") => ("kotlin/jvm/internal/Ref$CharRef", "C"),
        Ty::Obj(name, _) if name.matches("kotlin/Byte") || name.matches("kotlin/UByte") => {
            ("kotlin/jvm/internal/Ref$ByteRef", "B")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Short") || name.matches("kotlin/UShort") => {
            ("kotlin/jvm/internal/Ref$ShortRef", "S")
        }
        _ => ("kotlin/jvm/internal/Ref$ObjectRef", "Ljava/lang/Object;"),
    }
}

/// The holder's type. kotlinc types an object cell as `Ref.ObjectRef<T>` over the element, so its
/// generic `Signature` names the element (`Ref$ObjectRef<Ljava/lang/String;>`); a primitive cell
/// is not generic.
pub(super) fn holder_ty(elem: &Ty) -> Ty {
    match holder_class(elem) {
        (holder, _) if holder == OBJECT_REF => Ty::obj_args(holder, &[*elem]),
        (holder, _) => Ty::obj(holder),
    }
}

/// Replace only backend declaration slots. The marker map remains logical so metadata, diagnostics,
/// and non-JVM backends never observe `Ref$*Ref` as the captured source value's type.
pub(super) fn lower_class_capture_slots(ir: &mut IrFile) {
    let mut physical = HashMap::<ClassId, Vec<(usize, Ty)>>::new();
    for (&(class, field), _) in &ir.shared_class_capture_fields {
        let field = field as usize;
        let element = ir.classes[class as usize].fields[field].ty;
        physical
            .entry(class)
            .or_default()
            .push((field, holder_ty(&element)));
    }
    for slots in physical.values_mut() {
        slots.sort_unstable_by_key(|(field, _)| *field);
    }

    for (&class, slots) in &physical {
        let declaration = &mut ir.classes[class as usize];
        for &(field, holder) in slots {
            declaration.fields[field].ty = holder;
            let argument = &mut declaration.ctor_args[field];
            argument.ty = holder;
            argument.check = None;
            for constructor in &mut declaration.secondary_ctors {
                constructor.prefix_params[field] = holder;
            }
        }
    }

    for (&(class, parameter), &element) in &ir.shared_super_capture_parameters {
        ir.classes[class as usize].super_ctor_params[parameter as usize] = holder_ty(&element);
    }
    for (&(class, secondary, parameter), &element) in &ir.shared_secondary_super_capture_parameters
    {
        let crate::ir::CtorDelegateTarget::Super { target_params, .. } =
            &mut ir.classes[class as usize].secondary_ctors[secondary as usize].delegate
        else {
            unreachable!(
                "secondary superclass capture ({class}, {secondary}, {parameter}) is not a super delegation"
            );
        };
        target_params[parameter as usize] = holder_ty(&element);
    }

    let class_names = physical
        .iter()
        .map(|(&class, slots)| (ir.classes[class as usize].fq_name_id(), slots.clone()))
        .collect::<HashMap<_, _>>();
    for expression in &mut ir.exprs {
        let IrExpr::New {
            internal,
            ctor_params: Some(parameters),
            ..
        } = expression
        else {
            continue;
        };
        let Some(slots) = class_names.get(internal) else {
            continue;
        };
        for &(field, holder) in slots {
            parameters[field] = holder;
        }
    }
}

/// Put a specialized shared cell back on the holder an uncloned escaping lambda still takes.
///
/// Inlining substitutes a type parameter in the copied template, including `RefNew`/`RefGet`/
/// `RefSet`. That selects a primitive holder when the argument is `Int`. The lambda's
/// implementation is not cloned for an ordinary parameter, and its capture parameter stays the
/// erased `Ref$ObjectRef`. The cell has to use that holder; a primitive element is boxed into it
/// and unboxed on the way out. A capture whose recorded element already selects the same holder,
/// including a primitive cell an inlined lambda splices, is left alone.
pub(crate) fn restore_erased_escaping_capture_holders(ir: &mut IrFile) {
    let bodies = ir
        .functions
        .iter()
        .filter_map(|function| function.body)
        .collect::<Vec<_>>();
    for body in bodies {
        restore_body(ir, body);
    }
}

fn restore_body(ir: &mut IrFile, body: ExprId) {
    let mut declared: HashMap<u32, (Ty, Vec<ExprId>)> = HashMap::new();
    let mut captured: Vec<(u32, Ty)> = Vec::new();
    let mut reads: Vec<(ExprId, ExprId, Ty)> = Vec::new();
    let mut writes: Vec<(ExprId, ExprId, Ty)> = Vec::new();
    collect_body(ir, body, &mut declared, &mut captured, &mut reads, &mut writes);

    let mut erased_of: HashMap<u32, Ty> = HashMap::new();
    for (slot, erased) in captured {
        let Some((specialized, _)) = declared.get(&slot) else {
            continue;
        };
        if holder_class(&erased).0 == holder_class(specialized).0 {
            continue;
        }
        match erased_of.get(&slot).copied() {
            Some(existing) if holder_class(&existing).0 != holder_class(&erased).0 => {
                if holder_class(&erased).0 == OBJECT_REF {
                    erased_of.insert(slot, erased);
                }
            }
            None => {
                erased_of.insert(slot, erased);
            }
            Some(_) => {}
        }
    }
    if erased_of.is_empty() {
        return;
    }
    for (id, holder, element) in reads {
        let Some(slot) = value_slot(ir, holder) else {
            continue;
        };
        let Some(erased) = erased_of.get(&slot).copied() else {
            continue;
        };
        if holder_class(&element).0 == holder_class(&erased).0 {
            continue;
        }
        crate::trace_compiler!(
            "lower",
            "erased shared-cell read slot={slot} element={element:?} holder={erased:?}"
        );
        reread_through_erased_holder(ir, id, element, erased);
    }
    for (id, holder, element) in writes {
        let Some(slot) = value_slot(ir, holder) else {
            continue;
        };
        let Some(erased) = erased_of.get(&slot).copied() else {
            continue;
        };
        if holder_class(&element).0 == holder_class(&erased).0 {
            continue;
        }
        box_into_erased_holder(ir, id, erased);
    }
    for (slot, (_, holders)) in declared {
        let Some(erased) = erased_of.get(&slot).copied() else {
            continue;
        };
        for holder in holders {
            let IrExpr::RefNew { elem, .. } = ir.expr(holder) else {
                continue;
            };
            if holder_class(elem).0 == holder_class(&erased).0 {
                continue;
            }
            crate::trace_compiler!(
                "lower",
                "erased shared-cell slot={slot} element={elem:?} holder={erased:?}"
            );
            box_into_erased_holder(ir, holder, erased);
        }
    }
}

fn reread_through_erased_holder(ir: &mut IrFile, id: ExprId, specialized: Ty, erased: Ty) {
    let moved = ir.exprs[id as usize].clone();
    let moved_id = ir.add_expr(moved);
    if let IrExpr::RefGet { elem, .. } = &mut ir.exprs[moved_id as usize] {
        *elem = erased;
    }
    ir.exprs[id as usize] = IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: moved_id,
        type_operand: specialized,
    };
}

fn box_into_erased_holder(ir: &mut IrFile, id: ExprId, erased: Ty) {
    match ir.expr(id) {
        IrExpr::RefNew { init, .. } => {
            let init = *init;
            let init = init.map(|value| coerce_to(ir, value, erased));
            if let IrExpr::RefNew { elem, init: slot } = &mut ir.exprs[id as usize] {
                *elem = erased;
                *slot = init;
            }
        }
        IrExpr::RefSet { value, .. } => {
            let value = *value;
            let value = coerce_to(ir, value, erased);
            if let IrExpr::RefSet { elem, value: slot, .. } = &mut ir.exprs[id as usize] {
                *elem = erased;
                *slot = value;
            }
        }
        _ => {}
    }
}

fn coerce_to(ir: &mut IrFile, value: ExprId, target: Ty) -> ExprId {
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: value,
        type_operand: target,
    })
}

fn value_slot(ir: &IrFile, expr: ExprId) -> Option<u32> {
    match ir.expr(expr) {
        IrExpr::GetValue(slot) => Some(*slot),
        _ => None,
    }
}

enum BodyNode {
    Lambda {
        function: u32,
        captures: Vec<ExprId>,
    },
    Variable {
        index: u32,
        init: Option<ExprId>,
    },
    Read {
        holder: ExprId,
        element: Ty,
    },
    Write {
        holder: ExprId,
        element: Ty,
    },
    Descend,
}

fn collect_body(
    ir: &IrFile,
    root: ExprId,
    declared: &mut HashMap<u32, (Ty, Vec<ExprId>)>,
    captured: &mut Vec<(u32, Ty)>,
    reads: &mut Vec<(ExprId, ExprId, Ty)>,
    writes: &mut Vec<(ExprId, ExprId, Ty)>,
) {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = match ir.expr(id) {
            IrExpr::Lambda {
                impl_fn, captures, ..
            } => BodyNode::Lambda {
                function: *impl_fn,
                captures: captures.clone(),
            },
            IrExpr::Variable { index, init, .. } => BodyNode::Variable {
                index: *index,
                init: *init,
            },
            IrExpr::RefGet { holder, elem } => BodyNode::Read {
                holder: *holder,
                element: *elem,
            },
            IrExpr::RefSet { holder, elem, .. } => BodyNode::Write {
                holder: *holder,
                element: *elem,
            },
            _ => BodyNode::Descend,
        };
        match node {
            BodyNode::Lambda { function, captures } => {
                for (ordinal, capture) in captures.iter().copied().enumerate() {
                    let ordinal = u32::try_from(ordinal).expect("capture ordinal fits u32");
                    if let Some(erased) = ir.shared_capture_parameters.get(&(function, ordinal)) {
                        if let Some(slot) = value_slot(ir, capture) {
                            captured.push((slot, *erased));
                        }
                    }
                    pending.push(capture);
                }
            }
            BodyNode::Variable { index, init } => {
                if let Some(init) = init {
                    if let IrExpr::RefNew { elem, .. } = ir.expr(init) {
                        declared
                            .entry(index)
                            .or_insert_with(|| (*elem, Vec::new()))
                            .1
                            .push(init);
                    }
                    pending.push(init);
                }
            }
            BodyNode::Read { holder, element } => {
                reads.push((id, holder, element));
                pending.push(holder);
            }
            BodyNode::Write { holder, element } => {
                writes.push((id, holder, element));
                pending.push(holder);
                if let IrExpr::RefSet { value, .. } = ir.expr(id) {
                    pending.push(*value);
                }
            }
            BodyNode::Descend => {
                crate::ir::for_each_child(&ir.exprs, id, &mut |child| pending.push(child));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrClass, IrCtorArg, IrField};

    #[test]
    fn realizes_only_marked_class_capture_slots() {
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(crate::types::type_name("Captured"));
        class.fields = vec![
            IrField::new("shared".to_string(), Ty::String),
            IrField::new("plain".to_string(), Ty::String),
        ];
        class.ctor_args = vec![
            IrCtorArg {
                name: Some("shared".to_string()),
                context_kind: crate::types::ContextParameterKind::None,
                ty: Ty::String,
                declared_ty: None,
                is_field: true,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: Some("shared".to_string()),
                anonymous_super_forward: None,
                capture: None,
                provenance: crate::ir::IrCtorParameterProvenance::Value,
                capture_identity: None,
            },
            IrCtorArg {
                name: Some("plain".to_string()),
                context_kind: crate::types::ContextParameterKind::None,
                ty: Ty::String,
                declared_ty: None,
                is_field: true,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: Some("plain".to_string()),
                anonymous_super_forward: None,
                capture: None,
                provenance: crate::ir::IrCtorParameterProvenance::Value,
                capture_identity: None,
            },
        ];
        let class = ir.add_class(class);
        ir.shared_class_capture_fields
            .insert((class, 0), Ty::String);

        lower_class_capture_slots(&mut ir);

        assert_eq!(
            ir.classes[class as usize].fields[0].ty,
            Ty::obj_args(OBJECT_REF, &[Ty::String])
        );
        assert_eq!(ir.classes[class as usize].fields[1].ty, Ty::String);
        assert_eq!(
            ir.classes[class as usize].ctor_args[0].ty,
            Ty::obj_args(OBJECT_REF, &[Ty::String])
        );
        assert_eq!(ir.classes[class as usize].ctor_args[1].ty, Ty::String);
        assert_eq!(
            ir.shared_class_capture_fields.get(&(class, 0)),
            Some(&Ty::String)
        );
    }

    #[test]
    fn nullable_primitive_uses_object_holder() {
        assert_eq!(
            holder_class(&Ty::nullable(Ty::Int)).0,
            "kotlin/jvm/internal/Ref$ObjectRef"
        );
    }

    #[test]
    fn realizes_only_the_exact_shared_super_capture_parameter() {
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(crate::types::type_name("Derived"));
        class.super_ctor_params = vec![Ty::String, Ty::String];
        let class = ir.add_class(class);
        ir.shared_super_capture_parameters
            .insert((class, 0), Ty::String);

        lower_class_capture_slots(&mut ir);

        assert_eq!(
            ir.classes[class as usize].super_ctor_params,
            [Ty::obj_args(OBJECT_REF, &[Ty::String]), Ty::String]
        );
    }

    fn function(name: &str) -> crate::ir::IrFunction {
        crate::ir::IrFunction {
            name: name.to_string(),
            params: Vec::new(),
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        }
    }

    #[test]
    fn specialized_cell_captured_by_an_erased_lambda_keeps_the_erased_holder() {
        let mut ir = IrFile::default();
        let lambda = ir.add_fun(function("read"));
        let initial = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
        let cell = ir.add_expr(IrExpr::RefNew {
            elem: Ty::Int,
            init: Some(initial),
        });
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(cell),
            named: true,
        });
        let holder = ir.add_expr(IrExpr::GetValue(0));
        let read = ir.add_expr(IrExpr::RefGet {
            holder,
            elem: Ty::Int,
        });
        let written = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(2)));
        let write_holder = ir.add_expr(IrExpr::GetValue(0));
        let write = ir.add_expr(IrExpr::RefSet {
            holder: write_holder,
            elem: Ty::Int,
            value: written,
        });
        let capture = ir.add_expr(IrExpr::GetValue(0));
        let inner_holder = ir.add_expr(IrExpr::GetValue(0));
        let inner = ir.add_expr(IrExpr::RefGet {
            holder: inner_holder,
            elem: Ty::Int,
        });
        let lambda_expr = ir.add_expr(IrExpr::Lambda {
            impl_fn: lambda,
            arity: 0,
            captures: vec![capture],
            sam: None,
            inline_body: Some(inner),
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, write, lambda_expr],
            value: Some(read),
        });
        let caller = ir.add_fun(function("box"));
        ir.functions[caller as usize].body = Some(body);
        ir.shared_capture_parameters
            .insert((lambda, 0), Ty::ty_param("R", Ty::obj("kotlin/Any")));

        restore_erased_escaping_capture_holders(&mut ir);

        let IrExpr::RefNew {
            elem,
            init: Some(init),
        } = ir.expr(cell)
        else {
            panic!("cell");
        };
        assert!(matches!(elem, Ty::TyParam(name, _) if *name == "R"));
        assert!(matches!(
            ir.expr(*init),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == initial
        ));
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: moved,
            type_operand,
        } = ir.expr(read)
        else {
            panic!("read");
        };
        assert_eq!(*type_operand, Ty::Int);
        assert!(matches!(
            ir.expr(*moved),
            IrExpr::RefGet { elem, .. } if matches!(elem, Ty::TyParam(name, _) if *name == "R")
        ));
        let IrExpr::RefSet { elem, value, .. } = ir.expr(write) else {
            panic!("write");
        };
        assert!(matches!(elem, Ty::TyParam(name, _) if *name == "R"));
        assert!(matches!(
            ir.expr(*value),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == written
        ));
        assert!(matches!(
            ir.expr(inner),
            IrExpr::RefGet { elem: Ty::Int, .. }
        ));
    }

    #[test]
    fn primitive_capture_on_the_same_holder_stays_primitive() {
        let mut ir = IrFile::default();
        let lambda = ir.add_fun(function("bump"));
        let cell = ir.add_expr(IrExpr::RefNew {
            elem: Ty::Int,
            init: None,
        });
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(cell),
            named: true,
        });
        let capture = ir.add_expr(IrExpr::GetValue(0));
        let lambda_expr = ir.add_expr(IrExpr::Lambda {
            impl_fn: lambda,
            arity: 0,
            captures: vec![capture],
            sam: None,
            inline_body: None,
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, lambda_expr],
            value: None,
        });
        let caller = ir.add_fun(function("box"));
        ir.functions[caller as usize].body = Some(body);
        ir.shared_capture_parameters.insert((lambda, 0), Ty::Int);

        restore_erased_escaping_capture_holders(&mut ir);

        assert!(matches!(
            ir.expr(cell),
            IrExpr::RefNew { elem: Ty::Int, .. }
        ));
    }
}
