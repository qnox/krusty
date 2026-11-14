//! The classes earlier files of one source module laid out, as a later file extending one reads
//! them.
//!
//! A subclass's object begins with its superclass's fields, its vtable holds the superclass's
//! entries wherever it overrides nothing, constructing it runs the superclass's constructor, and a
//! `super` call runs the superclass's body. Each file is lowered to its own object without seeing
//! another file's lowering, so the file declaring a class another file extends records all of this
//! here, by the class's identity, under the symbols its object defines for that purpose; the later
//! file of the same source module reads the record ([`krusty::backend::class_tables::LaidOutSuperclass`]
//! is the part the shared tables read). A transient record's slot numbers and semantic handles do
//! not outlive its module. An exported source class also publishes a provider-free physical record
//! by exact KLIB identity; each consumer rebinds declaration identities and local interface numbers
//! before using that layout.

use cranelift_codegen::ir::Signature;
use krusty::backend::class_tables::LaidOutSuperclass;
use krusty::fir::CallableId;
use krusty::fx_hash::HashMap;
use krusty::types::{Ty, TypeName};

/// Authenticate the producer identity at the boundary which owns this class declaration.
/// Source classes use the actual KLIB export registry; dependency support uses its frozen fact.
pub(crate) fn producer_class_identity(
    unit: krusty::backend::CheckedIrUnitKind,
    exported: Option<&krusty::backend::KlibClassIdentity>,
    classifier: Option<&krusty::backend::KlibClassIdentity>,
) -> Result<Option<krusty::backend::KlibClassIdentity>, &'static str> {
    if let (Some(exported), Some(classifier)) = (exported, classifier) {
        if exported != classifier {
            return Err("a Native class layout identity differs from its exact classifier fact");
        }
    }
    Ok(match unit {
        krusty::backend::CheckedIrUnitKind::Source => exported.cloned(),
        krusty::backend::CheckedIrUnitKind::DependencySupport => classifier.cloned(),
    })
}

/// A function an earlier file's object defines or imports, as a later file names it.
#[derive(Clone, Debug)]
pub(crate) enum ObjectFunction {
    /// Linked by this symbol, with this signature.
    Linked {
        symbol: String,
        signature: Signature,
    },
    /// A function its object keeps to itself, which no other object can name.
    Local(String),
}

/// One class an earlier file of the source module laid out.
#[derive(Clone, Debug)]
pub(crate) struct LaidOutClass {
    /// The exact provider identity published for this physical class, if it is an exported
    /// source classifier. This is used only to create a provider-free persistent copy.
    pub(crate) class_identity: Option<krusty::backend::KlibClassIdentity>,
    /// A provider-neutral copy of this record, present only when every persisted entry is
    /// physically linkable and all module-numbered identities were authenticated.
    pub(crate) published_layout: Option<PublishedNativeClassLayout>,
    /// What the shared class tables read of it.
    pub(crate) table: LaidOutSuperclass,
    /// The symbol of its descriptor, which a subclass's descriptor points at.
    pub(crate) descriptor: String,
    /// The first byte after its last field, where a subclass's own fields begin.
    pub(crate) fields_end: u32,
    /// Every reference field of it, inherited ones first: what the collector traces.
    pub(crate) reference_offsets: Vec<u32>,
    /// What each slot of its vtable holds.
    pub(crate) vtable: Vec<ObjectFunction>,
    /// What its interface table holds at each number no class slot of it fills: a bridge.
    pub(crate) interface_bodies: HashMap<u32, ObjectFunction>,
    /// Each constructor by ordinal, the primary first: an entry running its file's initializer
    /// and then the constructor on an allocated object, and the parameters it takes after it.
    pub(crate) constructors: Vec<Option<(ObjectFunction, Vec<Ty>)>>,
    /// Every method of it and its superclasses with a body, by identity: what a `super` call of a
    /// subclass calls, with the parameters it carries and its result.
    pub(crate) methods: HashMap<CallableId, (ObjectFunction, Vec<Ty>, Ty)>,
    /// What initializing it runs before a constructor's arguments: its file's initializer, then
    /// its companions' and its superclasses', root first.
    pub(crate) activation: ObjectFunction,
    /// How it implements each member of another source module's interface, by the member's
    /// identity, most derived first.
    pub(crate) dependency_implementations: Vec<(Vec<u8>, DependencyImplementation)>,
    /// Exact KLIB constructor entries rebound for a selected declaration in this module.
    pub(crate) published_constructors:
        HashMap<krusty::backend::KlibDependencyCallable, ObjectFunction>,
    /// Exact KLIB method bodies a checked `super` call may name from a consumer module.
    pub(crate) published_methods: HashMap<krusty::backend::KlibDependencyCallable, ObjectFunction>,
}

