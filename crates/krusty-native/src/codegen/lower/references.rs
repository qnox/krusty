//! Property references as values: `::foo`, `C::p`, `x::p`.
//!
//! A property reference is an OBJECT, the way a lambda is: one emitted type per site, and one
//! instance per evaluation — or one static instance for the whole program when the site binds no
//! receiver, which is also what makes `::foo == ::foo` true, since the generator answers `equals`
//! by identity and Kotlin answers it by declaration.
//!
//! What makes it more than a lambda is the surface it has to answer. `KProperty` declares `get`,
//! `KMutableProperty` adds `set`, and `KCallable` declares `name`, so the type carries three slots
//! beyond `kotlin.Any`'s three and the site's own bodies fill them. Those bodies are emitted here
//! rather than lowered from IR because there is no IR to lower: common lowering leaves the
//! reference CHECKED — it names the property and says whether a receiver is bound, and nothing
//! more — precisely so that each target may choose its own representation.
//!
//! What is realized: a property of this file, top-level or a class member, bound or not, and
//! however its value is reached — a slot, a source-written accessor pair, or a delegate. A
//! reference to a DEPENDENCY property (`"kotlin"::length`) or to a MEMBER extension property
//! declines by name: each wants more operands than the receiver and context values this object has
//! room for, and answering it wrongly would be worse than skipping the program. The values a
//! reference binds for the property's context parameters are fields of the object after its
//! receiver; its descriptor lists their offsets, which equality and hashing read.

use super::*;
use krusty::fir::FirPropertyReferenceTarget;

/// Where the three members live, after `kotlin.Any`'s.
const GET: u32 = 3;
const SET: u32 = 4;
const NAME: u32 = 5;

/// The emitted pieces of one property-reference site.
#[derive(Clone, Copy)]
pub(super) struct ReferenceItems {
    descriptor: DataId,
    instance_size: u32,
    /// Byte offset of the bound receiver, for a site that binds one.
    receiver_offset: Option<u32>,
    /// Byte offset of the first of the bound context values, which follow one another.
    context_offset: u32,
    /// The one instance a site with no bound receiver has, in static storage.
    singleton: Option<DataId>,
}

/// What ONE SITE makes: the emitted pieces, which sites sharing a declaration share, and the
/// expression THIS site binds as the receiver, which they do not.
///
/// Kept beside the items rather than read back out of the node, because the node says it in a
/// different place for each kind of target and only [`reference_site`] knows the module one.
#[derive(Clone)]
pub(super) struct ReferenceSite {
    items: ReferenceItems,
    bound: Option<u32>,
    /// The expressions this site binds for the property's context parameters, in parameter order.
    contexts: Vec<u32>,
}

/// What a site's `get` and `set` reach.
#[derive(Clone, Copy)]
enum Access {
    /// A top-level property: a global slot, or a source-written accessor pair.
    TopLevel {
        /// Exact checked declaration whose backend layout selects storage or an accessor.
        property: krusty::fir::PropertyId,
    },
    /// A member of a class of this file, read and written through the receiver.
    Member { class: ClassId, index: usize },
    /// A `const val` of an object or companion, whose value lives in a STATIC rather than in the
    /// object's own storage — Kotlin folds such a property at every use site, and a reference's
    /// `get` answers that same value. Never mutable, so there is no write to reach.
    Static { index: u32 },
    /// A top-level property reached through its ACCESSOR pair rather than through a slot: an
    /// extension property, which has no object of its own to keep a field in, and a delegated or
    /// custom-accessor property, whose value is not in any slot this reference could name. The two
    /// differ only in whether the accessor leads with a receiver, so that is what `receiver` says
    /// — and a site that has none has none to pass, bound or not.
    Accessor {
        getter: FunId,
        setter: Option<FunId>,
        receiver: bool,
    },
    /// A property with context parameters, reached through its accessors: the object holds the
    /// values it bound for them, and `get`/`set` pass them first, as the accessors declare.
    ContextAccessor {
        getter: FunId,
        setter: Option<FunId>,
        /// The class a MEMBER property's accessors belong to, which dispatches them; `None` for a
        /// top-level property.
        owner: Option<TypeName>,
        /// Whether the property takes a receiver: its class's instance for a member, its extension
        /// receiver for a top-level one.
        receiver: bool,
        contexts: usize,
    },
}

/// One reference site, as the declaration pass reads it out of the checked node.
struct Site {
    /// The Kotlin name of the property, which `KCallable.name` answers.
    name: String,
    access: Access,
    ty: Ty,
    mutable: bool,
    /// The expression supplying `x` in `x::p`.
    bound: Option<u32>,
    /// How many values the reference binds for the property's context parameters.
    contexts: usize,
}

/// The reference a checked node creates, or `None` when it creates none.
fn reference_site(expr: &IrExpr) -> Option<(&krusty::fir::PropertyId, Option<u32>, bool, &[u32])> {
    let IrExpr::Checked(checked_op) = expr else {
        return None;
    };
    let IrCheckedOperation::PropertyReference {
        target,
        dispatch_receiver,
        extension_receiver,
        context_arguments,
        mutable,
        ..
    } = &**checked_op
    else {
        return None;
    };
    // The object carries at most ONE receiver, and which of the two it is does not matter to it:
    // `x::p` binds a class's, `"ab"::ext` an extension one, and both arrive as the single operand
    // the accessor leads with. BOTH at once is a member extension property, whose accessor wants
    // two receivers this object has no room for; that shape is left to decline by name.
    let bound = match (dispatch_receiver, extension_receiver) {
        (None, None) => None,
        (Some(receiver), None) | (None, Some(receiver)) => Some(*receiver),
        (Some(_), Some(_)) => return None,
    };
    // `extension_receiver` says the property's receiver is an EXTENSION one rather than a class's,
    // which is a fact about the accessor's parameter list and not about this object: either way the
    // reference carries one receiver, bound here or handed to `get`. What the site cannot realize
    // is a property whose accessor wants more than that, and the layout below is what says so.
    let property = match target {
        FirPropertyReferenceTarget::Module(property)
        | FirPropertyReferenceTarget::SpecializedModule { property, .. } => property,
        _ => return None,
    };
    Some((property, bound, *mutable, context_arguments))
}

