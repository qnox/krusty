//! The source input declaration retained when a receiver applies a member signature.

use super::GenericSig;
use crate::fx_hash::HashMap;
use crate::types::{Ty, TypeParameters};

/// Complete declaration-owned bounds of the formals applied by a receiver. Callable formals
/// remain in `GenericSig`; this frame never changes explicit callable type-argument arity.
#[derive(Clone, Default)]
pub(crate) struct ReceiverInputBounds(HashMap<String, Vec<Ty>>);

impl ReceiverInputBounds {
    pub(crate) fn from_parameters(parameters: &TypeParameters<Vec<Vec<Ty>>>) -> Self {
        Self::from_declared(
            parameters
                .type_params()
                .iter()
                .cloned()
                .zip(parameters.type_param_bounds().iter().cloned()),
        )
    }

    pub(crate) fn from_declared(bounds: impl IntoIterator<Item = (String, Vec<Ty>)>) -> Self {
        Self(bounds.into_iter().collect())
    }

    pub(crate) fn get(&self, formal: &str) -> Option<&[Ty]> {
        self.0.get(formal).map(Vec::as_slice)
    }
}

/// Classifier application changes callable parameter types, but checking a source argument still
/// needs the declaration's original input positions and authenticated receiver bindings. This is
/// declaration evidence, not a call-scoped capture or a physical parameter list.
#[derive(Clone)]
pub(crate) struct SourceInputDeclaration {
    pub(crate) signature: GenericSig,
    pub(crate) receiver_bindings: HashMap<String, Ty>,
    pub(crate) receiver_bounds: ReceiverInputBounds,
}

impl SourceInputDeclaration {
    pub(crate) fn declared_input(&self, parameter: usize) -> Option<Ty> {
        let declared = *self.signature.params.get(parameter)?;
        let bounds = self
            .receiver_bindings
            .keys()
            .filter_map(|formal| {
                let bounds = self.receiver_bounds.get(formal)?;
                let upper = if bounds.is_empty() {
                    Ty::nullable(Ty::obj_name(crate::types::wk::any()))
                } else {
                    Ty::intersection(bounds)
                };
                Some((formal.clone(), upper))
            })
            .collect();
        Some(crate::types::ty_with_param_bounds(declared, &bounds))
    }

    pub(crate) fn apply_receiver(
        &mut self,
        bindings: &HashMap<String, Ty>,
        bounds: &ReceiverInputBounds,
    ) {
        for actual in self.receiver_bindings.values_mut() {
            *actual = crate::types::ty_subst_keep_unbound(*actual, bindings);
        }
        self.receiver_bindings.extend(
            bindings
                .iter()
                .map(|(name, actual)| (name.clone(), *actual)),
        );
        self.receiver_bounds.0.extend(bounds.0.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        EmptySymbolSource, FnKind, FunctionInfo, GenericReturnPolicy, LibraryCallable,
    };

    fn member(parameters: Vec<Ty>, receiver: Ty) -> FunctionInfo {
        let callable = LibraryCallable::library(
            crate::types::type_name("sample/Bucket"),
            "operation",
            parameters.clone(),
            Ty::Unit,
            Ty::Unit,
            "()V",
        );
        let mut member = FunctionInfo::plain(FnKind::Member, Some(receiver), callable);
        member.generic_sig = Some(GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: parameters,
            ret: Ty::Unit,
            return_policy: GenericReturnPolicy::Exact,
        });
        member
    }

    #[test]
    fn receiver_application_retains_raw_input_slots_and_exact_bindings() {
        let parameter = Ty::ty_param("input-owner:T", Ty::nullable(Ty::obj("kotlin/Any")));
        let receiver = Ty::obj_args("sample/Bucket", &[parameter]);
        let mut function = member(vec![parameter, Ty::array(parameter)], receiver);
        let bindings = HashMap::from_iter([("input-owner:T".into(), Ty::out_projection(Ty::Int))]);
        crate::symbol_resolver::specialize_member_function(
            &EmptySymbolSource,
            Ty::obj_args("sample/Bucket", &[Ty::out_projection(Ty::Int)]),
            &mut function,
            &bindings,
            &ReceiverInputBounds::from_declared([("input-owner:T".into(), vec![])]),
        );
        assert_eq!(
            function.generic_sig.as_ref().unwrap().params[0],
            Ty::Nothing
        );
        let retained = function.source_inputs.as_ref().unwrap();
        assert_eq!(retained.signature.params, [parameter, Ty::array(parameter)]);
        assert_eq!(retained.receiver_bindings, bindings);
        let original = retained.signature.clone();
        let next = HashMap::from_iter([("input-owner:T".into(), Ty::out_projection(Ty::String))]);
        crate::symbol_resolver::specialize_member_function(
            &EmptySymbolSource,
            Ty::obj_args("sample/Bucket", &[Ty::out_projection(Ty::String)]),
            &mut function,
            &next,
            &ReceiverInputBounds::from_declared([("input-owner:T".into(), vec![])]),
        );
        assert_eq!(function.source_inputs.as_ref().unwrap().signature, original);
        assert_eq!(
            function.source_inputs.as_ref().unwrap().receiver_bindings,
            next
        );
    }

