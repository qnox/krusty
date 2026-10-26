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

use crate::assignable::{is_subtype, TyCtx, TypeOracle};
use crate::backend::BackendClassifierSource;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{IrClass, IrFile};
use crate::jvm::backend::SkipReason;
use crate::jvm::classpath::Classpath;
use crate::jvm::collection_barriers::BarrierOutcome;
use crate::jvm::inherited_collection_bridges::InheritedCollectionBridges;
use crate::jvm::jvm_class_map::{
    jvm_collection_to_kotlin_mutable_type_name, jvm_collection_to_kotlin_type_name,
    mapped_collection, to_jvm_type_name,
};
use crate::jvm::mapped_builtin_declarations::{
    collection_bridge_policy_for_declaration, collection_signature_policy_for_declaration,
    realization_for_declaration, CollectionBridgeFailure, CollectionSignaturePolicy,
    MappedBuiltinMemberKind,
};
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::jvm::names::method_descriptor;
use crate::jvm::override_results::scalar_over_reference;
use crate::types::{ty_subst, wk, CollectionKind, Ty, TypeName, Visibility};

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

/// The faces whose stubs are realized. The JDK members of the mutable collections (`removeIf`) and
/// the `toArray` lowering are not yet realized for any face.
const REALIZED_FACES: [CollectionKind; 7] = [
    CollectionKind::Collection,
    CollectionKind::Set,
    CollectionKind::List,
    CollectionKind::Map,
    CollectionKind::MapEntry,
    CollectionKind::Iterator,
    CollectionKind::ListIterator,
];

const ACC_STATIC: u16 = 0x0008;
const ACC_BRIDGE: u16 = 0x0040;
const ACC_SYNTHETIC: u16 = 0x1000;
const ACC_ABSTRACT: u16 = 0x0400;

/// The JVM-only collection members of every class: bridges for members inherited from a Java
/// collection superclass, throwing stubs for a read-only face's mutable members, and the JDK's
/// `toArray` members.
#[derive(Default)]
pub(super) struct CollectionMemberPlans {
    pub(super) inherited_bridges: InheritedCollectionBridges,
    pub(super) read_only_stubs: ReadOnlyCollectionStubs,
    pub(super) to_array: crate::jvm::collection_to_array::CollectionToArray,
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

/// One candidate member of a read-only face, in the substitution context of the class that declares
/// it: a throwing stub, or a member the class already implements that still needs its bridge.
#[derive(Clone, Debug)]
pub(super) struct ReadOnlyStub {
    /// The mutable declaration's source name, which an implementing member declares.
    pub(super) name: String,
    /// The JVM name the stub is realized under: the source name, or the special realization of a
    /// builtin with a different JVM name (`removeAt` is `remove`).
    pub(super) physical_name: String,
    pub(super) body: StubBody,
    /// The stub's parameter types: the declared ones substituted with the face's type arguments,
    /// except that a type-safe declaration's checked parameters keep their erased `Any?`.
    pub(super) parameters: Vec<Ty>,
    /// The declared parameter types substituted with the face's type arguments, all of them: what
    /// a member that implements the declaration declares (`remove(element: String)`).
    pub(super) override_parameters: Vec<Ty>,
    pub(super) parameter_names: Vec<String>,
    pub(super) result: Ty,
    /// kotlinc's `forceBoxedReturnTypeOnOverride`: a scalar result over the mutable declaration's
    /// reference result is the scalar's wrapper on the JVM (`set(II)Ljava/lang/Integer;`).
    pub(super) boxed_result: bool,
    /// The generic signature writes every declaration-site variance of the parameters (kotlinc's
    /// methods with declaration-site wildcards).
    pub(super) declaration_site_wildcards: bool,
    /// The erased JVM parameters and result of the mutable declaration, when they differ from the
    /// stub's own descriptor: kotlinc's bridge lowering gives the stub a throwing bridge there. A
    /// renamed member's bridge is its own parameters with an `Any?` result. An implemented member's
    /// bridge is the descriptor it is reached at, whenever the implementation's own complete
    /// physical descriptor differs from it, so a `Nothing` member's `Void` result alone also takes
    /// an adapter.
    pub(super) bridge: Option<(Vec<Ty>, Ty)>,
    /// The declaration is type-safe (`MutableMap.remove(key)`): the stub already takes the erased
    /// parameters, and kotlinc's bridge for its erased result forwards to it instead of throwing.
    pub(super) type_safe: bool,
    /// The type-safe declaration's barrier (`MutableCollection.remove`): the bridge to a member
    /// that implements it with a narrower parameter first tests each checked argument.
    pub(super) barrier: Option<StubBarrier>,
}

/// kotlinc's type check in the special bridge of a type-safe declaration: each checked argument
/// that is not an instance of the implementation's parameter answers the neutral result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StubBarrier {
    pub(super) checked_parameters: &'static [u16],
    pub(super) outcome: BarrierOutcome,
}

