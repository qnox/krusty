//! Calls of dependency constructors and reads of dependency singletons and enum constants.
//!
//! A constructor call lowers as checked FIR lowers the source construction of a dependency class:
//! one construction of the class by the provider's constructor identity, with its supplied operands
//! in declaration order and the ordinals of the parameters it leaves to their defaults. An object
//! value lowers as the source reference of the singleton: its classifier's singleton value. An enum
//! entry lowers as the source reference of the constant, of the enum class the linker realized.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrArguments, KlibIrMemberAccess};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Ty;

pub(super) struct ConstructorOperands {
    pub(super) declaration: super::super::constructor_calls::ConstructorDeclaration,
    pub(super) external_target: Option<crate::fir::ExternalCallableId>,
    pub(super) local_access: Option<crate::ir::IrConstructorAccess>,
    /// The constructor of the class this unit materializes that a dependency constructor call
    /// selects.
    pub(super) materialized: Option<crate::ir::IrConstructorTarget>,
    pub(super) params: Vec<Ty>,
    pub(super) args: Vec<ExprId>,
    pub(super) defaults: Vec<u32>,
}

/// A constructor call's arguments in its constructor's parameter order: an inner class's outer
/// instance, its dispatch receiver, comes first, as a flat argument list already orders it.
fn constructor_arguments(
    access: &KlibIrMemberAccess,
    enclosing_instance: bool,
) -> Lowered<Vec<Option<crate::metadata::klib_ir::tree::KlibIrExprId>>> {
    match &access.arguments {
        KlibIrArguments::Flat(values) => Ok(values.clone()),
        KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } if !enclosing_instance => Ok(values.clone()),
        KlibIrArguments::Split {
            dispatch_receiver: Some(outer),
            extension_receiver: None,
            values,
        } if enclosing_instance => Ok(std::iter::once(Some(*outer))
            .chain(values.iter().copied())
            .collect()),
        _ => Err(mismatch(
            "a constructor call supplies a dispatch or extension receiver",
        )),
    }
}