/// The physical part of a source class layout that may outlive the semantic provider which
/// produced it. Unlike [`LaidOutClass`], this record contains no `Ty`, `CallableId`, or
/// module-local interface number. Consumers rebind its identities against their own frozen
/// classifier and callable facts before constructing the transient layout view.
#[derive(Clone, Debug)]
pub(crate) struct PublishedNativeClassLayout {
    pub(crate) identity: krusty::backend::KlibClassIdentity,
    pub(crate) descriptor: String,
    pub(crate) fields_end: u32,
    pub(crate) reference_offsets: Vec<u32>,
    pub(crate) slots: krusty::backend::class_tables::module_slots::PublishedClassSlots,
    /// The type identities inherited by this class; consumers translate these through their
    /// current classifier facts instead of retaining provider `TypeName` values.
    pub(crate) interfaces: Vec<krusty::backend::KlibClassIdentity>,
    /// Stable interface-member identity to the vtable slot (or bridge marker) it implements.
    pub(crate) interface_entries: HashMap<Vec<u8>, Option<u32>>,
    pub(crate) vtable: Vec<ObjectFunction>,
    /// Interface member identities are module-local numbers in the producer, so persist keys.
    pub(crate) interface_bodies: HashMap<Vec<u8>, ObjectFunction>,
    pub(crate) constructors: HashMap<krusty::backend::KlibDependencyCallable, ObjectFunction>,
    pub(crate) methods: HashMap<krusty::backend::KlibDependencyCallable, ObjectFunction>,
    pub(crate) activation: ObjectFunction,
    /// Stable dependency-interface member identity to the actual linked Native implementation.
    pub(crate) dependency_implementations: Vec<(Vec<u8>, ObjectFunction)>,
}

/// How a class implements a member of another source module's interface, as a subclass inherits
/// it.
#[derive(Clone, Debug)]
pub(crate) enum DependencyImplementation {
    /// The member at this vtable slot, which a subclass overriding it implements it with in turn.
    Slot(u32),
    /// The default of this module's interface at this number of the interface table.
    Interface(u32),
    /// A function no table of the class holds: a dependency interface's default.
    Function(ObjectFunction),
    /// The member at this vtable slot, carried differently from how the interface declares it
    /// (`next(): Int` implementing `Iterator<T>.next(): T`): a bridge converting between the
    /// member's parameter and result types, `declared`, and the override's, `implemented`, and
    /// dispatching through the slot, which a subclass overriding it fills in turn.
    Bridge {
        slot: u32,
        declared: (Vec<Ty>, Ty),
        implemented: (Vec<Ty>, Ty),
    },
}