/// What realizes a candidate member at the stub's own descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StubBody {
    /// Nothing implements it: the class gets the throwing stub, and its bridge throws too.
    Throwing,
    /// A member the class declares or inherits effectively overrides it (kotlinc's non-abstract
    /// fake override). No stub is written, but the mutable declaration's erased descriptor still
    /// needs a bridge, which forwards to that member.
    Implemented {
        /// The source order of the class's own implementing member, which places its bridge among
        /// the ordinary ones; `u32::MAX` for an inherited member, whose bridge follows them.
        order: u32,
        /// The implementing member's physical JVM parameters and result, which the bridge calls.
        target: Vec<Ty>,
        target_result: Ty,
    },
}

impl ReadOnlyStub {
    pub(super) fn descriptor(&self) -> String {
        method_descriptor(&jvm_tys(&self.parameters), self.physical_result())
    }

    /// The JVM result of the stub's own descriptor.
    pub(super) fn physical_result(&self) -> Ty {
        physical_stub_result(self.result, self.boxed_result)
    }

    /// The semantic result the generic signature is written from: the wrapper when it is boxed.
    pub(super) fn signature_result(&self) -> Ty {
        if self.boxed_result {
            Ty::nullable(self.result)
        } else {
            self.result
        }
    }

    /// kotlinc's `createRemoveAtStub`: a builtin with a different JVM name is realized under that
    /// name without overriding the mutable declaration, so its result is not boxed and its erased
    /// bridge directly follows it.
    pub(super) fn is_renamed(&self) -> bool {
        self.physical_name != self.name
    }

    /// The declaration an implementing member matches, by its substituted parameters.
    fn parameter_key(&self) -> (String, String) {
        (
            self.name.clone(),
            parameter_descriptor(&self.override_parameters),
        )
    }
}

