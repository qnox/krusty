//! The `$EntriesIntrinsicMappings` class kotlinc gives each class that inlines a reified
//! `enumEntries<T>()` for an enum without a static `getEntries()`: a Java enum, or a Kotlin enum
//! compiled before `entries` existed.
//!
//! The class holds one `EnumEntries` per such enum, numbered `entries$N` in the order the owning
//! class first reads one, each built in `<clinit>` from the enum's `values()`
//! (`EnumEntriesKt.enumEntries(E[])`). Which class reads the entries is a placement fact of this
//! backend, so the fields are claimed as inlined code is emitted.

use super::*;
use crate::jvm::classfile::InnerClassSpec;
use crate::jvm::reified_arguments::{EnumEntriesRead, EnumEntriesReads};

/// kotlinc's `JvmLoweredDeclarationOrigin.ENUM_MAPPINGS_FOR_ENTRIES` class for an inlined
/// intrinsic names its class `EntriesIntrinsicMappings`.
const ENTRIES_INTRINSIC_MAPPINGS: &str = "EntriesIntrinsicMappings";
const ENUM_ENTRIES: &str = "kotlin/enums/EnumEntries";
/// `PUBLIC | STATIC | FINAL | SYNTHETIC`, as both sides of the nesting declare it.
const INNER_CLASS_ACCESS: u16 = 0x1019;

/// The mapped enums of every class this emission run wrote code for, in first-read order.
#[derive(Default)]
pub(super) struct EntriesMappingTables {
    owners: Vec<(String, Vec<String>)>,
}

impl EntriesMappingTables {
    pub(super) fn clear(&mut self) {
        self.owners.clear();
    }

    /// The field index `owner` reads `enum_class`'s entries from, assigned on first claim.
    fn claim(&mut self, owner: &str, enum_class: &str) -> usize {
        let Some(at) = self.owners.iter().position(|(name, _)| name == owner) else {
            self.owners
                .push((owner.to_string(), vec![enum_class.to_string()]));
            return 0;
        };
        let enums = &mut self.owners[at].1;
        if let Some(index) = enums.iter().position(|claimed| claimed == enum_class) {
            return index;
        }
        enums.push(enum_class.to_string());
        enums.len() - 1
    }

    /// The `InnerClasses` row an owning class declares for its mappings, once it has any.
    pub(super) fn inner_class(&self, owner: &str) -> Option<InnerClassSpec> {
        self.owners
            .iter()
            .any(|(name, _)| name == owner)
            .then(|| inner_class_spec(owner))
    }

    /// Write the mapping class of every owner, in first-claim order.
    pub(super) fn write_classes(
        &self,
        env: &EmitEnv,
        opts: &EmitOptions,
    ) -> Vec<(String, Vec<u8>)> {
        self.owners
            .iter()
            .map(|(owner, enums)| {
                let internal = mappings_class(owner);
                let bytes = write_class(owner, &internal, enums, env, opts);
                (internal, bytes)
            })
            .collect()
    }
}

fn mappings_class(owner: &str) -> String {
    format!("{owner}${ENTRIES_INTRINSIC_MAPPINGS}")
}

fn entries_field(index: usize) -> String {
    format!("entries${index}")
}

fn inner_class_spec(owner: &str) -> InnerClassSpec {
    InnerClassSpec {
        inner: mappings_class(owner),
        outer: Some(owner.to_string()),
        name: Some(ENTRIES_INTRINSIC_MAPPINGS.to_string()),
        access: INNER_CLASS_ACCESS,
    }
}

