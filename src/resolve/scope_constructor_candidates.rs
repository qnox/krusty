//! Constructors reached by a bare `Name(args)` on the file and import scope tower, normalized into
//! the [`FunctionInfo`](crate::libraries::FunctionInfo) candidate structure same-named functions use.

use super::*;

impl Checker<'_> {
    /// The constructors of `internal` as receiver-less candidates of a bare call, from the same
    /// declaration set the constructor path materializes. A classifier that captures an outer
    /// instance contributes nothing here: its constructors are member candidates of the receiver
    /// supplying that instance.
    ///
    /// The classifier's type parameters are the constructor's inference variables. Inside the
    /// classifier's own body those parameters are also fixed types in `scope`, distinct from the
    /// ones this construction infers, so a signature whose formals are lexically in scope is
    /// instantiated with the variables `call` owns.
    pub(super) fn scope_constructor_candidates(
        &self,
        scope: &CheckerScope<'_>,
        internal: TypeName,
        call: ExprId,
    ) -> Vec<crate::libraries::FunctionInfo> {
        let lexical = scope
            .lexical_tparam_identities()
            .into_iter()
            .map(crate::types::TyParamName::from)
            .collect::<Vec<_>>();
        let Some(classifier) = self.resolved_type_name(internal) else {
            return Vec::new();
        };
        if classifier.outer_instance.is_some() {
            return Vec::new();
        }
        let mut declarations = self.constructor_declarations(internal, &classifier);
        if declarations.is_empty() {
            declarations.extend(self.sam_constructor_declaration(internal));
        }
        declarations
            .into_iter()
            .map(|declaration| {
                let mut candidate = crate::libraries::FunctionInfo::scope_constructor(
                    internal,
                    &classifier,
                    declaration,
                );
                if let Some(signature) = candidate
                    .generic_sig
                    .as_ref()
                    .filter(|signature| signature.formals.iter().any(|f| lexical.contains(f)))
                {
                    let instantiated = crate::symbol_resolver::CallSiteVariables::instantiate(
                        signature,
                        self.call_site(call),
                    );
                    candidate.generic_sig = Some(instantiated.signature().clone());
                }
                candidate
            })
            .collect()
    }

    /// A functional interface's language-level constructor: one operand of the abstract method's
    /// function type. It has no `<init>` declaration, so it is published here with the classifier's
    /// type parameters as its inference variables, like any other constructor.
    fn sam_constructor_declaration(
        &self,
        internal: TypeName,
    ) -> Option<crate::libraries::LibraryMember> {
        let sam = self.semantic_sam_signature(Ty::obj_name(internal))?;
        let function = Ty::fun_with_shape(
            sam.params,
            sam.ret,
            sam.context_count,
            sam.has_receiver,
            sam.suspend,
        );
        Some(crate::libraries::LibraryMember::new(
            "<init>".into(),
            vec![function],
            Ty::Unit,
            String::new(),
        ))
    }

    /// The scope constructor that the shared callable selection chooses for a bare call. Only
    /// a declared constructor is returned: the functional-interface constructor has no
    /// declaration to materialize, and its own conversion path commits it.
    pub(super) fn scope_constructor_choice(
        &self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        type_args: &[Ty],
        expected: Option<Ty>,
        candidates: &[crate::libraries::FunctionInfo],
    ) -> Option<crate::libraries::FunctionInfo> {
        if candidates.is_empty() {
            return None;
        }
        match self.select_callable_candidate(
            scope,
            call_args,
            type_args,
            None,
            CallResultConstraint::direct(expected),
            candidates.to_vec(),
        )? {
            CallableCandidateSelection::Selected(selected)
                if selected.info.stable_declaration.is_some()
                    || !selected.info.callable.descriptor.is_empty() =>
            {
                Some(selected.info)
            }
            _ => None,
        }
    }

    /// Whether a selected constructor yields to the implicit-receiver rungs.
    pub(super) fn is_low_priority_constructor(
        &self,
        selected: &crate::libraries::FunctionInfo,
    ) -> bool {
        selected
            .callable
            .annotations
            .contains(&crate::types::type_name(
                "kotlin/internal/LowPriorityInOverloadResolution",
            ))
    }
}