/// Select the read-only collection stubs of every class that realizes its overrides.
pub(super) fn select(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &dyn BackendClassifierSource,
) -> Result<ReadOnlyCollectionStubs, SkipReason> {
    let mut result = ReadOnlyCollectionStubs::default();
    for class_index in 0..ir.classes.len() {
        if !crate::jvm::override_results::realizes_overrides(ir, class_index) {
            continue;
        }
        let class = &ir.classes[class_index];
        if class.is_interface || class.is_annotation {
            continue;
        }
        let stubs = class_stubs(ir, classpath, classifiers, class)?;
        if !stubs.is_empty() {
            result.by_class.insert(class.fq_name_id(), stubs);
        }
    }
    Ok(result)
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
) -> Result<Vec<ReadOnlyStub>, SkipReason> {
    let Some(hierarchy) = ir.classifier_hierarchies.get(&class.fq_name_id()) else {
        return Ok(Vec::new());
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
        return Ok(candidates);
    }

    // kotlinc's `computeStubsForSuperClasses`: a stub a superclass already realizes, built in this
    // class's substitution context, is not repeated.
    let chain = superclass_chain(ir, classpath, classifiers, class, &candidates)?;
    let mut present: HashSet<(String, String)> = HashSet::default();
    for superclass in &chain {
        let faces = applicable_faces(classpath, &superclass.supertypes);
        for stub in own_stubs(&faces) {
            if !superclass.abstract_members.contains(&stub.parameter_key()) {
                present.insert((stub.name.clone(), stub.descriptor()));
            }
        }
    }

    // An effectively overridden member keeps only its bridge, and only when the mutable
    // declaration's erased descriptor differs from the implementation's.
    let implemented = implemented_members(ir, class, &chain);
    let own_order: HashMap<(String, String), u32> = class
        .methods
        .iter()
        .filter(|&&function| !ir.functions[function as usize].is_static)
        .map(|&function| {
            let declaration = &ir.functions[function as usize];
            (
                (
                    declaration.name.clone(),
                    parameter_descriptor(&declaration.params),
                ),
                ir.fn_source_order
                    .get(&function)
                    .copied()
                    .unwrap_or(u32::MAX),
            )
        })
        .collect();
    Ok(candidates
        .into_iter()
        .filter(|stub| present.insert((stub.name.clone(), stub.descriptor())))
        .filter_map(|stub| {
            // kotlinc's override check: a member with a matching signature implements the
            // candidate only when its result is a subtype of the candidate's, so
            // `fun set(e: String): Int` leaves `set` its throwing stub.
            let Some(implementation) =
                implemented
                    .get(&stub.parameter_key())
                    .filter(|implementation| {
                        is_subtype(
                            &TyCtx::new(),
                            &AppliedHierarchy {
                                ir,
                                classpath,
                                classifiers,
                            },
                            implementation.result,
                            read_only_result(stub.result),
                        )
                    })
            else {
                return Some(stub);
            };
            // The member is reached at the mutable declaration's erased descriptor, or at the
            // stub's own when they agree (a type-safe `remove(Object)`, a renamed `remove(int)`).
            // It needs an adapter there whenever its complete physical descriptor differs, the
            // result included: a generic implementation can already have that descriptor, while
            // `set(e: Any): Nothing` matches its parameters but returns its physical `Void`, and
            // `String remove(int)` of a `List<Any>` is covariantly narrower. A renamed member
            // always needs the special bridge under its JVM name besides.
            let erased = stub
                .bridge
                .clone()
                .unwrap_or_else(|| (jvm_tys(&stub.parameters), stub.physical_result()));
            let bridge = (method_descriptor(&erased.0, erased.1)
                != method_descriptor(&implementation.parameters, implementation.physical_result))
            .then_some(erased);
            if bridge.is_none() && !stub.is_renamed() {
                return None;
            }
            let order = own_order
                .get(&stub.parameter_key())
                .copied()
                .unwrap_or(u32::MAX);
            Some(ReadOnlyStub {
                body: StubBody::Implemented {
                    order,
                    target: implementation.parameters.clone(),
                    target_result: implementation.physical_result,
                },
                bridge,
                ..stub
            })
        })
        .collect())
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
    let mut stubs: Vec<ReadOnlyStub> = classpath
        .decoded_builtin_members_name(face.mutable)
        .into_iter()
        .filter(|member| member.kind == MappedBuiltinMemberKind::Function)
        .map(|member| {
            let declared = &member.generic_sig;
            let parameters: Vec<Ty> = declared
                .params
                .iter()
                .map(|ty| ty_subst(*ty, &bindings))
                .collect();
            let result = ty_subst(declared.ret, &bindings);
            let erased_parameters = jvm_tys(
                &declared
                    .params
                    .iter()
                    .map(|ty| ty_subst(*ty, &erasures))
                    .collect::<Vec<_>>(),
            );
            let erased_result = jvm_declared_ty(&ty_subst(declared.ret, &erasures));
            // kotlinc's type-safe special signatures (`MutableCollection.remove(Object)`): the
            // checked parameters keep their erased `Any?` in the stub as in the JDK declaration.
            // An implementing member still declares the substituted types.
            let override_parameters = parameters.clone();
            let mut parameters = parameters;
            let erased_descriptor = method_descriptor(&erased_parameters, erased_result);
            // The policies belong to the declaration the member overrides: `MutableList.remove`
            // overrides `MutableCollection.remove` with the same erased descriptor.
            let type_safe_policy =
                overridden_declaration_policy(classpath, face.mutable, |owner| {
                    collection_bridge_policy_for_declaration(
                        owner,
                        &member.source_name,
                        &erased_descriptor,
                        MappedBuiltinMemberKind::Function,
                    )
                });
            if let Some(policy) = type_safe_policy {
                for &index in policy.checked_parameters {
                    parameters[usize::from(index)] = Ty::nullable(Ty::obj("kotlin/Any"));
                }
            }
            let barrier = type_safe_policy.and_then(|policy| {
                let outcome = match policy.failure {
                    CollectionBridgeFailure::False => BarrierOutcome::False,
                    CollectionBridgeFailure::Null => BarrierOutcome::Null,
                    CollectionBridgeFailure::NotFound => BarrierOutcome::NotFound,
                    // A default-value declaration (`getOrDefault`) is no mutable member.
                    CollectionBridgeFailure::Argument(_) => return None,
                };
                Some(StubBarrier {
                    checked_parameters: policy.checked_parameters,
                    outcome,
                })
            });
            // kotlinc's signature policies for the realization of this exact declaration.
            let signature_policy =
                overridden_declaration_policy(classpath, face.mutable, |owner| {
                    collection_signature_policy_for_declaration(
                        owner,
                        &member.source_name,
                        &erased_descriptor,
                        MappedBuiltinMemberKind::Function,
                    )
                });
            if let Some(CollectionSignaturePolicy::ErasedCollectionParameter(index)) =
                signature_policy
            {
                let parameter = &mut parameters[usize::from(index)];
                *parameter = star_projected(*parameter);
            }
            let declaration_site_wildcards =
                signature_policy == Some(CollectionSignaturePolicy::DeclarationSiteWildcards);
            let physical_name = realization_for_declaration(
                face.mutable,
                &member.source_name,
                &erased_descriptor,
                MappedBuiltinMemberKind::Function,
            )
            .map_or_else(|| member.source_name.clone(), |(_, name)| name.to_string());
            let renamed = physical_name != member.source_name;
            let boxed_result = !renamed && scalar_over_reference(result, declared.ret);
            let stub_parameters = jvm_tys(&parameters);
            let stub_descriptor =
                method_descriptor(&stub_parameters, physical_stub_result(result, boxed_result));
            let (bridge_parameters, bridge_result) = if renamed {
                (
                    stub_parameters,
                    jvm_declared_ty(&Ty::nullable(Ty::obj("kotlin/Any"))),
                )
            } else {
                (erased_parameters, erased_result)
            };
            let bridge = (method_descriptor(&bridge_parameters, bridge_result) != stub_descriptor)
                .then_some((bridge_parameters, bridge_result));
            ReadOnlyStub {
                name: member.source_name,
                physical_name,
                body: StubBody::Throwing,
                parameters,
                override_parameters,
                parameter_names: member.parameter_names,
                result,
                boxed_result,
                declaration_site_wildcards,
                bridge,
                type_safe: type_safe_policy.is_some(),
                barrier,
            }
        })
        .collect();
    // kotlinc reads a builtin's members through its member scope, which keeps one name's overloads
    // together in their first declaration's place (`add(E)`, then `add(Int, E)`).
    let mut names: Vec<String> = Vec::new();
    for stub in &stubs {
        if !names.contains(&stub.name) {
            names.push(stub.name.clone());
        }
    }
    stubs.sort_by_key(|stub| names.iter().position(|name| *name == stub.name));
    stubs
}

