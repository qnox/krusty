//! The `toArray` members of a collection class (kotlinc's `ToArrayLowering`).
//!
//! Kotlin's `Collection` declares no `toArray`, but `java.util.Collection` does, so kotlinc gives
//! every class that implements a collection face, read-only or mutable and abstract or not, a
//! public `<T> T[] toArray(T[])` and a public `Object[] toArray()` that delegate to
//! `kotlin.jvm.internal.CollectionToArray`. A class whose superclass chain already has a collection
//! class inherits that superclass's members and gets none. A member that matches one of the two
//! shapes, declared by the class or inherited from any non-collection supertype, suppresses that one.
//! The shapes are the declaration's, never the name alone: `toArray(): Array<…>` with no type
//! parameters, and `<T> toArray(Array<T>): Array<T>`.
//!
//! Each supertype's members come from the one source that owns its declaration: the module's frozen
//! records for a named source classifier, whichever file declares it; the active file's IR for a
//! body-local class, which only that file can name; and the classpath for a dependency. Only a
//! member the class inherits counts: never a private one, and a package-private Java one only along
//! a superclass path that stays in the class's package.

use crate::backend::{
    BackendClassifierSource, BackendModuleFacts, CheckedBackendClassifiers, SourceClassMembers,
};
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{IrClass, IrFile};
use crate::jvm::backend::SkipReason;
use crate::jvm::classpath::Classpath;
use crate::jvm::classreader::ClassInfo;
use crate::jvm::jvm_class_map::{
    is_mapped_collection_face, jvm_collection_to_kotlin_type_name, mapped_collection,
    to_jvm_type_name,
};
use crate::jvm::read_only_collection_stubs::inherited_along;
use crate::types::{wk, CollectionKind, Ty, TyKind, TypeName};

const NAME: &str = "toArray";
const ACC_STATIC: u16 = 0x0008;

/// Which of the two `toArray` members a class realizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ToArrayMembers {
    /// `public <T> T[] toArray(T[])`.
    pub(super) generic: bool,
    /// `public Object[] toArray()`.
    pub(super) non_generic: bool,
}

#[derive(Default)]
pub(super) struct CollectionToArray {
    by_class: HashMap<TypeName, ToArrayMembers>,
}

impl CollectionToArray {
    pub(super) fn for_class(&self, class: TypeName) -> ToArrayMembers {
        self.by_class.get(&class).copied().unwrap_or_default()
    }
}

pub(super) fn select(
    ir: &IrFile,
    classpath: &Classpath,
    classifiers: &CheckedBackendClassifiers<'_>,
) -> Result<CollectionToArray, SkipReason> {
    let module = classifiers.module();
    let mut result = CollectionToArray::default();
    for (class_index, class) in ir.classes.iter().enumerate() {
        if !crate::jvm::override_results::realizes_overrides(ir, class_index)
            || class.is_interface
            || class.is_annotation
        {
            continue;
        }
        let Some(hierarchy) = ir.classifier_hierarchies.get(&class.fq_name_id()) else {
            continue;
        };
        let supertypes: Vec<TypeName> = hierarchy
            .iter()
            .filter(|entry| entry.depth > 0)
            .map(|entry| entry.classifier)
            .collect();
        if !supertypes.iter().copied().any(is_collection) {
            continue;
        }
        let chain = superclass_chain(ir, module, classpath, class)?;
        if chain
            .iter()
            .any(|ancestor| ancestor.is_collection(ir, classifiers, classpath))
        {
            continue;
        }
        let shapes = existing_shapes(ir, module, classpath, class, &supertypes, &chain)?;
        let members = ToArrayMembers {
            generic: !shapes.generic,
            non_generic: !shapes.non_generic,
        };
        if members != ToArrayMembers::default() {
            result.by_class.insert(class.fq_name_id(), members);
        }
    }
    Ok(result)
}

/// `kotlin.collections.Collection` in any of its faces, `java.util.Collection` included.
fn is_collection(classifier: TypeName) -> bool {
    jvm_collection_to_kotlin_type_name(to_jvm_type_name(classifier))
        .and_then(mapped_collection)
        .is_some_and(|mapped| mapped.kind == CollectionKind::Collection)
}

/// The one source that owns a supertype's declaration.
enum Declaring<'a> {
    /// A named source classifier of the module, whichever file declares it.
    Module(&'a SourceClassMembers),
    /// A body-local class of the active file.
    File(&'a IrClass),
    /// A dependency classifier, with its class file when the classpath has one.
    Dependency(Option<std::sync::Arc<ClassInfo>>),
}

fn declaring<'a>(
    ir: &'a IrFile,
    module: &'a BackendModuleFacts,
    classpath: &Classpath,
    classifier: TypeName,
) -> Result<Declaring<'a>, SkipReason> {
    if let Some(members) = module.source_class_members(classifier) {
        return Ok(Declaring::Module(members));
    }
    match ir
        .classes
        .iter()
        .find(|class| class.fq_name_id() == classifier)
    {
        Some(class) if class.enclosure.is_some() => Ok(Declaring::File(class)),
        Some(_) => Err(SkipReason::CollectionToArray(format!(
            "missing module declaration record for source supertype {}",
            classifier.render()
        ))),
        None => Ok(Declaring::Dependency(classpath.find_name(classifier))),
    }
}

