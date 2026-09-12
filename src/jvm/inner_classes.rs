//! JVM `InnerClasses` candidates prepared once per emitted IR file.
//!
//! Source resolution has already assigned every classifier a stable [`TypeName`]. This boundary
//! derives nesting from those identities and renders only the final classfile payload; it never uses
//! rendered spellings as lookup keys or reparses them to recover semantic owners.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrClass, IrFile};
use crate::jvm::classfile::{ClassWriter, InnerClassSpec};
use crate::types::{type_name, TypeName};

/// The complete ordered candidate list for one IR file. Every class writer receives the same list;
/// `ClassWriter::finish` retains only entries referenced by that classfile.
pub(super) struct InnerClasses {
    specs: Vec<InnerClassSpec>,
}

impl InnerClasses {
    pub(super) fn new(ir: &IrFile) -> Self {
        let declared: HashMap<TypeName, &IrClass> = ir
            .classes
            .iter()
            .map(|class| (class.fq_name_id(), class))
            .collect();
        let mut specs = Vec::new();
        let mut serializers = HashSet::new();
        let generated_serializer = type_name("kotlinx/serialization/internal/GeneratedSerializer");

        // Serialization's compiler-generated nested class is literally named `$serializer`, hence
        // the doubled `$` in its JVM name. Resolve it as an existing interned child of the exact
        // serialized classifier instead of recognizing the rendered `$$serializer` suffix.
        for owner in &ir.classes {
            let Some(serializer) = owner.fq_name_id().existing_nested_child("$serializer") else {
                continue;
            };
            let Some(class) = declared.get(&serializer).copied() else {
                continue;
            };
            if !ir.is_synthetic_class(serializer)
                || !class.interfaces.contains_name(generated_serializer)
            {
                continue;
            }
            serializers.insert(serializer);
            specs.push(InnerClassSpec {
                inner: serializer.render(),
                outer: Some(owner.fq_name_id().render()),
                name: Some("$serializer".to_string()),
                access: class_access(ir, class),
            });
        }

        // A companion's owner records its exact interned identity. Preserve kotlinc's candidate
        // order: every serializer first, then every companion.
        for owner in &ir.classes {
            let Some(companion) = owner.companion_class else {
                continue;
            };
            specs.push(InnerClassSpec {
                inner: companion.render(),
                outer: Some(owner.fq_name_id().render()),
                name: Some(
                    companion
                        .nested_segment_within(owner.fq_name_id())
                        .expect("a recorded companion identity must be nested in its exact owner")
                        .to_string(),
                ),
                access: declared
                    .get(&companion)
                    .copied()
                    .map_or(0x0019, |class| class_access(ir, class)),
            });
        }

        for class in &ir.classes {
            let identity = class.fq_name_id();
            if serializers.contains(&identity) || class.is_companion {
                continue;
            }
            if class.is_anonymous_object {
                specs.push(InnerClassSpec {
                    inner: identity.render(),
                    outer: None,
                    name: None,
                    access: 0x0019,
                });
                continue;
            }

            // Generated local names and source identifiers containing `$` make textual boundaries
            // ambiguous. Compare every already-interned candidate, deepest first, with the exact
            // declaration set; do not reinterpret any of them as a semantic lexical-owner edge.
            let declared_owner = identity
                .existing_nested_owners()
                .into_iter()
                .find(|owner| declared.contains_key(owner));
            let (outer, name) = match declared_owner {
                Some(owner) => (
                    owner.render(),
                    identity
                        .nested_segment_within(owner)
                        .expect("a resolved nested owner must prefix its nested segment")
                        .to_string(),
                ),
                None => {
                    let Some(parts) = identity.jvm_nested_parts() else {
                        continue; // top-level class
                    };
                    parts
                }
            };

            // A local class is not a member of the textual classifier prefix in its generated JVM
            // name. A coroutine state machine is anonymous in `InnerClasses` even though its own IR
            // class is not a source anonymous-object declaration.
            let coroutine = is_coroutine_state_machine(class);
            let member = !coroutine && !class.is_local_class;
            specs.push(InnerClassSpec {
                inner: identity.render(),
                outer: member.then_some(outer),
                name: (!coroutine).then_some(name),
                access: if coroutine {
                    0x0008 | 0x0010
                } else {
                    class_access(ir, class)
                },
            });
        }

        Self { specs }
    }