/// The JVM result of a stub whose semantic result is `result`.
fn physical_stub_result(result: Ty, boxed: bool) -> Ty {
    if boxed {
        jvm_declared_ty(&Ty::nullable(result))
    } else {
        jvm_declared_ty(&result)
    }
}

/// The policy of the declaration a member of the builtin `owner` is, or overrides: `owner`'s own
/// row first, then each builtin supertype's, nearest first.
fn overridden_declaration_policy<T>(
    classpath: &Classpath,
    owner: TypeName,
    policy: impl Fn(TypeName) -> Option<T>,
) -> Option<T> {
    let mut pending = std::collections::VecDeque::from([owner]);
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop_front() {
        if !seen.insert(current) {
            continue;
        }
        if let Some(found) = policy(current) {
            return Some(found);
        }
        pending.extend(classpath.builtin_supertypes_name(current).iter_ids());
    }
    None
}

/// A collection parameter with each of its arguments star-projected, `Collection<*>` for
/// `Collection<E>`.
fn star_projected(parameter: Ty) -> Ty {
    let inner = parameter.non_null();
    let Some(classifier) = inner.kotlin_class_internal() else {
        return parameter;
    };
    let stars: Vec<Ty> = inner
        .type_args()
        .iter()
        .map(|_| Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any"))))
        .collect();
    let projected = Ty::obj_args_name(classifier, &stars);
    if parameter.is_nullable() {
        Ty::nullable(projected)
    } else {
        projected
    }
}

/// kotlinc's `liftStubMethodReturnType`: a mutable member whose result is itself a mutable
/// collection (`MutableCollection.iterator(): MutableIterator<E>`) is implemented by a member that
/// returns the read-only counterpart, as the read-only face's own member does.
fn read_only_result(result: Ty) -> Ty {
    let inner = result.non_null();
    let Some(classifier) = inner.kotlin_class_internal() else {
        return result;
    };
    let Some(read_only) = mapped_collection(classifier)
        .filter(|mapped| mapped.mutable)
        .and_then(|_| jvm_collection_to_kotlin_type_name(to_jvm_type_name(classifier)))
    else {
        return result;
    };
    let lifted = Ty::obj_args_name(read_only, inner.type_args());
    if result.is_nullable() {
        Ty::nullable(lifted)
    } else {
        lifted
    }
}

/// One superclass of the class, with what its stubs depend on.
struct Superclass {
    supertypes: Vec<TypeName>,
    /// The members it declares abstract (kotlinc's abstract-superclass filter).
    abstract_members: HashSet<(String, String)>,
    /// The members it implements, inherited by the class as non-abstract fake overrides, keyed in
    /// the class's substitution context.
    implemented_members: HashMap<(String, String), Implementation>,
}

/// A member that may implement a candidate, as the class reaches it.
#[derive(Clone)]
struct Implementation {
    /// Its result in the class's substitution context, compared with the candidate's.
    result: Ty,
    /// Its physical JVM parameters and result, which a bridge to it calls.
    parameters: Vec<Ty>,
    physical_result: Ty,
}

/// The applied classifier hierarchy for the override check's result subtyping, so that
/// `MutableListIterator<String>` is a `ListIterator<String>`: a builtin's generic supertypes from
/// `.kotlin_builtins`, a source classifier's checked applied hierarchy, and otherwise the erased
/// supertypes the backend publishes. Unknown classifiers prove no relationship.
struct AppliedHierarchy<'a> {
    ir: &'a IrFile,
    classpath: &'a Classpath,
    classifiers: &'a dyn BackendClassifierSource,
}

