//! kotlinc's materialization of a builtin member a class leaves abstract.
//!
//! A mapped builtin member whose JVM realization differs from its Kotlin one is reached from Java
//! under that realization: a renamed member (`Collection.size` is `size()` beside the Kotlin getter
//! `getSize()`, `Number.toInt` is `intValue()`, `MutableList.removeAt` is `remove(int)`) or a
//! type-safe one (`Collection.contains(element: E)` is `contains(Object)`). When a non-interface
//! class inherits such a member as an abstract fake override, kotlinc's bridge lowering
//! materializes it: an abstract method under its Kotlin JVM signature with the class's substituted
//! types (`abstract boolean contains(String)`, `abstract int getSize()`), followed by the final
//! special bridge under the JVM signature that calls it. A class below a superclass that reaches
//! the builtin inherits what that superclass already wrote, so only the first class writes them.
//! The plans remain backend facts; common IR never carries a fake override.

use crate::backend::{BackendClassifierSource, CheckedBackendClassifiers, SourceClassMembers};
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{IrAppliedClassifier, IrClass, IrFile};
use crate::jvm::backend::SkipReason;
use crate::jvm::classpath::mapped_builtin_realizations::DecodedBuiltinMember;
use crate::jvm::classpath::Classpath;
use crate::jvm::jvm_class_map::to_jvm_type_name;
use crate::jvm::mapped_builtin_declarations::{
    collection_bridge_policy_for_declaration, MappedBuiltinMemberKind,
};
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::jvm::names::method_descriptor;
use crate::jvm::read_only_collection_stubs::{
    builtin_extends, classifier_closure, overridden_declaration_policy, parameter_descriptor,
    stub_barrier, StubBarrier,
};
use crate::names::property_getter_name;
use crate::types::{ty_subst, Ty, TypeName, Visibility};

#[derive(Default)]
pub(super) struct AbstractBuiltinOverrides {
    by_class: HashMap<TypeName, Vec<AbstractBuiltinOverride>>,
}

impl AbstractBuiltinOverrides {
    pub(super) fn for_class(&self, class: TypeName) -> &[AbstractBuiltinOverride] {
        self.by_class.get(&class).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The `(name, descriptor)` of each final special bridge `class` writes, which its subclasses
    /// inherit and must not declare again.
    pub(super) fn final_bridges(&self, class: TypeName) -> Vec<(String, String)> {
        self.for_class(class)
            .iter()
            .flat_map(|member| {
                let parameters = member.physical_parameters();
                let result = member.physical_result();
                match &member.bridge {
                    SpecialBridge::Renamed { jvm_name, .. } => {
                        vec![(jvm_name.clone(), method_descriptor(&parameters, result))]
                    }
                    SpecialBridge::TypeSafe {
                        erased: (erased_parameters, erased_result),
                        ..
                    } => vec![
                        (
                            member.name.clone(),
                            method_descriptor(erased_parameters, *erased_result),
                        ),
                        (
                            member.name.clone(),
                            method_descriptor(erased_parameters, result),
                        ),
                    ],
                }
            })
            .collect()
    }
}

/// One materialized abstract member and the special bridge that reaches it.
#[derive(Clone, Debug)]
pub(super) struct AbstractBuiltinOverride {
    /// The Kotlin JVM name: the function's own name, or the property's getter.
    pub(super) name: String,
    /// The declaration's parameter types, substituted in the class's context.
    pub(super) parameters: Vec<Ty>,
    pub(super) parameter_names: Vec<String>,
    /// The declaration's result type, substituted in the class's context.
    pub(super) result: Ty,
    pub(super) bridge: SpecialBridge,
}

impl AbstractBuiltinOverride {
    pub(super) fn physical_parameters(&self) -> Vec<Ty> {
        jvm_tys(&self.parameters)
    }

