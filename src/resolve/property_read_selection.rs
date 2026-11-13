//! Property read selection and the operations over its result.
//!
//! [`Checker::select_property_read`] walks a read's scope-tower levels: the receiver's member
//! property, its member extensions, then extension properties. The rest of this boundary exposes
//! the semantic type, visibility check, and external-property identity consumed after selection.

use super::*;

pub(super) struct PropertyReadMemberSelection {
    pub(super) name: String,
    pub(super) ty: Ty,
    pub(super) owner: TypeName,
    pub(super) interface: bool,
    /// Selected declaration shape used only to publish the property's generic result. A source
    /// property may have no physical getter call at this use site, but its result still binds
    /// through the checked dispatch receiver.
    pub(super) publication: Option<crate::symbol_resolver::ResolvedMember>,
    pub(super) getter: Option<crate::symbol_resolver::ResolvedMember>,
    pub(super) accessor: Option<Box<crate::libraries::LibraryCallable>>,
    /// Provider-chosen field, Kotlin accessor, or Java accessor. The platform null-check names it.
    pub(super) producer: crate::libraries::PropertyProducer,
    /// Provider-published constant-value expression fact for this selected read.
    pub(super) metadata_constant_read: bool,
    pub(super) context_access: Option<Box<ResolvedPropertyAccess>>,
    pub(super) compiler_intrinsic: Option<crate::libraries::CompilerIntrinsic>,
    pub(super) compile_time_constant: Option<crate::libraries::LibraryConst>,
    pub(super) source_member: Option<crate::libraries::SourceMember>,
    pub(super) stable_declaration: Option<crate::fir::DeclarationId>,
    pub(super) access: Option<(Visibility, TypeName)>,
    /// Whether a second read through the same receiver returns the same value.
    pub(super) stable_read: bool,
}

pub(super) enum PropertyReadAmbiguity {
    MemberExtension,
    Extension,
    MissingContext,
    /// Synthetic properties of one classifier declared by different accessor methods.
    Accessors(Vec<String>),
}

impl PropertyReadSelection {
    pub(super) fn ty(&self) -> Ty {
        match self {
            Self::Member(member) => member.ty,
            Self::MemberExtension(property) => property.ty,
            Self::Extension(access, _) => access.property.ty,
        }
    }

    pub(super) fn access(&self) -> Option<(Visibility, TypeName)> {
        match self {
            Self::Member(member) => member.access,
            Self::MemberExtension(property) => Some((property.visibility, property.owner)),
            // ModuleSymbols is constructed for the current source file: it excludes private
            // extensions from every sibling file and admits the current file's declaration. Once
            // admitted, a second file-blind visibility check would reject the legal same-file read.
            Self::Extension(access, _)
                if matches!(access.property.getter.origin, Origin::Module { .. })
                    && (access.property.stable_declaration.is_some()
                        || access.property.source_key.is_some()) =>
            {
                None
            }
            Self::Extension(access, _) => Some((access.property.visibility, access.property.owner)),
        }
    }

    /// Stable provider identity of the selected getter, when this is a dependency declaration.
    pub(super) fn callable_identity(&self) -> Option<crate::fir::ExternalCallableId> {
        match self {
            Self::Member(member) => member
                .getter
                .as_ref()
                .and_then(|getter| getter.member.external_identity)
                .or_else(|| {
                    member
                        .accessor
                        .as_deref()
                        .and_then(|accessor| accessor.external_identity)
                }),
            Self::MemberExtension(property) => property
                .getter
                .as_ref()
                .and_then(|getter| getter.external_identity),
            Self::Extension(access, _) => access.property.getter.external_identity,
        }
    }

    /// A non-public read the visibility gate rejects. A member extension is gated on its dispatch
    /// receiver — the extension receiver is an ordinary argument — so `f.observable` inside the
    /// declaring class reads a `protected` member of `this`, not of `f`. Public reads stay visible.
    pub(super) fn hidden_from(
        &self,
        receiver: Ty,
        gate: impl FnOnce(Visibility, TypeName, Ty, Option<crate::fir::ExternalCallableId>) -> bool,
    ) -> Option<(Visibility, TypeName)> {
        let (visibility, owner) = self.access()?;
        if visibility == Visibility::Public {
            return None;
        }
        let probed = match self {
            Self::MemberExtension(property) => property.dispatch_receiver.ty,
            Self::Member(_) | Self::Extension(..) => receiver,
        };
        (!gate(visibility, owner, probed, self.callable_identity())).then_some((visibility, owner))
    }