impl TypeOracle for AppliedHierarchy<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        let Some(class) = ty.kotlin_class_internal() else {
            return Vec::new();
        };
        let arguments = ty.non_null().type_args();
        let applied = |formals: Vec<String>, supertypes: Vec<Ty>| -> Vec<Ty> {
            if formals.len() != arguments.len() {
                return supertypes;
            }
            let bindings: HashMap<String, Ty> =
                formals.into_iter().zip(arguments.iter().copied()).collect();
            substituted(&supertypes, &bindings)
        };
        if let Some((formals, supertypes)) = self.classpath.builtin_class_gsig_name(class) {
            return applied(formals, supertypes);
        }
        if let (Some(hierarchy), Some(signature)) = (
            self.ir.classifier_hierarchies.get(&class),
            self.ir.class_signature_name(class),
        ) {
            let formals = signature
                .type_params
                .iter()
                .map(|parameter| parameter.semantic_name.clone())
                .collect();
            let supertypes = hierarchy
                .iter()
                .filter(|entry| entry.depth > 0)
                .map(|entry| entry.applied)
                .collect();
            return applied(formals, supertypes);
        }
        self.classifiers
            .classifier(class)
            .map(|fact| fact.supertypes.iter().copied().map(Ty::obj_name).collect())
            .unwrap_or_default()
    }

    fn type_param_variance(&self, internal: TypeName, index: usize) -> crate::types::TypeVariance {
        self.classifiers
            .classifier(internal)
            .and_then(|fact| fact.type_param_variances.get(index).copied())
            .unwrap_or_default()
    }
}