/// `final class Owner$EntriesIntrinsicMappings` with one `public static final synthetic
/// EnumEntries<E> entries$N` per enum, each initialized in `<clinit>` as kotlinc's field
/// initializer does: `EnumEntriesKt.enumEntries((Enum[]) E.values())`.
fn write_class(
    owner: &str,
    internal: &str,
    enums: &[String],
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let mut cw = new_writer(internal, "java/lang/Object", opts);
    cw.set_access(0x1031); // PUBLIC | FINAL | SUPER | SYNTHETIC
    cw.reserve_method_name("<clinit>");
    cw.reserve_descriptor("()V");
    let mut code = CodeBuilder::new(0);
    for (index, enum_class) in enums.iter().enumerate() {
        let values = cw.methodref(enum_class, "values", &format!("()[L{enum_class};"));
        code.invokestatic(values, 0, 1);
        let array = cw.class_ref("[Ljava/lang/Enum;");
        code.checkcast(array);
        let entries = cw.methodref(
            "kotlin/enums/EnumEntriesKt",
            "enumEntries",
            &format!("([Ljava/lang/Enum;)L{ENUM_ENTRIES};"),
        );
        code.invokestatic(entries, 1, 1);
        let field = cw.fieldref(
            internal,
            &entries_field(index),
            &format!("L{ENUM_ENTRIES};"),
        );
        code.putstatic(field, 1);
    }
    code.ret_void();
    code.link();
    cw.add_method(0x0008, "<clinit>", "()V", &code); // STATIC
    for (index, enum_class) in enums.iter().enumerate() {
        let signature = format!("L{ENUM_ENTRIES}<L{enum_class};>;");
        cw.add_field_sig(
            0x1019, // PUBLIC | STATIC | FINAL | SYNTHETIC
            &entries_field(index),
            &format!("L{ENUM_ENTRIES};"),
            Some(&signature),
        );
    }
    // The owner may itself be nested (an object expression), which the file's nest declares.
    env.inner_classes.register(&mut cw);
    cw.add_inner_class(inner_class_spec(owner));
    metadata_policy::finish_public_synthetic_class(cw, env)
}

/// How one inline call of the class `owner` reads an enum's entries: through the static accessor
/// the enum's declaration publishes ([`EnumEntriesAccessor`](crate::backend::EnumEntriesAccessor),
/// which every provider normalizes the same way), otherwise through `owner`'s mapping class. The
/// declaration is found by the argument's semantic identity, never by its JVM spelling.
pub(super) struct CallEnumEntries<'e> {
    pub(super) classifiers: &'e dyn crate::backend::BackendClassifierSource,
    pub(super) run: &'e EmitRun,
    pub(super) owner: &'e str,
}

impl EnumEntriesReads for CallEnumEntries<'_> {
    fn entries(
        &mut self,
        classifier: crate::types::TypeName,
        internal: &str,
    ) -> Option<EnumEntriesRead> {
        let declaration = self.classifiers.classifier(classifier)?;
        if declaration.kind != crate::libraries::TypeKind::Enum {
            return None;
        }
        if let Some(accessor) = &declaration.enum_entries_accessor {
            return Some(accessor_call(accessor));
        }
        let index = self
            .run
            .entries_mappings
            .borrow_mut()
            .claim(self.owner, internal);
        Some(EnumEntriesRead::Mapping {
            class: mappings_class(self.owner),
            field: entries_field(index),
        })
    }
}

/// The static call of a published entries accessor. Its owner is a semantic classifier identity
/// (`pkg/Outer.Inner` for a nested enum), so the instruction names the JVM binary class
/// (`pkg/Outer$Inner`) the classfile boundary spells for it.
fn accessor_call(accessor: &crate::backend::EnumEntriesAccessor) -> EnumEntriesRead {
    EnumEntriesRead::Accessor {
        owner: crate::jvm::names::owned_classfile_internal_name(accessor.owner),
        name: accessor.name.to_string(),
        descriptor: accessor.descriptor.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_enum_s_accessor_is_called_on_its_jvm_binary_class() {
        let accessor = crate::backend::EnumEntriesAccessor {
            owner: crate::types::type_name("entries_probe/Outer.Inner"),
            name: "getEntries".into(),
            descriptor: "()Lkotlin/enums/EnumEntries;".into(),
        };
        assert_eq!(
            accessor_call(&accessor),
            EnumEntriesRead::Accessor {
                owner: "entries_probe/Outer$Inner".to_owned(),
                name: "getEntries".to_owned(),
                descriptor: "()Lkotlin/enums/EnumEntries;".to_owned(),
            }
        );
    }
}