    #[test]
    fn method_owned_formals_never_become_receiver_bindings() {
        let parameter = Ty::ty_param("input-method:T", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut function = member(vec![parameter], Ty::obj("sample/Bucket"));
        function
            .generic_sig
            .as_mut()
            .unwrap()
            .formals
            .push("input-method:T".into());
        let bindings = HashMap::from_iter([("input-method:T".into(), Ty::out_projection(Ty::Int))]);
        crate::symbol_resolver::specialize_member_function(
            &EmptySymbolSource,
            Ty::obj("sample/Bucket"),
            &mut function,
            &bindings,
            &ReceiverInputBounds::from_declared([("input-method:T".into(), vec![])]),
        );
        assert!(function.source_inputs.is_none());
        assert_eq!(function.generic_sig.as_ref().unwrap().params, [parameter]);
    }

    #[test]
    fn receiver_application_retains_complete_symbolic_owner_bounds_separately() {
        let number = Ty::obj("kotlin/Number");
        let marker = Ty::obj("sample/Marker");
        let first = Ty::ty_param("owner:A", number);
        let second = Ty::ty_param("owner:B", first);
        let owner = TypeParameters::invariant(
            vec!["owner:A".into(), "owner:B".into()],
            vec![vec![number], vec![first, marker]],
        );
        let mut function = member(
            vec![first, second],
            Ty::obj_args("sample/Bucket", &[first, second]),
        );
        let method = function.generic_sig.as_mut().unwrap();
        method.formals.push("method:M".into());
        method.formal_bounds.push(vec![]);
        let method_formals = method.formals.clone();
        let method_bounds = method.formal_bounds.clone();
        let bindings = HashMap::from_iter([
            ("owner:A".into(), Ty::out_projection(number)),
            ("owner:B".into(), Ty::star_projection(first)),
        ]);
        crate::symbol_resolver::specialize_member_function(
            &EmptySymbolSource,
            Ty::obj_args(
                "sample/Bucket",
                &[Ty::out_projection(number), Ty::star_projection(first)],
            ),
            &mut function,
            &bindings,
            &ReceiverInputBounds::from_parameters(&owner),
        );
        let retained = function.source_inputs.as_ref().unwrap();
        assert_eq!(retained.signature.params, [first, second]);
        assert_eq!(
            retained.receiver_bounds.get("owner:A"),
            Some([number].as_slice())
        );
        assert_eq!(
            retained.receiver_bounds.get("owner:B"),
            Some([first, marker].as_slice())
        );
        assert_eq!(retained.signature.formals, method_formals);
        assert_eq!(retained.signature.formal_bounds, method_bounds);
        assert_eq!(
            function.generic_sig.as_ref().unwrap().formals,
            method_formals
        );
    }

    #[test]
    fn retained_suspend_inputs_use_the_same_logical_normalization_as_the_member() {
        let parameter = Ty::ty_param("suspend-owner:T", Ty::nullable(Ty::obj("kotlin/Any")));
        let continuation = Ty::obj_args_name(crate::types::wk::continuation(), &[Ty::Int]);
        let mut function = member(vec![parameter, continuation], Ty::obj("sample/Bucket"));
        function.flags.suspend = true;
        let bindings =
            HashMap::from_iter([("suspend-owner:T".into(), Ty::out_projection(Ty::String))]);
        crate::symbol_resolver::specialize_member_function(
            &EmptySymbolSource,
            Ty::obj("sample/Bucket"),
            &mut function,
            &bindings,
            &ReceiverInputBounds::from_declared([("suspend-owner:T".into(), vec![])]),
        );
        let retained = function.source_inputs.as_ref().unwrap();
        assert_eq!(retained.signature.params, [parameter]);
        assert_eq!(retained.signature.ret, Ty::Int);
        assert_eq!(
            function.generic_sig.as_ref().unwrap().params.len(),
            retained.signature.params.len()
        );
        assert_eq!(
            function.generic_sig.as_ref().unwrap().ret,
            retained.signature.ret
        );
    }
}