    pub(super) fn external_property(&self) -> Option<crate::fir::ExternalPropertyId> {
        match self {
            Self::Member(member) => member
                .accessor
                .as_deref()
                .and_then(|accessor| accessor.external_property_identity),
            Self::MemberExtension(property) => property
                .getter
                .as_ref()
                .and_then(|getter| getter.external_property_identity),
            Self::Extension(access, _) => access.property.getter.external_property_identity,
        }
    }
}

impl Checker<'_> {
    pub(super) fn select_property_read(
        &self,
        scope: &CheckerScope<'_>,
        receiver: Ty,
        name: &str,
    ) -> Result<Option<PropertyReadSelection>, PropertyReadAmbiguity> {
        // The first candidate the use site cannot see. Each later level is still asked for a
        // visible one; this is the answer only when none has it, so its access is what is reported.
        let mut invisible = None;
        if !receiver.is_nullable() {
            let local = self.checked_body_local_property(receiver, name).cloned();
            if let Some(local) = local {
                return Ok(Some(PropertyReadSelection::Member(Box::new(
                    PropertyReadMemberSelection {
                        name: name.to_string(),
                        ty: local.ty,
                        owner: local.owner,
                        interface: local.interface,
                        publication: None,
                        getter: None,
                        accessor: None,
                        producer: crate::libraries::PropertyProducer::KotlinAccessor,
                        metadata_constant_read: false,
                        context_access: None,
                        compiler_intrinsic: None,
                        compile_time_constant: None,
                        source_member: Some(local.source_member),
                        stable_declaration: local.stable_declaration,
                        access: Some((local.visibility, local.owner)),
                        stable_read: false,
                    },
                ))));
            }
            let resolver = self.resolver();
            let declaration =
                resolver.select_member_property_applicable_where(receiver, name, |property| {
                    let context_types = property.getter.params.get(..property.context_count)?;
                    self.select_context_arguments_with_types(scope, context_types)
                        .ok()
                        .map(|_| {
                            (
                                self.receiver_property_accessible(
                                    property.visibility,
                                    property.owner,
                                    receiver,
                                    property.getter.external_identity,
                                ),
                                property.context_count,
                            )
                        })
                });
            if let Some(selected) = declaration {
                if !selected.competing_accessors.is_empty() {
                    return Err(self.competing_accessor_ambiguity(name, &selected));
                }
                let owner = selected.owner;
                let visibility = selected.visibility;
                let publication = selected.read(receiver);
                let mut selected_property = selected.property;
                let checked_ty = selected_property.as_mut().and_then(|property| {
                    self.apply_checked_source_property_result(receiver, property)
                });
                // A captured invariant argument is an out-projection (`Inv<out (X & Y)>`). The
                // projection is the argument, not the value a read produces: the read is the upper
                // bound, which is what exposes `X` and `Y` on `sel(Inv(A), Inv(B)).v`.
                let ty = checked_ty.unwrap_or(selected.ty).projection_read_ty();
                // Core property selection already incorporates a nearer covariant accessor override.
                // The inherited declaration still owns the physical accessor/field shape; its erased
                // return must not overwrite the selected logical property type.
                let (getter, context_access) = if let Some(declaration) = selected_property.as_ref()
                {
                    if declaration.context_count == 0 {
                        (
                            (declaration.getter_declaration.is_some()
                                || !declaration.getter.descriptor.is_empty()
                                || (declaration.getter.is_abstract
                                    && !declaration.getter.name.is_empty()
                                    && declaration.getter.name != property_getter_name(name)))
                            .then(|| {
                                let mut getter =
                                    crate::symbol_resolver::ResolvedMember::from_callable(
                                        receiver,
                                        declaration.getter.clone(),
                                        false,
                                    );
                                getter.member.stable_declaration = declaration
                                    .getter_declaration
                                    .or(declaration.stable_declaration);
                                getter
                            }),
                            None,
                        )
                    } else {
                        let context_types = declaration
                            .getter
                            .params
                            .get(..declaration.context_count)
                            .ok_or(PropertyReadAmbiguity::MissingContext)?;
                        let context_args = self
                            .select_context_arguments(scope, context_types)
                            .ok_or(PropertyReadAmbiguity::MissingContext)?;
                        let mut getter = crate::symbol_resolver::ResolvedMember::from_callable(
                            receiver,
                            declaration.getter.clone(),
                            false,
                        );
                        getter.member.context_count = declaration.context_count;
                        getter.member.stable_declaration = declaration
                            .getter_declaration
                            .or(declaration.stable_declaration);
                        (
                            Some(getter),
                            Some(Box::new(ResolvedPropertyAccess {
                                property: declaration.clone(),
                                context_args,
                            })),
                        )
                    }
                } else {
                    (None, None)
                };
                let member = PropertyReadSelection::Member(Box::new(PropertyReadMemberSelection {
                    name: name.to_string(),
                    ty,
                    owner,
                    interface: selected.interface,
                    publication,
                    getter,
                    accessor: selected_property
                        .as_ref()
                        .map(|property| Box::new(property.getter.clone())),
                    producer: selected_property.as_ref().map_or(
                        crate::libraries::PropertyProducer::KotlinAccessor,
                        |property| property.producer,
                    ),
                    metadata_constant_read: selected_property
                        .as_ref()
                        .is_some_and(|property| property.metadata_constant_read),
                    context_access,
                    compiler_intrinsic: selected_property
                        .as_ref()
                        .and_then(|property| property.getter.compiler_intrinsic),
                    compile_time_constant: selected_property
                        .as_ref()
                        .and_then(|property| property.compile_time_constant.clone()),
                    source_member: selected_property
                        .as_ref()
                        .and_then(|property| property.source_member),
                    stable_declaration: selected_property
                        .as_ref()
                        .and_then(|property| property.stable_declaration),
                    access: (visibility != Visibility::Public).then_some((visibility, owner)),
                    stable_read: selected_property.as_ref().is_some_and(|property| {
                        let receiver_final = receiver
                            .obj_internal()
                            .and_then(|owner| self.resolver().classifier(owner))
                            .is_some_and(|class| class.is_final());
                        property
                            .read_stability
                            .permits(receiver_final, self.file.smart_cast_value_class_components)
                    }),
                }));
                // kotlinc's tower does not stop at an invisible member: a visible candidate at a
                // later level wins, and the member is reported only when no level has one.
                if self.hidden_property_read(&member, receiver).is_none() {
                    return Ok(Some(member));
                }
                invisible = Some(member);
            }
            // A body-local classifier's applied supertypes are finalized on this Pass-2 lexical
            // rung, so it is not yet present in the immutable module provider borrowed by the
            // active checker. Retry only the inherited MEMBER facet through that semantic edge;
            // extension rungs remain selected against the original receiver below.
            for supertype in invisible
                .is_none()
                .then(|| self.body_local_supertypes(receiver))
                .into_iter()
                .flatten()
            {
                if let Ok(Some(selection)) = self.select_property_read(scope, supertype, name) {
                    let private = match &selection {
                        PropertyReadSelection::Member(member) => {
                            member.access.is_some_and(|(visibility, _)| {
                                !crate::symbol_resolver::member_is_inheritable(visibility)
                            })
                        }
                        PropertyReadSelection::MemberExtension(_)
                        | PropertyReadSelection::Extension(_, _) => false,
                    };
                    if private {
                        continue;
                    }
                    if matches!(selection, PropertyReadSelection::Member(_)) {
                        return Ok(Some(selection));
                    }
                }
            }
            match self.member_extension_property(scope, receiver, name) {
                Ok(Some(property)) => {
                    let member_extension =
                        PropertyReadSelection::MemberExtension(Box::new(property));
                    if self
                        .hidden_property_read(&member_extension, receiver)
                        .is_none()
                    {
                        return Ok(Some(member_extension));
                    }
                    invisible.get_or_insert(member_extension);
                }
                Err(()) => return Err(PropertyReadAmbiguity::MemberExtension),
                Ok(None) => {}
            }
        }
        // Extension properties are a lower scope-tower level than members. Do not even collect
        // them while a member declaration can win: an ambiguous or inapplicable extension family
        // cannot poison an otherwise exact member read. An invisible member does not win.
        let type_variables = self.postponed_type_variables_in(receiver);
        let resolver = self.resolver().with_type_variables(&type_variables);
        let extension = match resolver.select_extension_property(receiver, name) {
            Ok(property) => property,
            Err(_) => return Err(PropertyReadAmbiguity::Extension),
        };
        if let Some(property) = extension {
            let context_args = if property.context_count == 0 {
                Vec::new()
            } else {
                let start = usize::from(property.receiver.is_some());
                let context_types = property
                    .getter
                    .params
                    .get(start..start + property.context_count)
                    .ok_or(PropertyReadAmbiguity::MissingContext)?;
                self.select_context_arguments(scope, context_types)
                    .ok_or(PropertyReadAmbiguity::MissingContext)?
            };
            let extension = PropertyReadSelection::Extension(
                Box::new(ResolvedPropertyAccess {
                    property,
                    context_args,
                }),
                (!type_variables.is_empty()).then_some(receiver),
            );
            if self.hidden_property_read(&extension, receiver).is_none() {
                return Ok(Some(extension));
            }
            invisible.get_or_insert(extension);
        }
        Ok(invisible)
    }

    /// Whether the read's scope tower passes over the receiver's member property `name`, which
    /// the site cannot see, for a visible member extension or extension property. A write
    /// resolves its property through that same tower before it checks the setter's visibility.
    pub(super) fn invisible_member_property_yields(
        &self,
        scope: &CheckerScope<'_>,
        receiver: Ty,
        name: &str,
    ) -> bool {
        if receiver.is_nullable() {
            return false;
        }
        let Some(member) = self.resolver().select_member_property(receiver, name) else {
            return false;
        };
        let visible = self.receiver_property_accessible(
            member.visibility,
            member.owner,
            receiver,
            member
                .property
                .as_ref()
                .and_then(|property| property.getter.external_identity),
        );
        !visible
            && matches!(
                self.select_property_read(scope, receiver, name),
                Ok(Some(
                    PropertyReadSelection::MemberExtension(_)
                        | PropertyReadSelection::Extension(..)
                ))
            )
    }

    pub(super) fn hidden_property_read(
        &self,
        selection: &PropertyReadSelection,
        receiver: Ty,
    ) -> Option<(Visibility, TypeName)> {
        selection.hidden_from(receiver, |visibility, owner, probed, callable| {
            self.receiver_property_accessible(visibility, owner, probed, callable)
        })
    }
}

