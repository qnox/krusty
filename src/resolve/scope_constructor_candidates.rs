//! Constructors reached by a bare `Name(args)` on the file and import scope tower, normalized into
//! the [`FunctionInfo`](crate::libraries::FunctionInfo) candidate structure same-named functions use.

use super::*;

/// A bare call's receiver-less candidates: the same-named functions at their nearest scope level,
/// and the constructors of the classifier the name also denotes.
pub(super) struct ReceiverlessCallFamily {
    pub(super) functions: Vec<crate::libraries::FunctionInfo>,
    /// The classifier's constructors when no same-named function exists: the call's whole family.
    pub(super) scope_constructors: Vec<crate::libraries::FunctionInfo>,
    /// The classifier's constructors competing with `functions`, each family stamped with its own
    /// scope rung; `functions` then carry theirs too.
    pub(super) competing_constructors: Vec<crate::libraries::FunctionInfo>,
}

/// The classifier's constructors of a bare call, whichever role they play in its family.
pub(super) fn constructors<'a>(
    scope_constructors: &'a [crate::libraries::FunctionInfo],
    competing: &'a [crate::libraries::FunctionInfo],
) -> &'a [crate::libraries::FunctionInfo] {
    if competing.is_empty() {
        scope_constructors
    } else {
        competing
    }
}

impl Checker<'_> {
    /// The receiver-less candidates of a bare `name(args)` that may also construct `classifier`.
    pub(super) fn receiverless_call_family(
        &self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        name: &str,
        classifier: Option<TypeName>,
    ) -> ReceiverlessCallFamily {
        let functions = self.resolver().top_level_candidates(name);
        let mut family = ReceiverlessCallFamily {
            functions,
            scope_constructors: Vec::new(),
            competing_constructors: Vec::new(),
        };
        let Some(internal) = classifier.filter(|_| !self.lexical_value_declares(scope, name))
        else {
            return family;
        };
        if family.functions.is_empty() {
            family.scope_constructors = self.scope_constructor_candidates(scope, internal, call);
            return family;
        }
        family.competing_constructors =
            self.competing_scope_constructors(scope, internal, call, name);
        if !family.competing_constructors.is_empty() {
            family.functions = self.resolver().ranked_top_level_candidates(name);
        }
        family
    }

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
        let origin = if declarations.is_empty() {
            declarations.extend(self.sam_constructor_declaration(internal));
            crate::libraries::ConstructorOrigin::FunctionalInterface
        } else {
            crate::libraries::ConstructorOrigin::Declared
        };
        declarations
            .into_iter()
            .map(|declaration| {
                let mut candidate = crate::libraries::FunctionInfo::scope_constructor(
                    internal,
                    &classifier,
                    declaration,
                    origin,
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

    /// The constructors of `internal` competing with the same-named receiver-less functions of a
    /// bare call, stamped with the scope rung on which the file/import tower selects `internal`
    /// for `name`, so one selection ranks the two families. Empty when that tower does not select
    /// `internal` itself: a local, nested or inherited classifier, or one named through an alias.
    fn competing_scope_constructors(
        &self,
        scope: &CheckerScope<'_>,
        internal: TypeName,
        call: ExprId,
        name: &str,
    ) -> Vec<crate::libraries::FunctionInfo> {
        let crate::symbol_resolver::CandidateSelection::Selected(selected) =
            self.resolver().scoped_classifier_in_scope(name)
        else {
            return Vec::new();
        };
        if selected.classifier != internal || selected.alias.is_some() {
            return Vec::new();
        }
        let mut constructors = self.scope_constructor_candidates(scope, internal, call);
        for constructor in &mut constructors {
            constructor.scope_rung = selected.scope_rung;
            constructor.scope_rank = selected.scope_rank;
        }
        constructors
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

    /// Whether this provider family contains the annotation-defined fallback constructor tier.
    pub(super) fn has_low_priority_constructor(
        &self,
        internal: TypeName,
        classifier: &crate::libraries::LibraryType,
    ) -> bool {
        classifier
            .constructors
            .iter()
            .chain(
                self.checked_local_constructors
                    .get(&internal)
                    .into_iter()
                    .flatten(),
            )
            .any(|constructor| is_low_priority_in_overload_resolution(&constructor.annotations))
    }

    /// Whether a selected constructor yields to the implicit-receiver rungs.
    pub(super) fn is_low_priority_constructor(
        &self,
        selected: &crate::libraries::FunctionInfo,
    ) -> bool {
        is_low_priority_in_overload_resolution(&selected.callable.annotations)
    }
}

/// Whether a constructor annotated with `annotations` is `@LowPriorityInOverloadResolution`: the
/// one check both a selected scope constructor and a classifier with no selected winner read.
pub(super) fn is_low_priority_in_overload_resolution(
    annotations: &[crate::types::TypeName],
) -> bool {
    annotations.contains(&crate::types::type_name(
        "kotlin/internal/LowPriorityInOverloadResolution",
    ))
}