impl BodyLowering<'_> {
    /// The value of the dependency object `symbol`, typed `ty`: the object's own class type.
    pub(super) fn object_value(&mut self, symbol: &KlibIrSymbol, ty: Ty) -> Lowered<ExprId> {
        if symbol.kind != KlibIrSymbolKind::Class {
            return Err(mismatch("an object value's symbol is not a class"));
        }
        // An object private to this body's library is installed in the unit, as a construction of
        // a private class is; any other object is the singleton its provider declares.
        let local = self.linker.local_class(&symbol.signature)?;
        let classifier = match (local, &symbol.signature) {
            (Some(classifier), _) => classifier,
            (None, KlibIrSignature::Public(object)) => {
                super::super::klib_types::classifier_name(object)?
            }
            (None, _) => return unsupported("a file-private object value"),
        };
        let object = crate::types::builtin_semantic(classifier).unwrap_or(Ty::obj_name(classifier));
        if ty != object {
            return Err(mismatch("an object value is typed apart from its object"));
        }
        if local.is_some() {
            self.linker.link_local_object(self.ir, &symbol.signature)?;
        } else {
            self.linker.singleton(self.ir, classifier)?;
        }
        // Recorded as checked FIR lowering records the source reference: `Unit` is its builtin
        // type rather than its class.
        let value = self.ir.add_expr(IrExpr::SingletonValue { classifier });
        self.ir.set_logical_type(value, object);
        Ok(value)
    }

    pub(super) fn enum_entry_value(&mut self, symbol: &KlibIrSymbol, ty: Ty) -> Lowered<ExprId> {
        if symbol.kind != KlibIrSymbolKind::EnumEntry {
            return Err(mismatch("an enum entry read's symbol is not an enum entry"));
        }
        let KlibIrSignature::Public(entry) = &symbol.signature else {
            return unsupported("a file-private enum entry");
        };
        let (classifier, name) = self.linker.enum_entry(self.ir, entry)?;
        if ty != Ty::obj_name(classifier) {
            return Err(mismatch("an enum entry read is typed apart from its enum"));
        }
        let value = self.ir.add_expr(IrExpr::EnumEntry {
            classifier,
            name: name.into(),
        });
        self.ir.set_logical_type(value, ty);
        Ok(value)
    }

    /// The constructor a call selects, with its supplied operands lowered in declaration order
    /// against the parameters the construction of `ty` instantiates, and the ordinals it leaves to
    /// their defaults. A construction and a delegation to a superclass constructor share it.
    pub(super) fn constructor_operands(
        &mut self,
        access: &KlibIrMemberAccess,
        constructor_type_arguments: u32,
        ty: Ty,
        role: super::ConstructorRole,
    ) -> Lowered<ConstructorOperands> {
        if access.symbol.kind != KlibIrSymbolKind::Constructor {
            return Err(mismatch("a constructor call's symbol is not a constructor"));
        }
        if constructor_type_arguments != 0 {
            return unsupported("a call of a generic constructor");
        }
        let mut type_arguments = access
            .type_arguments
            .iter()
            .map(|argument| {
                let argument = argument
                    .ok_or_else(|| mismatch("a constructor leaves a class type argument unset"))?;
                self.semantic(argument)
            })
            .collect::<Lowered<Vec<_>>>()?;
        let (declaration, external_target, local_access, materialized) =
            match &access.symbol.signature {
                KlibIrSignature::Public(callee)
                    if !self.linker.links_privately(&access.symbol.signature) =>
                {
                    let linked = self.linker.link_constructor(self.ir, callee, role)?;
                    (
                        linked.declaration,
                        Some(linked.target),
                        None,
                        linked.materialized,
                    )
                }
                signature => {
                    let linked = self.linker.link_local_constructor(self.ir, signature)?;
                    if linked.object {
                        return Err(mismatch("a constructor call creates an object's instance"));
                    }
                    (linked.declaration, None, Some(linked.access), None)
                }
            };
        // An inner class's type applies its outer class's type arguments after its own; the call
        // passes only its own, and the outer ones are those of the class type it constructs.
        if declaration.enclosing_instance {
            let applied = ty.type_args();
            if applied.len() < type_arguments.len()
                || applied[..type_arguments.len()] != type_arguments[..]
            {
                return Err(mismatch(
                    "an inner class construction is typed apart from its type arguments",
                ));
            }
            type_arguments = applied.to_vec();
        }
        let params = declaration.instantiate(&type_arguments, ty, |argument, bound| {
            self.linker
                .constructor_bound(self.header.type_parameters(), argument, bound)
        })?;
        let arguments = constructor_arguments(access, declaration.enclosing_instance)?;
        if arguments.len() != params.len() {
            return Err(mismatch(
                "a constructor call passes another number of arguments than its constructor declares",
            ));
        }
        let mut args = Vec::with_capacity(arguments.len());
        let mut defaults = Vec::new();
        for (ordinal, (argument, parameter)) in arguments.into_iter().zip(&params).enumerate() {
            match argument {
                Some(argument) => args.push(self.operand(argument, *parameter)?),
                // A default's ordinal counts the value parameters, after an outer instance.
                None if declaration.default_allowed(ordinal) => {
                    defaults.push((ordinal - usize::from(declaration.enclosing_instance)) as u32)
                }
                None => {
                    return Err(mismatch(
                        "a constructor omits a parameter without a declared default",
                    ))
                }
            }
        }
        Ok(ConstructorOperands {
            declaration,
            external_target,
            local_access,
            materialized,
            params,
            args,
            defaults,
        })
    }

    pub(super) fn constructor_call(
        &mut self,
        access: &KlibIrMemberAccess,
        constructor_type_arguments: u32,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let ConstructorOperands {
            declaration,
            external_target,
            local_access,
            materialized,
            params,
            args,
            defaults,
        } = self.constructor_operands(
            access,
            constructor_type_arguments,
            ty,
            super::ConstructorRole::Allocation,
        )?;
        match declaration.semantic_role {
            Some(crate::types::SemanticCallRole::KotlinArrayAllocation) => {
                // The allocated type is the constructed class's application: `Array<T>` with its
                // one element type, or the primitive array class itself.
                let allocates = if declaration.classifier == crate::types::wk::array() {
                    ty.is_reference_array() && ty.type_args().len() == 1
                } else {
                    ty == Ty::obj_name(declaration.classifier)
                        && crate::types::prim_array_element(declaration.classifier).is_some()
                };
                if !allocates || params != [Ty::Int] || !defaults.is_empty() {
                    return Err(mismatch(
                        "an array allocation has another constructor shape",
                    ));
                }
                let [size] = args.as_slice() else {
                    return Err(mismatch("an array allocation does not supply its size"));
                };
                return Ok(self.ir.add_expr(IrExpr::NewArray {
                    array_type: ty,
                    size: *size,
                }));
            }
            Some(_) => return unsupported("an unmodelled constructor semantic role"),
            None => {}
        }
        let site = self.ir.add_expr(IrExpr::New {
            internal: declaration.classifier,
            args,
            ctor_params: Some(params),
            declared_params: Some(declaration.declared_parameters().into()),
            ctor_desc: None,
            external_target,
            defaults: defaults.into_boxed_slice(),
            default_prefix_count: u32::from(declaration.enclosing_instance),
        });
        // The construction selects its constructor by place, as a source construction records it:
        // a generic class's instantiated parameters need not spell its declared ones.
        let target = local_access
            .map(super::super::same_library_class::construction_target)
            .or(materialized);
        if let Some(target) = target {
            self.ir.construction_targets.insert(site, target);
        }
        Ok(site)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrArguments, KlibIrExprKind};

    #[test]
    fn klib_constructor_call_rejects_receiver_slots_instead_of_value_arguments() {
        let mut arena = KlibIrArena::default();
        let value = arena.push_expr(None, KlibIrExprKind::Missing);
        let symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::Constructor,
            signature: KlibIrSignature::Public(
                crate::metadata::id_signature::KlibPublicIdSignature::classifier(
                    crate::types::type_name("demo/Carrier"),
                ),
            ),
        };
        for (dispatch_receiver, extension_receiver) in [
            (Some(value), None),
            (None, Some(value)),
            (Some(value), Some(value)),
        ] {
            let access = KlibIrMemberAccess {
                symbol: symbol.clone(),
                arguments: KlibIrArguments::Split {
                    dispatch_receiver,
                    extension_receiver,
                    values: Vec::new(),
                },
                type_arguments: Vec::new(),
                origin: None,
            };
            assert!(
                matches!(
                    constructor_arguments(&access, false),
                    Err(super::super::super::decline::KlibBodyDeclineReason::SignatureMismatch(_))
                ),
                "a constructor cannot reinterpret a receiver as a value argument"
            );
        }
        for arguments in [
            KlibIrArguments::Flat(vec![Some(value), None]),
            KlibIrArguments::Split {
                dispatch_receiver: None,
                extension_receiver: None,
                values: vec![Some(value), None],
            },
        ] {
            let access = KlibIrMemberAccess {
                symbol: symbol.clone(),
                arguments,
                type_arguments: Vec::new(),
                origin: None,
            };
            assert_eq!(
                constructor_arguments(&access, false).unwrap(),
                [Some(value), None]
            );
        }
    }
}