/// The classes laid out so far in the current source module, and the dependency classes support
/// units materialized for the whole program, by identity.
#[derive(Debug, Default)]
pub(crate) struct LaidOutClasses {
    source_module: Option<String>,
    /// Only descriptors actually defined by source producers survive their provider lifetime.
    /// An occupied `None` records conflicting definitions and is never a usable binding.
    published_descriptors: HashMap<krusty::libraries::KlibClassIdentity, Option<String>>,
    /// Exact-identity Native layouts whose physical definitions survived object emission.
    /// Conflicting producers poison the identity instead of letting a later file pick one.
    published_layouts:
        HashMap<krusty::backend::KlibClassIdentity, Option<PublishedNativeClassLayout>>,
    classes: HashMap<TypeName, LaidOutClass>,
    /// The exact KLIB layouts currently rebound to this module's TypeNames and interface numbers.
    /// These views are disposable; only `published_layouts` survives a module transition.
    published_by_name: HashMap<TypeName, LaidOutClass>,
    /// A dependency class is materialized once for the program, by provider identities, so its
    /// record outlives the source module whose file selected its support unit.
    dependencies: HashMap<TypeName, LaidOutClass>,
    /// Whether the file being laid out is a dependency support unit.
    support: bool,
}

impl LaidOutClasses {
    /// Begin (or continue) one source module's file, or with `support` a dependency support unit
    /// it selected, forgetting another module's classes.
    pub(crate) fn enter_source_module(&mut self, module_name: &str, support: bool) {
        self.support = support;
        if self.source_module.as_deref() != Some(module_name) {
            self.source_module = Some(module_name.to_owned());
            self.classes.clear();
            self.published_by_name.clear();
        }
    }

    pub(crate) fn bind_published_layouts(
        &mut self,
        classifier_names: impl IntoIterator<Item = TypeName>,
        classifiers: &dyn krusty::backend::BackendClassifierSource,
        slot_plan: &krusty::backend::class_tables::module_slots::ModuleSlotPlan,
        ir: &krusty::ir::IrFile,
        exports: &krusty::backend::DependencyExportSignatures,
        callables: &krusty::backend::CheckedBackendCallables,
    ) {
        self.published_by_name.clear();
        let mut names = HashMap::default();
        let mut ambiguous = krusty::fx_hash::HashSet::default();
        let mut pending = classifier_names.into_iter().collect::<Vec<_>>();
        let mut seen = krusty::fx_hash::HashSet::default();
        while let Some(name) = pending.pop() {
            if !seen.insert(name) {
                continue;
            }
            if let Some(identity) = classifiers.classifier(name) {
                pending.extend(identity.supertypes.iter().copied());
                if let Some(identity) = identity.klib_identity.clone() {
                    if let Some(previous) = names.insert(identity.clone(), name) {
                        if previous != name {
                            names.remove(&identity);
                            ambiguous.insert(identity);
                        }
                    }
                }
            }
        }
        let layouts: Vec<_> = self
            .published_layouts
            .iter()
            .filter_map(|(identity, layout)| {
                if ambiguous.contains(identity) {
                    return None;
                }
                names
                    .get(identity)
                    .copied()
                    .map(|name| (name, identity, layout))
            })
            .collect();
        for (name, identity, layout) in layouts {
            let Some(layout) = layout.as_ref() else {
                continue;
            };
            let mut interfaces = Vec::with_capacity(layout.interfaces.len());
            let mut complete = true;
            for interface in &layout.interfaces {
                let Some(interface) = names.get(interface) else {
                    complete = false;
                    break;
                };
                interfaces.push(*interface);
            }
            if !complete {
                continue;
            }
            let mut interface_entries = HashMap::default();
            let mut interface_bodies = HashMap::default();
            let mut complete = true;
            for number in 0..slot_plan.interfaces().count() {
                let Some(keys) = slot_plan.interface_member_keys(number) else {
                    complete = false;
                    break;
                };
                for key in keys {
                    let Some(member) = plan_key_identity(key, ir, exports, callables) else {
                        continue;
                    };
                    if let Some(entry) = layout.interface_entries.get(&member) {
                        if interface_entries
                            .insert(number, *entry)
                            .is_some_and(|previous| previous != *entry)
                        {
                            complete = false;
                            break;
                        }
                    }
                    if let Some(body) = layout.interface_bodies.get(&member) {
                        if interface_bodies
                            .insert(number, body.clone())
                            .is_some_and(|previous| !same_object_function(&previous, body))
                        {
                            complete = false;
                            break;
                        }
                    }
                }
            }
            if !complete {
                continue;
            }
            let table = krusty::backend::class_tables::LaidOutSuperclass {
                slots: layout.slots.count,
                interfaces,
                interface_entries,
            };
            let dependency_implementations = layout
                .dependency_implementations
                .iter()
                .map(|(identity, function)| {
                    (
                        identity.clone(),
                        DependencyImplementation::Function(function.clone()),
                    )
                })
                .collect();
            self.published_by_name.insert(
                name,
                LaidOutClass {
                    class_identity: Some(identity.clone()),
                    published_layout: None,
                    table,
                    descriptor: layout.descriptor.clone(),
                    fields_end: layout.fields_end,
                    reference_offsets: layout.reference_offsets.clone(),
                    vtable: layout.vtable.clone(),
                    interface_bodies,
                    constructors: Vec::new(),
                    methods: HashMap::default(),
                    activation: layout.activation.clone(),
                    dependency_implementations,
                    published_constructors: layout.constructors.clone(),
                    published_methods: layout.methods.clone(),
                },
            );
        }
    }

