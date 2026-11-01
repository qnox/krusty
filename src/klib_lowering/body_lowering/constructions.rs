//! Calls of dependency constructors and reads of dependency singletons.
//!
//! A constructor call lowers as checked FIR lowers the source construction of a dependency class:
//! one construction of the class by the provider's constructor identity, with its supplied operands
//! in declaration order and the ordinals of the parameters it leaves to their defaults. An object
//! value lowers as the source reference of the singleton: its classifier's singleton value.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrArguments, KlibIrMemberAccess};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Ty;

fn constructor_arguments(
    access: &KlibIrMemberAccess,
) -> Lowered<Vec<Option<crate::metadata::klib_ir::tree::KlibIrExprId>>> {
    match &access.arguments {
        KlibIrArguments::Flat(values)
        | KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => Ok(values.clone()),
        KlibIrArguments::Split { .. } => Err(mismatch(
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
        let KlibIrSignature::Public(object) = &symbol.signature else {
            return unsupported("a file-private object value");
        };
        let classifier = super::super::klib_types::classifier_name(object)?;
        let object = crate::types::builtin_semantic(classifier).unwrap_or(Ty::obj_name(classifier));
        if ty != object {
            return Err(mismatch("an object value is typed apart from its object"));
        }
        self.linker.singleton(classifier)?;
        Ok(self.ir.add_expr(IrExpr::SingletonValue { classifier }))
    }

    pub(super) fn constructor_call(
        &mut self,
        access: &KlibIrMemberAccess,
        constructor_type_arguments: u32,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if access.symbol.kind != KlibIrSymbolKind::Constructor {
            return Err(mismatch("a constructor call's symbol is not a constructor"));
        }
        let KlibIrSignature::Public(callee) = &access.symbol.signature else {
            return unsupported("a call of a file-private constructor");
        };
        if constructor_type_arguments != 0 {
            return unsupported("a call of a generic constructor");
        }
        let linked = self.linker.link_constructor(self.ir, callee)?;
        let type_arguments = access
            .type_arguments
            .iter()
            .map(|argument| {
                let argument = argument
                    .ok_or_else(|| mismatch("a constructor leaves a class type argument unset"))?;
                super::super::klib_types::semantic_type(
                    self.arena,
                    self.header.type_parameters(),
                    argument,
                )
            })
            .collect::<Lowered<Vec<_>>>()?;
        let params = linked
            .declaration
            .instantiate(&type_arguments, ty, |argument, bound| {
                self.linker
                    .constructor_bound(self.header.type_parameters(), argument, bound)
            })?;
        let arguments = constructor_arguments(access)?;
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
                None if linked.declaration.default_allowed(ordinal) => {
                    defaults.push(ordinal as u32)
                }
                None => {
                    return Err(mismatch(
                        "a constructor omits a parameter without a declared default",
                    ))
                }
            }
        }
        match linked.declaration.semantic_role {
            Some(crate::types::SemanticCallRole::KotlinArrayAllocation) => {
                if linked.declaration.classifier != crate::types::wk::array()
                    || !ty.is_reference_array()
                    || ty.type_args().len() != 1
                    || params != [Ty::Int]
                    || !defaults.is_empty()
                {
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
        let construction = self.ir.add_expr(IrExpr::New {
            internal: linked.declaration.classifier,
            args,
            ctor_params: Some(params),
            ctor_desc: None,
            external_target: Some(linked.target),
            defaults: defaults.into_boxed_slice(),
            default_prefix_count: 0,
        });
        self.ir.construction_declared_params.insert(
            construction,
            linked.declaration.declared_parameters().into(),
        );
        Ok(construction)
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
                    constructor_arguments(&access),
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
            assert_eq!(constructor_arguments(&access).unwrap(), [Some(value), None]);
        }
    }
}
