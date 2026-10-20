//! Throwing stubs for the mutable members of a read-only collection face (kotlinc's
//! `CollectionStubMethodLowering` and `CollectionStubComputer`).
//!
//! Kotlin's read-only collections map onto mutable JDK interfaces, so a class that implements a
//! read-only face without its mutable counterpart realizes each candidate member of the mutable
//! declaration that nothing in the class effectively overrides as a public method throwing
//! `UnsupportedOperationException`. Which faces apply comes from the class's checked applied
//! hierarchy; which members are candidates, and their shapes, come from the mutable declaration in
//! `.kotlin_builtins`. The only tables here are kotlinc's own: the order of its face pairs and the
//! faces whose stubs this backend realizes so far. The plans remain backend facts; common IR never
//! carries a stub.

use crate::backend::BackendClassifierSource;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{IrClass, IrFile};
use crate::jvm::classpath::Classpath;
use crate::jvm::inherited_collection_bridges::InheritedCollectionBridges;
use crate::jvm::jvm_class_map::{
    jvm_collection_to_kotlin_mutable_type_name, mapped_collection, to_jvm_type_name,
};
use crate::jvm::mapped_builtin_declarations::MappedBuiltinMemberKind;
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::jvm::names::method_descriptor;
use crate::types::{ty_subst, wk, CollectionKind, Ty, TypeName};

/// kotlinc's `preComputedStubs` order: a class's applicable faces contribute their stubs in this
/// order.
const FACE_ORDER: [CollectionKind; 8] = [
    CollectionKind::Collection,
    CollectionKind::Set,
    CollectionKind::List,
    CollectionKind::Map,
    CollectionKind::MapEntry,
    CollectionKind::Iterable,
    CollectionKind::Iterator,
    CollectionKind::ListIterator,
];

/// The faces whose stubs are realized. The remaining faces need kotlinc's `removeAt` and
/// `liftStubMethodReturnType` rewrites, the JDK members of the mutable collections, and the
/// `toArray` lowering before they can be enabled.
const REALIZED_FACES: [CollectionKind; 2] =
    [CollectionKind::Iterator, CollectionKind::ListIterator];

const ACC_STATIC: u16 = 0x0008;
const ACC_PRIVATE: u16 = 0x0002;
const ACC_BRIDGE: u16 = 0x0040;
const ACC_SYNTHETIC: u16 = 0x1000;
const ACC_ABSTRACT: u16 = 0x0400;

/// The JVM-only collection members of every class: bridges for members inherited from a Java
/// collection superclass, and throwing stubs for a read-only face's mutable members.
#[derive(Default)]
pub(super) struct CollectionMemberPlans {
    pub(super) inherited_bridges: InheritedCollectionBridges,
    pub(super) read_only_stubs: ReadOnlyCollectionStubs,
}

#[derive(Default)]
pub(super) struct ReadOnlyCollectionStubs {
    by_class: HashMap<TypeName, Vec<ReadOnlyStub>>,
}