    pub(crate) fn publish_descriptor(
        &mut self,
        identity: krusty::libraries::KlibClassIdentity,
        symbol: String,
    ) {
        let native_identity = krusty::backend::KlibClassIdentity {
            module: identity.module.clone(),
            signature: identity.signature.clone(),
        };
        let conflicting = self.published_descriptors.contains_key(&identity);
        self.published_descriptors
            .insert(identity, if conflicting { None } else { Some(symbol) });
        if conflicting {
            self.published_layouts.insert(native_identity.clone(), None);
            self.published_by_name
                .retain(|_, class| class.class_identity.as_ref() != Some(&native_identity));
            self.dependencies
                .retain(|_, class| class.class_identity.as_ref() != Some(&native_identity));
        }
    }

    pub(crate) fn published_descriptor(
        &self,
        identity: &krusty::libraries::KlibClassIdentity,
    ) -> Result<Option<&str>, String> {
        match self.published_descriptors.get(identity) {
            None => Ok(None),
            Some(Some(symbol)) => Ok(Some(symbol.as_str())),
            Some(None) => {
                Err("a KLIB classifier with conflicting Native descriptor definitions".into())
            }
        }
    }

    fn publish_layout(&mut self, layout: PublishedNativeClassLayout) {
        self.published_layouts
            .entry(layout.identity.clone())
            .and_modify(|known| *known = None)
            .or_insert(Some(layout));
    }

    pub(crate) fn published_layout(
        &self,
        identity: &krusty::backend::KlibClassIdentity,
    ) -> Result<Option<&PublishedNativeClassLayout>, String> {
        match self.published_layouts.get(identity) {
            None => Ok(None),
            Some(Some(layout)) => Ok(Some(layout)),
            Some(None) => {
                Err("a KLIB classifier with conflicting Native layout definitions".into())
            }
        }
    }

    /// The record of `class`: a class of the current source module, or a dependency class.
    pub(crate) fn get(&self, class: TypeName) -> Option<&LaidOutClass> {
        self.classes
            .get(&class)
            .or_else(|| self.published_by_name.get(&class))
            .or_else(|| self.dependencies.get(&class))
    }

    /// Record `class` as its declaring file laid it out.
    pub(crate) fn insert(&mut self, class: TypeName, laid_out: LaidOutClass) {
        if let Some(published) = laid_out.published_layout.clone() {
            // The classifier fact proves semantic identity, while the existing descriptor
            // publication registry proves that this object actually defined the linkable symbol
            // the identity promises. A source fact alone must never publish a dangling layout.
            if self
                .published_descriptor(&published.identity)
                .ok()
                .flatten()
                == Some(published.descriptor.as_str())
            {
                self.publish_layout(published);
            }
        }
        if self.support {
            self.dependencies.insert(class, laid_out);
        } else {
            self.classes.insert(class, laid_out);
        }
    }
}

