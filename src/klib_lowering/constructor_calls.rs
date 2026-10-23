//! Constructor declarations authenticated independently of function invocation signatures.

use super::class_headers;
use super::decline::KlibBodyDeclineReason;
use super::function_lowering::vararg_parameters::VarargParameters;
use super::klib_types::semantic_type;
use crate::fir::ResolvedParameterIdentity;
use crate::fx_hash::{HashMap, HashSet};
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{
    ExternalCallableKind, KlibBodyCallable, KlibBodyClassParameters, ParamFlags,
};
use crate::metadata::klib_ir::tree::{KlibIrFunction, KlibIrNullability, KlibIrType};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{Ty, TyKind, TypeName};

pub(super) struct ConstructorDeclaration {
    pub(super) classifier: TypeName,
    params: Vec<Ty>,
    class_parameters: KlibBodyClassParameters,
    defaults: ParamFlags,
    pub(super) semantic_role: Option<crate::types::SemanticCallRole>,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

pub(super) fn require_constructor_kind(
    function: &KlibIrFunction,
) -> Result<(), KlibBodyDeclineReason> {
    if function.base.symbol.kind != KlibIrSymbolKind::Constructor || !function.constructor {
        return Err(mismatch(
            "the selected constructor is not serialized in a constructor slot",
        ));
    }
    Ok(())
}

pub(super) fn authenticate(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<ConstructorDeclaration, KlibBodyDeclineReason> {
    if selected.kind() != ExternalCallableKind::Constructor
        || selected.ret() != Ty::Unit
        || selected.generic_signature().is_some()
        || !selected.type_parameters().is_empty()
        || !selected.reified_type_parameter_ordinals().is_empty()
    {
        return Err(mismatch(
            "a constructor has an incompatible selected invocation contract",
        ));
    }
    let header = class_headers::authenticate(selected, bodies)?;
    let function = header.function;
    require_constructor_kind(function)?;
    if !function.type_parameters.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "constructor-owned type parameters",
        ));
    }
    if function.dispatch_receiver.is_some()
        || function.extension_receiver.is_some()
        || !function.context_parameters.is_empty()
        || selected.context_count() != 0
        || function.companion_extension_class.is_some()
    {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a constructor receiver or context",
        ));
    }
    if function.name != selected.name()
        || crate::metadata::semantic::declaration_visibility(function.base.flags)
            != selected.visibility()
        || !function.overridden.is_empty()
        || function.regular_parameters.len() != selected.params().len()
        || selected.parameter_identities().len() != selected.params().len()
    {
        return Err(mismatch(
            "the serialized constructor header disagrees with its declaration",
        ));
    }
    let defaults = selected
        .parameter_defaults()
        .ok_or_else(|| mismatch("a constructor has no known declared default parameters"))?;
    if defaults.len() != selected.params().len() {
        return Err(mismatch(
            "constructor default flags are not parallel to its parameters",
        ));
    }
    let mut symbols = HashSet::default();
    let mut varargs = VarargParameters::default();
    for (index, parameter) in function.regular_parameters.iter().enumerate() {
        let ty = semantic_type(header.arena, &header.types, parameter.ty)?;
        if parameter.base.symbol.kind != KlibIrSymbolKind::ValueParameter
            || !symbols.insert(&parameter.base.symbol)
            || selected.parameter_identities()[index]
                != ResolvedParameterIdentity::Source(parameter.name.as_str().into())
            || ty != selected.params()[index]
            || parameter.default_value.is_some() != defaults[index]
        {
            return Err(mismatch(
                "a constructor value parameter disagrees with its declaration",
            ));
        }
        if let Some(element) = parameter.vararg_element_type {
            varargs.record(
                index,
                true,
                ty,
                semantic_type(header.arena, &header.types, element)?,
            )?;
        }
    }
    varargs.authenticate(Some(selected), false)?;
    if !matches!(header.arena.ty(function.return_type),
        KlibIrType::Simple { classifier, nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull, .. }
        if classifier.kind == KlibIrSymbolKind::Class
            && classifier.signature == KlibIrSignature::Public(header.contract.signature.clone()))
    {
        return Err(mismatch(
            "a constructor result names another class declaration",
        ));
    }
    let result = semantic_type(header.arena, &header.types, function.return_type)?;
    let expected = Ty::obj_args_name(
        header.contract.classifier,
        &header.contract.parameters.formals,
    );
    if result != expected {
        return Err(mismatch(
            "a constructor result is not its declared class application",
        ));
    }
    Ok(ConstructorDeclaration {
        classifier: header.contract.classifier,
        params: selected.params().to_vec(),
        class_parameters: header.contract.parameters.clone(),
        defaults,
        semantic_role: selected.semantic_role(),
    })
}

impl ConstructorDeclaration {
    pub(super) fn default_allowed(&self, ordinal: usize) -> bool {
        self.defaults.get(ordinal).copied().unwrap_or(false)
    }

