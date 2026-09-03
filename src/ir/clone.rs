//! Structural cloning for one common-IR expression DAG.
//!
//! Inline expansion needs a private copy of a retained body. Keeping the arena plumbing here gives
//! every consumer one exhaustive child remapper instead of growing another ad-hoc expression walker.

use std::collections::HashMap;

use super::{ExprId, IrCheckedArgument, IrCheckedOperation, IrExpr, IrFile};

/// Clone `root` and every reachable expression, preserving sparse semantic/source facts. Returns
/// the new root and the complete old-to-new identity map.
pub fn clone_expression_dag(ir: &mut IrFile, root: ExprId) -> (ExprId, HashMap<ExprId, ExprId>) {
    fn clone_one(ir: &mut IrFile, source: ExprId, cloned: &mut HashMap<ExprId, ExprId>) -> ExprId {
        if let Some(&existing) = cloned.get(&source) {
            return existing;
        }
        let mut children = Vec::new();
        super::for_each_child(&ir.exprs, source, &mut |child| children.push(child));
        for child in children {
            clone_one(ir, child, cloned);
        }
        let mut expression = ir.exprs[source as usize].clone();
        remap_direct_children(&mut expression, |child| cloned[&child]);
        let target = ir.add_expr(expression);
        copy_expression_facts(ir, source, target);
        cloned.insert(source, target);
        target
    }

    let mut cloned = HashMap::new();
    let root = clone_one(ir, root, &mut cloned);
    (root, cloned)
}

fn map_option(value: &mut Option<ExprId>, map: &mut impl FnMut(ExprId) -> ExprId) {
    if let Some(value) = value {
        *value = map(*value);
    }
}

fn remap_argument(argument: &mut IrCheckedArgument, map: &mut impl FnMut(ExprId) -> ExprId) {
    match argument {
        IrCheckedArgument::Expression { value, .. } => *value = map(*value),
        IrCheckedArgument::Default { .. } => {}
        IrCheckedArgument::Vararg { elements, .. } => {
            for (value, _) in elements {
                *value = map(*value);
            }
        }
    }
}