fn superclass_bindings(
    owner: TypeName,
    formals: Vec<String>,
    applied: Ty,
) -> Result<HashMap<String, Ty>, SkipReason> {
    let arguments = applied.non_null().type_args();
    if formals.len() != arguments.len() {
        return Err(SkipReason::ReadOnlyCollectionStubs(format!(
            "superclass {} declares {} type parameters but its checked application has {} arguments",
            owner.render(), formals.len(), arguments.len()
        )));
    }
    Ok(formals.into_iter().zip(arguments.iter().copied()).collect())
}

/// A source superclass's type parameters, captured ones included, bound to the class's checked
/// application of it.
fn source_superclass_bindings(
    ir: &IrFile,
    owner: TypeName,
    source: &IrClass,
    applied: Ty,
) -> Result<HashMap<String, Ty>, SkipReason> {
    let mut formals = match ir.class_signature_name(owner) {
        Some(signature) => signature
            .type_params
            .iter()
            .map(|parameter| parameter.semantic_name.clone())
            .collect::<Vec<_>>(),
        None if source.type_params.is_empty() => Vec::new(),
        None => {
            return Err(SkipReason::ReadOnlyCollectionStubs(format!(
                "missing checked generic superclass declaration {}",
                owner.render()
            )));
        }
    };
    formals.extend(source.captured_type_params.iter().cloned());
    superclass_bindings(owner, formals, applied)
}

fn substituted(parameters: &[Ty], bindings: &HashMap<String, Ty>) -> Vec<Ty> {
    parameters
        .iter()
        .map(|parameter| ty_subst(*parameter, bindings))
        .collect()
}