/// A class of the superclass chain, with whether every class from the collection class up to it
/// shares the collection class's package.
struct Ancestor<'a> {
    classifier: TypeName,
    declaring: Declaring<'a>,
    same_package_path: bool,
}

impl Ancestor<'_> {
    /// kotlinc's indirect collection subclass: this superclass is itself a collection.
    fn is_collection(
        &self,
        ir: &IrFile,
        classifiers: &dyn BackendClassifierSource,
        classpath: &Classpath,
    ) -> bool {
        match &self.declaring {
            Declaring::Module(_) => classifier_closure(classifiers, self.classifier)
                .into_iter()
                .any(is_collection),
            Declaring::File(_) => {
                ir.classifier_hierarchies
                    .get(&self.classifier)
                    .is_some_and(|hierarchy| {
                        hierarchy
                            .iter()
                            .any(|entry| is_collection(entry.classifier))
                    })
            }
            Declaring::Dependency(_) => classpath_is_collection(classpath, self.classifier),
        }
    }
}

fn superclass_chain<'a>(
    ir: &'a IrFile,
    module: &'a BackendModuleFacts,
    classpath: &Classpath,
    class: &IrClass,
) -> Result<Vec<Ancestor<'a>>, SkipReason> {
    let package = class.fq_name_id().namespace();
    let mut same_package_path = true;
    let mut chain = Vec::new();
    let mut seen = HashSet::default();
    let mut current = Some(class.superclass);
    while let Some(classifier) = current
        .filter(|&classifier| classifier != wk::java_object() && classifier != wk::any())
        .filter(|&classifier| seen.insert(classifier))
    {
        same_package_path &= classifier.namespace() == package;
        let declaring = declaring(ir, module, classpath, classifier)?;
        current = match &declaring {
            Declaring::Module(members) => members.superclass,
            Declaring::File(source) => Some(source.superclass),
            Declaring::Dependency(info) => info.as_ref().and_then(|info| info.super_class),
        };
        chain.push(Ancestor {
            classifier,
            declaring,
            same_package_path,
        });
    }
    Ok(chain)
}

/// A classifier and every classifier it extends, through the published direct supertypes.
fn classifier_closure(classifiers: &dyn BackendClassifierSource, start: TypeName) -> Vec<TypeName> {
    let mut closure = Vec::new();
    let mut pending = vec![start];
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        closure.push(current);
        if let Some(fact) = classifiers.classifier(current) {
            pending.extend(fact.supertypes.iter().copied());
        }
    }
    closure
}

/// Whether a dependency class implements a collection, through its class-file supertypes.
fn classpath_is_collection(classpath: &Classpath, start: TypeName) -> bool {
    let mut pending = vec![start];
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if is_collection(current) {
            return true;
        }
        if let Some(info) = classpath.find_name(current) {
            pending.extend(info.super_class);
            pending.extend(info.interfaces.iter());
        }
    }
    false
}

#[derive(Default)]
struct Shapes {
    generic: bool,
    non_generic: bool,
}

impl Shapes {
    fn record(&mut self, formals: &[&str], extension: bool, parameters: &[Ty], result: Ty) {
        if extension {
            return;
        }
        match (formals, parameters) {
            ([], []) => self.non_generic |= result.non_null().is_reference_array(),
            ([formal], [parameter]) => {
                self.generic |= array_of(*parameter, formal) && array_of(result, formal);
            }
            _ => {}
        }
    }
}

/// `Array<T>` or `Array<T>?` of the function's own type parameter `T`.
fn array_of(ty: Ty, formal: &str) -> bool {
    let ty = ty.non_null();
    if !ty.is_reference_array() {
        return false;
    }
    let Some(element) = ty.array_elem() else {
        return false;
    };
    let element = match element.kind() {
        TyKind::OutProjection(inner) | TyKind::InProjection(inner) => inner,
        _ => element,
    };
    matches!(element.non_null().kind(), TyKind::TyParam(name, _) if name == formal)
}