impl Checker<'_> {
    /// The ambiguity a read reports when accessor methods of one classifier declare the same
    /// synthetic property. Candidates are listed the way kotlinc looks them up: the `isX` getter,
    /// then the `get` spellings.
    pub(super) fn competing_accessor_candidates(
        &self,
        name: &str,
        selected: &crate::symbol_resolver::SelectedMemberProperty,
    ) -> Option<Vec<String>> {
        if selected.competing_accessors.is_empty() {
            return None;
        }
        let candidates = selected
            .property
            .iter()
            .chain(&selected.competing_accessors)
            .collect::<Vec<_>>();
        Some(
            candidates
                .into_iter()
                .map(|candidate| {
                    let keyword = if candidate.setter.is_some() {
                        "var"
                    } else {
                        "val"
                    };
                    let ty = self.diagnostic_type_name(candidate.ty, &[candidate.ty]);
                    format!("{keyword} {name}: {ty}")
                })
                .collect(),
        )
    }

    pub(super) fn competing_accessor_ambiguity(
        &self,
        name: &str,
        selected: &crate::symbol_resolver::SelectedMemberProperty,
    ) -> super::PropertyReadAmbiguity {
        super::PropertyReadAmbiguity::Accessors(
            self.competing_accessor_candidates(name, selected)
                .expect("an accessor ambiguity has competing candidates"),
        )
    }

    pub(super) fn report_accessor_ambiguity(&mut self, span: super::Span, candidates: Vec<String>) {
        let mut message = "overload resolution ambiguity between candidates:".to_string();
        for candidate in candidates {
            message.push('\n');
            message.push_str(&candidate);
        }
        self.diags.error(span, message);
    }
}
