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
    /// Select the runtime receiver denoted by a `super` spelling. Bare `super` uses the current
    /// dispatch receiver; `super@Label` and `super<T>@Label` use the exact labeled receiver-stack
    /// coordinate. The type drives supertype/member selection, while the coordinate is preserved for
    /// lowering so it never reconstructs a label or searches for a same-typed enclosing instance.
    pub(super) fn super_receiver_selection(
        &self,
        scope: &CheckerScope<'_>,
        spelling: &str,
    ) -> Option<ImplicitReceiverSelection> {
        let (ty, current, receiver_depth) = if let Some((_, label)) = spelling.rsplit_once('@') {
            let index = self
                .this_labels
                .iter()
                .rposition(|(candidate, _, is_class)| *is_class && candidate == label)?;
            let top = self.this_labels.len().checked_sub(1)?;
            (self.this_labels[index].1, index == top, top - index)
        } else {
            // An unlabeled `super` always belongs to the nearest CLASS dispatch receiver. Inside a
            // member extension, `scope.this_ty()` is the extension receiver (`fun D.foo()` sees
            // `D` as plain `this`), but `super<B>.m()` still dispatches through the containing
            // class. Use the same receiver-stack coordinate as an explicit `super@Class` spelling.
            let index = self
                .this_labels
                .iter()
                .rposition(|(_, _, is_class)| *is_class)
                .or_else(|| {
                    scope.this_ty().and_then(|ty| {
                        self.this_labels
                            .iter()
                            .rposition(|(_, candidate, _)| *candidate == ty)
                    })
                })?;
            let top = self.this_labels.len().checked_sub(1)?;
            (self.this_labels[index].1, index == top, top - index)
        };
        Some(self.implicit_receiver_selection(ImplicitReceiver {
            ty,
            declared_ty: ty,
            identity: (0, receiver_depth),
            extension_receiver: None,
            class_receiver: true,
            current,
            receiver_depth,
        }))
    }

    /// An enum entry with a declaration body is an anonymous subclass, while its source-level
    /// `this` type remains the enum. Its direct `super` target is therefore the enum declaration
    /// itself, not the enum's own `kotlin.Enum` superclass. The entry label pushed by `check_class`
    /// identifies that exact context without inventing a persistent anonymous classifier type.
    pub(super) fn enum_entry_supertype(&self, receiver: Ty) -> Option<Ty> {
        let internal = receiver.obj_internal()?;
        let class = self.resolver().classifier(internal)?;
        if class.source_file != Some(self.file_index) {
            return None;
        }
        let (entry, labelled_receiver, is_class) = self.this_labels.last()?;
        if *is_class || *labelled_receiver != receiver {
            return None;
        }
        let declaration = class
            .stable_declaration
            .and_then(|declaration| {
                self.active_declarations?
                    .class(self.file, declaration)
                    .map(|(_, class)| class)
            })
            .or_else(|| {
                // Whole-file legacy checks still use the parser declaration recorded during
                // collection. A bounded Pass-2 unit must never reinterpret that arena index.
                self.active_declarations.is_none().then(|| {
                    let declaration = self
                        .module
                        .legacy_symbols()?
                        .class_by_type_name(internal)?
                        .source_decl?;
                    match self.file.decl_arena.get(declaration.0 as usize)? {
                        Decl::Class(class) => Some(class),
                        Decl::Fun(_) | Decl::Property(_) => None,
                    }
                })?
            })?;
        if !declaration.is_enum() {
            return None;
        }
        declaration
            .enum_entries
            .iter()
            .any(|candidate| candidate.name == *entry)
            .then_some(receiver)
    }

    /// Select a `super[<T>].property` accessor from the current class's direct supertypes.
    ///
    /// A superclass wins before interfaces. With no superclass declaration, exactly one matching
    /// interface is required unless `<T>` names one explicitly. The selected callable is rewritten
    /// to the DIRECT supertype owner because `invokespecial` starts resolution there; the declaration
    /// may physically live farther up the hierarchy.
    ///
    /// Visibility is judged on the class instance `super` denotes, not on the supertype being
    /// searched. A protected member stays visible on that subclass instance, including from a
    /// lambda, while a private Java field on the same rung (`ArrayList.size`) does not hide the
    /// public method. A write uses the setter's own visibility, so a private setter stays
    /// unwritable.
    pub(super) fn select_super_property_accessor(
        &self,
        receiver: ImplicitReceiverSelection,
        qualifier: Option<&str>,
        name: &str,
        setter: bool,
    ) -> Option<ResolvedSuperCall> {
        let current = receiver.ty.obj_internal()?;
        self.resolver().classifier(current)?;
        let access_receiver = receiver.ty;
        let matches_qualifier =
            |owner: TypeName| qualifier.is_none_or(|qualifier| owner.qualifier_matches(qualifier));
        let select = |applied_owner: Ty, interface: bool| {
            let owner = applied_owner.obj_internal()?;
            let selected = self.resolver().select_member_property_applicable_where(
                applied_owner,
                name,
                |property| {
                    let visibility = if setter {
                        property.setter_visibility
                    } else {
                        property.visibility
                    };
                    (property.context_count == 0).then_some((
                        self.receiver_property_accessible(
                            visibility,
                            property.owner,
                            access_receiver,
                        ),
                        0,
                    ))
                },
            )?;
            let access_visibility = if setter {
                selected
                    .property
                    .as_ref()
                    .map(|property| property.setter_visibility)
                    .unwrap_or(selected.visibility)
            } else {
                selected.visibility
            };
            if !self.receiver_property_accessible(
                access_visibility,
                selected.owner,
                access_receiver,
            ) {
                return None;
            }
            let property_declaration = selected
                .property
                .as_ref()
                .and_then(|property| property.stable_declaration);
            if setter {
                let setter = selected.setter()?;
                let stable_declaration = setter.stable_declaration;
                let callable = setter.callable;
                if callable.is_abstract {
                    return None;
                }
                let realization = callable.member_realization;
                let physical_owner = match realization {
                    crate::libraries::MemberRealization::Dispatch => owner,
                    crate::libraries::MemberRealization::Direct { .. } => callable.owner,
                    crate::libraries::MemberRealization::Intrinsic(_)
                    | crate::libraries::MemberRealization::RangeConstruction { .. } => return None,
                };
                Some(ResolvedSuperCall {
                    receiver: receiver.clone(),
                    owner: physical_owner,
                    name: callable.name,
                    params: callable.params,
                    physical_params: callable.physical_params,
                    ret: callable.ret,
                    physical_ret: callable.physical_ret,
                    descriptor: callable.descriptor,
                    interface,
                    realization,
                    stable_declaration,
                    property_declaration,
                    source_member: setter.source_member,
                    external_property: callable.external_property_identity,
                    suspend: false,
                })
            } else {
                let member = selected.read(applied_owner)?;
                if member.member.is_abstract() {
                    return None;
                }
                let mut target =
                    ResolvedSuperCall::selected(receiver.clone(), owner, interface, member.member)?;
                target.property_declaration = property_declaration;
                Some(target)
            }
        };

        if let Some(owner) = self
            .enum_entry_supertype(receiver.ty)
            .and_then(Ty::obj_internal)
            .filter(|owner| matches_qualifier(*owner))
        {
            if let Some(target) = select(Ty::obj_name(owner), false) {
                return Some(target);
            }
        }

        let source = self.fed_source();
        let direct = crate::symbol_resolver::direct_supertypes(&source, receiver.ty);
        if let Some(superclass) = direct.iter().copied().find(|supertype| {
            supertype.obj_internal().is_some_and(|owner| {
                matches_qualifier(owner)
                    && source
                        .classifier(owner)
                        .is_none_or(|classifier| !classifier.is_interface())
            })
        }) {
            if let Some(target) = select(superclass, false) {
                return Some(target);
            }
        }
        let matches = direct
            .into_iter()
            .filter(|supertype| {
                supertype.obj_internal().is_some_and(|owner| {
                    matches_qualifier(owner)
                        && source
                            .classifier(owner)
                            .is_some_and(|classifier| classifier.is_interface())
                })
            })
            .filter_map(|supertype| select(supertype, true))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [target] => Some(target.clone()),
            _ => None,
        }
    }

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