/// Rewrite every direct child identity. Exhaustive by design: adding an `IrExpr` shape must update
/// both this function and `for_each_child` before the compiler builds.
fn remap_direct_children(expression: &mut IrExpr, mut map: impl FnMut(ExprId) -> ExprId) {
    match expression {
        IrExpr::Checked(operation) => match operation {
            IrCheckedOperation::Call {
                dispatch_receiver,
                extension_receiver,
                arguments,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
                arguments
                    .iter_mut()
                    .for_each(|argument| remap_argument(argument, &mut map));
            }
            IrCheckedOperation::ConstructorDelegation {
                outer_receiver,
                arguments,
                ..
            } => {
                map_option(outer_receiver, &mut map);
                arguments
                    .iter_mut()
                    .for_each(|argument| remap_argument(argument, &mut map));
            }
            IrCheckedOperation::BackingFieldRead { .. }
            | IrCheckedOperation::LateinitFieldRead { .. } => {}
            IrCheckedOperation::BackingFieldWrite { value, .. } => *value = map(*value),
            IrCheckedOperation::PropertyRead {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
                context_arguments
                    .iter_mut()
                    .for_each(|value| *value = map(*value));
            }
            IrCheckedOperation::PropertyWrite {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
                context_arguments
                    .iter_mut()
                    .for_each(|value| *value = map(*value));
                *value = map(*value);
            }
            IrCheckedOperation::ExternalPropertyRead {
                receiver,
                arguments,
                ..
            }
            | IrCheckedOperation::ExternalPropertyWrite {
                receiver,
                arguments,
                ..
            } => {
                map_option(receiver, &mut map);
                arguments.iter_mut().for_each(|value| *value = map(*value));
            }
            IrCheckedOperation::RangeConstruction { start, end, .. } => {
                *start = map(*start);
                *end = map(*end);
            }
            IrCheckedOperation::RangeContains {
                value, start, end, ..
            } => {
                *value = map(*value);
                *start = map(*start);
                *end = map(*end);
            }
            IrCheckedOperation::RangeLoop {
                start, end, body, ..
            } => {
                *start = map(*start);
                *end = map(*end);
                *body = map(*body);
            }
            IrCheckedOperation::CallableReference {
                dispatch_receiver,
                extension_receiver,
                ..
            }
            | IrCheckedOperation::PropertyReference {
                dispatch_receiver,
                extension_receiver,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
            }
        },
        IrExpr::Block { stmts, value } => {
            stmts.iter_mut().for_each(|value| *value = map(*value));
            map_option(value, &mut map);
        }
        IrExpr::When { branches } => branches.iter_mut().for_each(|(condition, body)| {
            map_option(condition, &mut map);
            *body = map(*body);
        }),
        IrExpr::Return(value) => map_option(value, &mut map),
        IrExpr::TypeOp { arg, .. }
        | IrExpr::NotNullAssert { operand: arg, .. }
        | IrExpr::LateinitCheck { operand: arg, .. }
        | IrExpr::Throw { operand: arg }
        | IrExpr::EnumValueOf { arg, .. }
        | IrExpr::ReifiedTypeOp { arg, .. }
        | IrExpr::RefNew { init: arg, .. }
        | IrExpr::RefGet { holder: arg, .. }
        | IrExpr::NewArray { size: arg, .. }
        | IrExpr::PrimitiveNeg { operand: arg, .. } => *arg = map(*arg),
        IrExpr::StringConcat(values)
        | IrExpr::New { args: values, .. }
        | IrExpr::Vararg {
            elements: values, ..
        } => values.iter_mut().for_each(|value| *value = map(*value)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => {
            *lhs = map(*lhs);
            *rhs = map(*rhs);
        }
        IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => *value = map(*value),
        IrExpr::SetField {
            receiver, value, ..
        }
        | IrExpr::RefSet {
            holder: receiver,
            value,
            ..
        } => {
            *receiver = map(*receiver);
            *value = map(*value);
        }
        IrExpr::PropertyWrite {
            receiver, value, ..
        } => {
            map_option(receiver, &mut map);
            *value = map(*value);
        }
        IrExpr::Variable { init, .. } => map_option(init, &mut map),
        IrExpr::EnclosingInstance { receiver, .. }
        | IrExpr::GetField { receiver, .. }
        | IrExpr::LateinitInitialized { receiver, .. } => *receiver = map(*receiver),
        IrExpr::PropertyRead { receiver, .. } => map_option(receiver, &mut map),
        IrExpr::Call {
            args,
            dispatch_receiver,
            ..
        } => {
            map_option(dispatch_receiver, &mut map);
            args.iter_mut().for_each(|value| *value = map(*value));
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            *receiver = map(*receiver);
            args.iter_mut()
                .flatten()
                .for_each(|value| *value = map(*value));
        }
        IrExpr::InvokeFunction { func, args, .. } => {
            *func = map(*func);
            args.iter_mut().for_each(|value| *value = map(*value));
        }
        IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } => {
            captures.iter_mut().for_each(|value| *value = map(*value));
            map_option(inline_body, &mut map);
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            *cond = map(*cond);
            *body = map(*body);
            map_option(update, &mut map);
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            *body = map(*body);
            catches
                .iter_mut()
                .for_each(|catch| catch.body = map(catch.body));
            map_option(finally, &mut map);
        }
        IrExpr::PluginPlaceholder { exprs, .. } => {
            exprs.iter_mut().for_each(|value| *value = map(*value));
        }
        IrExpr::KClassLiteral { value, .. } => map_option(value, &mut map),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::LocalPropertyReference { .. }
        | IrExpr::SingletonValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::GetStatic(_)
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::EnumEntries { .. }
        | IrExpr::ReifiedClassMarker { .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation => {}
    }
}

fn copy_expression_facts(ir: &mut IrFile, source: ExprId, target: ExprId) {
    macro_rules! copy_map {
        ($field:ident) => {
            if let Some(value) = ir.$field.get(&source).cloned() {
                ir.$field.insert(target, value);
            }
        };
    }
    copy_map!(fir_origins);
    copy_map!(annotation_constructions);
    copy_map!(constructor_default_arguments);
    copy_map!(expr_lines);
    copy_map!(expr_source_lines);
    copy_map!(expr_end_lines);
    copy_map!(logical_types);
    copy_map!(exhaustive_whens);
    copy_map!(physical_types);
    copy_map!(reified_call_subst);
    copy_map!(ext_call_source_receiver);
    copy_map!(call_declared_ret);
    copy_map!(call_declared_params);
    copy_map!(suspend_calls);
    copy_map!(value_class_suspend_calls);
    copy_map!(intrinsic_suspension_points);
    if ir.property_initializer_stores.contains(&source) {
        ir.property_initializer_stores.insert(target);
    }
}