fn superclass_chain(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &dyn BackendClassifierSource,
    class: &IrClass,
    candidates: &[ReadOnlyStub],
) -> Result<Vec<Superclass>, SkipReason> {
    let relevant = |name: &str, count: usize| {
        candidates
            .iter()
            .any(|stub| stub.name == name && stub.parameters.len() == count)
    };
    let applied = |owner: TypeName| {
        ir.classifier_hierarchies
            .get(&class.fq_name_id())
            .and_then(|hierarchy| hierarchy.iter().find(|entry| entry.classifier == owner))
            .map(|entry| entry.applied)
            .ok_or_else(|| {
                SkipReason::ReadOnlyCollectionStubs(format!(
                    "missing checked applied superclass {}",
                    owner.render()
                ))
            })
    };
    let mut chain = Vec::new();
    let mut current = class.superclass;
    let mut seen = HashSet::default();
    let package = class.fq_name_id().namespace();
    let mut same_package_path = true;
    while current != wk::java_object() && current != wk::any() && seen.insert(current) {
        same_package_path &= current.namespace() == package;
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
                implemented_members: HashMap::default(),
            };
            for &function_id in &source.methods {
                let function = &ir.functions[function_id as usize];
                if function.is_static
                    || !inherited_along(ir.method_visibility(function_id), same_package_path)
                {
                    continue;
                }
                // Only a candidate's name and arity can implement or declare its member.
                if !relevant(&function.name, function.params.len()) {
                    continue;
                }
                // Common lowering records every class-parameter-dependent declaration here.
                // Without that record the function types are unchanged declaration types;
                // this is not recovery of a missing generic signature from erased parameters.
                let semantic = ir.member_semantic_sigs.get(&function_id);
                if function.body.is_some() {
                    // In the class's substitution context, so `Base<T>.set(e: T)` under
                    // `Base<String>` implements `set(String)`. The class calls it at its own
                    // physical descriptor.
                    let (parameters, result) = match semantic {
                        Some((parameters, result)) => {
                            let bindings =
                                source_superclass_bindings(ir, current, source, applied(current)?)?;
                            (
                                substituted(parameters, &bindings),
                                ty_subst(*result, &bindings),
                            )
                        }
                        None => (function.params.clone(), function.ret),
                    };
                    superclass
                        .implemented_members
                        .entry((function.name.clone(), parameter_descriptor(&parameters)))
                        .or_insert_with(|| Implementation {
                            result,
                            parameters: jvm_tys(&function.params),
                            physical_result: jvm_declared_ty(&function.ret),
                        });
                } else if source.is_abstract {
                    let bindings =
                        source_superclass_bindings(ir, current, source, applied(current)?)?;
                    let parameters = match semantic {
                        Some((parameters, _)) => parameters,
                        None => &function.params,
                    };
                    superclass.abstract_members.insert((
                        function.name.clone(),
                        parameter_descriptor(&substituted(parameters, &bindings)),
                    ));
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
            implemented_members: HashMap::default(),
        };
        for method in &info.methods {
            if method.access & (ACC_STATIC | ACC_BRIDGE | ACC_SYNTHETIC) != 0
                || method.name == "<init>"
                || !inherited_along(method.visibility(), same_package_path)
            {
                continue;
            }
            if is_kotlin && is_abstract && method.access & ACC_ABSTRACT != 0 {
                // Property accessors and unrelated abstract members cannot suppress these
                // function stubs. Only a candidate's physical method name is lookup input here.
                if !candidates.iter().any(|stub| stub.name == method.name) {
                    continue;
                }
                let missing = || {
                    SkipReason::ReadOnlyCollectionStubs(format!(
                        "missing exact semantic abstract declaration {}.{}{}",
                        current.render(),
                        method.name,
                        method.descriptor
                    ))
                };
                let declaration = info
                    .meta
                    .class_functions
                    .iter()
                    .find(|declaration| {
                        declaration.jvm_name == method.name
                            && declaration.jvm_desc == Some(method.descriptor.as_str())
                    })
                    .ok_or_else(missing)?;
                let signature = declaration.generic_sig.as_ref().ok_or_else(missing)?;
                let count = signature.params.len()
                    + usize::from(signature.receiver.is_some())
                    + usize::from(declaration.is_suspend());
                if !relevant(&method.name, count) {
                    continue;
                }
                // Metadata publishes own formals here. A captured-layout mismatch must be
                // reported explicitly, never truncated into a partial substitution.
                let bindings = superclass_bindings(
                    current,
                    info.meta.class_type_parameters.type_params().clone(),
                    applied(current)?,
                )?;
                superclass.abstract_members.insert((
                    method.name.clone(),
                    parameter_descriptor(
                        &signature
                            .receiver
                            .iter()
                            .chain(&signature.params)
                            .map(|parameter| ty_subst(*parameter, &bindings))
                            .collect::<Vec<_>>(),
                    ),
                ));
                continue;
            }
            // Concrete realizations (including generated stubs) and Java methods keep their
            // existing physical-member accounting, separate from Kotlin abstract declarations.
            let (Some(parameters), Some((physical, physical_result))) = (
                method.descriptor.split_once(')').map(|(params, _)| params),
                crate::jvm::physical_type::method_operand_tys(&method.descriptor),
            ) else {
                continue;
            };
            let raw_key = format!("{parameters})V");
            if method.access & ACC_ABSTRACT == 0 {
                // A member a Kotlin class declares implements its member with its metadata
                // signature, in the class's substitution context: `DBase<T>.set(e: T)` under
                // `DBase<String>` implements `set(String)`, and a non-generic
                // `set(e: String): Nothing` keeps its `Nothing` result, which its physical `Void`
                // does not tell. A generated member has no declaration and only its descriptor.
                // A suspend declaration's continuation is a real parameter its metadata signature
                // omits, so it keeps its physical key and implements no non-suspend member.
                let declaration = (is_kotlin
                    && candidates.iter().any(|stub| stub.name == method.name))
                .then(|| {
                    info.meta.class_functions.iter().find(|declaration| {
                        declaration.jvm_name == method.name
                            && declaration.jvm_desc == Some(method.descriptor.as_str())
                    })
                })
                .flatten()
                .filter(|declaration| !declaration.is_suspend())
                .and_then(|declaration| declaration.generic_sig.as_ref());
                let (key, result) = match declaration {
                    Some(signature) => {
                        let bindings = superclass_bindings(
                            current,
                            info.meta.class_type_parameters.type_params().clone(),
                            applied(current)?,
                        )?;
                        let declared: Vec<Ty> = signature
                            .receiver
                            .iter()
                            .chain(&signature.params)
                            .copied()
                            .collect();
                        (
                            parameter_descriptor(&substituted(&declared, &bindings)),
                            ty_subst(signature.ret, &bindings),
                        )
                    }
                    // A Java or generated method's result is its descriptor's.
                    None => (raw_key, physical_result),
                };
                superclass
                    .implemented_members
                    .entry((method.name.clone(), key))
                    .or_insert(Implementation {
                        result,
                        parameters: physical,
                        physical_result,
                    });
            } else if is_abstract {
                superclass
                    .abstract_members
                    .insert((method.name.clone(), raw_key));
            }
        }
        chain.push(superclass);
        let Some(next) = info.super_class else {
            break;
        };
        current = next;
    }
    Ok(chain)
}

