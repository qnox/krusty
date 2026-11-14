//! Stable identities shared by resolver data-flow analyses for lexical value bindings.
//!
//! A spelling is used only to reach a scope entry. Once selected, flow facts carry the binding's
//! identity so a same-named declaration cannot consume or invalidate another binding's facts.

use super::scope::Ns;
use super::{
    Checker, CheckerScope, ErrorProvenance, Local, ReceiverFnValueOrigin, ScopeBinding,
    ValueBindingDeclaration,
};
use crate::{ast::Param, diag::Span, types::Ty};

/// Ephemeral identity of one lexical value during a bounded checker run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct BindingIdentity(u32);

impl BindingIdentity {
    pub(super) const fn new(identity: u32) -> Self {
        Self(identity)
    }
}

impl Checker<'_> {
    /// Find the currently visible spelling of one exact lexical binding. A same-named inner value
    /// makes the older binding inaccessible and therefore stops data-flow traversal.
    pub(super) fn visible_local_binding(
        &self,
        scope: &CheckerScope<'_>,
        identity: BindingIdentity,
    ) -> Option<(String, Local)> {
        let mut found = None;
        scope.visit_bindings(Ns::Value, |name, binding| {
            if found.is_none() {
                if let Some(local) = binding
                    .value()
                    .filter(|local| local.lexical_capture_identity == Some(identity.0))
                {
                    found = Some((name.to_string(), local));
                }
            }
        });
        let (name, local) = found?;
        (self.lookup(scope, &name)?.lexical_capture_identity == Some(identity.0))
            .then_some((name, local))
    }
}

impl super::Checker<'_> {
    pub(super) fn capture_lexical_identity(
        &mut self,
        origin: super::ReceiverFnValueOrigin,
    ) -> Option<u32> {
        match origin {
            super::ReceiverFnValueOrigin::Local => Some(self.allocate_lexical_capture_identity()),
            super::ReceiverFnValueOrigin::ClassStorage(storage) => match storage.identity {
                crate::fir::ClassCaptureIdentity::Value { binding, .. } => Some(binding),
                _ => None,
            },
            _ => None,
        }
    }
    pub(super) fn allocate_flow_identity(&mut self) -> u32 {
        let identity = self.next_flow_identity;
        self.next_flow_identity = identity
            .checked_add(1)
            .expect("too many lexical bindings in one bounded checker");
        identity
    }

    pub(super) fn allocate_lexical_capture_identity(&mut self) -> u32 {
        let identity = self.next_lexical_capture_identity;
        self.next_lexical_capture_identity = identity
            .checked_add(1)
            .expect("too many lexical bindings in one bounded checker");
        identity
    }
}

/// A source declaration role, not a parser-arena or checker binding id. Source spans are retained
/// by inline syntax, just as for receiver declaration roles; slots distinguish declared siblings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LexicalValueDeclaration {
    file: u32,
    role: LexicalValueRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LexicalValueRole {
    Parameter(Span),
    Statement { declaration: Span, slot: u32 },
    LambdaParameter { expression: Span, slot: u32 },
    Catch(Span),
    Setter(Span),
}

impl Checker<'_> {
    pub(super) fn lexical_binding_by_declaration(
        &self,
        scope: &CheckerScope<'_>,
        declaration: LexicalValueDeclaration,
    ) -> Option<(String, Local, u32)> {
        let mut selected = None;
        let mut depths = crate::fx_hash::HashMap::<String, u32>::default();
        scope.visit_bindings(Ns::Value, |name, binding| {
            let depth = depths.entry(name.to_string()).or_default();
            if selected.is_none() {
                if let Some(local) = binding
                    .value()
                    .filter(|local| local.lexical_declaration == Some(declaration))
                {
                    selected = Some((name.to_string(), local, *depth));
                }
            }
            *depth = depth
                .checked_add(1)
                .expect("too many shadowed lexical values");
        });
        selected
    }
}