impl<'a> FileLowering<'a> {
    /// The Kotlin name of the declaration a callable-reference node names, which `KCallable.name`
    /// answers (`krusty::backend::reference_names`); `None` when the expression is not such a node.
    pub(super) fn reference_declaration_name(&self, id: u32) -> Option<String> {
        let IrExpr::CallableReference(reference) = self.ir.expr(id) else {
            return None;
        };
        krusty::backend::reference_names::callable_reference_name(
            self.ir,
            self.callables,
            reference,
        )
    }

    /// Why a property-reference site was not realized, named rather than numbered.
    ///
    /// The declare pass skips a site it cannot realize and leaves the decline to the lowering, so
    /// that the diagnostic names the CONSTRUCT rather than the pass. What it used to name was the
    /// IR node — `Checked(PropertyReference)` — which is the one thing every such site has in
    /// common and says nothing about which of them this is. The conditions below are the same ones
    /// [`reference_site`] and [`Self::reference_of`] decide by, read again to report them.
    pub(super) fn property_reference_decline(&self, id: u32) -> String {
        let IrExpr::Checked(checked_op) = self.ir.expr(id) else {
            return "a property reference of a shape this generator does not read".to_string();
        };
        let krusty::ir::IrCheckedOperation::PropertyReference {
            target,
            dispatch_receiver,
            extension_receiver,
            ..
        } = &**checked_op
        else {
            return "a property reference of a shape this generator does not read".to_string();
        };
        if dispatch_receiver.is_some() && extension_receiver.is_some() {
            return "a reference to a MEMBER EXTENSION property, whose accessor wants two \
                    receivers where this object has room for one"
                .to_string();
        }
        let property = match target {
            FirPropertyReferenceTarget::Module(property)
            | FirPropertyReferenceTarget::SpecializedModule { property, .. } => property,
            // A DEPENDENCY property's reference is supplied accessors by a pass of its own, which
            // says why it supplied none; see `native::dependency_references`.
            FirPropertyReferenceTarget::External { .. } => {
                return match self.dependency_property_skips.get(&id) {
                    Some(reason) => reason.clone(),
                    None => {
                        super::super::super::dependency_references::property_decline(self.ir, id)
                    }
                }
            }
            // A CLASSIFIER's own implicit property — `Colour::entries` and its siblings — which is
            // no declaration of this file and no dependency property either.
            FirPropertyReferenceTarget::Classifier {
                owner, property, ..
            } => {
                return format!(
                    "a reference to `{}`'s implicit `{property:?}`",
                    owner.render().replace('/', ".")
                )
            }
        };
        let Some(checked) = self.ir.checked_properties.get(property) else {
            return "a reference to a property this file does not declare".to_string();
        };
        match self.ir.local_property_layouts.get(property) {
            Some(krusty::ir::IrLocalPropertyLayout::MemberExtension { .. }) => format!(
                "a reference to the member extension property `{}`",
                checked.name
            ),
            _ => format!(
                "a reference to the property `{}`, whose storage this generator did not find",
                checked.name
            ),
        }
    }

    /// Declare a type and its member bodies for every property reference in the file.
    ///
    /// One type per (property, bound-or-not, effective mutability), NOT one per site. Mutability is
    /// a fact about the use site's access to the setter, so references to the same declaration can
    /// need different reflection interfaces. Both representations share one declaration marker:
    /// Kotlin equality still compares the declaration and bound receiver, not that interface set.
    pub(super) fn declare_property_references(&mut self) -> Result<(), Unsupported> {
        self.declare_dependency_property_references()?;
        let mut emitted: HashMap<(krusty::fir::PropertyId, bool, bool), ReferenceItems> =
            HashMap::default();
        for index in 0..self.ir.exprs.len() {
            let Some((property, bound, mutable, contexts)) = reference_site(&self.ir.exprs[index])
            else {
                continue;
            };
            let (property, bound, contexts) = (*property, bound, contexts.to_vec());
            let Some(site) = self.reference_of(&property, bound, mutable, contexts.len()) else {
                // Not a shape this realizes. The node keeps its decline, raised where it is
                // lowered, so the diagnostic still names the construct rather than this pass.
                continue;
            };
            let is_bound = bound.is_some();
            let key = (property, is_bound, mutable);
            let items = match emitted.get(&key) {
                Some(items) => *items,
                None => {
                    let identity = format!(
                        "kt_refid_prop_{}_m{}",
                        if is_bound { "b" } else { "u" },
                        property.raw()
                    );
                    let items = self.define_reference(emitted.len(), site, &identity)?;
                    emitted.insert(key, items);
                    items
                }
            };
            self.references.insert(
                index as u32,
                ReferenceSite {
                    items,
                    bound,
                    contexts,
                },
            );
        }
        Ok(())
    }

