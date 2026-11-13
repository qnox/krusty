//! Calls that select a supertype member a value class of this file overrides.
//!
//! A value class realizes each of its overrides as a static over its carrier (`ok-impl(LI;)`),
//! whether the override is written in source or generated for an interface delegation. kotlinc
//! dispatches a call on a receiver the program holds as the value class itself to that static,
//! even when the call selected the overridden supertype declaration, rather than boxing the
//! carrier to dispatch through the supertype. A receiver held as any supertype keeps virtual
//! dispatch; only the receiver's value-class representation selects the static.

use krusty::fir::ResolvedFunctionOverrideTarget;
use krusty::fx_hash::HashMap;
use krusty::ir::{Callee, ExprId, FunId, IrExpr, IrFile, IrTypeOp, IrVirtualTarget};
use krusty::types::{Ty, TypeName};

/// The function each value class of this file realizes an overridden declaration with.
pub(super) struct OverrideStatics {
    statics: HashMap<(TypeName, ResolvedFunctionOverrideTarget), OverrideStatic>,
    /// The selected declaration of each function in this IR file: a call to a same-file
    /// supertype names the function itself.
    module_functions: HashMap<FunId, ResolvedFunctionOverrideTarget>,
}

/// The function realizing one override, and the override's own checked parameters, which a call
/// that selected the overridden declaration is not represented for.
#[derive(Clone)]
struct OverrideStatic {
    function: FunId,
    parameters: Box<[Ty]>,
}

/// A call the receiver's value class answers with its override's static.
pub(super) struct OverrideStaticCall {
    pub(super) receiver: ExprId,
    pub(super) args: Vec<Option<ExprId>>,
    pub(super) value_class: TypeName,
    pub(super) function: FunId,
    pub(super) parameters: Box<[Ty]>,
}

impl OverrideStatics {
    /// Index the override edges of this file's value classes by the declaration they override.
    pub(super) fn plan(ir: &IrFile) -> Self {
        let module_functions = ir
            .checked_callable_functions
            .iter()
            .map(|(&callable, &function)| {
                (function, ResolvedFunctionOverrideTarget::Module(callable))
            })
            .collect::<HashMap<_, _>>();
        let statics = ir
            .classes
            .iter()
            .filter(|class| class.is_value)
            .flat_map(|class| {
                ir.function_overrides
                    .get(&class.fq_name)
                    .into_iter()
                    .flatten()
                    .filter(move |edge| edge.implementation_owner == class.fq_name)
            })
            .filter_map(|edge| {
                let function = match edge.implementation_function {
                    Some(function) => function,
                    None => match edge.implementation {
                        ResolvedFunctionOverrideTarget::Module(callable) => {
                            *ir.checked_callable_functions.get(&callable)?
                        }
                        ResolvedFunctionOverrideTarget::External(_) => return None,
                    },
                };
                let parameters = edge.implementation_parameters.clone().into_boxed_slice();
                Some((
                    (edge.implementation_owner, edge.overridden),
                    OverrideStatic {
                        function,
                        parameters,
                    },
                ))
            })
            .collect();
        Self {
            statics,
            module_functions,
        }
    }

    /// The override static `call` selects, when its receiver's value class (as `value_class_of`
    /// reports the receiver's representation) overrides the called declaration and has already
    /// realized that override as a static.
    pub(super) fn select(
        &self,
        ir: &IrFile,
        call: &IrExpr,
        value_class_of: impl Fn(ExprId) -> Option<TypeName>,
    ) -> Option<OverrideStaticCall> {
        let (overridden, receiver, args) = match call {
            IrExpr::Call {
                callee: call_callee,
                dispatch_receiver: Some(receiver),
                args,
            } if let Callee::Virtual {
                target: Some(IrVirtualTarget::Function(target)),
                ..
            } = &**call_callee =>
            {
                (*target, *receiver, args.iter().copied().map(Some).collect())
            }
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => {
                let function = *ir
                    .classes
                    .get(*class as usize)?
                    .methods
                    .get(*index as usize)?;
                // A call omitting a defaulted argument goes through the supertype's `$default`
                // stub, which dispatches on the box.
                if args.iter().any(Option::is_none) {
                    return None;
                }
                (
                    *self.module_functions.get(&function)?,
                    *receiver,
                    args.clone(),
                )
            }
            _ => return None,
        };
        let value_class = value_class_of(receiver)?;
        let realized = self.statics.get(&(value_class, overridden))?;
        if !ir
            .functions
            .get(realized.function as usize)
            .is_some_and(|function| function.is_static)
            || args.len() != realized.parameters.len()
        {
            return None;
        }
        // An argument the call adapted to the overridden declaration's parameter (`T` of a
        // generic supertype) is the override's own value; the override's static takes it as such.
        let args = args
            .into_iter()
            .zip(realized.parameters.iter())
            .map(|(argument, parameter)| {
                argument.map(|argument| match ir.exprs.get(argument as usize) {
                    Some(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg,
                        type_operand,
                    }) if type_operand != parameter => *arg,
                    _ => argument,
                })
            })
            .collect();
        Some(OverrideStaticCall {
            receiver,
            args,
            value_class,
            function: realized.function,
            parameters: realized.parameters.clone(),
        })
    }
}