impl Checker<'_> {
    /// Attach provenance immediately after this producer declares the binding on its own rung.
    /// This operation cannot select an outer same-named binding or a narrowing of another value.
    pub(super) fn record_lexical_declaration(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        role: LexicalValueRole,
    ) {
        let declaration = LexicalValueDeclaration {
            file: self.file_index,
            role,
        };
        assert_eq!(
            scope.rebind_nearest(name, Ns::Value, |binding, depth| {
                assert_eq!(
                    depth, 0,
                    "a source certificate belongs to its declaring rung"
                );
                let ScopeBinding::Value(mut local) = binding.clone() else {
                    panic!("a value declaration has a value binding")
                };
                assert!(local.lexical_capture_identity.is_some());
                local.lexical_declaration = Some(declaration);
                ScopeBinding::Value(local)
            }),
            Some(0)
        );
    }

    pub(super) fn record_context_declaration(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        receiver: &super::scope::ContextReceiver,
    ) {
        let Some(super::ReceiverDeclarationRole::ContextParameter(declaration)) =
            receiver.declaration_role()
        else {
            panic!("a declared named context parameter retains its declaration role")
        };
        self.record_lexical_declaration(scope, name, LexicalValueRole::Parameter(declaration));
    }

    /// Flow identity of the context parameter declared at `declaration`, when that binding is
    /// still visible. Capture order uses it to place the parameter beside the values it is read
    /// with; the parameter's receiver role is not itself a flow identity.
    pub(super) fn context_parameter_flow_identity(
        &self,
        scope: &CheckerScope<'_>,
        declaration: Span,
    ) -> Option<u32> {
        let declaration = LexicalValueDeclaration {
            file: self.file_index,
            role: LexicalValueRole::Parameter(declaration),
        };
        self.lexical_binding_by_declaration(scope, declaration)
            .map(|(_, local, _)| local.flow_identity)
    }

    pub(super) fn retain_capture_declaration(
        &self,
        scope: &CheckerScope<'_>,
        capture: &super::AnonymousObjectCapture,
    ) {
        scope
            .rebind_nearest(&capture.name, Ns::Value, |binding, depth| {
                assert_eq!(depth, 0);
                let ScopeBinding::Value(mut local) = binding.clone() else {
                    panic!("a realized capture has a value binding")
                };
                assert!(matches!(
                    local.origin,
                    ReceiverFnValueOrigin::ClassStorage(_)
                ));
                local.lexical_declaration = capture.lexical_declaration;
                ScopeBinding::Value(local)
            })
            .expect("a capture was declared on this rung");
    }
    pub(super) fn declare(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
        is_var: bool,
    ) -> u32 {
        self.declare_with_origin(
            scope,
            name,
            ty,
            is_var,
            ValueBindingDeclaration::ordinary(ReceiverFnValueOrigin::Local, ErrorProvenance::None),
        )
    }

    pub(super) fn declare_context_parameter(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
    ) {
        self.declare_with_origin(
            scope,
            name,
            ty,
            false,
            ValueBindingDeclaration::context_parameter(),
        );
    }

    pub(super) fn declare_inferred(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
        is_var: bool,
        error_provenance: ErrorProvenance,
    ) -> u32 {
        self.declare_with_origin(
            scope,
            name,
            ty,
            is_var,
            ValueBindingDeclaration::ordinary(ReceiverFnValueOrigin::Local, error_provenance),
        )
    }

    pub(super) fn declare_callable_reference(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
        is_var: bool,
        function_type: Ty,
    ) -> u32 {
        let lexical_capture_identity = Some(self.allocate_lexical_capture_identity());
        let flow_identity = self.allocate_flow_identity();
        scope.rebind(
            name,
            Ns::Value,
            ScopeBinding::Value(Local {
                ty,
                declared_ty: ty,
                write_ty: is_var.then_some(ty),
                is_var,
                error_provenance: ErrorProvenance::None,
                origin: ReceiverFnValueOrigin::Local,
                is_context_parameter: false,
                callable_reference_type: Some(function_type),
                delegate_storage_ty: None,
                shared_storage_cell: false,
                lexical_capture_identity,
                lexical_declaration: None,
                flow_identity,
                safe_call_origin: None,
            }),
        );
        flow_identity
    }

    pub(super) fn declare_with_origin(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
        is_var: bool,
        declaration: ValueBindingDeclaration,
    ) -> u32 {
        let ValueBindingDeclaration {
            origin,
            error_provenance,
            is_context_parameter,
            shared_storage_cell,
        } = declaration;
        // A NEW binding gets a new flow identity, so the property-path narrowings of a same-named
        // older value (`if (a.p == null) return; val a = …`) cannot apply to it.
        let write_ty = is_var.then_some(match origin {
            ReceiverFnValueOrigin::DispatchProperty { declared_ty, .. } => declared_ty,
            _ => ty,
        });
        let declared_ty = match origin {
            ReceiverFnValueOrigin::DispatchProperty { declared_ty, .. } => declared_ty,
            _ => ty,
        };
        let lexical_capture_identity = self.capture_lexical_identity(origin);
        let flow_identity = self.allocate_flow_identity();
        scope.rebind(
            name,
            Ns::Value,
            ScopeBinding::Value(Local {
                ty,
                declared_ty,
                write_ty,
                is_var,
                error_provenance,
                origin,
                is_context_parameter,
                callable_reference_type: ty.is_fun().then_some(ty),
                delegate_storage_ty: None,
                shared_storage_cell,
                lexical_capture_identity,
                lexical_declaration: None,
                flow_identity,
                safe_call_origin: None,
            }),
        );
        flow_identity
    }

    /// Shadow a binding with its flow-narrowed type (`if (x != null) …`). Unlike a fresh
    /// declaration this re-binds the SAME runtime value, so property-path narrowings rooted at it
    /// stay valid. A narrowed `var` stays writable (kotlinc allows `if (x != null) { x = null }`);
    /// `stmt_assign` then replaces or kills the shadow's proven type.
    pub(super) fn declare_narrowing_shadow(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
    ) {
        // Narrowing changes only the type of the existing value, never where that value comes from.
        // In particular, a bare member property is represented in the lexical namespace with a
        // `DispatchProperty` origin. Reclassifying its narrowed shadow as a local makes the checker
        // stop recording the receiver/property handoff and leaves lowering no legal operation to
        // consume.
        let previous = self.lookup(scope, name);
        let origin = previous
            .map(|local| local.origin)
            .unwrap_or(ReceiverFnValueOrigin::Local);
        let callable_reference_type = previous.and_then(|local| local.callable_reference_type);
        let delegate_storage_ty = previous.and_then(|local| local.delegate_storage_ty);
        let lexical_capture_identity = previous.and_then(|local| local.lexical_capture_identity);
        let flow_identity = match previous {
            Some(local) => local.flow_identity,
            None => self.allocate_flow_identity(),
        };
        let safe_call_origin = previous.and_then(|local| local.safe_call_origin);
        let declared_ty = previous.map(|local| local.declared_ty).unwrap_or(ty);
        let write_ty = previous.and_then(|local| local.write_ty);
        let is_var = previous.map(|local| local.is_var).unwrap_or(false);
        let error_provenance = previous
            .map(|local| local.error_provenance)
            .unwrap_or(ErrorProvenance::None);
        let is_context_parameter = previous.is_some_and(|local| local.is_context_parameter);
        scope.rebind(
            name,
            Ns::Value,
            ScopeBinding::Value(Local {
                ty,
                declared_ty,
                write_ty,
                is_var,
                error_provenance,
                origin,
                is_context_parameter,
                callable_reference_type,
                delegate_storage_ty,
                shared_storage_cell: previous.is_some_and(|local| local.shared_storage_cell),
                lexical_capture_identity,
                lexical_declaration: previous.and_then(|local| local.lexical_declaration),
                flow_identity,
                safe_call_origin,
            }),
        );
    }

    pub(super) fn declare_function_parameter(
        &mut self,
        scope: &CheckerScope<'_>,
        parameter: &Param,
        ty: Ty,
        inherited_equality_bound: Option<Ty>,
        is_context_parameter: bool,
    ) {
        if is_context_parameter {
            self.declare_context_parameter(scope, &parameter.name, ty);
        } else {
            self.declare(scope, &parameter.name, ty, false);
        }
        self.record_lexical_declaration(
            scope,
            &parameter.name,
            LexicalValueRole::Parameter(parameter.span),
        );
        if let Some(bound) = self
            .equality_bound_parameter_ty(scope, parameter)
            .or(inherited_equality_bound)
            .filter(|bound| self.receiver_is_assignable(*bound, ty))
        {
            // This is a declaration contract, not a branch-local proof. Rebind the same lexical
            // value so the refined read type survives child blocks and short-circuit RHS scopes;
            // `declare_narrowing_shadow` retains the original declared/storage type for writes and
            // checked-FIR smart-cast materialization.
            self.declare_narrowing_shadow(scope, &parameter.name, bound);
            self.equality_bound_parameters
                .insert((parameter.ty.span.lo, parameter.ty.span.hi), bound);
        }
    }
}

#[cfg(test)]
mod tests;