    /// Declare a type for every reference to a DEPENDENCY property the pass supplied accessors
    /// for; see [`crate::dependency_references`].
    ///
    /// The object is the same one a reference to a property of this file becomes: one type per
    /// property, `get`/`set`/`name` in its table, the bound receiver in its one field. All that
    /// differs is where `get` and `set` lead — a pair of synthesized functions that reach the
    /// dependency through the ordinary dependency-property path, rather than storage or an
    /// accessor this file declares. One type per PROPERTY and boundness, for the reason the other
    /// pass has one. References with different setter access need different interface sets, but
    /// share the declaration marker used by equality.
    fn declare_dependency_property_references(&mut self) -> Result<(), Unsupported> {
        let mut emitted: HashMap<(krusty::fir::ExternalPropertyId, bool, bool), ReferenceItems> =
            HashMap::default();
        for index in 0..self.ir.exprs.len() {
            let index = index as u32;
            let Some(realized) = self.dependency_properties.get(&index) else {
                continue;
            };
            // The property's Kotlin name, which `KCallable.name` answers. A property the provider
            // cannot name gets no object rather than one answering a spelling it invented.
            let Some(name) = self
                .callables
                .property(realized.property)
                .map(|property| property.name.clone())
            else {
                self.dependency_property_skips.insert(
                    index,
                    "a reference to a dependency property the provider cannot name".to_string(),
                );
                continue;
            };
            let site = Site {
                name: name.to_string(),
                access: Access::Accessor {
                    getter: realized.getter,
                    setter: realized.setter,
                    receiver: realized.receiver,
                },
                ty: realized.ty,
                mutable: realized.mutable,
                bound: realized.bound,
                contexts: 0,
            };
            let is_bound = realized.bound.is_some();
            let key = (realized.property, is_bound, realized.mutable);
            let items = match emitted.get(&key) {
                Some(items) => *items,
                None => {
                    // The ordinal only names the emitted symbols, and the two passes share the
                    // namespace — so this one counts on from where a property of this file would.
                    let ordinal = self.references.len() + emitted.len();
                    let identity = format!(
                        "kt_refid_prop_{}_e{}",
                        if is_bound { "b" } else { "u" },
                        realized.property.raw()
                    );
                    let items = self.define_reference(ordinal, site, &identity)?;
                    emitted.insert(key, items);
                    items
                }
            };
            self.references.insert(
                index,
                ReferenceSite {
                    items,
                    bound: realized.bound,
                    contexts: Vec::new(),
                },
            );
        }
        Ok(())
    }

    /// Declare a type for every LOCAL delegated property's `KProperty` metadata.
    ///
    /// A local delegated property has no storage and no accessors to reach: the metadata exists so
    /// that `getValue(thisRef, property)` can ask the property its name, and that is the whole of
    /// what it answers. `get` and `set` are the runtime's loud failure rather than a body, because
    /// no type a program can name here declares them — a local property reference is a
    /// `KProperty<*>`, and Kotlin gives it no receiver to read through.
    ///
    /// One type per checked declaration identity: two references to the same local property are
    /// equal, while unrelated same-named locals remain distinct.
    pub(super) fn declare_local_property_references(&mut self) -> Result<(), Unsupported> {
        let mut emitted: HashMap<krusty::fir::LocalDelegatedPropertyId, ReferenceItems> =
            HashMap::default();
        for index in 0..self.ir.exprs.len() {
            let IrExpr::LocalPropertyReference(reference) = &self.ir.exprs[index] else {
                continue;
            };
            let name = reference.name.clone();
            let declaration = reference.declaration;
            let items = match emitted.get(&declaration) {
                Some(items) => *items,
                None => {
                    let items = self.define_local_reference(emitted.len(), &name)?;
                    emitted.insert(declaration, items);
                    items
                }
            };
            // A LOCAL delegated property's metadata binds no receiver: Kotlin gives it none.
            self.references.insert(
                index as u32,
                ReferenceSite {
                    items,
                    bound: None,
                    contexts: Vec::new(),
                },
            );
        }
        Ok(())
    }

    fn define_local_reference(
        &mut self,
        ordinal: usize,
        property: &str,
    ) -> Result<ReferenceItems, Unsupported> {
        let base = format!("kt_local_prop_{ordinal}");
        let instance_size = model::HEADER_SIZE.next_multiple_of(8);
        let name = self.declare_local_function(&format!("{base}_name"), &[any()], any())?;
        let equals =
            self.declare_local_function(&format!("{base}_equals"), &[any(), any()], Ty::Boolean)?;
        let unreachable = self.import("kt_abstract_method_called", &[], Ty::Unit)?;

        let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
        let mut vtable = self.any_vtable()?;
        vtable[0] = equals;
        vtable.push(unreachable);
        vtable.push(unreachable);
        vtable.push(name);
        let any_type = self.import_data("kt_type_any")?;
        // A local delegated property's metadata is a KProperty, but not a KProperty0: Kotlin does
        // not expose a callable `get()` for a local declaration. Keep the exact common reflection
        // supertypes so casts and generic convention parameters recognize the object without
        // inventing an accessor surface it does not have.
        let interfaces = ["kt_type_kcallable", "kt_type_kproperty"]
            .into_iter()
            .map(|symbol| self.import_data(symbol))
            .collect::<Result<Vec<_>, _>>()?;
        self.define_type_descriptor(
            descriptor,
            &base,
            super::objects::DescriptorShape {
                kotlin_name: "kotlin.reflect.KProperty",
                instance_size,
                reference_offsets: &[],
                vtable: &vtable,
                itable: &[],
                instantiable: true,
                superclass: any_type,
                interfaces: &interfaces,
                reference_target: None,
                walk: super::objects::WalkMembers::default(),
                qualified_name: None,
                simple_name: None,
                class_names: 0,
            },
        )?;
        self.define_reference_equals(equals, &base, descriptor, None)?;
        self.define_reference_name(name, &base, property)?;

        let instance = self.declare_local_data(&format!("{base}_instance"), false)?;
        let mut description = DataDescription::new();
        description.define(vec![0; model::HEADER_SIZE as usize].into_boxed_slice());
        description.set_align(8);
        let global = self
            .module
            .declare_data_in_data(descriptor, &mut description);
        description.write_data_addr(0, global, 0);
        self.module
            .define_data(instance, &description)
            .map_err(|error| format!("defining `{base}_instance` ({error})"))?;

        Ok(ReferenceItems {
            descriptor,
            instance_size,
            receiver_offset: None,
            context_offset: 0,
            singleton: Some(instance),
        })
    }