    pub(super) fn physical_result(&self) -> Ty {
        jvm_declared_ty(&self.result)
    }
}

/// How Java reaches a materialized member.
#[derive(Clone, Debug)]
pub(super) enum SpecialBridge {
    /// A builtin with a different JVM name: the final bridge under that name takes the abstract
    /// member's own descriptor. When the declaration's erased descriptor differs from it
    /// (`Object remove(int)` beside `String removeAt(int)`), an ordinary bridge there calls the
    /// abstract member too.
    Renamed {
        jvm_name: String,
        erased: Option<(Vec<Ty>, Ty)>,
    },
    /// A type-safe declaration: the final bridge at its erased descriptor tests each checked
    /// argument before calling the abstract member.
    TypeSafe {
        barrier: StubBarrier,
        erased: (Vec<Ty>, Ty),
    },
}

/// Select the materialized abstract builtin members of every abstract class.
pub(super) fn select(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &CheckedBackendClassifiers<'_>,
) -> Result<AbstractBuiltinOverrides, SkipReason> {
    let mut result = AbstractBuiltinOverrides::default();
    for class_index in 0..ir.classes.len() {
        if !crate::jvm::override_results::realizes_overrides(ir, class_index) {
            continue;
        }
        let class = &ir.classes[class_index];
        // Only an abstract class can leave a member abstract; an interface's member stays its own.
        if class.is_interface || class.is_annotation || !(class.is_abstract || class.is_sealed) {
            continue;
        }
        let members = class_overrides(ir, classpath, classifiers, class)?;
        if !members.is_empty() {
            result.by_class.insert(class.fq_name_id(), members);
        }
    }
    Ok(result)
}

fn class_overrides(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &CheckedBackendClassifiers<'_>,
    class: &IrClass,
) -> Result<Vec<AbstractBuiltinOverride>, SkipReason> {
    let Some(hierarchy) = ir.classifier_hierarchies.get(&class.fq_name_id()) else {
        return Ok(Vec::new());
    };
    let through_superclass = superclass_classifiers(ir, classpath, classifiers, class);
    let builtins: Vec<&IrAppliedClassifier> = hierarchy
        .iter()
        .filter(|entry| {
            entry.depth > 0
                && classpath
                    .builtin_class_gsig_name(entry.classifier)
                    .is_some()
                && !through_superclass.contains(&to_jvm_type_name(entry.classifier))
        })
        .collect();
    // The most specific builtins: each one's member order already holds what it inherits.
    let roots = builtins.iter().filter(|entry| {
        !builtins.iter().any(|other| {
            other.classifier != entry.classifier
                && builtin_extends(classpath, other.classifier, entry.classifier)
        })
    });
    let mut seen: Vec<DecodedBuiltinMember> = Vec::new();
    for root in roots {
        for member in member_order(classpath, root.classifier) {
            if !seen.iter().any(|known| same_declaration(known, &member)) {
                seen.push(member);
            }
        }
    }
    let candidates = seen
        .into_iter()
        .filter(|member| member.is_abstract)
        .filter_map(|member| {
            let applied = hierarchy
                .iter()
                .find(|entry| entry.classifier == member.declaration_owner)?
                .applied;
            materialization(classpath, &member, applied)
        });
    let ancestors = superclass_chain(ir, classifiers, classpath, class)?;
    let mut members = Vec::new();
    for candidate in candidates {
        if !implemented(ir, &ancestors, class, &candidate)? {
            members.push(candidate);
        }
    }
    Ok(members)
}

/// The classifiers a non-builtin superclass reaches, by JVM name: that superclass, or one above
/// it, already wrote whatever these builtins' members need.
fn superclass_classifiers(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &dyn BackendClassifierSource,
    class: &IrClass,
) -> HashSet<TypeName> {
    let superclass = class.superclass;
    if classpath.builtin_class_gsig_name(superclass).is_some() {
        return HashSet::default();
    }
    let closure = match ir.classifier_hierarchies.get(&superclass) {
        Some(hierarchy) => hierarchy.iter().map(|entry| entry.classifier).collect(),
        None => classifier_closure(classifiers, superclass),
    };
    closure
        .into_iter()
        .chain(std::iter::once(superclass))
        .map(to_jvm_type_name)
        .collect()
}

/// kotlinc's member order of a builtin: its declared functions, its declared properties, then the
/// fake overrides of what it inherits without redeclaring, properties before functions.
fn member_order(classpath: &Classpath, owner: TypeName) -> Vec<DecodedBuiltinMember> {
    let (functions, properties): (Vec<_>, Vec<_>) = classpath
        .decoded_builtin_members_name(owner)
        .into_iter()
        .partition(|member| member.kind == MappedBuiltinMemberKind::Function);
    let own: Vec<DecodedBuiltinMember> = functions.into_iter().chain(properties).collect();
    let mut inherited: Vec<DecodedBuiltinMember> = Vec::new();
    for supertype in classpath.builtin_supertypes_name(owner).iter_ids() {
        for member in member_order(classpath, supertype) {
            if !own
                .iter()
                .chain(&inherited)
                .any(|known| same_declaration(known, &member))
            {
                inherited.push(member);
            }
        }
    }
    let (inherited_functions, inherited_properties): (Vec<_>, Vec<_>) = inherited
        .into_iter()
        .partition(|member| member.kind == MappedBuiltinMemberKind::Function);
    own.into_iter()
        .chain(inherited_properties)
        .chain(inherited_functions)
        .collect()
}

/// Whether two builtin members are one declaration and its override: the same kind, name and
/// erased signature.
fn same_declaration(left: &DecodedBuiltinMember, right: &DecodedBuiltinMember) -> bool {
    left.kind == right.kind
        && left.source_name == right.source_name
        && left.descriptor == right.descriptor
}

/// The materialization of an abstract builtin member in the substitution context `applied` of its
/// declaring builtin, or `None` when Java reaches it at its Kotlin signature already.
fn materialization(
    classpath: &Classpath,
    member: &DecodedBuiltinMember,
    applied: Ty,
) -> Option<AbstractBuiltinOverride> {
    let (formals, _) = classpath.builtin_class_gsig_name(member.declaration_owner)?;
    let arguments = applied.non_null().type_args();
    if formals.len() != arguments.len() {
        return None;
    }
    let bindings: HashMap<String, Ty> = formals
        .iter()
        .cloned()
        .zip(arguments.iter().copied())
        .collect();
    let erasures: HashMap<String, Ty> = formals
        .iter()
        .map(|formal| (formal.clone(), Ty::nullable(Ty::obj("kotlin/Any"))))
        .collect();
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
    let erased_descriptor = method_descriptor(&erased_parameters, erased_result);
    let name = match member.kind {
        MappedBuiltinMemberKind::Function => member.source_name.clone(),
        MappedBuiltinMemberKind::Property => property_getter_name(&member.source_name),
    };
    let own_parameters = jvm_tys(&parameters);
    let own_result = jvm_declared_ty(&result);
    let bridge = if member.physical_name != name {
        let own_descriptor = method_descriptor(&own_parameters, own_result);
        SpecialBridge::Renamed {
            jvm_name: member.physical_name.clone(),
            erased: (erased_descriptor != own_descriptor)
                .then_some((erased_parameters, erased_result)),
        }
    } else {
        let policy = overridden_declaration_policy(classpath, member.declaration_owner, |owner| {
            collection_bridge_policy_for_declaration(
                owner,
                &member.source_name,
                &erased_descriptor,
                member.kind,
            )
        })?;
        let barrier = stub_barrier(policy)?;
        // A type parameter that erases like the declaration's needs no bridge (`contains(T)`).
        if parameter_descriptor(&parameters) == parameter_descriptor(&erased_parameters) {
            return None;
        }
        SpecialBridge::TypeSafe {
            barrier,
            erased: (erased_parameters, erased_result),
        }
    };
    Some(AbstractBuiltinOverride {
        name,
        parameters,
        parameter_names: member.parameter_names.clone(),
        result,
        bridge,
    })
}

/// The one source that owns a superclass's declaration.
enum Declaring<'a> {
    /// A named source classifier of the module, whichever file declares it.
    Module(&'a SourceClassMembers),
    /// A body-local class of the active file.
    File(&'a IrClass),
    /// A dependency class file.
    Dependency(std::sync::Arc<crate::jvm::classreader::ClassInfo>),
}

/// A class of the superclass chain and its application in the materializing class.
struct Ancestor<'a> {
    classifier: TypeName,
    applied: Option<Ty>,
    declaring: Declaring<'a>,
}

/// The superclass chain above `class`, each class read from the source that declares it: the
/// module's declaration record for a named source class, so the answer is the same whichever
/// file declares it.
fn superclass_chain<'a>(
    ir: &'a IrFile,
    classifiers: &'a CheckedBackendClassifiers<'_>,
    classpath: &Classpath,
    class: &IrClass,
) -> Result<Vec<Ancestor<'a>>, SkipReason> {
    let hierarchy = ir.classifier_hierarchies.get(&class.fq_name_id());
    let mut chain = Vec::new();
    let mut seen = HashSet::default();
    let mut current = Some(class.superclass);
    while let Some(classifier) = current
        .filter(|&classifier| {
            classifier != crate::types::wk::java_object() && classifier != crate::types::wk::any()
        })
        .filter(|&classifier| seen.insert(classifier))
    {
        let declaring = if let Some(members) = classifiers.module().source_class_members(classifier)
        {
            Declaring::Module(members)
        } else {
            match ir
                .classes
                .iter()
                .find(|class| class.fq_name_id() == classifier)
            {
                Some(source) if source.enclosure.is_some() => Declaring::File(source),
                Some(_) => {
                    return Err(SkipReason::AbstractBuiltinOverrides(format!(
                        "missing module declaration record for source superclass {}",
                        classifier.render()
                    )))
                }
                None => match classpath.find_name(classifier) {
                    Some(info) => Declaring::Dependency(info),
                    None => break,
                },
            }
        };
        current = match &declaring {
            Declaring::Module(members) => members.superclass,
            Declaring::File(source) => Some(source.superclass),
            Declaring::Dependency(info) => info.super_class,
        };
        let applied = hierarchy
            .and_then(|hierarchy| {
                hierarchy
                    .iter()
                    .find(|entry| entry.classifier == classifier)
            })
            .map(|entry| entry.applied);
        chain.push(Ancestor {
            classifier,
            applied,
            declaring,
        });
    }
    Ok(chain)
}