    pub(super) fn register(&self, writer: &mut ClassWriter) {
        for spec in &self.specs {
            writer.add_inner_class(spec.clone());
        }
    }
}

fn is_coroutine_state_machine(class: &IrClass) -> bool {
    class
        .superclass
        .matches("kotlin/coroutines/jvm/internal/ContinuationImpl")
        || class
            .superclass
            .matches("kotlin/coroutines/jvm/internal/RestrictedContinuationImpl")
}

pub(super) fn class_access(ir: &IrFile, class: &IrClass) -> u16 {
    const PUBLIC: u16 = 0x0001;
    const STATIC: u16 = 0x0008;
    const FINAL: u16 = 0x0010;
    const INTERFACE: u16 = 0x0200;
    const ABSTRACT: u16 = 0x0400;
    const ANNOTATION: u16 = 0x2000;
    const ENUM: u16 = 0x4000;

    let visibility = match ir.class_visibilities.get(&class.fq_name_id()) {
        Some(crate::types::Visibility::Protected) => 0x0004,
        Some(crate::types::Visibility::Private) => 0x0002,
        _ => PUBLIC,
    };
    let mut access = visibility | if class.is_inner_class { 0 } else { STATIC };
    if class.is_annotation {
        access |= INTERFACE | ABSTRACT | ANNOTATION;
    } else if class.is_interface {
        access |= INTERFACE | ABSTRACT;
    } else if !class.enum_entries.is_empty() {
        access |= FINAL | ENUM;
    } else if class.is_sealed || class.is_abstract {
        access |= ABSTRACT;
    } else if !class.is_open {
        access |= FINAL;
    }
    if ir.is_synthetic_class(class.fq_name_id()) {
        access |= 0x1000;
    }
    access
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrClass;

    #[test]
    fn prepares_identity_based_candidates_once_in_kotlinc_order() {
        let mut ir = IrFile::default();
        let outer = type_name("sample/Outer");
        let serializer = outer.nested_child("$serializer");
        let companion = outer.nested_child("Companion");
        let nested_with_dollars = type_name("sample/Outer$Nested$With$Dollars");

        let mut owner = IrClass::synthetic(outer);
        owner.companion_class = Some(companion);
        ir.add_class(owner);

        let mut serializer_class = IrClass::synthetic(serializer);
        serializer_class.is_object = true;
        serializer_class.interfaces = vec![type_name(
            "kotlinx/serialization/internal/GeneratedSerializer",
        )]
        .into();
        ir.mark_synthetic_class(serializer);
        ir.add_class(serializer_class);

        let mut companion_class = IrClass::synthetic(companion);
        companion_class.is_companion = true;
        ir.add_class(companion_class);
        ir.add_class(IrClass::synthetic(nested_with_dollars));

        let prepared = InnerClasses::new(&ir);
        assert_eq!(
            prepared.specs,
            vec![
                InnerClassSpec {
                    inner: "sample/Outer$$serializer".to_string(),
                    outer: Some("sample/Outer".to_string()),
                    name: Some("$serializer".to_string()),
                    access: 0x1019,
                },
                InnerClassSpec {
                    inner: "sample/Outer$Companion".to_string(),
                    outer: Some("sample/Outer".to_string()),
                    name: Some("Companion".to_string()),
                    access: 0x0019,
                },
                InnerClassSpec {
                    inner: "sample/Outer$Nested$With$Dollars".to_string(),
                    outer: Some("sample/Outer".to_string()),
                    name: Some("Nested$With$Dollars".to_string()),
                    access: 0x0019,
                },
            ]
        );
    }
}
