//! Calls that leave arguments out, rewritten to call their declaration's default-argument entry
//! with the whole frame: each supplied argument in its parameter's place, an
//! [`IrExpr::AbsentArgument`] in each omitted one, and the omission masks after them.
//!
//! A call this lowering cannot realize keeps its checked form, so the backend's own decline
//! names it: a member-extension receiver, a `super` call, an inherited default whose provider is
//! in another file, a dependency callable, and a constructor.

use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{Callee, ExprId, FunId, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

use super::entries::{Entries, StaticForm};
use super::masks::omission_masks;

/// The argument of each parameter of a call, `None` for one left to its default.
type Frame = Vec<Option<ExprId>>;

/// What a call with omitted arguments becomes.
enum Rewrite {
    /// A call of the entry of `declaration`, a function of this file.
    Entry {
        declaration: FunId,
        form: Option<StaticForm>,
        receiver: Option<ExprId>,
        frame: Frame,
    },
    /// A call of the entry another file of the module publishes, a
    /// [`Callee::ModuleDefaultEntry`].
    ModuleEntry { callee: Callee, frame: Frame },
}

/// Rewrite `site` when it is a call with omitted arguments this lowering realizes.
pub(super) fn rewrite(ir: &mut IrFile, entries: &mut Entries, site: ExprId) {
    match rewrite_of(ir, site) {
        Some(Rewrite::Entry {
            declaration,
            form,
            receiver,
            frame,
        }) => to_entry(ir, entries, site, declaration, form, receiver, frame),
        Some(Rewrite::ModuleEntry { callee, frame }) => to_module_entry(ir, site, callee, frame),
        None => {}
    }
}

fn rewrite_of(ir: &IrFile, site: ExprId) -> Option<Rewrite> {
    match ir.expr(site) {
        IrExpr::Call {
            callee,
            dispatch_receiver: None,
            args,
        } => match &**callee {
            Callee::LocalWithDefaults { function, defaults } => Some(Rewrite::Entry {
                declaration: *function,
                form: Some(StaticForm::Local),
                receiver: None,
                frame: frame(args, defaults, parameter_count(ir, *function))?,
            }),
            Callee::ClassStaticWithDefaults {
                owner,
                function,
                defaults,
            } => Some(Rewrite::Entry {
                declaration: *function,
                form: Some(StaticForm::ClassStatic(*owner)),
                receiver: None,
                frame: frame(args, defaults, parameter_count(ir, *function))?,
            }),
            Callee::ModuleWithDefaults {
                target,
                default_provider: ResolvedFunctionOverrideTarget::Module(provider),
                params,
                ret,
                defaults,
                dispatch_receiver_ty: None,
                ..
            } => Some(Rewrite::ModuleEntry {
                frame: frame(args, defaults, params.len())?,
                callee: Callee::ModuleDefaultEntry {
                    target: *target,
                    provider: *provider,
                    params: params.clone(),
                    ret: *ret,
                },
            }),
            _ => None,
        },
        IrExpr::Call {
            callee,
            dispatch_receiver: Some(receiver),
            args,
        } => {
            // An override inherits the defaults of the member it overrides; the provider's entry
            // fills them and dispatches through the slot the override shares.
            let Callee::ModuleWithDefaults {
                default_provider: ResolvedFunctionOverrideTarget::Module(provider),
                params,
                defaults,
                dispatch_receiver_ty: Some(_),
                extension_receiver_parameter: None,
                ..
            } = &**callee
            else {
                return None;
            };
            let declaration = *ir.checked_callable_functions.get(provider)?;
            let function = &ir.functions[declaration as usize];
            if function.dispatch_receiver.is_none() || function.params.len() != params.len() {
                return None;
            }
            Some(Rewrite::Entry {
                declaration,
                form: None,
                receiver: Some(*receiver),
                frame: frame(args, defaults, params.len())?,
            })
        }
        IrExpr::MethodCall {
            class,
            index,
            receiver,
            args,
        } if args.iter().any(Option::is_none) => {
            let declaration = ir.classes[*class as usize].methods[*index as usize];
            let function = &ir.functions[declaration as usize];
            (function.dispatch_receiver.is_some() && function.params.len() == args.len()).then(
                || Rewrite::Entry {
                    declaration,
                    form: None,
                    receiver: Some(*receiver),
                    frame: args.clone(),
                },
            )
        }
        _ => None,
    }
}

fn parameter_count(ir: &IrFile, function: FunId) -> usize {
    ir.functions[function as usize].params.len()
}

/// The frame of a call that supplies `args` in declaration order and leaves `defaults` out.
fn frame(args: &[ExprId], defaults: &[u32], parameters: usize) -> Option<Frame> {
    if args.len() + defaults.len() != parameters
        || defaults.windows(2).any(|pair| pair[0] >= pair[1])
        || defaults
            .last()
            .is_some_and(|&last| last as usize >= parameters)
    {
        return None;
    }
    let mut supplied = args.iter().copied();
    Some(
        (0..parameters)
            .map(|ordinal| {
                if defaults.contains(&(ordinal as u32)) {
                    None
                } else {
                    supplied.next()
                }
            })
            .collect(),
    )
}

/// Call the entry of `declaration`, a function of this file.
fn to_entry(
    ir: &mut IrFile,
    entries: &mut Entries,
    site: ExprId,
    declaration: FunId,
    form: Option<StaticForm>,
    receiver: Option<ExprId>,
    frame: Frame,
) {
    let omits_only_defaults = ir.param_defaults(declaration).is_some_and(|defaults| {
        frame.iter().enumerate().all(|(ordinal, argument)| {
            argument.is_some() || defaults.get(ordinal).is_some_and(Option::is_some)
        })
    });
    if !omits_only_defaults {
        return;
    }
    let Some(entry) = entries.entry(ir, declaration, form) else {
        return;
    };
    let parameters = ir.functions[declaration as usize].params.clone();
    let result = ir.functions[declaration as usize].ret;
    let mut args = receiver.into_iter().collect::<Vec<_>>();
    args.extend(arguments(ir, &parameters, frame));
    replace(
        ir,
        site,
        IrExpr::Call {
            callee: Box::new(Callee::Local(entry)),
            dispatch_receiver: None,
            args,
        },
        result,
    );
}

/// Call the entry another file of the module publishes for `callee`'s provider.
fn to_module_entry(ir: &mut IrFile, site: ExprId, callee: Callee, frame: Frame) {
    let Callee::ModuleDefaultEntry { params, ret, .. } = &callee else {
        unreachable!("a module default entry call")
    };
    let result = *ret;
    let args = arguments(ir, &params.clone(), frame);
    replace(
        ir,
        site,
        IrExpr::Call {
            callee: Box::new(callee),
            dispatch_receiver: None,
            args,
        },
        result,
    );
}

/// The entry's operands for `frame`: every parameter's, then the mask words.
fn arguments(ir: &mut IrFile, parameters: &[Ty], frame: Frame) -> Vec<ExprId> {
    let masks = omission_masks(
        parameters.len(),
        frame
            .iter()
            .enumerate()
            .filter(|(_, argument)| argument.is_none())
            .map(|(ordinal, _)| ordinal),
    );
    let mut args = frame
        .into_iter()
        .zip(parameters)
        .map(|(argument, &ty)| {
            argument.unwrap_or_else(|| ir.add_expr(IrExpr::AbsentArgument { ty }))
        })
        .collect::<Vec<_>>();
    args.extend(
        masks
            .into_iter()
            .map(|mask| ir.add_expr(IrExpr::Const(IrConst::Int(mask as i32)))),
    );
    args
}

/// Replace the call at `site` by `call`, whose value has the entry's declared `result`. The site
/// keeps the type it yields. A call whose recorded declaration result is the entry's, or that
/// recorded no type, stays a call; one that yields another type, as an override narrows the
/// result of the provider whose entry it calls, becomes a coercion of the entry's value.
fn replace(ir: &mut IrFile, site: ExprId, call: IrExpr, result: Ty) {
    let declared = ir.call_declared_ret.get(&site).copied();
    let site_type = ir.checked_type(site).or(declared);
    match site_type {
        _ if declared == Some(result) => {}
        None => {
            ir.logical_types.insert(site, result);
        }
        Some(site_type) if site_type == result => {}
        Some(site_type) => {
            let call = ir.add_expr(call);
            ir.logical_types.insert(call, result);
            ir.call_declared_ret.remove(&site);
            ir.exprs[site as usize] = IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: call,
                type_operand: site_type,
            };
            return;
        }
    }
    ir.exprs[site as usize] = call;
}