/// kotlinc's effective-override check for a materialization: a member of the class, of a
/// superclass, or an inherited interface default with the same JVM name and parameters already
/// stands for it, abstract or not.
fn implemented(
    ir: &IrFile,
    ancestors: &[Ancestor<'_>],
    class: &IrClass,
    candidate: &AbstractBuiltinOverride,
) -> Result<bool, SkipReason> {
    let parameters = parameter_descriptor(&candidate.parameters);
    let declares = |class: &IrClass| {
        class.methods.iter().any(|&function| {
            let function = &ir.functions[function as usize];
            !function.is_static
                && function.name == candidate.name
                && parameter_descriptor(&function.params) == parameters
        }) || (candidate.parameters.is_empty()
            && class
                .properties
                .iter()
                .any(|property| property_getter_name(&property.name) == candidate.name))
    };
    if declares(class) {
        return Ok(true);
    }
    let defaulted = ir
        .inherited_defaults
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
        .any(|default| {
            let name =
                crate::jvm::inherited_default_realization::inherited_member_declared_name(default);
            name == candidate.name
                && parameter_descriptor(&default.applied_parameters) == parameters
        });
    if defaulted {
        return Ok(true);
    }
    let jvm_parameters = &parameters[..parameters.len() - 1];
    for ancestor in ancestors {
        let found = match &ancestor.declaring {
            Declaring::Module(members) => {
                module_declares(ancestor.classifier, members, ancestor.applied, candidate)?
            }
            Declaring::File(source) => declares(source),
            Declaring::Dependency(info) => info.methods.iter().any(|method| {
                !method.is_static()
                    && !method.is_private()
                    && method.name == candidate.name
                    && method
                        .descriptor
                        .split_once(')')
                        .is_some_and(|(own, _)| format!("{own})") == jvm_parameters)
            }),
        };
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a named source superclass declares a member that stands for `candidate`: a visible
/// non-extension function whose parameters, applied in the materializing class, erase to the
/// candidate's (`fun contains(element: T)` of `Base<T>` under `Base<String>`), or, for a getter,
/// a visible property without receivers.
fn module_declares(
    owner: TypeName,
    members: &SourceClassMembers,
    applied: Option<Ty>,
    candidate: &AbstractBuiltinOverride,
) -> Result<bool, SkipReason> {
    let refuse = |what: &str| {
        SkipReason::AbstractBuiltinOverrides(format!(
            "missing checked {what} of {}.{}",
            owner.render(),
            candidate.name
        ))
    };
    let formals = members
        .type_parameters
        .as_deref()
        .ok_or_else(|| refuse("type parameters"))?;
    let bindings: HashMap<String, Ty> = formals
        .iter()
        .map(|formal| formal.to_string())
        .zip(
            applied
                .map(|applied| applied.non_null().type_args().to_vec())
                .unwrap_or_default(),
        )
        .collect();
    let parameters = parameter_descriptor(&candidate.parameters);
    for function in members.functions.iter() {
        if *function.name != *candidate.name || function.visibility == Visibility::Private {
            continue;
        }
        let signature = function
            .signature
            .as_ref()
            .ok_or_else(|| refuse("signature"))?;
        if signature.extension {
            continue;
        }
        let applied_parameters: Vec<Ty> = signature
            .parameters
            .iter()
            .map(|&parameter| ty_subst(parameter, &bindings))
            .collect();
        if parameter_descriptor(&applied_parameters) == parameters {
            return Ok(true);
        }
    }
    if !candidate.parameters.is_empty() {
        return Ok(false);
    }
    for property in members.properties.iter() {
        if property.visibility == Visibility::Private
            || property_getter_name(&property.name) != candidate.name
        {
            continue;
        }
        let receivers = property
            .receivers
            .ok_or_else(|| refuse("property receivers"))?;
        if !receivers.extension && receivers.context_parameters == 0 {
            return Ok(true);
        }
    }
    Ok(false)
}
