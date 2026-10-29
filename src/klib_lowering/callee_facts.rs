//! The provider records of the dependency declarations a checked call selected, by exact KLIB
//! identity.
//!
//! The body library's producer graph resolves a serialized call to an exact module-qualified
//! identity before this table is consulted. When a checked source call also selected that identity,
//! its provider record (normalized parameters, result and parameter identities, exactly as the
//! backend handoff froze them) must agree with the serialized header. A bare signature is never a
//! key and an unrelated source selection is never an origin fallback.

use crate::fx_hash::{HashMap, HashSet};

use crate::libraries::{KlibBodyCallable, KlibDependencyCallable};

/// Frozen dependency declarations by their exact module-qualified KLIB identity.
#[derive(Clone, Default)]
pub struct KlibCalleeFacts<'a> {
    callables: HashMap<&'a KlibDependencyCallable, IndexedKlibCallee<'a>>,
    /// Header evidence is not a checked source call which requires an executable export.
    selected: HashSet<&'a KlibDependencyCallable>,
    /// Explicit same-compilation hierarchy evidence for authenticated dependency types.
    class_type_oracle: Option<&'a dyn crate::assignable::TypeOracle>,
}

#[derive(Clone)]
enum IndexedKlibCallee<'a> {
    Selected(Vec<KlibBodyCallable<'a>>),
    /// Two frozen declarations claim the signature. Each identity names one declaration, so the
    /// table answers for neither rather than picking one.
    Ambiguous,
}

/// What one exact identity contributes to a lowering operation.
#[derive(Clone, Copy)]
pub(super) enum KlibCalleeFact<'facts, 'callable> {
    /// One declaration selected through one or more exact call-edge singleton variants.
    Selected(&'facts [KlibBodyCallable<'callable>]),
    Ambiguous,
}

impl<'a> KlibCalleeFacts<'a> {
    pub(crate) fn with_type_oracle(
        mut self,
        oracle: &'a dyn crate::assignable::TypeOracle,
    ) -> Self {
        self.class_type_oracle = Some(oracle);
        self
    }

    pub(super) fn constructor_bound(
        &self,
        parameters: &super::klib_types::KlibTypeParameters,
        argument: crate::types::Ty,
        bound: crate::types::Ty,
    ) -> bool {
        super::type_evidence::satisfies(self.class_type_oracle, parameters, argument, bound)
    }

    pub(super) fn is_subtype(
        &self,
        parameters: &super::klib_types::KlibTypeParameters,
        sub: crate::types::Ty,
        sup: crate::types::Ty,
    ) -> bool {
        super::type_evidence::ordinary_subtype(self.class_type_oracle, parameters, sub, sup)
    }

