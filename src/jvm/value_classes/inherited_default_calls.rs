//! Calls that select an interface default a value class inherits without overriding.
//!
//! kotlinc's value-class lowering replaces the fake override of such a member with a static over
//! the carrier (`doubled-impl(I)I`, `getY-impl(I)I`, written beside the box's compatibility
//! forwarder), and a call or property access whose dispatch receiver is the value class itself
//! calls that static instead of boxing the carrier to dispatch through the interface. A receiver
//! typed as any supertype keeps interface dispatch; only the receiver's value-class representation
//! selects the static.

use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::fx_hash::HashMap;
use crate::ir::{Callee, ExprId, IrExpr, IrFile, IrVirtualTarget};
use crate::jvm::inherited_default_realization::{
    value_class_inherited_static, ValueClassInheritedStatic,
};
use crate::jvm::ir_emit::JvmDefaultMode;
use crate::jvm::property_realizations::{PropertyRealization, PropertyRealizations};
use crate::types::TypeName;

/// The inherited member a static realizes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum InheritedMember {
    Function(ResolvedFunctionOverrideTarget),
    Getter(ResolvedPropertyOverrideTarget),
    Setter(ResolvedPropertyOverrideTarget),
}

impl InheritedMember {
    fn from_default(default: &crate::fir::ResolvedInheritedDefault) -> Self {
        match (&default.name, default.function, default.accessor) {
            (crate::fir::InheritedMemberName::Function(_), Some(function), None) => {
                Self::Function(function)
            }
            (crate::fir::InheritedMemberName::PropertyGetter(_), None, Some(property)) => {
                Self::Getter(property)
            }
            (crate::fir::InheritedMemberName::PropertySetter(_), None, Some(property)) => {
                Self::Setter(property)
            }
            _ => unreachable!("published inherited defaults carry one validated selected target"),
        }
    }

    fn from_virtual(target: IrVirtualTarget) -> Self {
        match target {
            IrVirtualTarget::Function(target) => Self::Function(target),
            IrVirtualTarget::PropertyGetter(target) => Self::Getter(target),
            IrVirtualTarget::PropertySetter(target) => Self::Setter(target),
        }
    }
}

/// The inherited-default statics of the module's value classes, by class and selected declaration.
#[derive(Default)]
pub(crate) struct InheritedDefaultStatics {
    statics: HashMap<(TypeName, InheritedMember), ValueClassInheritedStatic>,
    /// The selected declaration of each function in this IR file that one of those statics
    /// realizes: a call to a same-file interface initially names the function itself.
    module_functions: HashMap<u32, ResolvedFunctionOverrideTarget>,
    /// The selected declaration of each still-semantic local property operation. Sibling-module
    /// and dependency accessors already carry the same identity in their `IrVirtualTarget`.
    local_properties: HashMap<ExprId, ResolvedPropertyOverrideTarget>,
}

/// A call the selected value class answers with its inherited-default static.
pub(super) struct InheritedDefaultCall<'a> {
    pub(super) receiver: ExprId,
    pub(super) args: Vec<ExprId>,
    pub(super) value_class: TypeName,
    pub(super) realized: &'a ValueClassInheritedStatic,
}