    /// Resolve the checked property a site names to what this generator can read and write.
    fn reference_of(
        &self,
        property: &krusty::fir::PropertyId,
        bound: Option<u32>,
        mutable: bool,
        contexts: usize,
    ) -> Option<Site> {
        let checked = self.ir.checked_properties.get(property)?;
        let access = match self.ir.local_property_layouts.get(property) {
            // A top-level extension property: one receiver, which this object carries or is handed.
            Some(IrLocalPropertyLayout::TopLevelAccessor {
                getter,
                setter,
                receiver,
                context_parameters,
                ..
            }) if context_parameters.is_empty() => Access::Accessor {
                getter: *getter,
                setter: *setter,
                receiver: receiver.is_some(),
            },
            // A top-level property with context parameters: the object holds the values the site
            // binds for them and hands them to its accessors ahead of the extension receiver.
            Some(IrLocalPropertyLayout::TopLevelAccessor {
                getter,
                setter,
                receiver,
                context_parameters,
                ..
            }) if context_parameters.len() == contexts => Access::ContextAccessor {
                getter: *getter,
                setter: *setter,
                owner: None,
                receiver: receiver.is_some(),
                contexts,
            },
            // A member property with context parameters: its accessors are the class's methods,
            // dispatched on the receiver the object binds or is handed.
            Some(IrLocalPropertyLayout::Member {
                owner,
                getter,
                setter,
                context_parameters,
                ..
            }) if !context_parameters.is_empty() && context_parameters.len() == contexts => {
                Access::ContextAccessor {
                    getter: (*getter)?,
                    setter: *setter,
                    owner: Some(*owner),
                    receiver: true,
                    contexts,
                }
            }
            // Context values this site does not bind, or a MEMBER extension property: its
            // accessor takes operands this object does not carry, so the site is left to decline
            // by name.
            Some(IrLocalPropertyLayout::TopLevelAccessor { .. })
            | Some(IrLocalPropertyLayout::MemberExtension { .. }) => return None,
            _ => match checked.class {
                None => Access::TopLevel {
                    property: *property,
                },
                Some(class) => match self.ir.classes[class as usize]
                    .properties
                    .iter()
                    .position(|candidate| candidate.name == checked.name)
                {
                    Some(index) => Access::Member { class, index },
                    // Not in the object's own storage: a `const val` is a STATIC. Whose static is
                    // not always this class's — a COMPANION's `const val` lives on the OUTER
                    // class, which is where kotlinc puts it and what the layout here follows — so
                    // both owners are admitted, and only under the property's own name.
                    None => Access::Static {
                        index: self.constant_static(class, &checked.name)?,
                    },
                },
            },
        };
        Some(Site {
            name: checked.name.clone(),
            access,
            ty: checked.ty,
            mutable,
            bound,
            contexts,
        })
    }

    /// The static holding a `const val` declared by `class`, by name.
    ///
    /// Either the class's own or, when `class` is a COMPANION, its outer one: kotlin puts a
    /// companion's `const val` on the outer class, and this layout follows it. Only a CONST is
    /// admitted — an ordinary property reached this way would be a guess about where its value is.
    fn constant_static(&self, class: ClassId, name: &str) -> Option<u32> {
        let own = self.ir.classes[class as usize].fq_name_id();
        let outer = self
            .ir
            .classes
            .iter()
            .find(|candidate| candidate.companion_class == Some(own))
            .map(|candidate| candidate.fq_name_id());
        let index = self.ir.statics.iter().position(|declaration| {
            declaration.is_const
                && declaration.name == name
                && matches!(declaration.owner, Some(owner) if owner == own || Some(owner) == outer)
        })?;
        u32::try_from(index).ok()
    }

    fn define_reference(
        &mut self,
        ordinal: usize,
        site: Site,
        identity: &str,
    ) -> Result<ReferenceItems, Unsupported> {
        let base = format!("kt_prop_{ordinal}");
        // The bound receiver is the object's one field, and the one reference the collector traces.
        // The context values the site binds follow the receiver, one reference each, and are
        // traced like it.
        let receiver_offset = site.bound.map(|_| model::HEADER_SIZE);
        let context_offset = model::HEADER_SIZE + if site.bound.is_some() { 8 } else { 0 };
        let context_offsets: Vec<u32> = (0..site.contexts)
            .map(|index| context_offset + 8 * index as u32)
            .collect();
        let references: Vec<u32> = receiver_offset
            .into_iter()
            .chain(context_offsets.iter().copied())
            .collect();
        let instance_size = (context_offset + 8 * site.contexts as u32).next_multiple_of(8);

        let getter = self.declare_local_function(&format!("{base}_get"), &[any(), any()], any())?;
        let setter = site
            .mutable
            .then(|| {
                self.declare_local_function(
                    &format!("{base}_set"),
                    &[any(), any(), any()],
                    Ty::Unit,
                )
            })
            .transpose()?;
        let name = self.declare_local_function(&format!("{base}_name"), &[any()], any())?;
        let to_string =
            self.declare_local_function(&format!("{base}_to_string"), &[any()], any())?;

        let mut vtable = self.any_vtable()?;
        // Kotlin compares two references by the declaration they name and the receiver they bind.
        // Effective setter access can give that declaration two emitted types, so both publish the
        // same exact declaration marker and use the common callable-reference equality and hash.
        let marker = self.reference_identity_marker(identity)?;
        vtable[0] = self.runtime_member_import("kt_reference_equals")?;
        vtable[1] = self.runtime_member_import("kt_reference_hash_code")?;
        vtable[2] = to_string;
        vtable.push(getter);
        vtable.push(match setter {
            Some(setter) => setter,
            // A `val`'s `set` slot is unreachable: `KMutableProperty` is the only type that names
            // one, and a reference to a `val` never wears it.
            None => getter,
        });
        vtable.push(name);
        debug_assert_eq!(vtable.len() as u32, NAME + 1);

        let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
        let any_type = self.import_data("kt_type_any")?;
        let kotlin_name = if site.mutable {
            "kotlin.reflect.KMutableProperty"
        } else {
            "kotlin.reflect.KProperty"
        };
        // How many receivers `get` takes, which is the N in `KPropertyN`. A BOUND reference
        // carries its receiver in the object and takes none; an unbound one takes the receiver its
        // property reads through, and a top-level property has none to take.
        let arity = if site.bound.is_some() {
            0
        } else {
            match site.access {
                Access::Member { .. } => 1,
                Access::Accessor { receiver, .. } | Access::ContextAccessor { receiver, .. } => {
                    usize::from(receiver)
                }
                Access::TopLevel { .. } | Access::Static { .. } => 0,
            }
        };
        // What an `is` against a reflection type asks about: this object's own type is one of a
        // kind, so the markers are what it shares with the type written at the check.
        let markers =
            super::super::super::intrinsics::property_reference_markers(site.mutable, arity);
        let markers = markers
            .into_iter()
            .map(|symbol| self.import_data(symbol))
            .collect::<Result<Vec<_>, _>>()?;
        self.define_type_descriptor(
            descriptor,
            &base,
            super::objects::DescriptorShape {
                kotlin_name,
                instance_size,
                reference_offsets: &references,
                vtable: &vtable,
                itable: &[],
                instantiable: true,
                superclass: any_type,
                interfaces: &markers,
                reference_target: Some(super::objects::ReferenceTarget {
                    marker,
                    receiver_offset: receiver_offset.unwrap_or(0),
                    context_offsets,
                }),
                walk: super::objects::WalkMembers::default(),
                qualified_name: None,
                simple_name: None,
                class_names: 0,
            },
        )?;

        self.define_reference_get(getter, &base, &site, receiver_offset, context_offset)?;
        if let Some(setter) = setter {
            self.define_reference_set(setter, &base, &site, receiver_offset, context_offset)?;
        }
        self.define_reference_name(name, &base, &site.name)?;
        self.define_reference_to_string(to_string, &base, &site.name)?;

        let singleton = match (site.bound, site.contexts) {
            (Some(_), _) | (None, 1..) => None,
            (None, 0) => {
                let instance = self.declare_local_data(&format!("{base}_instance"), false)?;
                let mut description = DataDescription::new();
                description.define(vec![0; model::HEADER_SIZE as usize].into_boxed_slice());
                description.set_align(8);
                let global = self
                    .module
                    .declare_data_in_data(descriptor, &mut description);
                description.write_data_addr(0, global, 0);
                self.module
                    .define_data(instance, &description)
                    .map_err(|error| format!("defining `{base}_instance` ({error})"))?;
                Some(instance)
            }
        };

        Ok(ReferenceItems {
            descriptor,
            instance_size,
            receiver_offset,
            context_offset,
            singleton,
        })
    }