    pub(super) fn constructor(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Option<super::constructor_calls::ConstructorDeclaration>,
        super::decline::KlibBodyDeclineReason,
    > {
        match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a constructor declaration".to_owned(),
                ))
            }
            Some(KlibCalleeFact::Selected(callables))
                if callables[0].kind() == crate::libraries::ExternalCallableKind::Constructor =>
            {
                super::constructor_calls::authenticate(callables[0], bodies).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn authenticates_constructor(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<bool, super::decline::KlibBodyDeclineReason> {
        self.constructor(identity, bodies)
            .map(|constructor| constructor.is_some())
    }

    /// The initializer graph of the same positive ordinary-class capability used by lowering.
    pub(crate) fn constructor_initialization_callees(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Option<Vec<KlibDependencyCallable>>, super::decline::KlibBodyDeclineReason> {
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => {
                super::class_initialization::constructor_initialization_callees(
                    callables[0],
                    bodies,
                )
            }
            _ => Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                "an initializer has no unique selected constructor declaration".to_owned(),
            )),
        }
    }

    /// Authenticate a declaration the target implements without lowering its serialized body.
    pub(crate) fn target_intrinsic(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
        supports: &dyn Fn(crate::libraries::CompilerIntrinsic) -> bool,
    ) -> Result<Option<crate::libraries::CompilerIntrinsic>, super::decline::KlibBodyDeclineReason>
    {
        use super::decline::KlibBodyDeclineReason;
        use super::function_lowering::{raw_function_header, HeaderPurpose};
        let selected = match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a target declaration".to_owned(),
                ));
            }
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            None => return Ok(None),
        };
        let Some(operation) = selected
            .compiler_intrinsic()
            .filter(|operation| supports(*operation))
        else {
            return Ok(None);
        };
        let (arena, declaration) = bodies
            .function(identity)
            .ok_or(KlibBodyDeclineReason::Unjoined)?;
        // A body-bearing target leaf still authenticates its complete declared header, not the
        // narrower set of declarations whose executable bodies common lowering supports.
        raw_function_header(Some(selected), HeaderPurpose::External, arena, declaration)?;
        Ok(Some(operation))
    }

    pub(crate) fn with_declaration(&self, declaration: KlibBodyCallable<'a>) -> Self {
        let mut facts = self.clone();
        facts.record(declaration, false);
        facts
    }

    pub(crate) fn target_getter(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
        supports: &dyn Fn(crate::libraries::DependencyGetterSignature<'_>) -> bool,
    ) -> Result<bool, super::decline::KlibBodyDeclineReason> {
        let selected = match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a getter declaration".to_owned(),
                ));
            }
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            None => return Ok(false),
        };
        if !super::accessor_calls::signature(selected).is_some_and(supports) {
            return Ok(false);
        }
        super::accessor_calls::authenticate(selected, bodies).map(|getter| getter.is_some())
    }

    pub(crate) fn common_primitive(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Option<super::primitive_calls::CommonPrimitiveCall>,
        super::decline::KlibBodyDeclineReason,
    > {
        match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a primitive declaration".to_owned(),
                ))
            }
            Some(KlibCalleeFact::Selected(callables)) if !self.is_selected(identity) => {
                super::primitive_calls::authenticate(callables[0], bodies)
            }
            _ => Ok(None),
        }
    }

    /// Index the frozen views of a caller's selected dependency declarations.
    pub fn new(callables: impl IntoIterator<Item = KlibBodyCallable<'a>>) -> Self {
        let mut facts = Self::default();
        for callable in callables {
            facts.selected.insert(callable.identity());
            facts.record(callable, true);
        }
        facts
    }

    /// Add exact provider header contracts without inventing source-call/export obligations.
    pub(crate) fn with_declarations(
        selected: impl IntoIterator<Item = KlibBodyCallable<'a>>,
        declarations: impl IntoIterator<Item = KlibBodyCallable<'a>>,
    ) -> Self {
        let mut facts = Self::new(selected);
        for declaration in declarations {
            facts.record(declaration, false);
        }
        facts
    }

    pub(super) fn is_selected(&self, identity: &KlibDependencyCallable) -> bool {
        self.selected.contains(identity)
    }

    fn record(&mut self, callable: KlibBodyCallable<'a>, call_edge: bool) {
        self.callables
            .entry(callable.identity())
            .and_modify(|fact| match fact {
                IndexedKlibCallee::Selected(selected)
                    if selected[0].same_declaration_contract(callable) =>
                {
                    if call_edge
                        && !selected.iter().any(|selected| {
                            selected.singleton_dispatch() == callable.singleton_dispatch()
                        })
                    {
                        selected.push(callable);
                    }
                }
                fact => *fact = IndexedKlibCallee::Ambiguous,
            })
            .or_insert_with(|| IndexedKlibCallee::Selected(vec![callable]));
    }

    pub(super) fn get<'facts>(
        &'facts self,
        identity: &KlibDependencyCallable,
    ) -> Option<KlibCalleeFact<'facts, 'a>> {
        match self.callables.get(identity)? {
            IndexedKlibCallee::Selected(selected) => {
                Some(KlibCalleeFact::Selected(selected.as_slice()))
            }
            IndexedKlibCallee::Ambiguous => Some(KlibCalleeFact::Ambiguous),
        }
    }
}