    pub(super) fn instantiate(
        &self,
        arguments: &[Ty],
        result: Ty,
        mut proves: impl FnMut(Ty, Ty) -> bool,
    ) -> Result<Vec<Ty>, KlibBodyDeclineReason> {
        if arguments.len() != self.class_parameters.formals.len() {
            return Err(mismatch(
                "a constructor passes another number of class type arguments",
            ));
        }
        let mut bindings = HashMap::default();
        for (formal, argument) in self.class_parameters.formals.iter().zip(arguments) {
            if argument.contains_error()
                || argument.mentions_pending()
                || matches!(
                    argument.kind(),
                    TyKind::InProjection(_) | TyKind::OutProjection(_) | TyKind::StarProjection(_)
                )
            {
                return Err(mismatch("a constructor has an invalid class type argument"));
            }
            let TyKind::TyParam(name, _) = formal.kind() else {
                return Err(mismatch(
                    "a constructor class formal has no semantic identity",
                ));
            };
            bindings.insert(name.as_str().to_owned(), *argument);
        }
        if result != Ty::obj_args_name(self.classifier, arguments) {
            return Err(mismatch(
                "a constructor call result disagrees with its class arguments",
            ));
        }
        for (argument, bounds) in arguments.iter().zip(&self.class_parameters.bounds) {
            let implicit;
            let bounds = if bounds.is_empty() {
                implicit = [Ty::nullable(Ty::obj("kotlin/Any"))];
                implicit.as_slice()
            } else {
                bounds.as_slice()
            };
            for bound in bounds {
                let bound = crate::types::ty_subst_applied_arguments(*bound, &bindings);
                if bound.contains_error() || bound.mentions_pending() || !proves(*argument, bound) {
                    return Err(mismatch(
                        "a constructor class argument does not meet its declared bounds",
                    ));
                }
            }
        }
        Ok(self
            .params
            .iter()
            .map(|parameter| crate::types::ty_subst_applied_arguments(*parameter, &bindings))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::klib_lowering::{constructor_bounds, klib_types::KlibTypeParameters};
    use crate::types::{type_name, TypeVariance};

    fn declaration(formals: Vec<Ty>, bounds: Vec<Vec<Ty>>) -> ConstructorDeclaration {
        let count = formals.len();
        ConstructorDeclaration {
            classifier: type_name("demo/Carrier"),
            params: formals.clone(),
            class_parameters: KlibBodyClassParameters {
                formals: formals.into_boxed_slice(),
                bounds: bounds.into_boxed_slice(),
                variances: vec![TypeVariance::Invariant; count].into_boxed_slice(),
                own_count: count,
            },
            defaults: ParamFlags::repeat(false, count),
            semantic_role: None,
        }
    }

    #[test]
    fn klib_constructor_substitution_proves_secondary_interdependent_and_f_bounds() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let left = Ty::obj("demo/Left");
        let right = Ty::obj("demo/Right");
        let a = Ty::ty_param("demo.Carrier.A", any);
        let b = Ty::ty_param("demo.Carrier.B", any);
        let c = Ty::ty_param("demo.Carrier.C", any);
        let comparable = |ty| Ty::obj_args_name(type_name("demo/Comparable"), &[ty]);
        let declaration = declaration(
            vec![a, b, c],
            vec![vec![left, right], vec![a], vec![comparable(c)]],
        );
        let t = Ty::ty_param("demo.caller.T", left);
        let u = Ty::ty_param("demo.caller.U", any);
        let v = Ty::ty_param("demo.caller.V", any);
        let args = [t, u, v];
        let result = Ty::obj_args_name(declaration.classifier, &args);
        let mut caller = KlibTypeParameters::default();
        assert!(caller.record_bounds(t, vec![left, right]));
        assert!(caller.record_bounds(u, vec![any, t]));
        assert!(caller.record_bounds(v, vec![comparable(v)]));
        let instantiate = |caller: &KlibTypeParameters| {
            declaration.instantiate(&args, result, |argument, bound| {
                constructor_bounds::satisfies(None, caller, argument, bound)
            })
        };
        assert_eq!(instantiate(&caller).unwrap(), args);
        for missing in 0..3 {
            let mut incomplete = KlibTypeParameters::default();
            assert!(incomplete.record_bounds(
                t,
                if missing == 0 {
                    vec![left]
                } else {
                    vec![left, right]
                }
            ));
            assert!(incomplete.record_bounds(
                u,
                if missing == 1 {
                    vec![any]
                } else {
                    vec![any, t]
                }
            ));
            assert!(incomplete.record_bounds(
                v,
                if missing == 2 {
                    vec![any]
                } else {
                    vec![comparable(v)]
                }
            ));
            assert!(
                instantiate(&incomplete).is_err(),
                "bound {missing} cannot be inferred from a carried primary bound"
            );
        }
    }

    #[test]
    fn klib_constructor_substitution_checks_result_arity_and_every_bound() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let formal = Ty::ty_param("demo.Carrier.T", any);
        let marker = Ty::obj("demo/Marker");
        let declaration = declaration(vec![formal], vec![vec![any, marker]]);
        let argument = Ty::obj("demo/Actual");
        let result = Ty::obj_args_name(declaration.classifier, &[argument]);
        let mut proofs = Vec::new();
        assert_eq!(
            declaration
                .instantiate(&[argument], result, |source, bound| {
                    proofs.push((source, bound));
                    true
                })
                .unwrap(),
            [argument]
        );
        assert_eq!(proofs, [(argument, any), (argument, marker)]);
        assert!(declaration
            .instantiate(&[argument], result, |_, bound| bound == any)
            .is_err());
        assert!(declaration
            .instantiate(&[argument], Ty::obj_name(declaration.classifier), |_, _| {
                true
            })
            .is_err());
        assert!(declaration.instantiate(&[], result, |_, _| true).is_err());
        for invalid in [Ty::Error, Ty::out_projection(argument)] {
            assert!(declaration
                .instantiate(
                    &[invalid],
                    Ty::obj_args_name(declaration.classifier, &[invalid]),
                    |_, _| true
                )
                .is_err());
        }
        assert!(!declaration.default_allowed(0));
        assert!(!declaration.default_allowed(1));
    }
}
