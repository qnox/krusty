//! The classes earlier files of one source module laid out, as a later file extending one reads
//! them.
//!
//! A class's struct repeats its superclass's fields and its table type its superclass's slot
//! types; its table holds the superclass's functions wherever it overrides nothing; constructing it
//! runs the superclass's constructor. Same-source-module records retain only that module's live
//! declaration identities. Exported source layouts are program-owned physical records keyed by
//! exact KLIB class identity, with no provider-local CallableIds, types, or interface numbers.
//! Constructor identities index those records for authenticated current-provider frame binding;
//! type checks consume the exact class identity independently. Dependency support classes retain
//! their own program-wide records.

use krusty::fx_hash::HashMap;

use krusty::backend::class_tables::LaidOutSuperclass;
use krusty::backend::CheckedBackendCallables;
use krusty::fir::CallableId;
use krusty::ir::{ClassId, CtorDelegateTarget, IrFile};
use krusty::libraries::{ExternalCallableKind, KlibClassIdentity, KlibDependencyCallable};
use krusty::types::SemanticCallableOwner;
use krusty::types::{Ty, TypeName};

use super::super::boxes::BoxAdapter;
use super::super::encode::{FieldType, Module};
use super::super::interface_dispatch::MemberRef;
use super::classes::OwnedBase;
use super::interface_members::Signature;

/// A function and the signature it is carried as.
pub(super) type Placed = (u32, Signature);

/// The erased function-interface entry a class table physically carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FunctionBridgeEntry {
    pub(super) arity: usize,
    pub(super) function: u32,
    pub(super) signature: u32,
}

/// Program-owned operations that adapt one physical signature to another, without provider types.
#[derive(Clone, Debug)]
pub(super) struct SignatureAdapters {
    /// Indexed by physical parameter, including an empty receiver entry at index zero.
    pub(super) parameters: Vec<Option<BoxAdapter>>,
    pub(super) result: Option<BoxAdapter>,
}

impl SignatureAdapters {
    pub(super) fn none(parameter_count: usize) -> Self {
        Self {
            parameters: vec![None; parameter_count],
            result: None,
        }
    }
}

/// One class an earlier file of the source module laid out.
#[derive(Clone, Debug)]
pub(super) struct LaidOutClass {
    pub(super) class_identity: Option<KlibClassIdentity>,
    pub(super) structure: u32,
    pub(super) table_type: u32,
    /// Exact published callable/accessor identities at physical class slots.
    pub(super) declaration_slots: HashMap<Vec<u8>, u32>,
    /// Its struct's fields after the dispatch table, which a subclass's struct begins with.
    pub(super) fields: Vec<FieldType>,
    /// The base no file declares that its struct extends, whose fields lead `fields`.
    pub(super) base: OwnedBase,
    /// The function type of each slot of its table; `None` for an untyped one.
    pub(super) slot_types: Vec<Option<u32>>,
    /// The function each slot of its table holds; `None` for an untyped one.
    pub(super) entries: Vec<Option<u32>>,
    /// The fixed function slot's defined adapter, if this class is a function value.
    pub(super) function_bridge: Option<FunctionBridgeEntry>,
    /// Its struct, then every struct it is declared a subtype of, nearest first.
    pub(super) ancestors: Vec<u32>,
    /// Each constructor by ordinal, the primary first: its function and the parameters it takes.
    pub(super) constructors: Vec<Option<(u32, Vec<Ty>)>>,
    /// Constructor function and actual physical type by ordinal, with no provider-local types.
    pub(super) constructor_entries: Vec<Option<(u32, u32)>>,
    /// Exact KLIB identities published for each constructor ordinal, when one is visible.
    pub(super) constructor_identities: Vec<Option<KlibDependencyCallable>>,
    /// Every method of it and its superclasses that has a body, by identity: its function and that
    /// function's type, which a `super` call of a subclass calls directly.
    pub(super) methods: HashMap<CallableId, (u32, u32)>,
    /// The getters of the companions constructing an instance initializes first, root first.
    pub(super) activation: Vec<u32>,
    /// What the shared class tables read of it.
    pub(super) table: LaidOutSuperclass,
    /// The function its interface table holds at each number that no class slot of it fills.
    pub(super) interface_bodies: HashMap<u32, Placed>,
    /// How it implements each member of a dependency's interface, most derived first.
    pub(super) dependency_implementations: Vec<(MemberRef, DependencyImplementation)>,
    /// Why a subclass cannot extend it, where it has no instances of its own and so its file
    /// needed none of what a subclass would inherit.
    pub(super) refusal: Option<String>,
}