    /// `equals(self, other)`: the same declaration, and the same bound receiver when there is one.
    fn define_reference_equals(
        &mut self,
        id: FuncId,
        base: &str,
        descriptor: DataId,
        receiver_offset: Option<u32>,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any(), any()], Ty::Boolean)?;
        self.emit_function(
            id,
            signature,
            Ty::Boolean,
            &format!("{base}_equals"),
            &mut |body, params| {
                let no = body.builder.ins().iconst(types::I8, 0);
                let mine = body.data_address(descriptor);
                let theirs = body.builder.ins().load(types::I64, trusted(), params[1], 0);
                let same_type = body.builder.ins().icmp(IntCC::Equal, mine, theirs);
                // A null `other`, or one of another type, is not this reference.
                let is_null = body.is_null(params[1]);
                let not_null = body.builder.ins().bxor_imm_u(is_null, 1);
                let candidate = body.builder.ins().band(not_null, same_type);
                let compare = body.builder.create_block();
                let done = body.builder.create_block();
                body.builder.append_block_param(done, types::I8);
                body.builder
                    .ins()
                    .brif(candidate, compare, &[], done, &[no.into()]);

                body.continue_in(compare);
                let answer = match receiver_offset {
                    // Unbound: the type alone settles it.
                    None => body.builder.ins().iconst(types::I8, 1),
                    Some(offset) => {
                        let flags = trusted();
                        let mine =
                            body.builder
                                .ins()
                                .load(types::I64, flags, params[0], offset as i32);
                        let theirs =
                            body.builder
                                .ins()
                                .load(types::I64, flags, params[1], offset as i32);
                        body.runtime_call(
                            "kt_equals",
                            &[any(), any()],
                            Ty::Boolean,
                            &[mine, theirs],
                        )?
                        .expect("`kt_equals` returns a Boolean")
                    }
                };
                body.builder.ins().jump(done, &[answer.into()]);

                body.continue_in(done);
                let answer = body.builder.block_params(done)[0];
                body.builder.ins().return_(&[answer]);
                body.terminate();
                Ok(())
            },
        )
    }

    /// `get(self, receiver)`: the property's value, as a reference.
    ///
    /// Which receiver is used is the site's, not the caller's: a bound reference reads the one it
    /// stored and ignores what it is passed, which is what `KProperty0.get()` passing nothing
    /// means. An unbound member reference reads its argument.
    fn define_reference_get(
        &mut self,
        id: FuncId,
        base: &str,
        site: &Site,
        receiver_offset: Option<u32>,
        context_offset: u32,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any(), any()], any())?;
        let (access, ty) = (site.access, site.ty);
        self.emit_function(
            id,
            signature,
            any(),
            &format!("{base}_get"),
            &mut |body, params| {
                let value = match access {
                    Access::TopLevel { property } => body.top_level_read(&property)?,
                    Access::Static { index } => body.static_read(index)?,
                    Access::Member { class, index } => {
                        let object = receiver(body, params, receiver_offset);
                        body.null_check(object)?;
                        body.property_read_of(class, index, object)?
                    }
                    Access::Accessor {
                        getter,
                        receiver: takes_receiver,
                        ..
                    } => {
                        let object =
                            takes_receiver.then(|| receiver(body, params, receiver_offset));
                        body.accessor_of(getter, object, None)?
                    }
                    Access::ContextAccessor {
                        getter,
                        owner,
                        receiver: takes_receiver,
                        contexts,
                        ..
                    } => {
                        let object =
                            takes_receiver.then(|| receiver(body, params, receiver_offset));
                        let contexts = bound_contexts(body, params[0], context_offset, contexts);
                        body.context_accessor_of(getter, owner, object, &contexts, None)?
                    }
                };
                let answer = match value {
                    Some(value) => body
                        .convert(value, Some(ty), any())?
                        .expect("a reference carrier"),
                    None => body
                        .runtime_call("kt_unit", &[], any(), &[])?
                        .expect("`kt_unit` returns the singleton"),
                };
                body.builder.ins().return_(&[answer]);
                body.terminate();
                Ok(())
            },
        )
    }

    /// `set(self, receiver, value)`.
    fn define_reference_set(
        &mut self,
        id: FuncId,
        base: &str,
        site: &Site,
        receiver_offset: Option<u32>,
        context_offset: u32,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any(), any(), any()], Ty::Unit)?;
        let (access, name) = (site.access, site.name.clone());
        self.emit_function(
            id,
            signature,
            Ty::Unit,
            &format!("{base}_set"),
            &mut |body, params| {
                match access {
                    // A `const val` is not mutable, so no site that reaches this storage asks for
                    // a setter — and if one somehow did, writing a constant is not a thing to do
                    // quietly.
                    Access::Static { .. } => {
                        return Err(declined!("a write to the constant `{name}`"));
                    }
                    Access::TopLevel { property } => {
                        let ty = body.top_level_written_ty(&property)?;
                        let written = body.check_erased_value(params[2], ty)?;
                        let Some(value) = body.convert(written, Some(any()), ty)? else {
                            return Err(declined!("a `Unit` value assigned to `{name}`"));
                        };
                        body.top_level_write_value(&property, value)?;
                    }
                    Access::Member { class, index } => {
                        let ty = body.written_property_ty(class, index)?;
                        let object = receiver(body, params, receiver_offset);
                        body.null_check(object)?;
                        let written = body.check_erased_value(params[2], ty)?;
                        let Some(value) = body.convert(written, Some(any()), ty)? else {
                            return Err(declined!("a `Unit` value assigned to `{name}`"));
                        };
                        body.property_write_of(class, index, object, value)?;
                    }
                    Access::Accessor {
                        setter,
                        receiver: takes_receiver,
                        ..
                    } => {
                        let setter =
                            setter.ok_or_else(|| format!("a write to the read-only `{name}`"))?;
                        let object =
                            takes_receiver.then(|| receiver(body, params, receiver_offset));
                        let written_ty = body.file.ir.functions[setter as usize]
                            .params
                            .last()
                            .copied()
                            .ok_or_else(|| format!("a setter of `{name}` without a value"))?;
                        let written = body.check_erased_value(params[2], written_ty)?;
                        body.accessor_of(setter, object, Some(written))?;
                    }
                    Access::ContextAccessor {
                        setter,
                        owner,
                        receiver: takes_receiver,
                        contexts,
                        ..
                    } => {
                        let setter =
                            setter.ok_or_else(|| format!("a write to the read-only `{name}`"))?;
                        let object =
                            takes_receiver.then(|| receiver(body, params, receiver_offset));
                        let written_ty = body.file.ir.functions[setter as usize]
                            .params
                            .last()
                            .copied()
                            .ok_or_else(|| format!("a setter of `{name}` without a value"))?;
                        let written = body.check_erased_value(params[2], written_ty)?;
                        let contexts = bound_contexts(body, params[0], context_offset, contexts);
                        body.context_accessor_of(setter, owner, object, &contexts, Some(written))?;
                    }
                }
                body.builder.ins().return_(&[]);
                body.terminate();
                Ok(())
            },
        )
    }

    /// `name(self)`: the property's Kotlin name.
    /// `toString(self)`: a property reference prints as `property NAME (Kotlin reflection is not
    /// available)`, which is what kotlinc-native's references print.
    fn define_reference_to_string(
        &mut self,
        id: FuncId,
        base: &str,
        property: &str,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any()], any())?;
        let text = format!("property {property} (Kotlin reflection is not available)").into_bytes();
        self.emit_function(
            id,
            signature,
            any(),
            &format!("{base}_to_string"),
            &mut |body, _| {
                let value = body.string_literal(&text)?;
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            },
        )
    }

    pub(super) fn define_reference_name(
        &mut self,
        id: FuncId,
        base: &str,
        property: &str,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any()], any())?;
        let text = property.as_bytes().to_vec();
        self.emit_function(
            id,
            signature,
            any(),
            &format!("{base}_name"),
            &mut |body, _| {
                let value = body.string_literal(&text)?;
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            },
        )
    }
}