/// The `toArray` shapes the class declares or inherits from a supertype that is not itself one of
/// the mapped collection interfaces (whose JDK `toArray` Kotlin does not see).
fn existing_shapes(
    ir: &IrFile,
    module: &BackendModuleFacts,
    classpath: &Classpath,
    class: &IrClass,
    supertypes: &[TypeName],
    chain: &[Ancestor<'_>],
) -> Result<Shapes, SkipReason> {
    let mut shapes = Shapes::default();
    source_shapes(ir, class, None, &mut shapes);
    let package = class.fq_name_id().namespace();
    for &supertype in supertypes {
        if is_mapped_collection_face(supertype) {
            continue;
        }
        // A superclass is reached along the chain; an interface declares no package-private member.
        let interface;
        let (declaring, same_package_path) = match chain
            .iter()
            .find(|ancestor| ancestor.classifier == supertype)
        {
            Some(ancestor) => (&ancestor.declaring, ancestor.same_package_path),
            None => {
                interface = declaring(ir, module, classpath, supertype)?;
                (&interface, supertype.namespace() == package)
            }
        };
        match declaring {
            Declaring::Module(members) => {
                module_shapes(supertype, members, same_package_path, &mut shapes)?;
            }
            Declaring::File(source) => {
                source_shapes(ir, source, Some(same_package_path), &mut shapes);
            }
            Declaring::Dependency(Some(info)) => {
                classpath_shapes(info, same_package_path, &mut shapes);
            }
            // Only a builtin without a class file (`kotlin.Any`) is absent here.
            Declaring::Dependency(None) => {}
        }
    }
    Ok(shapes)
}

/// The shapes of the members a module classifier declares that reach the class.
fn module_shapes(
    owner: TypeName,
    members: &SourceClassMembers,
    same_package_path: bool,
    shapes: &mut Shapes,
) -> Result<(), SkipReason> {
    for function in members.functions.iter() {
        if &*function.name != NAME || !inherited_along(function.visibility, same_package_path) {
            continue;
        }
        let signature = function.signature.as_ref().ok_or_else(|| {
            SkipReason::CollectionToArray(format!(
                "missing checked signature of {}.{NAME}",
                owner.render()
            ))
        })?;
        let formals: Vec<&str> = signature
            .type_parameters
            .iter()
            .map(AsRef::as_ref)
            .collect();
        shapes.record(
            &formals,
            signature.extension,
            &signature.parameters,
            signature.result,
        );
    }
    Ok(())
}

/// The shapes of the members a dependency class declares that reach the class: from its Kotlin
/// metadata, or from its class file for a Java class.
fn classpath_shapes(info: &ClassInfo, same_package_path: bool, shapes: &mut Shapes) {
    if info.meta.class_kind.is_some() {
        for function in info.meta.class_functions.iter() {
            if function.kotlin_name != NAME
                || !inherited_along(function.visibility, same_package_path)
            {
                continue;
            }
            let Some(signature) = function.generic_sig.as_ref() else {
                continue;
            };
            let formals: Vec<&str> = signature.formals.iter().map(String::as_str).collect();
            shapes.record(
                &formals,
                function.is_extension(),
                &signature.params,
                signature.ret,
            );
        }
        return;
    }
    for method in &info.methods {
        if method.name != NAME
            || method.access & ACC_STATIC != 0
            || !inherited_along(method.visibility(), same_package_path)
        {
            continue;
        }
        match method.signature.as_deref() {
            None => shapes.non_generic |= method.descriptor.starts_with("()["),
            Some(signature) => shapes.generic |= java_generic_shape(signature),
        }
    }
}

/// The shapes of a class of the active file: all its own members, or, given the path from the
/// collection class, only the ones that class inherits.
fn source_shapes(
    ir: &IrFile,
    class: &IrClass,
    inherited_along_path: Option<bool>,
    shapes: &mut Shapes,
) {
    for &function_id in &class.methods {
        let function = &ir.functions[function_id as usize];
        if function.is_static
            || function.name != NAME
            || inherited_along_path.is_some_and(|same_package_path| {
                !inherited_along(ir.method_visibility(function_id), same_package_path)
            })
        {
            continue;
        }
        let extension = ir.extension_receiver_fns.contains(&function_id);
        match ir.signatures.get(&function_id) {
            Some(signature) => {
                let formals: Vec<&str> = signature
                    .type_params
                    .iter()
                    .map(|parameter| parameter.semantic_name.as_str())
                    .collect();
                let result = signature.ret.unwrap_or(function.ret);
                shapes.record(&formals, extension, &signature.params, result);
            }
            None => shapes.record(&[], extension, &function.params, function.ret),
        }
    }
}

/// A Java method `<T:…>([TT;)[TT;`.
fn java_generic_shape(signature: &str) -> bool {
    let Some(rest) = signature.strip_prefix('<') else {
        return false;
    };
    let Some((formal, rest)) = rest.split_once(':') else {
        return false;
    };
    let Some((_, method)) = rest.split_once('>') else {
        return false;
    };
    method == format!("([T{formal};)[T{formal};")
}
