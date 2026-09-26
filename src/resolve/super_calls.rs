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
    /// The selected declaration's own parameter types, before the call site's generic
    /// substitution. A `super` call dispatches to that declaration, so its physical signature, not
    /// the substituted one, names the method: `super.foo(r)` through `B : A<String>` calls
    /// `A.foo(T)`.
    pub physical_params: Vec<Ty>,
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
    /// The selected declaration is a `suspend` function, so the call is a suspension point.
    pub suspend: bool,
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
        let suspend = member.suspend();
        let physical_owner = member.owner?;
        let (owner, interface) = match realization {
            // A dispatched `super` call names the supertype it is qualified with, as kotlinc's
            // `invokespecial` does, and the JVM resolves the declaration from there: an interface
            // declaration through the InterfaceMethodref search path, a class declaration inherited
            // through a class supertype through that supertype. The normalized declaration kind,
            // not its symbol-source origin, decides the shape.
            crate::libraries::MemberRealization::Dispatch
                if member.is_interface() || !interface =>
            {
                (dispatch_owner, interface)
            }
            // A class declaration reached through an interface qualifier is named on its declaring
            // class through a Methodref: `super<I>.hashCode()` calls `Object.hashCode`, which `I`
            // only inherits.
            crate::libraries::MemberRealization::Dispatch => (physical_owner, false),
            crate::libraries::MemberRealization::Direct { .. } => (physical_owner, interface),
            crate::libraries::MemberRealization::Intrinsic(_)
            | crate::libraries::MemberRealization::RangeConstruction { .. } => return None,
        };
        Some(Self {
            receiver,
            owner,
            name: member.physical_name.unwrap_or(member.name),
            params: member.params,
            physical_params: member.physical_params,
            ret: member.ret,
            physical_ret: member.physical_ret,
            descriptor: member.descriptor,
            interface,
            realization,
            stable_declaration,
            property_declaration: None,
            source_member,
            external_property,
            suspend,
        })
    }
}

impl Checker<'_> {
    /// Refuse a `super` call to a `suspend` member that dispatches on an ENCLOSING instance
    /// (`super@Outer.f()` from a nested body), at its source position.
    ///
    /// Such a call crosses a physical class boundary through a nonvirtual bridge on the outer
    /// class, and that bridge is not a suspend function: it would neither pass a continuation nor
    /// name the member's CPS descriptor, so it would not link. A super call on the current
    /// instance is an ordinary suspension point and is not refused. The project's rule for a
    /// construct it does not model is to decline the source.
    ///
    /// The refusal belongs HERE, where the target and its receiver were selected and its suspend
    /// shape is in hand; every spelling and every origin passes through this one selection.
    ///
    /// Returns whether the call was refused, so the caller stops rather than recording a target.
    pub(super) fn reject_suspend_super_call(
        &mut self,
        suspend: bool,
        receiver: &ImplicitReceiverSelection,
        span: Span,
        name: &str,
    ) -> bool {
        if !suspend || receiver.current {
            return false;
        }
        self.diags.error(
            span,
            format!("krusty: a super call to the suspend member '{name}' is not supported yet."),
        );
        true
    }
}
