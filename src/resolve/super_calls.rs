//! What a `super` call may select, beyond finding the member.

use super::*;

#[derive(Clone, Debug)]
pub struct ResolvedSuperCall {
    /// Exact dispatch receiver selected by `super` / `super@Label`. A labeled super may target an
    /// enclosing class instance, so the callable target alone is not enough for lowering.
    pub receiver: ImplicitReceiverSelection,
    pub owner: TypeName,
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub physical_ret: Ty,
    /// Empty for a source signature whose descriptor is derived from the semantic parameter types.
    pub descriptor: String,
    pub interface: bool,
    /// Provider-owned physical realization of the selected semantic declaration. A JVM-default
    /// holder is an ordinary direct realization; lowering does not rediscover it from a mode or
    /// owner spelling.
    pub realization: crate::libraries::MemberRealization,
    /// Stable source declaration selected for this call. Dependency declarations leave this unset;
    /// current-compilation defaults use it to retain their exact checked default-expression owner.
    pub stable_declaration: Option<crate::fir::DeclarationId>,
    /// Kotlin property declaration selected by property syntax. The callable declaration above is
    /// still the exact accessor target used by FIR; editor/navigation consumers use this identity
    /// to reach the source property rather than its generated getter or setter.
    pub property_declaration: Option<crate::fir::DeclarationId>,
    pub source_member: Option<crate::libraries::SourceMember>,
    /// Exact semantic dependency property when this selection came from property syntax. The
    /// callable identity remains available for ordinary accessor calls, but checked FIR uses this
    /// declaration identity to retain property semantics through target-independent lowering.
    pub external_property: Option<crate::fir::ExternalPropertyId>,
}

impl ResolvedSuperCall {
    pub(super) fn selected(
        receiver: ImplicitReceiverSelection,
        dispatch_owner: TypeName,
        interface: bool,
        member: crate::libraries::LibraryMember,
    ) -> Option<Self> {
        let realization = member.realization;
        let stable_declaration = member.stable_declaration;
        let source_member = member.source_member;
        let external_property = member.external_property_identity;
        let physical_owner = member.owner?;
        let owner = match realization {
            // A selected class declaration remains the exact non-virtual target even when it was
            // inherited through another class. An interface declaration reached through a class
            // supertype must instead name that direct class in the InterfaceMethodref search path;
            // the normalized declaration kind, not its symbol-source origin, decides the shape.
            crate::libraries::MemberRealization::Dispatch if member.is_interface() => {
                dispatch_owner
            }
            crate::libraries::MemberRealization::Dispatch => physical_owner,
            crate::libraries::MemberRealization::Direct { .. } => physical_owner,
            crate::libraries::MemberRealization::Intrinsic(_)
            | crate::libraries::MemberRealization::RangeConstruction { .. } => return None,
        };
        Some(Self {
            receiver,
            owner,
            name: member.physical_name.unwrap_or(member.name),
            params: member.params,
            ret: member.ret,
            physical_ret: member.physical_ret,
            descriptor: member.descriptor,
            interface,
            realization,
            stable_declaration,
            property_declaration: None,
            source_member,
            external_property,
        })
    }
}

impl Checker<'_> {
    /// Refuse a `super` call whose selected target is a `suspend` member, at its source position.
    ///
    /// Threading a continuation through a NON-VIRTUAL dispatch and resuming back into it is not
    /// modeled. Emitting it anyway produced an `invokespecial` naming the SOURCE descriptor
    /// (`A.f:()Ljava/lang/String;`) against a declaration that is
    /// `A.f:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;` — an artifact that cannot link,
    /// with no diagnostic. The project's rule for a construct it does not model is to decline the
    /// source.
    ///
    /// The refusal belongs HERE, where the target was selected and its suspend shape is in hand.
    /// A backend traversal would have to rediscover the fact from a realization that no longer
    /// names it, and could only recognize the call shapes that reach one particular node: the same
    /// source with its superclass in a SIBLING FILE, or behind an `@Outer`-labeled enclosing
    /// dispatch, reaches a different one and slipped through. Every spelling and every origin
    /// passes through this one selection.
    ///
    /// Returns whether the call was refused, so the caller stops rather than recording a target.
    pub(super) fn reject_suspend_super_call(
        &mut self,
        suspend: bool,
        span: Span,
        name: &str,
    ) -> bool {
        if !suspend {
            return false;
        }
        self.diags.error(
            span,
            format!("krusty: a super call to the suspend member '{name}' is not supported yet."),
        );
        true
    }
}