fn same_object_function(left: &ObjectFunction, right: &ObjectFunction) -> bool {
    match (left, right) {
        (
            ObjectFunction::Linked {
                symbol: left_symbol,
                signature: left_signature,
            },
            ObjectFunction::Linked {
                symbol: right_symbol,
                signature: right_signature,
            },
        ) => left_symbol == right_symbol && left_signature == right_signature,
        (ObjectFunction::Local(left), ObjectFunction::Local(right)) => left == right,
        _ => false,
    }
}

fn plan_key_identity(
    key: krusty::backend::class_tables::module_slots::PlanKey,
    ir: &krusty::ir::IrFile,
    exports: &krusty::backend::DependencyExportSignatures,
    callables: &krusty::backend::CheckedBackendCallables,
) -> Option<Vec<u8>> {
    use krusty::backend::class_tables::module_slots::PlanKey;
    use krusty::backend::interface_identity::{member_identity, property_member_identity};
    match key {
        PlanKey::Callable(callable) => ir
            .checked_callable_functions
            .get(&callable)
            .and_then(|function| {
                exports
                    .interface_member(*function)
                    .or_else(|| exports.member_declaration(*function))
            })
            .map(member_identity),
        PlanKey::FileFunction(_) | PlanKey::FileGetter { .. } | PlanKey::FileSetter { .. } => None,
        PlanKey::Getter(property) | PlanKey::Setter(property) => exports
            .interface_property(property)
            .or_else(|| exports.property_declaration(property))
            .and_then(|declaration| {
                property_member_identity(declaration, matches!(key, PlanKey::Setter(_)))
            }),
        PlanKey::DependencyCallable(callable) => callables
            .declaration_identity(callable)
            .map(member_identity),
        PlanKey::DependencyGetter(getter) | PlanKey::DependencySetter(getter) => callables
            .declaration_identity(getter)
            .and_then(|declaration| {
                property_member_identity(declaration, matches!(key, PlanKey::DependencySetter(_)))
            }),
        PlanKey::Forwarder { .. } => None,
    }
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;
    use cranelift_codegen::isa::CallConv;
    use krusty::libraries::KlibClassIdentity;
    use krusty::metadata::id_signature::{
        metadata_class_signature, MetadataClass, MetadataContainer,
    };

    fn identity(module: &str) -> KlibClassIdentity {
        KlibClassIdentity {
            module: module.into(),
            signature: metadata_class_signature(
                MetadataContainer {
                    package: &["sample".into()],
                    classes: &[],
                    native_interop_library: false,
                },
                MetadataClass {
                    name: "Owner",
                    type_params: &[],
                    expect: false,
                },
            ),
        }
    }

    fn published_layout(
        identity: KlibClassIdentity,
        descriptor: &str,
    ) -> PublishedNativeClassLayout {
        PublishedNativeClassLayout {
            identity,
            descriptor: descriptor.into(),
            fields_end: 8,
            reference_offsets: Vec::new(),
            slots: krusty::backend::class_tables::module_slots::PublishedClassSlots {
                count: 3,
                declarations: HashMap::default(),
            },
            interfaces: Vec::new(),
            interface_entries: HashMap::default(),
            vtable: Vec::new(),
            interface_bodies: HashMap::default(),
            constructors: HashMap::default(),
            methods: HashMap::default(),
            activation: ObjectFunction::Linked {
                symbol: "kt_sample_activate".into(),
                signature: Signature::new(CallConv::SystemV),
            },
            dependency_implementations: Vec::new(),
        }
    }

    #[test]
    fn published_native_layouts_are_keyed_by_exact_klib_identity() {
        let first = identity("first");
        let second = identity("second");
        let mut layouts = LaidOutClasses::default();
        layouts.publish_layout(published_layout(first.clone(), "kt_type_sample_Owner"));
        assert!(layouts.published_layout(&first).unwrap().is_some());
        assert!(layouts.published_layout(&second).unwrap().is_none());
    }

    #[test]
    fn conflicting_native_layout_definitions_remain_unusable() {
        let identity = identity("producer");
        let mut layouts = LaidOutClasses::default();
        layouts.publish_layout(published_layout(identity.clone(), "first_descriptor"));
        layouts.publish_layout(published_layout(identity.clone(), "first_descriptor"));
        assert!(matches!(
            layouts.published_layout(&identity),
            Err(reason) if reason == "a KLIB classifier with conflicting Native layout definitions"
        ));
    }

    #[test]
    fn published_descriptors_survive_modules_without_aliasing_equal_signatures() {
        let mut layouts = LaidOutClasses::default();
        layouts.enter_source_module("producer", false);
        layouts.publish_descriptor(identity("first"), "first_descriptor".into());
        layouts.enter_source_module("consumer", false);
        assert_eq!(
            layouts.published_descriptor(&identity("first")),
            Ok(Some("first_descriptor"))
        );
        assert_eq!(layouts.published_descriptor(&identity("second")), Ok(None));
        layouts.publish_descriptor(identity("second"), "second_descriptor".into());
        assert_eq!(
            layouts.published_descriptor(&identity("first")),
            Ok(Some("first_descriptor"))
        );
        assert_eq!(
            layouts.published_descriptor(&identity("second")),
            Ok(Some("second_descriptor"))
        );
    }

    #[test]
    fn conflicting_descriptor_definitions_are_sticky_in_both_orders() {
        for definitions in [["first", "second"], ["second", "first"]] {
            let mut layouts = LaidOutClasses::default();
            let identity = identity("producer");
            layouts.publish_descriptor(identity.clone(), definitions[0].into());
            layouts.publish_layout(published_layout(identity.clone(), definitions[0]));
            assert!(layouts.published_layout(&identity).unwrap().is_some());

            layouts.publish_descriptor(identity.clone(), definitions[1].into());
            let expected =
                Err("a KLIB classifier with conflicting Native descriptor definitions".into());
            assert_eq!(layouts.published_descriptor(&identity), expected);
            assert!(matches!(
                layouts.published_layout(&identity),
                Err(reason) if reason == "a KLIB classifier with conflicting Native layout definitions"
            ));

            // A later producer cannot restore a layout after its descriptor identity was
            // poisoned, even if the attempted physical layout is byte-for-byte identical.
            layouts.publish_layout(published_layout(identity.clone(), definitions[0]));
            layouts.publish_descriptor(identity.clone(), definitions[0].into());
            assert_eq!(layouts.published_descriptor(&identity), expected);
            assert!(matches!(
                layouts.published_layout(&identity),
                Err(reason) if reason == "a KLIB classifier with conflicting Native layout definitions"
            ));
        }
    }

    #[test]
    fn source_layout_identity_comes_from_its_canonical_export() {
        use krusty::backend::CheckedIrUnitKind::{DependencySupport, Source};
        let exported = identity("source");
        assert_eq!(
            producer_class_identity(Source, Some(&exported), None),
            Ok(Some(exported.clone()))
        );
        assert_eq!(
            producer_class_identity(Source, None, Some(&exported)),
            Ok(None)
        );
        assert_eq!(
            producer_class_identity(DependencySupport, None, Some(&exported)),
            Ok(Some(exported))
        );
    }

    #[test]
    fn producer_and_frozen_classifier_identities_must_agree() {
        use krusty::backend::CheckedIrUnitKind::{DependencySupport, Source};
        let exported = identity("source");
        let other = identity("other");
        for unit in [Source, DependencySupport] {
            assert_eq!(
                producer_class_identity(unit, Some(&exported), Some(&exported)),
                Ok(Some(exported.clone()))
            );
            assert_eq!(
                producer_class_identity(unit, Some(&exported), Some(&other)),
                Err("a Native class layout identity differs from its exact classifier fact")
            );
        }
    }
}