impl ReadOnlyCollectionStubs {
    pub(super) fn for_class(&self, class: TypeName) -> &[ReadOnlyStub] {
        self.by_class.get(&class).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// One throwing stub, in the substitution context of the class that declares it.
#[derive(Clone, Debug)]
pub(super) struct ReadOnlyStub {
    pub(super) name: String,
    /// The declared parameter types, substituted with the face's type arguments.
    pub(super) parameters: Vec<Ty>,
    pub(super) parameter_names: Vec<String>,
    pub(super) result: Ty,
    /// The erased JVM parameters and result of the mutable declaration, when they differ from the
    /// stub's own descriptor: kotlinc's bridge lowering gives the stub a throwing bridge there.
    pub(super) bridge: Option<(Vec<Ty>, Ty)>,
}

impl ReadOnlyStub {
    pub(super) fn descriptor(&self) -> String {
        method_descriptor(&jvm_tys(&self.parameters), jvm_declared_ty(&self.result))
    }

    fn parameter_key(&self) -> (String, String) {
        (self.name.clone(), parameter_descriptor(&self.parameters))
    }
}

/// Select the read-only collection stubs of every class that realizes its overrides.
pub(super) fn select(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &dyn BackendClassifierSource,
) -> ReadOnlyCollectionStubs {
    let mut result = ReadOnlyCollectionStubs::default();
    for class_index in 0..ir.classes.len() {
        if !crate::jvm::override_results::realizes_overrides(ir, class_index) {
            continue;
        }
        let class = &ir.classes[class_index];
        if class.is_interface || class.is_annotation {
            continue;
        }
        let stubs = class_stubs(ir, classpath, classifiers, class);
        if !stubs.is_empty() {
            result.by_class.insert(class.fq_name_id(), stubs);
        }
    }
    result
}

/// A read-only face that applies to a class, with its mutable counterpart.
#[derive(Clone, Copy)]
struct Face {
    kind: CollectionKind,
    read_only: TypeName,
    mutable: TypeName,
}

fn class_stubs(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &dyn BackendClassifierSource,
    class: &IrClass,
) -> Vec<ReadOnlyStub> {
    let Some(hierarchy) = ir.classifier_hierarchies.get(&class.fq_name_id()) else {
        return Vec::new();
    };
    let supertypes: Vec<TypeName> = hierarchy
        .iter()
        .filter(|entry| entry.depth > 0)
        .map(|entry| entry.classifier)
        .collect();
    let applied_face = |face: TypeName| {
        hierarchy
            .iter()
            .find(|entry| entry.depth > 0 && entry.classifier == face)
            .map(|entry| entry.applied)
    };
    let own_stubs = |faces: &[Face]| -> Vec<ReadOnlyStub> {
        faces
            .iter()
            .filter(|face| REALIZED_FACES.contains(&face.kind))
            .filter_map(|face| Some((face, applied_face(face.read_only)?)))
            .flat_map(|(face, applied)| face_stubs(classpath, *face, applied))
            .collect()
    };

    let faces = applicable_faces(classpath, &supertypes);
    let candidates = own_stubs(&faces);
    if candidates.is_empty() {
        return candidates;
    }

    // kotlinc's `computeStubsForSuperClasses`: a stub a superclass already realizes, built in this
    // class's substitution context, is not repeated.
    let chain = superclass_chain(ir, classpath, classifiers, class);
    let mut present: HashSet<(String, String)> = HashSet::default();
    for superclass in &chain {
        let faces = applicable_faces(classpath, &superclass.supertypes);
        for stub in own_stubs(&faces) {
            if !superclass.abstract_members.contains(&stub.parameter_key()) {
                present.insert((stub.name.clone(), stub.descriptor()));
            }
        }
    }

    let implemented = implemented_members(ir, class, &chain);
    candidates
        .into_iter()
        .filter(|stub| present.insert((stub.name.clone(), stub.descriptor())))
        .filter(|stub| !implemented.contains(&stub.parameter_key()))
        .collect()
}

/// kotlinc's `computeStubsForCollectionClasses`: each read-only face the class strictly extends
/// without its mutable counterpart, keeping only the most specific of related faces, in kotlinc's
/// pair order.
fn applicable_faces(classpath: &Classpath, supertypes: &[TypeName]) -> Vec<Face> {
    let mut faces: Vec<Face> = Vec::new();
    for &supertype in supertypes {
        let Some(mapped) = mapped_collection(supertype) else {
            continue;
        };
        if mapped.mutable || faces.iter().any(|face| face.kind == mapped.kind) {
            continue;
        }
        let jvm = to_jvm_type_name(supertype);
        let Some(mutable) = jvm_collection_to_kotlin_mutable_type_name(jvm) else {
            continue;
        };
        // The mutable face, or the platform interface itself as a Java supertype sees it.
        let extends_mutable = supertypes.iter().any(|&other| {
            to_jvm_type_name(other) == jvm
                && mapped_collection(other).is_none_or(|other| other.mutable)
        });
        if !extends_mutable {
            faces.push(Face {
                kind: mapped.kind,
                read_only: supertype,
                mutable,
            });
        }
    }
    let specific: Vec<Face> = faces
        .iter()
        .filter(|face| {
            !faces.iter().any(|other| {
                other.read_only != face.read_only
                    && builtin_extends(classpath, other.read_only, face.read_only)
            })
        })
        .copied()
        .collect();
    let mut ordered = specific;
    ordered.sort_by_key(|face| FACE_ORDER.iter().position(|kind| *kind == face.kind));
    ordered
}

fn builtin_extends(classpath: &Classpath, sub: TypeName, sup: TypeName) -> bool {
    let mut pending = vec![sub];
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop() {
        if current == sup {
            return true;
        }
        if seen.insert(current) {
            pending.extend(classpath.builtin_supertypes_name(current).iter_ids());
        }
    }
    false
}

/// kotlinc's `candidatesForStubs` and `createStubMethod`: the members the mutable declaration
/// itself declares, substituted with the read-only face's type arguments.
fn face_stubs(classpath: &Classpath, face: Face, applied: Ty) -> Vec<ReadOnlyStub> {
    let Some((formals, _)) = classpath.builtin_class_gsig_name(face.mutable) else {
        return Vec::new();
    };
    let arguments = applied.non_null().type_args();
    assert_eq!(
        formals.len(),
        arguments.len(),
        "a read-only collection face and its mutable counterpart share their type parameters"
    );
    let bindings: HashMap<String, Ty> = formals
        .iter()
        .cloned()
        .zip(arguments.iter().copied())
        .collect();
    let erasures: HashMap<String, Ty> = formals
        .iter()
        .map(|formal| (formal.clone(), Ty::nullable(Ty::obj("kotlin/Any"))))
        .collect();
    classpath
        .decoded_builtin_members_name(face.mutable)
        .into_iter()
        .filter(|member| member.kind == MappedBuiltinMemberKind::Function)
        .map(|member| {
            let declared = &member.generic_sig;
            // kotlinc types every parameter of a `remove` stub `Any?`, the `Object` its JDK
            // counterpart takes.
            let parameters: Vec<Ty> = if member.source_name == "remove" {
                vec![Ty::nullable(Ty::obj("kotlin/Any")); declared.params.len()]
            } else {
                declared
                    .params
                    .iter()
                    .map(|ty| ty_subst(*ty, &bindings))
                    .collect()
            };
            let result = ty_subst(declared.ret, &bindings);
            let erased_parameters = jvm_tys(
                &declared
                    .params
                    .iter()
                    .map(|ty| ty_subst(*ty, &erasures))
                    .collect::<Vec<_>>(),
            );
            let erased_result = jvm_declared_ty(&ty_subst(declared.ret, &erasures));
            let stub_descriptor =
                method_descriptor(&jvm_tys(&parameters), jvm_declared_ty(&result));
            let bridge = (method_descriptor(&erased_parameters, erased_result) != stub_descriptor)
                .then_some((erased_parameters, erased_result));
            ReadOnlyStub {
                name: member.source_name,
                parameters,
                parameter_names: member.parameter_names,
                result,
                bridge,
            }
        })
        .collect()
}

/// One superclass of the class, with what its stubs depend on.
struct Superclass {
    supertypes: Vec<TypeName>,
    /// The members it declares abstract (kotlinc's abstract-superclass filter).
    abstract_members: HashSet<(String, String)>,
    /// The members it implements, inherited by the class as non-abstract fake overrides.
    implemented_members: HashSet<(String, String)>,
}

fn superclass_chain(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &dyn BackendClassifierSource,
    class: &IrClass,
) -> Vec<Superclass> {
    let mut chain = Vec::new();
    let mut current = class.superclass;
    let mut seen = HashSet::default();
    while current != wk::java_object() && current != wk::any() && seen.insert(current) {
        if let Some(source) = ir.classes.iter().find(|c| c.fq_name_id() == current) {
            let mut superclass = Superclass {
                supertypes: ir
                    .classifier_hierarchies
                    .get(&current)
                    .map(|hierarchy| {
                        hierarchy
                            .iter()
                            .filter(|entry| entry.depth > 0)
                            .map(|entry| entry.classifier)
                            .collect()
                    })
                    .unwrap_or_default(),
                abstract_members: HashSet::default(),
                implemented_members: HashSet::default(),
            };
            for &function in &source.methods {
                let function = &ir.functions[function as usize];
                if function.is_static {
                    continue;
                }
                let key = (
                    function.name.clone(),
                    parameter_descriptor(&function.params),
                );
                if function.body.is_some() {
                    superclass.implemented_members.insert(key);
                } else if source.is_abstract {
                    superclass.abstract_members.insert(key);
                }
            }
            chain.push(superclass);
            current = source.superclass;
            continue;
        }
        let Some(info) = classpath.find_name(current) else {
            break;
        };
        let is_kotlin = info.meta.class_kind.is_some();
        let is_abstract = info.access & ACC_ABSTRACT != 0;
        let mut superclass = Superclass {
            // kotlinc computes no stubs for a Java class.
            supertypes: if is_kotlin {
                classifier_closure(classifiers, current)
            } else {
                Vec::new()
            },
            abstract_members: HashSet::default(),
            implemented_members: HashSet::default(),
        };
        for method in &info.methods {
            if method.access & (ACC_STATIC | ACC_PRIVATE | ACC_BRIDGE | ACC_SYNTHETIC) != 0
                || method.name == "<init>"
            {
                continue;
            }
            let Some(parameters) = method.descriptor.split_once(')').map(|(params, _)| params)
            else {
                continue;
            };
            let key = (method.name.clone(), format!("{parameters})V"));
            if method.access & ACC_ABSTRACT == 0 {
                superclass.implemented_members.insert(key);
            } else if is_abstract {
                superclass.abstract_members.insert(key);
            }
        }
        chain.push(superclass);
        let Some(next) = info.super_class else {
            break;
        };
        current = next;
    }
    chain
}

/// Every classifier a dependency classifier extends, through its published direct supertypes.
fn classifier_closure(classifiers: &dyn BackendClassifierSource, start: TypeName) -> Vec<TypeName> {
    let mut closure = Vec::new();
    let mut pending = vec![start];
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if current != start {
            closure.push(current);
        }
        if let Some(fact) = classifiers.classifier(current) {
            pending.extend(fact.supertypes.iter().copied());
        }
    }
    closure
}

/// kotlinc's effective-override check: the class's own members, whatever their modality, and the
/// non-abstract members it inherits from its superclasses and interface defaults.
fn implemented_members(
    ir: &IrFile,
    class: &IrClass,
    chain: &[Superclass],
) -> HashSet<(String, String)> {
    let mut implemented: HashSet<(String, String)> = class
        .methods
        .iter()
        .map(|&function| &ir.functions[function as usize])
        .filter(|function| !function.is_static)
        .map(|function| {
            (
                function.name.clone(),
                parameter_descriptor(&function.params),
            )
        })
        .collect();
    for superclass in chain {
        implemented.extend(superclass.implemented_members.iter().cloned());
    }
    for default in ir
        .inherited_defaults
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
    {
        if let crate::fir::InheritedMemberName::Function(name) = &default.name {
            implemented.insert((
                name.to_string(),
                parameter_descriptor(&default.applied_parameters),
            ));
        }
    }
    implemented
}

/// A member's JVM parameter shape, keyed with a `void` result so that only the parameters compare.
fn parameter_descriptor(parameters: &[Ty]) -> String {
    method_descriptor(&jvm_tys(parameters), Ty::Unit)
}
