//! A class's `Companion` field: its declaration and its initialization in `<clinit>`.

use super::static_accessors::StaticAccessor;
use super::*;
use crate::jvm::private_static_access::StaticOwner;

/// `static final` plus the companion object's own visibility, as kotlinc writes the outer class's
/// `Companion` field: a `private companion object` keeps its field private, a protected one
/// protected. An interface field is always public.
pub(super) fn companion_field_access(ir: &IrFile, class: &IrClass, companion: TypeName) -> u16 {
    const STATIC_FINAL: u16 = 0x0018;
    if class.is_interface {
        return STATIC_FINAL | 0x0001;
    }
    STATIC_FINAL
        | match ir.class_visibilities.get(&companion) {
            Some(crate::types::Visibility::Private) => 0x0002,
            Some(crate::types::Visibility::Protected) => 0x0004,
            _ => 0x0001,
        }
}

/// A read of the `Companion` field through which `holder` publishes companion `companion`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CompanionFieldRead {
    pub(super) holder: TypeName,
    pub(super) companion: TypeName,
    pub(super) field: Box<str>,
    visibility: crate::types::Visibility,
}

/// The `Companion` field `expression` reads. A checked singleton value names its classifier and a
/// lowered read names the holder too; either way the holder's classifier record must declare that
/// companion, and the record is the same for a class of this file, of another file of this module,
/// and of a dependency. The field's access is the companion's declared visibility, except that an
/// interface publishes its companion through a public field.
pub(super) fn companion_field_read(
    ir: &IrFile,
    classifiers: &dyn crate::backend::BackendClassifierSource,
    expression: &IrExpr,
) -> Option<CompanionFieldRead> {
    let (holder, companion) = match expression {
        IrExpr::SingletonValue { classifier } => {
            let (holder, _) = crate::jvm::singleton_storage::of(classifiers, *classifier)?;
            (holder, *classifier)
        }
        IrExpr::StaticInstance { owner, ty, .. } if owner != ty => (
            ir.classes.get(*owner as usize)?.fq_name,
            ir.classes.get(*ty as usize)?.fq_name,
        ),
        IrExpr::ExternalStaticInstance { owner, ty, .. } if owner != ty => (*owner, *ty),
        _ => return None,
    };
    let declared = classifiers.classifier(holder)?;
    let (field, published) = declared.companion.as_ref()?;
    if *published != companion {
        return None;
    }
    let visibility = if declared.is_interface() {
        crate::types::Visibility::Public
    } else {
        match classifiers.classifier(companion)?.access {
            crate::libraries::ClassifierAccess::Private => crate::types::Visibility::Private,
            crate::libraries::ClassifierAccess::Protected => crate::types::Visibility::Protected,
            _ => crate::types::Visibility::Public,
        }
    };
    Some(CompanionFieldRead {
        holder,
        companion,
        field: field.clone(),
        visibility,
    })
}

/// The accessor through which code of `context` reads `read`'s field, as kotlinc's
/// `SyntheticAccessorLowering` places it, or `None` when `context` may read the field itself.
///
/// A private field is read directly only by its holder; every other class calls the holder's
/// `access$get<Companion>$p()`. A protected field is read directly from the holder's package and
/// from its subclasses. Any other class reads it through `access$get<Companion>$p$s<hash>()` on the
/// class that grants the access: the innermost lexically enclosing class that subclasses the
/// holder, else the innermost enclosing class's companion that does. A protected read with no such
/// class has no Kotlin access path and is an error.
pub(super) fn companion_field_accessor(
    ir: &IrFile,
    context: StaticOwner,
    read: &CompanionFieldRead,
) -> Result<Option<(TypeName, StaticAccessor)>, String> {
    let accessor = |owner: TypeName| {
        Ok(Some((
            owner,
            StaticAccessor::CompanionInstance {
                companion: read.companion,
                holder: read.holder,
            },
        )))
    };
    match read.visibility {
        crate::types::Visibility::Private => {
            if context == StaticOwner::Class(read.holder) {
                Ok(None)
            } else {
                accessor(read.holder)
            }
        }
        crate::types::Visibility::Protected => {
            let StaticOwner::Class(class) = context else {
                return Err(format!(
                    "a file-level read of protected companion {} has no subclass to grant it",
                    read.companion.render()
                ));
            };
            if class.namespace() == read.holder.namespace() || subclasses(ir, class, read.holder) {
                return Ok(None);
            }
            match protected_access_grantor(ir, class, read.holder) {
                Some(grantor) => accessor(grantor),
                None => Err(format!(
                    "{} reads protected companion {} with no enclosing subclass of {}",
                    class.render(),
                    read.companion.render(),
                    read.holder.render()
                )),
            }
        }
        _ => Ok(None),
    }
}

