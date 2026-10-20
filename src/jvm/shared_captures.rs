//! JVM realization of shared mutable capture cells.
//!
//! Checked FIR and common IR keep the captured element's Kotlin type. A sparse identity edge marks
//! declaration slots that physically carry the shared cell; this pass realizes class fields and
//! constructor parameters as `kotlin.jvm.internal.Ref` holders without making that JVM choice part
//! of frontend semantics.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{ClassId, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::{Ty, TypeName};

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
    // Each cell is a field and the constructor-prefix parameter that fills it. A local class can
    // keep a capture in its parameter alone, so the two positions can differ.
    let mut physical = HashMap::<ClassId, Vec<(usize, usize, Ty)>>::default();
    for &(class, field) in ir.shared_class_capture_fields.keys() {
        let declaration = &ir.classes[class as usize];
        let parameter = declaration
            .ctor_args
            .iter()
            .position(|argument| argument.field_index == Some(field))
            .expect("a shared capture field is filled by a constructor-prefix parameter");
        let field = field as usize;
        let element = declaration.fields[field].ty;
        physical
            .entry(class)
            .or_default()
            .push((field, parameter, holder_ty(&element)));
    }
    for slots in physical.values_mut() {
        slots.sort_unstable_by_key(|(field, _, _)| *field);
    }

    for (&class, slots) in &physical {
        let declaration = &mut ir.classes[class as usize];
        for &(field, parameter, holder) in slots {
            declaration.fields[field].ty = holder;
            let argument = &mut declaration.ctor_args[parameter];
            argument.ty = holder;
            argument.check = None;
            for constructor in &mut declaration.secondary_ctors {
                constructor.prefix_params[parameter] = holder;
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
        for &(_, parameter, holder) in slots {
            parameters[parameter] = holder;
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
    let declarations = crate::ir::value_namespace_expressions(ir, body)
        .into_iter()
        .filter_map(|expression| match ir.expr(expression) {
            IrExpr::Variable { index, .. } => Some((*index, expression)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut declared: HashMap<u32, (Ty, Vec<ExprId>)> = HashMap::default();
    let mut captured: Vec<(u32, Option<(u32, u32)>, Ty)> = Vec::new();
    let mut reads: Vec<(ExprId, ExprId, Ty)> = Vec::new();
    let mut writes: Vec<(ExprId, ExprId, Ty)> = Vec::new();
    collect_body(
        ir,
        body,
        &mut declared,
        &mut captured,
        &mut reads,
        &mut writes,
    );

    // One slot can be captured by several implementations. Each implementation's current
    // parameter is the holder that edge requires. The semantic type recorded when the capture
    // was built is not that holder: value-class lowering may already have specialized both the
    // cell and the parameter to `IntRef` while the record still names the value class.
    let mut captures_by_slot: HashMap<u32, Vec<(Option<(u32, u32)>, Ty)>> = HashMap::default();
    for (slot, parameter, required) in captured {
        captures_by_slot
            .entry(slot)
            .or_default()
            .push((parameter, required));
    }
    let mut erased_of = HashMap::default();
    let mut retargeted = Vec::new();
    for (slot, edges) in captures_by_slot {
        let Some((specialized, _)) = declared.get(&slot) else {
            continue;
        };
        let specialized_holder = holder_class(specialized).0;
        if edges
            .iter()
            .all(|(_, required)| holder_class(required).0 == specialized_holder)
        {
            continue;
        }
        // A shared implementation over an erased type parameter requires ObjectRef. If another
        // implementation was specialized to a primitive holder, move that exact capture contract
        // to the erased holder too; two holder objects could not represent one mutable source var.
        let Some(erased) = edges
            .iter()
            .map(|(_, required)| *required)
            .find(|required| holder_class(required).0 == OBJECT_REF)
        else {
            continue;
        };
        for (parameter, required) in edges {
            // A constructor's holder is its class's ABI and is never moved.
            if let Some((function, ordinal)) = parameter {
                if holder_class(&required).0 != OBJECT_REF {
                    retargeted.push((function, ordinal, erased));
                }
            }
        }
        erased_of.insert(slot, erased);
    }
    if erased_of.is_empty() {
        return;
    }
    for (function, ordinal, erased) in retargeted {
        retarget_capture_parameter(ir, function, ordinal, erased);
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
            let IrExpr::RefNew { elem, init } = ir.expr(holder) else {
                continue;
            };
            let operand_declaration = init
                .and_then(|init| value_slot(ir, init))
                .and_then(|slot| declarations.get(&slot).copied())
                .filter(|declaration| ir.call_operand_bindings.contains(declaration))
                .filter(|declaration| ir.inline_declared_type(*declaration).is_some());
            if holder_class(elem).0 == holder_class(&erased).0 {
                continue;
            }
            crate::trace_compiler!(
                "lower",
                "erased shared-cell slot={slot} element={elem:?} holder={erased:?}"
            );
            box_into_erased_holder(ir, holder, erased);
            // The holder crosses an escaping closure's declaration-erased capture boundary. Its
            // initializer is the exact materialized inline operand recorded by common lowering;
            // preserve that declaration type in the physical slot too, so boxing happens at the
            // inline-call boundary rather than later at the holder write.
            if let Some(declaration) = operand_declaration {
                let declared = ir
                    .inline_declared_type(declaration)
                    .expect("filtered inline declaration type");
                if let IrExpr::Variable { ty, .. } = &mut ir.exprs[declaration as usize] {
                    *ty = declared;
                }
            }
        }
    }
}

/// Move one implementation's exact shared-capture parameter to `erased`, including every access
/// through that parameter. This keeps all consumers of one mutable source cell on the same holder.
fn retarget_capture_parameter(ir: &mut IrFile, function: u32, ordinal: u32, erased: Ty) {
    let Some(parameter) = ir
        .functions
        .get_mut(function as usize)
        .and_then(|function| function.params.get_mut(ordinal as usize))
    else {
        return;
    };
    if holder_class(parameter).0 == holder_class(&erased).0 {
        return;
    }
    *parameter = erased;
    if let Some(element) = ir.shared_capture_parameters.get_mut(&(function, ordinal)) {
        *element = erased;
    }
    let Some(body) = ir
        .functions
        .get(function as usize)
        .and_then(|function| function.body)
    else {
        return;
    };
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for id in expressions {
        match ir.expr(id) {
            IrExpr::RefGet { holder, elem } if value_slot(ir, *holder) == Some(ordinal) => {
                reads.push((id, *elem));
            }
            IrExpr::RefSet { holder, elem, .. } if value_slot(ir, *holder) == Some(ordinal) => {
                writes.push((id, *elem));
            }
            _ => {}
        }
    }
    for (id, specialized) in reads {
        if holder_class(&specialized).0 != holder_class(&erased).0 {
            reread_through_erased_holder(ir, id, specialized, erased);
        }
    }
    for (id, specialized) in writes {
        if holder_class(&specialized).0 != holder_class(&erased).0 {
            box_into_erased_holder(ir, id, erased);
        }
    }
}

/// Reconcile a lambda class's captured parameters with their exact shared-cell contracts before
/// class realization consumes the parameter ordinals. An inline specialization may specialize the
/// body access to a primitive while the declaration-owned capture edge remains erased; reads and
/// writes must be adapted while that stable `(function, ordinal)` identity is still available.
pub(super) fn realize_lambda_capture_parameters(ir: &mut IrFile, function: u32, captured: usize) {
    let contracts = (0..captured)
        .filter_map(|ordinal| {
            let ordinal = u32::try_from(ordinal).expect("lambda capture ordinal fits u32");
            ir.shared_capture_parameters
                .get(&(function, ordinal))
                .copied()
                .map(|element| (ordinal, element))
        })
        .collect::<Vec<_>>();
    for (ordinal, element) in contracts {
        retarget_capture_parameter(ir, function, ordinal, element);
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
            if let IrExpr::RefSet {
                elem, value: slot, ..
            } = &mut ir.exprs[id as usize]
            {
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
    /// An instance of a class whose constructor takes shared cells, such as a lambda class another
    /// declaration wrote: its constructor fixes each cell's holder.
    Construct {
        arguments: Vec<ExprId>,
        parameters: Vec<Ty>,
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

/// The constructor parameters of a lambda class this file declares: one per field.
fn lambda_class_constructor(ir: &IrFile, internal: TypeName) -> Option<Vec<Ty>> {
    let class = &ir.classes[ir.class_id_by_name(internal)? as usize];
    class.lambda.as_ref()?;
    Some(class.fields.iter().map(|field| field.ty).collect())
}

/// The element a constructor parameter of the erased `Ref$ObjectRef<T>` holder carries.
fn erased_cell_element(parameter: Ty) -> Option<Ty> {
    let Ty::Obj(holder, arguments) = parameter.non_null() else {
        return None;
    };
    if !holder.matches(OBJECT_REF) {
        return None;
    }
    arguments.first().copied()
}

fn collect_body(
    ir: &IrFile,
    root: ExprId,
    declared: &mut HashMap<u32, (Ty, Vec<ExprId>)>,
    captured: &mut Vec<(u32, Option<(u32, u32)>, Ty)>,
    reads: &mut Vec<(ExprId, ExprId, Ty)>,
    writes: &mut Vec<(ExprId, ExprId, Ty)>,
) {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
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
            IrExpr::New {
                internal,
                args,
                ctor_params,
                ..
            // A foreign class publishes its physical constructor parameters on the expression;
            // a lambda class realized in this file owns the same contract in its capture fields.
            } => match ctor_params
                .clone()
                .or_else(|| lambda_class_constructor(ir, *internal))
            {
                Some(parameters) => BodyNode::Construct {
                    arguments: args.clone(),
                    parameters,
                },
                None => BodyNode::Descend,
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
                    if ir
                        .shared_capture_parameters
                        .contains_key(&(function, ordinal))
                    {
                        let required = ir
                            .functions
                            .get(function as usize)
                            .and_then(|function| function.params.get(ordinal as usize))
                            .copied();
                        if let (Some(slot), Some(required)) = (value_slot(ir, capture), required) {
                            captured.push((slot, Some((function, ordinal)), required));
                        }
                    }
                    pending.push(capture);
                }
            }
            BodyNode::Construct {
                arguments,
                parameters,
            } => {
                for (argument, parameter) in arguments.iter().copied().zip(parameters) {
                    if let (Some(slot), Some(element)) =
                        (value_slot(ir, argument), erased_cell_element(parameter))
                    {
                        captured.push((slot, None, element));
                    }
                }
                pending.extend(arguments);
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
        // A local class's constructor prefix: an unstored capture kept in its parameter alone,
        // then the shared and the plain stored captures, so field and parameter positions differ.
        let capture = |field_index: Option<u32>| IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: Ty::String,
            declared_ty: None,
            is_field: field_index.is_some(),
            field_index,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: crate::ir::IrCtorParameterProvenance::Capture,
            capture_identity: None,
        };
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(crate::types::type_name("Captured"));
        class.fields = vec![
            IrField::new("shared".to_string(), Ty::String),
            IrField::new("plain".to_string(), Ty::String),
        ];
        class.ctor_args = vec![capture(None), capture(Some(0)), capture(Some(1))];
        class.constructor_prefix_count = 3;
        let class = ir.add_class(class);
        ir.shared_class_capture_fields
            .insert((class, 0), Ty::String);

        lower_class_capture_slots(&mut ir);

        let holder = Ty::obj_args(OBJECT_REF, &[Ty::String]);
        let lowered = &ir.classes[class as usize];
        assert_eq!(
            lowered
                .fields
                .iter()
                .map(|field| field.ty)
                .collect::<Vec<_>>(),
            [holder, Ty::String]
        );
        assert_eq!(
            lowered
                .ctor_args
                .iter()
                .map(|argument| argument.ty)
                .collect::<Vec<_>>(),
            [Ty::String, holder, Ty::String]
        );
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
        let operand = ir.add_expr(IrExpr::Variable {
            index: 1,
            ty: Ty::Int,
            init: Some(initial),
            named: true,
        });
        let operand_read = ir.add_expr(IrExpr::GetValue(1));
        let cell = ir.add_expr(IrExpr::RefNew {
            elem: Ty::Int,
            init: Some(operand_read),
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
            stmts: vec![operand, declaration, write, lambda_expr],
            value: Some(read),
        });
        let caller = ir.add_fun(function("box"));
        ir.functions[caller as usize].body = Some(body);
        ir.functions[lambda as usize].params = vec![Ty::ty_param("R", Ty::obj("kotlin/Any"))];
        ir.shared_capture_parameters
            .insert((lambda, 0), Ty::ty_param("R", Ty::obj("kotlin/Any")));
        ir.call_operand_bindings.insert(operand);
        ir.record_inline_declared_type(operand, Ty::ty_param("R", Ty::obj("kotlin/Any")));

        restore_erased_escaping_capture_holders(&mut ir);

        assert!(matches!(
            ir.expr(operand),
            IrExpr::Variable {
                ty: Ty::TyParam(name, _),
                ..
            } if *name == "R"
        ));

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
            } if *arg == operand_read
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
        ir.functions[lambda as usize].params = vec![Ty::Int];
        ir.shared_capture_parameters.insert((lambda, 0), Ty::Int);

        restore_erased_escaping_capture_holders(&mut ir);

        assert!(matches!(
            ir.expr(cell),
            IrExpr::RefNew { elem: Ty::Int, .. }
        ));
    }

    #[test]
    fn copied_generic_temporary_is_not_retyped_as_an_inline_operand() {
        let mut ir = IrFile::default();
        let lambda = ir.add_fun(function("read"));
        ir.functions[lambda as usize].params = vec![Ty::ty_param("R", Ty::obj("kotlin/Any"))];
        let initial = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
        let temporary = ir.add_expr(IrExpr::Variable {
            index: 1,
            ty: Ty::Int,
            init: Some(initial),
            named: false,
        });
        ir.record_inline_declared_type(temporary, Ty::ty_param("R", Ty::obj("kotlin/Any")));
        let temporary_read = ir.add_expr(IrExpr::GetValue(1));
        let cell = ir.add_expr(IrExpr::RefNew {
            elem: Ty::Int,
            init: Some(temporary_read),
        });
        let cell_declaration = ir.add_expr(IrExpr::Variable {
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
            stmts: vec![temporary, cell_declaration, lambda_expr],
            value: None,
        });
        let caller = ir.add_fun(function("box"));
        ir.functions[caller as usize].body = Some(body);
        ir.shared_capture_parameters
            .insert((lambda, 0), Ty::ty_param("R", Ty::obj("kotlin/Any")));

        restore_erased_escaping_capture_holders(&mut ir);

        assert!(matches!(
            ir.expr(temporary),
            IrExpr::Variable { ty: Ty::Int, .. }
        ));
        assert!(matches!(
            ir.expr(cell),
            IrExpr::RefNew {
                elem: Ty::TyParam(name, _),
                ..
            } if *name == "R"
        ));
    }

    #[test]
    fn value_class_capture_keeps_the_specialized_holder() {
        let mut ir = IrFile::default();
        let lambda = ir.add_fun(function("write"));
        ir.functions[lambda as usize].params = vec![Ty::Int];
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
        // The capture record still names the value class. The implementation parameter was
        // specialized with the cell, and that parameter is the holder.
        ir.shared_capture_parameters
            .insert((lambda, 0), Ty::obj("demo/Z"));

        restore_erased_escaping_capture_holders(&mut ir);

        assert!(matches!(
            ir.expr(cell),
            IrExpr::RefNew { elem: Ty::Int, .. }
        ));
    }

    #[test]
    fn disagreeing_implementations_share_the_erased_holder() {
        let mut ir = IrFile::default();
        let erased = ir.add_fun(function("erased"));
        ir.functions[erased as usize].params = vec![Ty::ty_param("R", Ty::obj("kotlin/Any"))];
        let specialized = ir.add_fun(function("specialized"));
        ir.functions[specialized as usize].params = vec![Ty::Int];
        let specialized_holder = ir.add_expr(IrExpr::GetValue(0));
        let specialized_read = ir.add_expr(IrExpr::RefGet {
            holder: specialized_holder,
            elem: Ty::Int,
        });
        ir.functions[specialized as usize].body = Some(specialized_read);
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
        let erased_capture = ir.add_expr(IrExpr::GetValue(0));
        let erased_lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: erased,
            arity: 0,
            captures: vec![erased_capture],
            sam: None,
            inline_body: None,
        });
        let specialized_capture = ir.add_expr(IrExpr::GetValue(0));
        let specialized_lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: specialized,
            arity: 0,
            captures: vec![specialized_capture],
            sam: None,
            inline_body: None,
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, erased_lambda, specialized_lambda],
            value: None,
        });
        let caller = ir.add_fun(function("box"));
        ir.functions[caller as usize].body = Some(body);
        ir.shared_capture_parameters
            .insert((erased, 0), Ty::ty_param("R", Ty::obj("kotlin/Any")));
        ir.shared_capture_parameters
            .insert((specialized, 0), Ty::Int);

        restore_erased_escaping_capture_holders(&mut ir);

        assert!(matches!(
            ir.expr(cell),
            IrExpr::RefNew {
                elem: Ty::TyParam(name, _),
                ..
            } if *name == "R"
        ));
        assert!(matches!(
            ir.functions[specialized as usize].params.as_slice(),
            [Ty::TyParam(name, _)] if *name == "R"
        ));
        assert!(matches!(
            ir.shared_capture_parameters.get(&(specialized, 0)),
            Some(Ty::TyParam(name, _)) if *name == "R"
        ));
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: moved,
            type_operand: Ty::Int,
        } = ir.expr(specialized_read)
        else {
            panic!("specialized read");
        };
        assert!(matches!(
            ir.expr(*moved),
            IrExpr::RefGet {
                elem: Ty::TyParam(name, _),
                ..
            } if *name == "R"
        ));
    }

    #[test]
    fn lambda_class_reconciles_shared_accesses_before_consuming_capture_ordinals() {
        let mut ir = IrFile::default();
        let lambda = ir.add_fun(function("specialized"));
        ir.functions[lambda as usize].params = vec![Ty::Int];
        let read_holder = ir.add_expr(IrExpr::GetValue(0));
        let read = ir.add_expr(IrExpr::RefGet {
            holder: read_holder,
            elem: Ty::Int,
        });
        let written = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(2)));
        let write_holder = ir.add_expr(IrExpr::GetValue(0));
        let write = ir.add_expr(IrExpr::RefSet {
            holder: write_holder,
            elem: Ty::Int,
            value: written,
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![write],
            value: Some(read),
        });
        ir.functions[lambda as usize].body = Some(body);
        let erased = Ty::ty_param("R", Ty::obj("kotlin/Any"));
        ir.shared_capture_parameters.insert((lambda, 0), erased);

        realize_lambda_capture_parameters(&mut ir, lambda, 1);

        assert_eq!(ir.functions[lambda as usize].params, [erased]);
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: moved,
            type_operand: Ty::Int,
        } = ir.expr(read)
        else {
            panic!("specialized read");
        };
        assert!(matches!(
            ir.expr(*moved),
            IrExpr::RefGet { elem, .. } if *elem == erased
        ));
        let IrExpr::RefSet { elem, value, .. } = ir.expr(write) else {
            panic!("specialized write");
        };
        assert_eq!(*elem, erased);
        assert!(matches!(
            ir.expr(*value),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } if *arg == written && *type_operand == erased
        ));
    }
}
