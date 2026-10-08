//! JVM bridge plans for collection members inherited from a Java superclass.
//!
//! Declaration shape comes from `.kotlin_builtins`, Java substitution comes from classfile generic
//! signatures, and the small non-derivable type-safe-barrier policy is keyed by the complete builtin
//! declaration identity. The resulting plans remain backend facts; common IR never carries owners,
//! descriptors, classfile signatures, or runtime type checks.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::ir::{IrClass, IrFile};
use crate::jvm::classpath::Classpath;
use crate::jvm::classreader::{ClassInfo, MethodSig};
use crate::jvm::mapped_builtin_declarations::{
    CollectionBridgeFailure, CollectionBridgePolicy, MappedBuiltinMemberKind,
};
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::jvm::names::{method_descriptor, owned_classfile_internal_name};
use crate::names::property_getter_name;
use crate::types::{ty_subst, ty_subst_all, Ty, TypeName};

#[derive(Default)]
pub(super) struct InheritedCollectionBridges {
    by_class: HashMap<TypeName, Vec<InheritedCollectionBridge>>,
    replaced_ordinary: HashSet<(TypeName, String, String)>,
}

impl InheritedCollectionBridges {
    pub(super) fn for_class(&self, class: TypeName) -> &[InheritedCollectionBridge] {
        self.by_class.get(&class).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(super) fn replaces_ordinary(&self, class: TypeName, name: &str, descriptor: &str) -> bool {
        self.replaced_ordinary
            .contains(&(class, name.to_string(), descriptor.to_string()))
    }
}

#[derive(Clone, Debug)]
pub(super) struct InheritedCollectionBridge {
    pub(super) name: String,
    pub(super) parameters: Vec<Ty>,
    pub(super) parameter_names: Vec<String>,
    pub(super) result: Ty,
    pub(super) body: InheritedCollectionBridgeBody,
}

#[derive(Clone, Debug)]
pub(super) enum InheritedCollectionBridgeBody {
    /// `ACC_PUBLIC|ACC_BRIDGE`: call the immediate superclass implementation.
    SuperDelegate {
        owner: TypeName,
        name: String,
        descriptor: String,
        delegate_result: Ty,
        signature: Option<String>,
    },
    /// A bridge that delegates to another entry on the generated class.
    Checked {
        access: InheritedCollectionBridgeAccess,
        checks: Vec<InheritedCollectionCheck>,
        failure: Option<CollectionBridgeFailure>,
        delegate_name: String,
        delegate_descriptor: String,
        delegate_result: Ty,
        cast_arguments: Vec<InheritedCollectionCheck>,
        signature: Option<String>,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) enum InheritedCollectionBridgeAccess {
    Final,
    FinalSynthetic,
    Synthetic,
}

#[derive(Clone, Debug)]
pub(super) struct InheritedCollectionCheck {
    pub(super) parameter: u16,
    pub(super) class_name: String,
    pub(super) nullable: bool,
}

/// Select every inherited-collection bridge after ordinary override bridges are known. This is a
/// read-only JVM representation pass over common IR.
pub(super) fn select(
    ir: &IrFile,
    classpath: &Classpath,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> InheritedCollectionBridges {
    let mut result = InheritedCollectionBridges::default();
    for class_index in 0..ir.classes.len() {
        if !crate::jvm::override_results::realizes_overrides(ir, class_index) {
            continue;
        }
        let selection = synthesize(ir, class_index, classpath, override_results);
        let class = ir.classes[class_index].fq_name_id();
        result.replaced_ordinary.extend(
            selection
                .replaced_ordinary
                .into_iter()
                .map(|(name, descriptor)| (class, name, descriptor)),
        );
        if !selection.bridges.is_empty() {
            result.by_class.insert(class, selection.bridges);
        }
    }
    result
}

#[derive(Default)]
struct BridgeSelection {
    bridges: Vec<InheritedCollectionBridge>,
    replaced_ordinary: Vec<(String, String)>,
}

struct ReplacedPropertyBridge {
    name: String,
    parameters: Vec<Ty>,
    result: Ty,
    descriptor: String,
}

fn synthesize(
    ir: &IrFile,
    class_index: usize,
    classpath: &Classpath,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> BridgeSelection {
    let class = &ir.classes[class_index];
    if class.is_interface || class.is_annotation {
        return BridgeSelection::default();
    }
    let Some(super_info) = classpath.find_name(class.superclass) else {
        return BridgeSelection::default();
    };
    // A Kotlin superclass already publishes its final special bridges. Only the first Kotlin class
    // whose immediate superclass is Java materializes inherited fake overrides.
    if super_info.meta.class_kind.is_some() || class.superclass == crate::types::wk::java_object() {
        return BridgeSelection::default();
    }

    let Some(start) = direct_supertype(class) else {
        return BridgeSelection::default();
    };
    let Some(collection_face) = applied_collection_face(classpath, start) else {
        return BridgeSelection::default();
    };
    let specifications = builtin_bridge_members(classpath, collection_face);
    if specifications.is_empty() {
        return BridgeSelection::default();
    }

    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for node in java_superclass_chain(classpath, start) {
        for method in &node.info.methods {
            if !is_instance_api(method) {
                continue;
            }
            let key = (method.name.clone(), method.descriptor.clone());
            if !seen.insert(key) {
                continue;
            }
            let Some(specification) = specifications.iter().find(|specification| {
                specification.physical_name == method.name
                    && specification.descriptor == method.descriptor
            }) else {
                continue;
            };
            found.push(FoundMember {
                specification: specification.clone(),
                method: method.clone(),
                class_bindings: node.bindings.clone(),
            });
        }
    }

    let mut selection = BridgeSelection::default();
    for member in order_members(found) {
        let member = member_bridges(ir, class, classpath, override_results, member);
        selection.bridges.extend(member.bridges);
        selection.replaced_ordinary.extend(member.replaced_ordinary);
    }
    selection
}

#[derive(Clone)]
struct BridgeMember {
    source_name: String,
    physical_name: String,
    descriptor: String,
    kind: MappedBuiltinMemberKind,
    parameters: Vec<Ty>,
    parameter_names: Vec<String>,
    result: Ty,
    policy: Option<CollectionBridgePolicy>,
}

#[derive(Clone)]
struct FoundMember {
    specification: BridgeMember,
    method: MethodSig,
    class_bindings: HashMap<String, Ty>,
}

#[derive(Clone)]
struct AppliedClass {
    info: Arc<ClassInfo>,
    bindings: HashMap<String, Ty>,
}

fn direct_supertype(class: &IrClass) -> Option<Ty> {
    class
        .supertypes
        .iter()
        .copied()
        .find(|ty| ty.non_null().obj_internal() == Some(class.superclass))
}

/// The first mapped collection interface reached through the actual Java generic hierarchy. The
/// central JVM↔Kotlin class map identifies the mutable semantic face; no collection spelling is
/// reconstructed here.
fn applied_collection_face(classpath: &Classpath, start: Ty) -> Option<Ty> {
    let mut pending = VecDeque::from([start]);
    let mut seen = HashSet::new();
    while let Some(applied) = pending.pop_front() {
        let owner = applied.non_null().obj_internal()?;
        let jvm_owner = crate::jvm::jvm_class_map::to_jvm_type_name(owner);
        if !seen.insert(jvm_owner) {
            continue;
        }
        if let Some(kotlin) =
            crate::jvm::jvm_class_map::jvm_collection_to_kotlin_mutable_type_name(jvm_owner)
        {
            return Some(Ty::obj_args_name(kotlin, applied.non_null().type_args()));
        }
        let Some(info) = classpath.find_name(jvm_owner) else {
            continue;
        };
        pending.extend(applied_supertypes(&info, applied));
    }
    None
}

fn java_superclass_chain(classpath: &Classpath, start: Ty) -> Vec<AppliedClass> {
    let mut chain = Vec::new();
    let mut applied = start;
    for _ in 0..64 {
        let Some(owner) = applied.non_null().obj_internal() else {
            break;
        };
        if owner == crate::types::wk::java_object() {
            break;
        }
        let Some(info) = classpath.find_name(owner) else {
            break;
        };
        let bindings = class_bindings(&info, applied);
        let superclass = info.super_class;
        let next = superclass.and_then(|superclass| {
            applied_supertypes(&info, applied)
                .into_iter()
                .find(|ty| ty.non_null().obj_internal() == Some(superclass))
        });
        chain.push(AppliedClass { info, bindings });
        let Some(next) = next else { break };
        applied = next;
    }
    chain
}

fn class_bindings(info: &ClassInfo, applied: Ty) -> HashMap<String, Ty> {
    let formals = info
        .signature
        .as_deref()
        .and_then(crate::jvm::jvm_libraries::parse_class_gsig)
        .map(|(formals, _, _)| formals)
        .unwrap_or_default();
    formals
        .into_iter()
        .zip(applied.non_null().type_args().iter().copied())
        .collect()
}

fn applied_supertypes(info: &ClassInfo, applied: Ty) -> Vec<Ty> {
    if let Some((formals, _, supertypes)) = info
        .signature
        .as_deref()
        .and_then(crate::jvm::jvm_libraries::parse_class_gsig)
    {
        let bindings: HashMap<String, Ty> = formals
            .into_iter()
            .zip(applied.non_null().type_args().iter().copied())
            .collect();
        return ty_subst_all(&supertypes, &bindings);
    }
    info.super_class
        .into_iter()
        .chain(info.interfaces.iter_ids())
        .map(Ty::obj_name)
        .collect()
}

fn builtin_bridge_members(classpath: &Classpath, start: Ty) -> Vec<BridgeMember> {
    let mut members = Vec::new();
    let mut pending = VecDeque::from([start]);
    let mut seen_classes = HashSet::new();
    let mut seen_members = HashSet::new();
    while let Some(applied) = pending.pop_front() {
        let Some(owner) = applied.non_null().obj_internal() else {
            continue;
        };
        if !seen_classes.insert(owner) {
            continue;
        }
        let Some((formals, supertypes)) = classpath.builtin_class_gsig_name(owner) else {
            continue;
        };
        let bindings: HashMap<String, Ty> = formals
            .into_iter()
            .zip(applied.non_null().type_args().iter().copied())
            .collect();
        for declaration in classpath.decoded_builtin_members_name(owner) {
            let policy = classpath.collection_bridge_policy_name(
                declaration.declaration_owner,
                &declaration.source_name,
                &declaration.descriptor,
                declaration.kind,
            );
            let mapped = declaration.physical_name != declaration.source_name
                || declaration.kind == MappedBuiltinMemberKind::Property;
            if policy.is_none() && !mapped {
                continue;
            }
            let key = (
                declaration.source_name.clone(),
                declaration.descriptor.clone(),
                declaration.kind,
            );
            if !seen_members.insert(key) {
                continue;
            }
            members.push(BridgeMember {
                source_name: declaration.source_name,
                physical_name: declaration.physical_name,
                descriptor: declaration.descriptor,
                kind: declaration.kind,
                parameters: declaration
                    .generic_sig
                    .params
                    .iter()
                    .map(|ty| specialize_generic_slot(*ty, &bindings))
                    .collect(),
                parameter_names: declaration.parameter_names,
                result: specialize_generic_slot(declaration.generic_sig.ret, &bindings),
                policy,
            });
        }
        pending.extend(
            ty_subst_all(&supertypes, &bindings)
                .into_iter()
                .filter(|ty| ty.non_null().obj_internal().is_some()),
        );
    }
    members
}

fn specialize_generic_slot(declared: Ty, bindings: &HashMap<String, Ty>) -> Ty {
    let substituted = ty_subst(declared, bindings);
    if declared
        .non_null()
        .ty_param_name()
        .is_some_and(|name| bindings.contains_key(name))
    {
        reference_of(substituted)
    } else {
        substituted
    }
}

/// kotlinc groups overloads of one Kotlin name together, then emits renamed physical members last.
fn order_members(found: Vec<FoundMember>) -> Vec<FoundMember> {
    let mut grouped = Vec::new();
    let mut used = vec![false; found.len()];
    for index in 0..found.len() {
        if used[index] {
            continue;
        }
        used[index] = true;
        grouped.push(found[index].clone());
        let name = &found[index].specification.source_name;
        for later in index + 1..found.len() {
            if !used[later] && found[later].specification.source_name == *name {
                used[later] = true;
                grouped.push(found[later].clone());
            }
        }
    }
    let (mut direct, renamed): (Vec<_>, Vec<_>) = grouped
        .into_iter()
        .partition(|member| member.specification.physical_name == member.specification.source_name);
    direct.extend(renamed);
    direct
}

fn member_bridges(
    ir: &IrFile,
    class: &IrClass,
    classpath: &Classpath,
    override_results: &crate::jvm::override_results::OverrideResults,
    member: FoundMember,
) -> BridgeSelection {
    let specification = &member.specification;
    let specialized_name = if specification.kind == MappedBuiltinMemberKind::Property {
        property_getter_name(&specification.source_name)
    } else {
        specification.source_name.clone()
    };
    let Some((substituted_parameters, substituted_result)) =
        substituted_java_method(&member.method, &member.class_bindings)
    else {
        return BridgeSelection::default();
    };
    let Some((_, super_result)) =
        crate::jvm::jvm_libraries::parse_method_desc(&member.method.descriptor)
    else {
        return BridgeSelection::default();
    };
    let substituted_descriptor = descriptor(&substituted_parameters, substituted_result);
    let replaced_ordinary = replaceable_inherited_property_bridge(
        class,
        &specialized_name,
        &specification.physical_name,
    );
    // A generic property override has an erased Object bridge, but kotlinc's inherited entry uses
    // the boxed specialization (`Integer`) between that bridge and the primitive Java method.
    let specialized_result = if replaced_ordinary.as_ref().is_some_and(|bridge| {
        jvm_declared_ty(&bridge.result).is_reference()
            && jvm_declared_ty(&specification.result).is_jvm_scalar()
    }) {
        Ty::nullable(specification.result)
    } else {
        specification.result
    };
    let specialized_descriptor = descriptor(&specification.parameters, specialized_result);
    if specialized_name == specification.physical_name
        && specialized_descriptor == substituted_descriptor
    {
        return BridgeSelection::default();
    }
    if ordinary_bridge_covers_source(class, &specification.physical_name, &substituted_descriptor)
        || (ordinary_bridge_covers_source(class, &specialized_name, &specialized_descriptor)
            && replaced_ordinary.is_none())
    {
        return BridgeSelection::default();
    }
    if declares(
        ir,
        class,
        override_results,
        &specialized_name,
        &specialized_descriptor,
    ) {
        return BridgeSelection::default();
    }
    if inherits_final(
        classpath,
        class.superclass,
        &specification.physical_name,
        &substituted_descriptor,
    ) {
        return BridgeSelection::default();
    }

    let specialized_property_signature = (specification.kind == MappedBuiltinMemberKind::Property)
        .then(|| property_signature(specialized_result));
    let physical_property_signature = (specification.kind == MappedBuiltinMemberKind::Property)
        .then(|| property_signature(specification.result));
    let mut bridges = vec![InheritedCollectionBridge {
        name: specialized_name.clone(),
        parameters: specification.parameters.clone(),
        parameter_names: (0..specification.parameters.len())
            .map(crate::jvm::parameter_names::inherited_collection_specialized_local)
            .collect(),
        result: specialized_result,
        body: InheritedCollectionBridgeBody::SuperDelegate {
            owner: class.superclass,
            name: specification.physical_name.clone(),
            descriptor: member.method.descriptor.clone(),
            delegate_result: super_result,
            signature: specialized_property_signature.map(|pair| pair.0),
        },
    }];

    let needs_erased_barrier =
        specification.policy.is_some() && member.method.descriptor != substituted_descriptor;
    if needs_erased_barrier {
        let Some((parameters, result)) =
            crate::jvm::jvm_libraries::parse_method_desc(&member.method.descriptor)
        else {
            return BridgeSelection::default();
        };
        bridges.push(checked_bridge(CheckedBridgeInput {
            specification,
            name: specification.physical_name.clone(),
            parameters,
            result,
            delegate_name: &specialized_name,
            delegate_descriptor: &specialized_descriptor,
            delegate_result: specialized_result,
            access: InheritedCollectionBridgeAccess::FinalSynthetic,
            signature: None,
        }));
    }
    bridges.push(checked_bridge(CheckedBridgeInput {
        specification,
        name: specification.physical_name.clone(),
        parameters: substituted_parameters,
        result: substituted_result,
        delegate_name: &specialized_name,
        delegate_descriptor: &specialized_descriptor,
        delegate_result: specialized_result,
        access: InheritedCollectionBridgeAccess::Final,
        signature: physical_property_signature.map(|pair| pair.1),
    }));
    if let Some(ordinary) = &replaced_ordinary {
        if ordinary.descriptor != specialized_descriptor {
            bridges.push(checked_bridge(CheckedBridgeInput {
                specification,
                name: ordinary.name.clone(),
                parameters: ordinary.parameters.clone(),
                result: ordinary.result,
                delegate_name: &specialized_name,
                delegate_descriptor: &specialized_descriptor,
                delegate_result: specialized_result,
                access: InheritedCollectionBridgeAccess::Synthetic,
                signature: None,
            }));
        }
    }
    BridgeSelection {
        bridges,
        replaced_ordinary: replaced_ordinary
            .map(|bridge| (bridge.name, bridge.descriptor))
            .into_iter()
            .collect(),
    }
}

fn substituted_java_method(
    method: &MethodSig,
    bindings: &HashMap<String, Ty>,
) -> Option<(Vec<Ty>, Ty)> {
    let Some(raw_signature) = method.signature.as_deref() else {
        return crate::jvm::jvm_libraries::parse_method_desc(&method.descriptor);
    };
    let signature = crate::jvm::jvm_libraries::parse_method_gsig(raw_signature)?;
    let parameters = signature
        .params
        .iter()
        .map(|ty| specialize_generic_slot(*ty, bindings))
        .collect();
    let result = specialize_generic_slot(signature.ret, bindings);
    Some((parameters, result))
}

struct CheckedBridgeInput<'a> {
    specification: &'a BridgeMember,
    name: String,
    parameters: Vec<Ty>,
    result: Ty,
    delegate_name: &'a str,
    delegate_descriptor: &'a str,
    delegate_result: Ty,
    access: InheritedCollectionBridgeAccess,
    signature: Option<String>,
}

fn checked_bridge(input: CheckedBridgeInput<'_>) -> InheritedCollectionBridge {
    let CheckedBridgeInput {
        specification,
        name,
        parameters,
        result,
        delegate_name,
        delegate_descriptor,
        delegate_result,
        access,
        signature,
    } = input;
    let checks = specification
        .policy
        .into_iter()
        .flat_map(|policy| policy.checked_parameters.iter().copied())
        .filter_map(|parameter| {
            let semantic = specification
                .parameters
                .get(usize::from(parameter))
                .copied()?;
            Some(InheritedCollectionCheck {
                parameter,
                class_name: class_name(reference_of(semantic)),
                nullable: semantic.admits_null(),
            })
        })
        .collect();
    let cast_arguments = parameters
        .iter()
        .zip(&specification.parameters)
        .enumerate()
        .filter(|(_, (physical, semantic))| {
            descriptor_of(**physical) != descriptor_of(**semantic) && semantic.is_reference()
        })
        .map(|(index, (_, semantic))| InheritedCollectionCheck {
            parameter: u16::try_from(index).expect("bridge parameter count fits u16"),
            class_name: class_name(reference_of(*semantic)),
            nullable: false,
        })
        .collect();
    assert_eq!(
        parameters.len(),
        specification.parameter_names.len(),
        "an inherited collection bridge retains every metadata parameter identity"
    );
    InheritedCollectionBridge {
        name,
        parameters,
        parameter_names: specification.parameter_names.clone(),
        result,
        body: InheritedCollectionBridgeBody::Checked {
            access,
            checks,
            failure: specification.policy.map(|policy| policy.failure),
            delegate_name: delegate_name.to_string(),
            delegate_descriptor: delegate_descriptor.to_string(),
            delegate_result,
            cast_arguments,
            signature,
        },
    }
}

fn ordinary_bridge_covers_source(class: &IrClass, name: &str, expected_descriptor: &str) -> bool {
    class.bridges.iter().any(|bridge| {
        bridge.name == name
            && descriptor(&bridge.erased_params, bridge.erased_ret) == expected_descriptor
    })
}

/// A property satisfied only by an inherited Java method needs kotlinc's inherited pair instead
/// of the ordinary synthetic override bridge. The pair calls the Java superclass with
/// `invokespecial`, then publishes the final mapped-name bridge back to that entry.
fn replaceable_inherited_property_bridge(
    class: &IrClass,
    source_name: &str,
    physical_name: &str,
) -> Option<ReplacedPropertyBridge> {
    class.bridges.iter().find_map(|bridge| {
        (bridge.kind == crate::ir::BridgeKind::PropertyGetter
            && bridge.target_function.is_none()
            && bridge.property_implementation.is_none()
            && bridge.name == source_name
            && bridge.target_name.as_deref() == Some(physical_name)
            && bridge.erased_params.is_empty())
        .then(|| ReplacedPropertyBridge {
            name: bridge.name.clone(),
            parameters: bridge.erased_params.clone(),
            result: bridge.erased_ret,
            descriptor: descriptor(&bridge.erased_params, bridge.erased_ret),
        })
    })
}

fn declares(
    ir: &IrFile,
    class: &IrClass,
    override_results: &crate::jvm::override_results::OverrideResults,
    name: &str,
    descriptor: &str,
) -> bool {
    class.methods.iter().any(|function| {
        let declaration = &ir.functions[*function as usize];
        declaration.name == name
            && crate::jvm::ir_emit::function_descriptor(ir, override_results, *function)
                == descriptor
    })
}

fn inherits_final(classpath: &Classpath, mut next: TypeName, name: &str, descriptor: &str) -> bool {
    for _ in 0..64 {
        if next == crate::types::wk::java_object() {
            return false;
        }
        let Some(info) = classpath.find_name(next) else {
            return false;
        };
        if info.methods.iter().any(|method| {
            method.is_final() && method.name == name && method.descriptor == descriptor
        }) {
            return true;
        }
        match info.super_class {
            Some(superclass) => next = superclass,
            None => return false,
        }
    }
    false
}

fn property_signature(result: Ty) -> (String, String) {
    (
        format!("(){}", signature_type(result, false)),
        format!("(){}", signature_type(result, true)),
    )
}

fn signature_type(ty: Ty, keep_parameters: bool) -> String {
    match ty.non_null() {
        Ty::TyParam(name, bound) => {
            if keep_parameters {
                format!("T{};", crate::types::type_parameter_source_name(name))
            } else {
                signature_type(*bound, false)
            }
        }
        Ty::DefinitelyNotNull(inner) => signature_type(*inner, keep_parameters),
        Ty::StarProjection(_) | Ty::OutProjection(_) | Ty::InProjection(_) => {
            "Ljava/lang/Object;".to_string()
        }
        Ty::Obj(name, arguments) if !arguments.is_empty() => {
            let arguments = arguments
                .iter()
                .map(|argument| signature_argument(*argument, keep_parameters))
                .collect::<String>();
            format!("L{}<{}>;", owned_classfile_internal_name(name), arguments)
        }
        other => descriptor_of(other),
    }
}

fn signature_argument(ty: Ty, keep_parameters: bool) -> String {
    match ty.non_null() {
        Ty::TyParam(..) if keep_parameters => signature_type(ty, true),
        Ty::StarProjection(_) | Ty::OutProjection(_) | Ty::InProjection(_) => {
            signature_type(ty, keep_parameters)
        }
        _ if ty.nullable_primitive().is_some() || ty.non_null().is_jvm_scalar() => {
            signature_type(reference_of(ty), keep_parameters)
        }
        _ => signature_type(ty, keep_parameters),
    }
}

fn reference_of(ty: Ty) -> Ty {
    let ty = ty.non_null();
    if let Some(boxed) = ty.boxed_ref() {
        return boxed;
    }
    match ty {
        Ty::String => Ty::obj("java/lang/String"),
        Ty::TyParam(_, bound) => reference_of(*bound),
        Ty::DefinitelyNotNull(inner) => reference_of(*inner),
        Ty::StarProjection(_) | Ty::OutProjection(_) | Ty::InProjection(_) => {
            Ty::obj("java/lang/Object")
        }
        Ty::Obj(name, arguments) => Ty::obj_args_name(name, arguments),
        _ => Ty::obj("java/lang/Object"),
    }
}

fn class_name(ty: Ty) -> String {
    ty.obj_internal()
        .map(owned_classfile_internal_name)
        .unwrap_or_else(|| "java/lang/Object".to_string())
}

fn descriptor(parameters: &[Ty], result: Ty) -> String {
    method_descriptor(&jvm_tys(parameters), jvm_declared_ty(&result))
}

fn descriptor_of(ty: Ty) -> String {
    crate::jvm::names::type_descriptor(jvm_declared_ty(&ty))
}

fn is_instance_api(method: &MethodSig) -> bool {
    method.name != "<init>"
        && method.name != "<clinit>"
        && !method.is_static()
        && !method.is_private()
        && !method.is_compiler_generated()
        && (method.is_public() || method.is_protected())
}
