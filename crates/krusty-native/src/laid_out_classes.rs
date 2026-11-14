//! The classes earlier files of one source module laid out, as a later file extending one reads
//! them.
//!
//! A subclass's object begins with its superclass's fields, its vtable holds the superclass's
//! entries wherever it overrides nothing, constructing it runs the superclass's constructor, and a
//! `super` call runs the superclass's body. Each file is lowered to its own object without seeing
//! another file's lowering, so the file declaring a class another file extends records all of this
//! here, by the class's identity, under the symbols its object defines for that purpose; the later
//! file of the same source module reads the record ([`krusty::backend::class_tables::LaidOutSuperclass`]
//! is the part the shared tables read). A record does not outlive its source module: the slot plan
//! numbering the vtable and the callable identities naming its methods are that module's. A
//! dependency class a support unit materializes is the exception: the plan numbers it from its
//! provider's declarations alike in every module, by provider identities, so its record serves the
//! rest of the program.

use cranelift_codegen::ir::Signature;
use krusty::backend::class_tables::LaidOutSuperclass;
use krusty::fir::CallableId;
use krusty::fx_hash::HashMap;
use krusty::types::{Ty, TypeName};

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
    classes: HashMap<TypeName, LaidOutClass>,
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
        }
    }

    pub(crate) fn publish_descriptor(
        &mut self,
        identity: krusty::libraries::KlibClassIdentity,
        symbol: String,
    ) {
        self.published_descriptors
            .entry(identity)
            .and_modify(|known| *known = None)
            .or_insert(Some(symbol));
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

    /// The record of `class`: a class of the current source module, or a dependency class.
    pub(crate) fn get(&self, class: TypeName) -> Option<&LaidOutClass> {
        self.classes
            .get(&class)
            .or_else(|| self.dependencies.get(&class))
    }

    /// Record `class` as its declaring file laid it out.
    pub(crate) fn insert(&mut self, class: TypeName, laid_out: LaidOutClass) {
        if self.support {
            self.dependencies.insert(class, laid_out);
        } else {
            self.classes.insert(class, laid_out);
        }
    }
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;
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
            for symbol in definitions {
                layouts.publish_descriptor(identity("producer"), symbol.into());
            }
            let expected =
                Err("a KLIB classifier with conflicting Native descriptor definitions".into());
            assert_eq!(
                layouts.published_descriptor(&identity("producer")),
                expected
            );
            layouts.publish_descriptor(identity("producer"), definitions[0].into());
            assert_eq!(
                layouts.published_descriptor(&identity("producer")),
                expected
            );
        }
    }
}