/// How a class implements a member of a dependency's interface, as a subclass inherits it.
#[derive(Clone, Debug)]
pub(super) enum DependencyImplementation {
    /// The member at this class slot, carried from `implemented` to `declared` — what a subclass
    /// overriding the member at that slot implements it with in turn.
    Slot {
        slot: u32,
        declared: Signature,
        implemented: Signature,
        adapters: SignatureAdapters,
    },
    /// A function no class slot holds: a default.
    Function(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PublishedConstructor {
    Layout {
        class: KlibClassIdentity,
        ordinal: usize,
    },
    Conflict,
}

/// The classes laid out so far in the current source module, and the dependency classes support
/// units materialized for the whole program, by identity.
#[derive(Default)]
pub(super) struct LaidOutClasses {
    source_module: Option<String>,
    classes: HashMap<TypeName, LaidOutClass>,
    /// Canonical program-owned source layouts, independent of constructor selections.
    published_source: HashMap<KlibClassIdentity, LaidOutClass>,
    /// Constructor linkage only indexes the canonical class layout and its producer ordinal.
    published_constructors: HashMap<KlibDependencyCallable, PublishedConstructor>,
    /// Published source layouts authenticated by this file's exact selected super constructors.
    active_source: HashMap<TypeName, LaidOutClass>,
    /// A dependency class is materialized once for the program, by provider identities, so its
    /// record outlives the source module whose file selected its support unit.
    dependencies: HashMap<TypeName, LaidOutClass>,
    /// Whether the file being laid out is a dependency support unit.
    support: bool,
}

impl LaidOutClasses {
    /// Begin (or continue) one source module, forgetting another module's classes.
    pub(super) fn enter_source_module(
        &mut self,
        module_name: &str,
        ir: &IrFile,
        callables: &CheckedBackendCallables,
        classifiers: &dyn krusty::backend::BackendClassifierSource,
        module: &mut Module,
    ) {
        self.support = false;
        if self.source_module.as_deref() != Some(module_name) {
            self.source_module = Some(module_name.to_owned());
            self.classes.clear();
        }
        self.active_source.clear();

        self.activate_superclass_initializers(ir, callables, classifiers, module);
    }

    fn activate_superclass_initializers(
        &mut self,
        ir: &IrFile,
        callables: &CheckedBackendCallables,
        classifiers: &dyn krusty::backend::BackendClassifierSource,
        module: &mut Module,
    ) {
        for (class_id, target) in super_constructor_edges(ir) {
            let parent = ir.classes[class_id as usize].superclass;
            let Some(fact) = callables.callable(target) else {
                continue;
            };
            let Some(identity) = fact.dependency_callable.as_ref() else {
                continue;
            };
            if fact.kind != ExternalCallableKind::Constructor
                || fact.declaration_owner != Some(SemanticCallableOwner::Classifier(parent))
            {
                continue;
            }
            let Some(PublishedConstructor::Layout {
                class: class_identity,
                ordinal: _,
            }) = self.published_constructors.get(identity)
            else {
                continue;
            };
            if classifiers
                .classifier(parent)
                .and_then(|fact| fact.klib_identity.clone())
                .as_ref()
                != Some(class_identity)
            {
                continue;
            }
            let Some(published) = self.published_source.get(class_identity) else {
                continue;
            };
            let Ok(Some((_, _, signature))) = self.published_initializer(identity, module) else {
                continue;
            };
            let current = self.active_source.entry(parent).or_insert_with(|| {
                let mut current = published.clone();
                current.methods.clear();
                current.constructors.fill(None);
                current
            });
            let parameters = fact.declared_params.as_deref().unwrap_or(&fact.params);
            if !constructor_frame_matches(module, parameters, signature) {
                current.refusal = Some(
                    "a selected constructor differing from its producer physical frame".into(),
                );
                continue;
            }
            if !current
                .constructor_identities
                .iter()
                .any(|candidate| candidate.as_ref() == Some(identity))
                || current.structure != published.structure
                || current.table_type != published.table_type
            {
                current.refusal = Some("source classes with conflicting KLIB producers".into());
                continue;
            }
        }
    }

    /// Begin a dependency support unit, whose classes every later file of the program extends.
    pub(super) fn enter_dependency_support(
        &mut self,
        ir: &IrFile,
        callables: &CheckedBackendCallables,
        classifiers: &dyn krusty::backend::BackendClassifierSource,
        module: &mut Module,
    ) {
        self.support = true;
        self.active_source.clear();
        self.activate_superclass_initializers(ir, callables, classifiers, module);
    }

    pub(super) fn get(&self, class: TypeName) -> Option<&LaidOutClass> {
        if self.support {
            return self
                .active_source
                .get(&class)
                .or_else(|| self.dependencies.get(&class));
        }
        self.classes
            .get(&class)
            .or_else(|| self.active_source.get(&class))
            .or_else(|| self.dependencies.get(&class))
    }

    /// Record `class` as a file laid it out. A class an inline function's body declares is laid
    /// out again by every file its body is inlined into, each copy a type of its own, so no single
    /// record stands for it and a subclass of it declines.
    pub(super) fn insert(&mut self, class: TypeName, laid_out: LaidOutClass) {
        self.publish_source(laid_out.clone());
        let records = if self.support {
            &mut self.dependencies
        } else {
            &mut self.classes
        };
        match records.get_mut(&class) {
            Some(known) => {
                known.refusal = Some("a class more than one file lays out".to_string());
            }
            None => {
                records.insert(class, laid_out);
            }
        }
    }

    /// Record the exported form under its exact provider identities, never by a rendered class
    /// name alone. CallableIds and Ty values are local to the producing analysis.
    fn publish_source(&mut self, mut laid_out: LaidOutClass) {
        let Some(identity) = laid_out.class_identity.clone() else {
            return;
        };
        for (ordinal, constructor) in laid_out.constructor_identities.iter().enumerate() {
            if let Some(constructor) = constructor {
                self.publish_constructor(constructor.clone(), identity.clone(), ordinal);
            }
        }
        laid_out.methods.clear();
        laid_out.constructors.fill(None);
        laid_out.interface_bodies.clear();
        laid_out.table.interface_entries.clear();
        match self.published_source.get_mut(&identity) {
            Some(known) => {
                known.refusal = Some("a source class identity with conflicting layouts".into());
            }
            None => {
                self.published_source.insert(identity, laid_out);
            }
        }
    }

    fn publish_constructor(
        &mut self,
        constructor: KlibDependencyCallable,
        class: KlibClassIdentity,
        ordinal: usize,
    ) {
        let binding = PublishedConstructor::Layout { class, ordinal };
        self.published_constructors
            .entry(constructor)
            .and_modify(|known| {
                if known != &binding {
                    *known = PublishedConstructor::Conflict;
                }
            })
            .or_insert(binding);
    }

    /// The existing exact constructor index, independent of any consumer-local ordinal.
    pub(super) fn published_constructor(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<(&KlibClassIdentity, usize)>, String> {
        match self.published_constructors.get(identity) {
            Some(PublishedConstructor::Layout { class, ordinal }) => Ok(Some((class, *ordinal))),
            Some(PublishedConstructor::Conflict) => {
                Err("a constructor with conflicting KLIB producers".into())
            }
            None => Ok(None),
        }
    }

    /// Resolve the selected canonical initializer to its already-defined producer function.
    pub(super) fn published_initializer(
        &self,
        identity: &KlibDependencyCallable,
        module: &Module,
    ) -> Result<Option<(&KlibClassIdentity, u32, u32)>, String> {
        let Some((owner, ordinal)) = self.published_constructor(identity)? else {
            return Ok(None);
        };
        let layout = self
            .published_layout(owner)?
            .ok_or("a selected superclass initializer without its producer layout")?;
        let (function, signature) = layout
            .constructor_entries
            .get(ordinal)
            .copied()
            .flatten()
            .ok_or("a selected superclass initializer without its physical frame")?;
        if module.defined_function_type(function) != Some(signature) {
            return Err(
                "a selected superclass initializer without its defined producer frame".into(),
            );
        }
        Ok(Some((owner, function, signature)))
    }

    /// Read only the physical layout authenticated by the selected classifier record.
    pub(super) fn published_structure(
        &self,
        identity: &KlibClassIdentity,
    ) -> Result<Option<u32>, String> {
        Ok(self
            .published_layout(identity)?
            .map(|layout| layout.structure))
    }

    /// The producer's physical inventory, selected only by its frozen class identity.
    pub(super) fn published_layout(
        &self,
        identity: &KlibClassIdentity,
    ) -> Result<Option<&LaidOutClass>, String> {
        let Some(layout) = self.published_source.get(identity) else {
            return Ok(None);
        };
        if let Some(reason) = &layout.refusal {
            return Err(reason.clone());
        }
        Ok(Some(layout))
    }
}

/// Constructor bodies receive the allocated instance, followed by the complete declaration
/// parameters (including any declaration-owned prefix), and return no value.
pub(super) fn constructor_frame_matches(
    module: &mut Module,
    parameters: &[Ty],
    signature: u32,
) -> bool {
    let mut frame = vec![super::super::objects::REFERENCE];
    for &parameter in parameters {
        let Ok(Some(carrier)) = super::lower::carrier(parameter) else {
            return false;
        };
        frame.push(carrier);
    }
    module.func_type(frame, Vec::new()) == signature
}

/// Actual checked super-constructor declarations; consumer ordinals are not producer identities.
fn super_constructor_edges(ir: &IrFile) -> Vec<(ClassId, krusty::fir::ExternalCallableId)> {
    let mut selected = Vec::new();
    selected.extend(
        ir.external_super_constructors
            .iter()
            .filter_map(|(&child, target)| {
                let class = ir.class_id_by_name(child)?;
                Some((class, target.declaration))
            }),
    );
    selected.extend(ir.external_secondary_super_constructors.iter().filter_map(
        |(&(child, child_ordinal), target)| {
            let class = ir.class_id_by_name(child)?;
            let secondary = ir.classes[class as usize]
                .secondary_ctors
                .get(child_ordinal as usize)?;
            let CtorDelegateTarget::Super { .. } = &secondary.delegate else {
                return None;
            };
            Some((class, target.declaration))
        },
    ));
    selected.sort_unstable_by_key(|(class, target)| (*class, target.raw()));
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::metadata::id_signature::{
        metadata_class_signature, MetadataClass, MetadataContainer,
    };

    fn class(module: &str, name: &str) -> KlibClassIdentity {
        KlibClassIdentity {
            module: module.into(),
            signature: metadata_class_signature(
                MetadataContainer {
                    package: &["sample".into()],
                    classes: &[],
                    native_interop_library: false,
                },
                MetadataClass {
                    name,
                    type_params: &[],
                    expect: false,
                },
            ),
        }
    }

    #[test]
    fn constructor_binding_checks_receiver_prefix_order_count_and_result() {
        use super::super::super::encode::ValType;
        use super::super::super::objects::REFERENCE;
        let mut module = Module::default();
        let physical = module.func_type(vec![REFERENCE, ValType::I32, ValType::I64], Vec::new());
        assert!(constructor_frame_matches(
            &mut module,
            &[Ty::Int, Ty::Long],
            physical
        ));
        assert!(!constructor_frame_matches(
            &mut module,
            &[Ty::Long, Ty::Int],
            physical
        ));
        assert!(!constructor_frame_matches(
            &mut module,
            &[Ty::Long],
            physical
        ));
        assert!(!constructor_frame_matches(
            &mut module,
            &[Ty::Int, Ty::Long, Ty::Unit],
            physical
        ));
        let allocating =
            module.func_type(vec![REFERENCE, ValType::I32, ValType::I64], vec![REFERENCE]);
        assert!(!constructor_frame_matches(
            &mut module,
            &[Ty::Int, Ty::Long],
            allocating
        ));
        let missing_receiver = module.func_type(vec![ValType::I32, ValType::I64], Vec::new());
        assert!(!constructor_frame_matches(
            &mut module,
            &[Ty::Int, Ty::Long],
            missing_receiver
        ));
    }

    fn producer_layout(
        owner: &KlibClassIdentity,
        identities: &[KlibDependencyCallable; 2],
        functions: [u32; 2],
        signatures: [u32; 2],
    ) -> LaidOutClass {
        LaidOutClass {
            class_identity: Some(owner.clone()),
            structure: 0,
            table_type: 0,
            declaration_slots: HashMap::default(),
            fields: Vec::new(),
            base: OwnedBase::None,
            slot_types: Vec::new(),
            entries: Vec::new(),
            function_bridge: None,
            ancestors: Vec::new(),
            constructors: vec![None; 3],
            constructor_entries: vec![
                None,
                Some((functions[0], signatures[0])),
                Some((functions[1], signatures[1])),
            ],
            constructor_identities: vec![
                None,
                Some(identities[0].clone()),
                Some(identities[1].clone()),
            ],
            methods: HashMap::default(),
            activation: Vec::new(),
            table: LaidOutSuperclass::default(),
            interface_bodies: HashMap::default(),
            dependency_implementations: Vec::new(),
            refusal: None,
        }
    }

    #[test]
    fn exact_secondary_super_edges_select_distinct_defined_initializers() {
        use super::super::super::encode::{Code, Function, ValType};
        use super::super::super::objects::REFERENCE;
        let owner = class("producer", "Parent");
        let identities =
            ["FirstConstructor", "SecondConstructor"].map(|name| KlibDependencyCallable {
                module: "producer".into(),
                declaration: krusty::libraries::KlibDeclarationSignature::Public(
                    class("producer", name).signature,
                ),
            });
        let mut module = Module::default();
        let signatures = [
            module.func_type(vec![REFERENCE, ValType::I32], Vec::new()),
            module.func_type(vec![REFERENCE, REFERENCE], Vec::new()),
        ];
        let functions = [module.declare(), module.declare()];
        for index in 0..2 {
            module.define(
                functions[index],
                Function {
                    ty: signatures[index],
                    locals: Vec::new(),
                    code: Code::default(),
                },
            );
        }
        for order in [[0, 1], [1, 0]] {
            let mut layouts = LaidOutClasses::default();
            let layout = producer_layout(&owner, &identities, functions, signatures);
            layouts.publish_source(layout.clone());
            // Both consumer edges can have ordinal zero before provider rebinding. That slot
            // cannot select between the two producer constructors; the exact declaration does.
            let mut consumer = layout;
            consumer.constructors = vec![None];
            for index in order {
                consumer.constructors[0] = Some((functions[index], vec![Ty::Int]));
                layouts
                    .active_source
                    .insert(krusty::types::type_name("sample/Parent"), consumer.clone());
                let resolved = layouts
                    .published_initializer(&identities[index], &module)
                    .unwrap();
                assert_eq!(
                    resolved,
                    Some((&owner, functions[index], signatures[index]))
                );
            }
            for index in 0..2 {
                assert_eq!(
                    layouts
                        .published_initializer(&identities[index], &module)
                        .unwrap(),
                    Some((&owner, functions[index], signatures[index]))
                );
            }
            let reserved = module.declare();
            layouts
                .published_source
                .get_mut(&owner)
                .unwrap()
                .constructor_entries[1] = Some((reserved, signatures[0]));
            assert_eq!(
                layouts.published_initializer(&identities[0], &module),
                Err("a selected superclass initializer without its defined producer frame".into())
            );
        }
    }

    #[test]
    fn source_initializer_acknowledgement_requires_defined_frame_and_live_target() {
        use super::super::super::encode::{Code, Function, ValType};
        use super::super::super::objects::REFERENCE;
        use super::super::{Started, WasmBackend, WasmModule, WasmTarget};
        use krusty::backend::{Backend, ConstructorInitializerDeclaration};
        let backend = WasmBackend::new(WasmTarget::Js);
        let owner = class("producer", "Parent");
        let identities =
            ["FirstConstructor", "SecondConstructor"].map(|name| KlibDependencyCallable {
                module: "producer".into(),
                declaration: krusty::libraries::KlibDeclarationSignature::Public(
                    class("producer", name).signature,
                ),
            });
        let mut request = ConstructorInitializerDeclaration {
            constructor: identities[0].clone(),
            class: owner.clone(),
            parameters: vec![Ty::Int].into_boxed_slice(),
        };
        let mut state = WasmModule::default();
        assert!(!backend.emitted_constructor_initializer(&mut state, &request));
        state.started = Some(Started::new(WasmTarget::Js));
        let started = state.started.as_mut().unwrap();
        let signatures = [
            started
                .module
                .func_type(vec![REFERENCE, ValType::I32], Vec::new()),
            started
                .module
                .func_type(vec![REFERENCE, REFERENCE], Vec::new()),
        ];
        let functions = [started.module.declare(), started.module.declare()];
        started
            .classes
            .publish_source(producer_layout(&owner, &identities, functions, signatures));
        assert!(!backend.emitted_constructor_initializer(&mut state, &request));
        state.started.as_mut().unwrap().module.define(
            functions[0],
            Function {
                ty: signatures[0],
                locals: Vec::new(),
                code: Code::default(),
            },
        );
        assert!(backend.emitted_constructor_initializer(&mut state, &request));
        request.parameters = vec![Ty::Long].into_boxed_slice();
        assert!(!backend.emitted_constructor_initializer(&mut state, &request));
        request.parameters = vec![Ty::Int].into_boxed_slice();
        request.class = class("different", "Parent");
        assert!(!backend.emitted_constructor_initializer(&mut state, &request));
        request.class = owner;
        state.declined = true;
        assert!(!backend.emitted_constructor_initializer(&mut state, &request));
    }

    #[test]
    fn conflicting_constructor_producers_never_bind_in_either_insertion_order() {
        let first = class("first", "Owner");
        let second = class("second", "Owner");
        let constructor = KlibDependencyCallable {
            module: "producer".into(),
            declaration: krusty::libraries::KlibDeclarationSignature::Public(
                first.signature.clone(),
            ),
        };
        for producers in [[first.clone(), second.clone()], [second, first]] {
            let mut layouts = LaidOutClasses::default();
            for producer in &producers {
                layouts.publish_constructor(constructor.clone(), producer.clone(), 0);
            }
            assert_eq!(
                layouts.published_constructors.get(&constructor),
                Some(&PublishedConstructor::Conflict)
            );
            layouts.publish_constructor(constructor.clone(), producers[0].clone(), 0);
            assert_eq!(
                layouts.published_constructors.get(&constructor),
                Some(&PublishedConstructor::Conflict)
            );
        }
    }
}