/// Whether a member a superclass declares with `visibility` reaches the class, given whether every
/// class from the class up to that superclass shares the class's package. A private member is
/// never inherited, and a package-private one only along a path that stays in its package; only
/// an inherited member can implement a stub's member.
fn inherited_along(visibility: Visibility, same_package_path: bool) -> bool {
    match visibility {
        Visibility::Private => false,
        Visibility::PackagePrivate => same_package_path,
        Visibility::Public | Visibility::Internal | Visibility::Protected => true,
    }
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
/// Each key maps to the physical JVM parameters of the implementation the class reaches: its own
/// member first, then the nearest superclass's, then an interface default.
fn implemented_members(
    ir: &IrFile,
    class: &IrClass,
    chain: &[Superclass],
) -> HashMap<(String, String), Implementation> {
    let mut implemented: HashMap<(String, String), Implementation> = HashMap::default();
    for function in class
        .methods
        .iter()
        .map(|&function| &ir.functions[function as usize])
        .filter(|function| !function.is_static)
    {
        implemented
            .entry((
                function.name.clone(),
                parameter_descriptor(&function.params),
            ))
            .or_insert_with(|| Implementation {
                result: function.ret,
                parameters: jvm_tys(&function.params),
                physical_result: jvm_declared_ty(&function.ret),
            });
    }
    for superclass in chain {
        for (key, implementation) in &superclass.implemented_members {
            implemented
                .entry(key.clone())
                .or_insert_with(|| implementation.clone());
        }
    }
    for default in ir
        .inherited_defaults
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
    {
        if let crate::fir::InheritedMemberName::Function(name) = &default.name {
            implemented
                .entry((
                    name.to_string(),
                    parameter_descriptor(&default.applied_parameters),
                ))
                .or_insert_with(|| Implementation {
                    result: default.applied_result,
                    parameters: jvm_tys(&default.applied_parameters),
                    physical_result: jvm_declared_ty(&default.applied_result),
                });
        }
    }
    implemented
}

/// A member's JVM parameter shape, keyed with a `void` result so that only the parameters compare.
fn parameter_descriptor(parameters: &[Ty]) -> String {
    method_descriptor(&jvm_tys(parameters), Ty::Unit)
}
