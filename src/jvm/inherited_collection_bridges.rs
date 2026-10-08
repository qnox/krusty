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
}

impl InheritedCollectionBridges {
    pub(super) fn for_class(&self, class: TypeName) -> &[InheritedCollectionBridge] {
        self.by_class.get(&class).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[derive(Clone, Debug)]
pub(super) struct InheritedCollectionBridge {
    pub(super) name: String,
    pub(super) parameters: Vec<Ty>,
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
        result_cast: Option<String>,
        signature: Option<String>,
    },
    /// Final bridge under the physical Java name.
    Checked {
        synthetic: bool,
        checks: Vec<InheritedCollectionCheck>,
        failure: Option<CollectionBridgeFailure>,
        delegate_name: String,
        delegate_descriptor: String,
        cast_arguments: Vec<InheritedCollectionCheck>,
        signature: Option<String>,
    },
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
        let bridges = synthesize(ir, class_index, classpath, override_results);
        if !bridges.is_empty() {
            result
                .by_class
                .insert(ir.classes[class_index].fq_name_id(), bridges);
        }
    }
    result
}

fn synthesize(
    ir: &IrFile,
    class_index: usize,
    classpath: &Classpath,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> Vec<InheritedCollectionBridge> {
    let class = &ir.classes[class_index];
    if class.is_interface || class.is_annotation {
        return Vec::new();
    }
    let Some(super_info) = classpath.find_name(class.superclass) else {
        return Vec::new();
    };
    // A Kotlin superclass already publishes its final special bridges. Only the first Kotlin class
    // whose immediate superclass is Java materializes inherited fake overrides.
    if super_info.meta.class_kind.is_some() || class.superclass == crate::types::wk::java_object() {
        return Vec::new();
    }

    let Some(start) = direct_supertype(class) else {
        return Vec::new();
    };
    let Some(collection_face) = applied_collection_face(classpath, start) else {
        return Vec::new();
    };
    let specifications = builtin_bridge_members(classpath, collection_face);
    if specifications.is_empty() {
        return Vec::new();
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

    let mut bridges = Vec::new();
    for member in order_members(found) {
        bridges.extend(member_bridges(
            ir,
            class,
            classpath,
            override_results,
            member,
        ));
    }
    bridges
}

#[derive(Clone)]
struct BridgeMember {
    source_name: String,
    physical_name: String,
    descriptor: String,
    kind: MappedBuiltinMemberKind,
    parameters: Vec<Ty>,
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
) -> Vec<InheritedCollectionBridge> {
    let specification = &member.specification;
    let specialized_name = if specification.kind == MappedBuiltinMemberKind::Property {
        property_getter_name(&specification.source_name)
    } else {
        specification.source_name.clone()
    };
    let specialized_descriptor = descriptor(&specification.parameters, specification.result);
    let Some((substituted_parameters, substituted_result)) =
        substituted_java_method(&member.method, &member.class_bindings)
    else {
        return Vec::new();
    };
    let substituted_descriptor = descriptor(&substituted_parameters, substituted_result);
    if specialized_name == specification.physical_name
        && specialized_descriptor == substituted_descriptor
    {
        return Vec::new();
    }
    if ordinary_bridge_covers(
        class,
        &specialized_name,
        &specialized_descriptor,
        &specification.physical_name,
        &substituted_descriptor,
    ) {
        return Vec::new();
    }
    if declares(
        ir,
        class,
        override_results,
        &specialized_name,
        &specialized_descriptor,
    ) {
        return Vec::new();
    }
    if inherits_final(
        classpath,
        class.superclass,
        &specification.physical_name,
        &substituted_descriptor,
    ) {
        return Vec::new();
    }

    let property_signature = (specification.kind == MappedBuiltinMemberKind::Property)
        .then(|| property_signature(specification.result));
    let mut bridges = vec![InheritedCollectionBridge {
        name: specialized_name.clone(),
        parameters: specification.parameters.clone(),
        result: specification.result,
        body: InheritedCollectionBridgeBody::SuperDelegate {
            owner: class.superclass,
            name: specification.physical_name.clone(),
            descriptor: member.method.descriptor.clone(),
            result_cast: result_cast(&member.method.descriptor, specification.result),
            signature: property_signature.as_ref().map(|pair| pair.0.clone()),
        },
    }];

    let needs_erased_barrier =
        specification.policy.is_some() && member.method.descriptor != substituted_descriptor;
    if needs_erased_barrier {
        let Some((parameters, result)) =
            crate::jvm::jvm_libraries::parse_method_desc(&member.method.descriptor)
        else {
            return Vec::new();
        };
        bridges.push(checked_bridge(CheckedBridgeInput {
            specification,
            name: specification.physical_name.clone(),
            parameters,
            result,
            delegate_name: &specialized_name,
            delegate_descriptor: &specialized_descriptor,
            synthetic: true,
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
        synthetic: false,
        signature: property_signature.map(|pair| pair.1),
    }));
    bridges
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
    synthetic: bool,
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
        synthetic,
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
    InheritedCollectionBridge {
        name,
        parameters,
        result,
        body: InheritedCollectionBridgeBody::Checked {
            synthetic,
            checks,
            failure: specification.policy.map(|policy| policy.failure),
            delegate_name: delegate_name.to_string(),
            delegate_descriptor: delegate_descriptor.to_string(),
            cast_arguments,
            signature,
        },
    }
}

fn ordinary_bridge_covers(
    class: &IrClass,
    source_name: &str,
    source_descriptor: &str,
    physical_name: &str,
    physical_descriptor: &str,
) -> bool {
    class.bridges.iter().any(|bridge| {
        let bridge_descriptor = descriptor(&bridge.erased_params, bridge.erased_ret);
        (bridge.name == source_name && bridge_descriptor == source_descriptor)
            || (bridge.name == physical_name && bridge_descriptor == physical_descriptor)
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
                .map(|argument| signature_type(*argument, keep_parameters))
                .collect::<String>();
            format!("L{}<{}>;", owned_classfile_internal_name(name), arguments)
        }
        other => descriptor_of(other),
    }
}

fn reference_of(ty: Ty) -> Ty {
    let ty = ty.non_null();
    if let Some(boxed) = ty.jvm_boxed_ref() {
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

fn result_cast(java_descriptor: &str, specialized_result: Ty) -> Option<String> {
    let java_result = java_descriptor
        .rsplit_once(')')
        .map_or(java_descriptor, |(_, result)| result);
    let specialized = descriptor_of(specialized_result);
    (specialized != java_result && specialized_result.is_reference())
        .then(|| class_name(specialized_result))
}

fn is_instance_api(method: &MethodSig) -> bool {
    method.name != "<init>"
        && method.name != "<clinit>"
        && !method.is_static()
        && !method.is_private()
        && !method.is_compiler_generated()
        && (method.is_public() || method.is_protected())
}