/// Whether `class` has `target` in its checked hierarchy.
fn subclasses(ir: &IrFile, class: TypeName, target: TypeName) -> bool {
    ir.classifier_hierarchies
        .get(&class)
        .is_some_and(|hierarchy| hierarchy.iter().any(|applied| applied.classifier == target))
}

/// kotlinc's `accessorParent` for a protected declaration of `holder` used in `context`: of the
/// classes lexically enclosing the use (innermost first), the first that subclasses `holder`, else
/// the first whose companion does.
fn protected_access_grantor(ir: &IrFile, context: TypeName, holder: TypeName) -> Option<TypeName> {
    let mut scopes = Vec::new();
    let mut current = ir.class_id_by_name(context);
    while let Some(class) = current {
        if scopes.contains(&class) {
            break;
        }
        scopes.push(class);
        // A class declared in code is enclosed by the class that owns that code; a member class by
        // the classifier it is declared in.
        current = match ir.classes.get(class as usize) {
            Some(declared) if declared.enclosure.is_some() => {
                super::access_bridges::enclosing_classes(ir, class)
                    .first()
                    .copied()
            }
            Some(declared) => declared
                .fq_name
                .nested_owner()
                .and_then(|owner| ir.class_id_by_name(owner)),
            None => None,
        };
    }
    let names = scopes
        .iter()
        .filter_map(|&class| ir.classes.get(class as usize))
        .collect::<Vec<_>>();
    names
        .iter()
        .map(|class| class.fq_name)
        .find(|&class| subclasses(ir, class, holder))
        .or_else(|| {
            names
                .iter()
                .filter_map(|class| class.companion_class)
                .find(|&companion| subclasses(ir, companion, holder))
        })
}

/// The name and descriptor of `owner`'s accessor of the `field` of `holder` that holds
/// `companion`. An accessor declared by another class than the holder names the holder by
/// kotlinc's `$s<hash>` suffix: the Java `String.hashCode` of its simple name.
pub(super) fn companion_instance_accessor(
    owner: TypeName,
    holder: TypeName,
    companion: TypeName,
    field: &str,
) -> (String, String) {
    let suffix = if owner == holder {
        String::new()
    } else {
        format!("$s{}", java_string_hash(holder.nested_segment_ref()))
    };
    (
        format!("access${}$p{suffix}", property_getter_name(field)),
        format!("()L{};", companion.render()),
    )
}

fn java_string_hash(text: &str) -> i32 {
    text.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

pub(super) fn add_companion_field(cw: &mut ClassWriter, class: &IrClass) {
    let Some(companion) = class.companion_class else {
        return;
    };
    cw.add_field(
        0x0019,
        companion.nested_segment_ref(),
        &format!("L{};", companion.render()),
    );
}

pub(super) fn emit_companion_init(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    owner: &str,
    class: &IrClass,
) {
    let Some(companion) = class.companion_class else {
        return;
    };
    let companion_name = companion.render();
    let descriptor = format!("L{companion_name};");
    // An INTERFACE's companion self-hosts its singleton (`static final $$INSTANCE`, built in the
    // companion's own `<clinit>`); the interface's `Companion` field merely aliases it.
    if is_jvm_interface(class) {
        let instance = cw.fieldref(&companion_name, "$$INSTANCE", &descriptor);
        code.getstatic(instance, 1);
        let field = cw.fieldref(owner, companion.nested_segment_ref(), &descriptor);
        code.putstatic(field, 1);
        return;
    }
    let classifier = cw.class_ref(&companion_name);
    code.new_obj(classifier);
    code.dup();
    code.aconst_null();
    let constructor = cw.methodref(
        &companion_name,
        "<init>",
        "(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
    );
    code.invokespecial(constructor, 1, 0);
    let field = cw.fieldref(owner, companion.nested_segment_ref(), &descriptor);
    code.putstatic(field, 1);
}

#[cfg(test)]
mod tests {
    use super::java_string_hash;

    #[test]
    fn the_holder_suffix_is_the_java_hash_of_its_simple_name() {
        assert_eq!(java_string_hash("A"), 65);
        assert_eq!(java_string_hash("A2"), 2065);
        // The hash wraps like Java's `int` arithmetic, so a long name can be negative.
        assert_eq!(java_string_hash("AbstractBase"), -593764813);
    }
}