impl InheritedDefaultStatics {
    /// Plan, from override resolution's inherited-default records, the static each module or
    /// body-local value class writes for an inherited function or accessor under `jvm_default`.
    pub(crate) fn plan(
        ir: &IrFile,
        classifiers: &crate::backend::CheckedBackendClassifiers<'_>,
        callables: &crate::backend::CheckedBackendCallables,
        properties: &PropertyRealizations,
        jvm_default: JvmDefaultMode,
    ) -> Self {
        let local_properties = ir
            .exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::PropertyRead {
                    operation: Some(operation),
                    ..
                }
                | IrExpr::PropertyWrite {
                    operation: Some(operation),
                    ..
                } => match properties.get(*operation) {
                    Some(PropertyRealization::Local(property)) => Some((
                        *operation,
                        ResolvedPropertyOverrideTarget::Module(*property),
                    )),
                    _ => None,
                },
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let selected_members = ir
            .exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::Call {
                    callee: call_callee,
                    ..
                } if let Callee::Virtual {
                    target: Some(target),
                    ..
                } = &**call_callee =>
                {
                    Some(InheritedMember::from_virtual(*target))
                }
                IrExpr::PropertyRead {
                    operation: Some(operation),
                    ..
                } => Some(InheritedMember::Getter(*local_properties.get(operation)?)),
                IrExpr::PropertyWrite {
                    operation: Some(operation),
                    ..
                } => Some(InheritedMember::Setter(*local_properties.get(operation)?)),
                _ => None,
            })
            .collect::<crate::fx_hash::HashSet<_>>();
        let mut statics = HashMap::default();
        let module = classifiers.module();
        let module_value_classes = module.source_value_classes();
        let defaults = module_value_classes
            .keys()
            .filter(|&&class| ir.is_value_class_name(class))
            .filter_map(|&class| {
                Some((
                    class,
                    module.inherited_defaults(class)?,
                    ir.classes.iter().any(|local| local.fq_name == class),
                ))
            })
            .chain(
                ir.classes
                    .iter()
                    .filter(|class| {
                        class.is_value && !module_value_classes.contains_key(&class.fq_name)
                    })
                    .filter_map(|class| {
                        Some((
                            class.fq_name,
                            ir.inherited_defaults.get(&class.fq_name)?.as_slice(),
                            true,
                        ))
                    }),
            );
        for (class, defaults, local) in defaults {
            for default in defaults {
                let member = InheritedMember::from_default(default);
                // A named class in another source file contributes only statics selected by this
                // file's exact call/property identities. Its other dependency defaults need not be
                // frozen into this file's callable table. A class realized in this IR keeps every
                // default because its `MethodCall` nodes still identify same-file members by slot.
                if !local && !selected_members.contains(&member) {
                    continue;
                }
                if let Some(realized) =
                    value_class_inherited_static(ir, callables, default, jvm_default)
                {
                    statics.insert((class, member), realized);
                }
            }
        }
        let module_functions = statics
            .keys()
            .filter_map(|&(_, member)| match member {
                InheritedMember::Function(
                    target @ ResolvedFunctionOverrideTarget::Module(callable),
                ) => ir
                    .checked_callable_functions
                    .get(&callable)
                    .map(|&function| (function, target)),
                _ => None,
            })
            .collect();
        Self {
            statics,
            module_functions,
            local_properties,
        }
    }

    /// The inherited-default static `call` selects, when its receiver's value class (as
    /// `value_class_of` reports the receiver's representation) inherits the called interface
    /// function or accessed property without overriding it.
    pub(super) fn select<'a>(
        &'a self,
        ir: &IrFile,
        call: &IrExpr,
        value_class_of: impl Fn(ExprId) -> Option<TypeName>,
    ) -> Option<InheritedDefaultCall<'a>> {
        let (member, receiver, args) = match call {
            IrExpr::Call {
                callee: call_callee,
                dispatch_receiver: Some(receiver),
                args,
            } if let Callee::Virtual {
                target: Some(target),
                ..
            } = &**call_callee =>
            {
                (
                    InheritedMember::from_virtual(*target),
                    *receiver,
                    args.clone(),
                )
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
                // A call omitting a defaulted argument goes through the interface's `$default`
                // stub, which dispatches on the box.
                let args = args.iter().copied().collect::<Option<Vec<_>>>()?;
                let target = *self.module_functions.get(&function)?;
                (InheritedMember::Function(target), *receiver, args)
            }
            IrExpr::PropertyRead {
                receiver: Some(receiver),
                operation: Some(operation),
                ..
            } => (
                InheritedMember::Getter(*self.local_properties.get(operation)?),
                *receiver,
                Vec::new(),
            ),
            IrExpr::PropertyWrite {
                receiver: Some(receiver),
                value,
                operation: Some(operation),
                ..
            } => (
                InheritedMember::Setter(*self.local_properties.get(operation)?),
                *receiver,
                vec![*value],
            ),
            _ => return None,
        };
        let value_class = value_class_of(receiver)?;
        let realized = self.statics.get(&(value_class, member))?;
        Some(InheritedDefaultCall {
            receiver,
            args,
            value_class,
            realized,
        })
    }
}