/// The receiver a site's `get`/`set` reads: the bound one out of the object, else the argument.
fn receiver(body: &mut BodyLowering<'_, '_, '_>, params: &[Value], offset: Option<u32>) -> Value {
    match offset {
        Some(offset) => body
            .builder
            .ins()
            .load(types::I64, trusted(), params[0], offset as i32),
        None => params[1],
    }
}

/// The values a reference object holds for its property's context parameters, read out of its
/// fields; the first is at `offset`.
fn bound_contexts(
    body: &mut BodyLowering<'_, '_, '_>,
    object: Value,
    offset: u32,
    count: usize,
) -> Vec<Value> {
    (0..count)
        .map(|index| {
            body.builder.ins().load(
                types::I64,
                trusted(),
                object,
                (offset + 8 * index as u32) as i32,
            )
        })
        .collect()
}

impl BodyLowering<'_, '_, '_> {
    /// The `KProperty` metadata of a local delegated property: one object, for its name.
    pub(super) fn local_property_reference(
        &mut self,
        id: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(site) = self.file.references.get(&id) else {
            return Err("`LocalPropertyReference`".into());
        };
        let instance = site
            .items
            .singleton
            .expect("a local property reference has one instance");
        Ok(Some(self.data_address(instance)))
    }

    /// `::foo`, `C::p`, `x::p` — the reference object itself.
    pub(super) fn property_reference(&mut self, id: u32) -> Result<Option<Value>, Unsupported> {
        let Some(site) = self.file.references.get(&id) else {
            return Err(self.file.property_reference_decline(id).into());
        };
        let (descriptor, size, receiver_offset, context_offset, singleton, bound, contexts) = (
            site.items.descriptor,
            site.items.instance_size,
            site.items.receiver_offset,
            site.items.context_offset,
            site.items.singleton,
            site.bound,
            site.contexts.clone(),
        );
        if let Some(instance) = singleton {
            // No receiver to hold: one object for the program, which is also what makes
            // `::foo == ::foo` answer true through identity equality.
            return Ok(Some(self.data_address(instance)));
        }
        // The operands the DECLARE pass read out of this site — not re-read here, because the node
        // says them in a different place for each kind of target.
        let object = match bound {
            Some(bound) => Some(self.reference(bound)?),
            None => None,
        };
        if self.terminated {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(contexts.len());
        for context in contexts {
            values.push(self.reference(context)?);
            if self.terminated {
                return Ok(None);
            }
        }
        let instance = self.allocate(descriptor, size)?;
        if let Some(object) = object {
            let offset = receiver_offset.expect("a bound site stores its receiver");
            self.builder
                .ins()
                .store(trusted(), object, instance, offset as i32);
        }
        for (index, value) in values.into_iter().enumerate() {
            self.builder.ins().store(
                trusted(),
                value,
                instance,
                (context_offset + 8 * index as u32) as i32,
            );
        }
        Ok(Some(instance))
    }

    /// A member of a property reference, answered through the object's own table.
    ///
    /// Returns `None` unless the selected declaration's provider attached an exact reflection
    /// operation role. Native maps that checked role to its object ABI; it never reclassifies the
    /// receiver or callable spelling.
    pub(super) fn reference_member(
        &mut self,
        role: Option<krusty::types::SemanticCallRole>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let slot = reference_slot(role, args.len())?;
        Some(self.reference_call(slot, receiver, args, ret))
    }

    /// Call a top-level extension accessor: the receiver, then the written value for a setter.
    ///
    /// Everything crosses this object's members as a reference, so each operand is converted to the
    /// type the accessor declares before the call rather than handed over as a box.
    pub(super) fn accessor_of(
        &mut self,
        accessor: FunId,
        object: Option<Value>,
        value: Option<Value>,
    ) -> Result<Option<Value>, Unsupported> {
        let params = self.file.ir.functions[accessor as usize].params.clone();
        let mut arguments = Vec::with_capacity(params.len());
        for (operand, ty) in object.into_iter().chain(value).zip(&params) {
            let Some(argument) = self.convert(operand, Some(any()), *ty)? else {
                return Err("a `Unit` operand of a property accessor".into());
            };
            arguments.push(argument);
        }
        if arguments.len() != params.len() {
            return Err(declined!(
                "a property accessor taking {} operands for {} parameters",
                arguments.len(),
                params.len()
            ));
        }
        let id = self.file.functions[accessor as usize]
            .ok_or_else(|| "an extension accessor with no body".to_string())?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// Call the accessor of a property with context parameters on behalf of a reference object:
    /// the context values it holds, then the extension receiver when the property takes one, then
    /// the written value for a setter. A member's accessor is dispatched on `object`, the instance
    /// the reference binds or is handed. Every operand crosses the object's members as a
    /// reference and is converted to the type the accessor declares.
    pub(super) fn context_accessor_of(
        &mut self,
        accessor: FunId,
        owner: Option<TypeName>,
        object: Option<Value>,
        contexts: &[Value],
        value: Option<Value>,
    ) -> Result<Option<Value>, Unsupported> {
        let params = self.file.ir.functions[accessor as usize].params.clone();
        let extension = match owner {
            Some(_) => None,
            None => object,
        };
        let supplied = contexts
            .iter()
            .copied()
            .chain(extension)
            .chain(value)
            .collect::<Vec<_>>();
        if supplied.len() != params.len() {
            return Err(declined!(
                "a property accessor taking {} operands for {} parameters",
                params.len(),
                supplied.len()
            ));
        }
        let mut arguments = Vec::with_capacity(supplied.len());
        for (operand, ty) in supplied.into_iter().zip(&params) {
            let Some(argument) = self.convert(operand, Some(any()), *ty)? else {
                return Err("a `Unit` operand of a property accessor".into());
            };
            arguments.push(argument);
        }
        let dispatch = match (owner, object) {
            (Some(owner), Some(object)) => Some((owner, object)),
            (Some(_), None) => return Err("a member accessor with no receiver".into()),
            (None, _) => None,
        };
        self.call_receiver_accessor(accessor, dispatch, &arguments)
    }

    /// Whether a checked read of `target` is answered by a reference object's slot: its selected
    /// getter carries a reference role that takes no operands. Other getter roles are not this
    /// lowering's, and the read goes on to the path that owns them.
    pub(super) fn reads_reference_slot(&self, target: krusty::fir::ExternalPropertyId) -> bool {
        self.file
            .callables
            .property(target)
            .and_then(|property| self.file.callables.callable(property.getter))
            .is_some_and(|getter| reference_slot(getter.semantic_role, 0).is_some())
    }

    /// A checked read of a dependency property whose selected getter carries a reference role.
    ///
    /// Returns `None` when the getter has no such role, so the caller falls through to the ordinary
    /// dependency-property path.
    pub(super) fn reference_property(
        &mut self,
        target: krusty::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        let slot = reference_slot(getter.semantic_role, 0)?;
        let owner = getter.physical_owner;
        Some(self.reference_property_read(slot, owner, property.getter, receiver, property.result))
    }

    /// A reflection property read whose receiver may be a class of the module rather than a
    /// reference object: `KCallable.name` on a class that implements `KProperty` by delegation or
    /// override. Such a class lays its table out as a class, so the reference-object slot means
    /// something else in it. Each class of the module that can stand behind the getter's owner is
    /// tested first and enters its own getter: one of this file through its laid-out slot, one of
    /// another file through the slot the module plan gives it. Any other object is a reference
    /// object and answers from its fixed slot.
    fn reference_property_read(
        &mut self,
        slot: u32,
        owner: krusty::types::TypeName,
        getter: krusty::fir::ExternalCallableId,
        receiver: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let local = self.file.implementors_of(owner);
        let mut implementors = Vec::with_capacity(local.len());
        for &class in &local {
            let Some(arm) = self.external_property_getter_slot(class, getter) else {
                return Err(self.undispatched_reference_read(owner));
            };
            implementors.push(arm);
        }
        // The module's other files, by the dependency classifiers their classes name: the owner
        // itself or one of its dependency subtypes, such as `KProperty` for `KCallable.name`.
        let mut remote = Vec::new();
        for (dependency, classes) in self.file.module_facts.dependency_implementor_roots() {
            if !self.file.is_subclassifier(dependency, owner) {
                continue;
            }
            for implementor in classes {
                if implementor.file != self.file.source && !remote.contains(&implementor.class) {
                    remote.push(implementor.class);
                }
            }
        }
        for class in remote {
            let Some(arm) = self.planned_property_getter_slot(class, getter, ret)? else {
                return Err(self.undispatched_reference_read(owner));
            };
            implementors.push(arm);
        }
        if implementors.is_empty() {
            return self.reference_call(slot, receiver, &[], ret);
        }
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch_by_implementor_with(&implementors, object, &[], ret, false, |body, object| {
            body.reference_slot_call(slot, object, Vec::new(), ret)
        })
    }

    fn undispatched_reference_read(&self, owner: krusty::types::TypeName) -> Unsupported {
        declined!(
            "a read of a `{}` member where the module implements it with a class whose override \
             cannot be dispatched to",
            owner.render().replace('/', ".")
        )
    }

    /// `p.getValue(thisRef, property)` and `p.setValue(thisRef, property, value)`.
    ///
    /// `val x by ::top` delegates to the property reference itself, through two stdlib operators
    /// that are `inline` one-liners: `getValue` is `get()` on a `KProperty0` and `get(thisRef)` on
    /// a `KProperty1`, and `setValue` the same for `set`. A dependency `inline` body is not here to
    /// splice, so they are realized rather than inlined. The selected declaration's role records
    /// how many receivers cross the reference-object slot.
    ///
    /// `property` is the metadata object, which these operators ignore; it is still evaluated,
    /// because a program that can see the difference is entitled to.
    pub(super) fn reference_delegate(
        &mut self,
        role: Option<krusty::types::SemanticCallRole>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        use krusty::types::SemanticCallRole;
        // Which operands reach the slot: a `KProperty1` is handed `thisRef`, a `KProperty0` is not,
        // and the written value is always the last one.
        let (slot, kept): (u32, Vec<usize>) = match role {
            Some(SemanticCallRole::KotlinPropertyReferenceDelegateGet(arity))
                if args.len() == 2 && arity <= 1 =>
            {
                (GET, (arity == 1).then_some(0).into_iter().collect())
            }
            Some(SemanticCallRole::KotlinPropertyReferenceDelegateSet(arity))
                if args.len() == 3 && arity <= 1 =>
            {
                (
                    SET,
                    (arity == 1).then_some(0).into_iter().chain([2]).collect(),
                )
            }
            _ => return None,
        };
        Some(self.delegate_call(slot, receiver, args, &kept, ret))
    }

    fn delegate_call(
        &mut self,
        slot: u32,
        receiver: u32,
        args: &[u32],
        kept: &[usize],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        // Every argument is evaluated, in source order, whether or not the slot is handed it.
        let mut evaluated = Vec::with_capacity(args.len());
        for argument in args {
            evaluated.push(self.reference(*argument)?);
        }
        if self.terminated {
            return Ok(None);
        }
        let mut arguments: Vec<Value> = kept.iter().map(|index| evaluated[*index]).collect();
        // A bound reference's slot ignores the receiver operand; a null keeps one signature for
        // both shapes, exactly as `reference_call` does.
        let expected = usize::from(slot == SET) + 1;
        while arguments.len() < expected {
            arguments.insert(0, self.builder.ins().iconst(types::I64, 0));
        }
        let params = vec![any(); arguments.len()];
        let answer = self.dispatch(
            object,
            slot,
            &params,
            if slot == SET { Ty::Unit } else { any() },
            &arguments,
        )?;
        let Some(answer) = answer else {
            return Ok(None);
        };
        self.convert(answer, Some(any()), ret)
    }

    fn reference_call(
        &mut self,
        slot: u32,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        // Every operand crosses as a reference, which is what the emitted bodies take: the
        // receiver a member reference is asked about, and the value a `set` stores.
        let mut arguments = Vec::with_capacity(3);
        for argument in args {
            arguments.push(self.reference(*argument)?);
        }
        if self.terminated {
            return Ok(None);
        }
        self.reference_slot_call(slot, object, arguments, ret)
    }

    /// Enter `slot` of the reference object `object` with `arguments`, each already a reference.
    fn reference_slot_call(
        &mut self,
        slot: u32,
        object: Value,
        mut arguments: Vec<Value>,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        // `get()` on a BOUND reference passes no receiver and the body ignores the slot anyway; a
        // null keeps one signature for both shapes.
        let expected = usize::from(slot == SET) + 1;
        while arguments.len() < expected {
            arguments.insert(0, self.builder.ins().iconst(types::I64, 0));
        }
        let params = vec![any(); arguments.len()];
        let answer = self.dispatch(
            object,
            slot,
            &params,
            if slot == SET { Ty::Unit } else { any() },
            &arguments,
        )?;
        let Some(answer) = answer else {
            return Ok(None);
        };
        // The declared result is what the program reads: an `Int` property answers a boxed `Int`
        // here and is unboxed back at the call site.
        self.convert(answer, Some(any()), ret)
    }
}

/// The reference-object slot a member with `role` reaches when it is handed `operands` operands
/// after the reference itself, or `None` when the role is not a reference member's.
fn reference_slot(role: Option<krusty::types::SemanticCallRole>, operands: usize) -> Option<u32> {
    use krusty::types::SemanticCallRole;
    match role? {
        SemanticCallRole::KotlinCallableReferenceName if operands == 0 => Some(NAME),
        SemanticCallRole::KotlinPropertyReferenceGet(arity) if operands == usize::from(arity) => {
            Some(GET)
        }
        SemanticCallRole::KotlinPropertyReferenceSet(arity)
            if operands == usize::from(arity) + 1 =>
        {
            Some(SET)
        }
        _ => None,
    }
}
