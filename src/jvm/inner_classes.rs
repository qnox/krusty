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

        for (class_id, class) in ir.classes.iter().enumerate() {
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
            // A function reference class is anonymous too, and synthetic; it is public only where
            // spliced inline code constructs it from elsewhere.
            if class.func_ref.is_some() {
                let public = if ir.public_synthetics.contains(&identity) {
                    0x0001
                } else {
                    0
                };
                specs.push(InnerClassSpec {
                    inner: identity.render(),
                    outer: None,
                    name: None,
                    access: 0x1000 | 0x0010 | 0x0008 | public,
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

            // A source local class is not a member of the textual classifier prefix in its generated
            // JVM name, and kotlinc lists it under its source name from the checked naming
            // provenance. A class nested in a local class is a member of that class. A coroutine
            // state machine is anonymous in `InnerClasses` even though its own IR class is not a
            // source anonymous-object declaration.
            let coroutine = is_coroutine_state_machine(class);
            let local = class.is_local_class;
            let name = if local && !coroutine {
                source_name(
                    ir,
                    u32::try_from(class_id).expect("too many classes for a packed class id"),
                )
            } else {
                name
            };
            specs.push(InnerClassSpec {
                inner: identity.render(),
                outer: (!coroutine && !local).then_some(outer),
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

/// The source name of a named local class: the last segment of its checked naming provenance.
fn source_name(ir: &IrFile, class: crate::ir::ClassId) -> String {
    let provenance = ir
        .local_class_name_provenance
        .get(&class)
        .expect("a named local class must carry checked naming provenance");
    assert!(
        provenance.ordinal.is_none(),
        "a named local class cannot carry an anonymous-class ordinal"
    );
    provenance
        .segments
        .last()
        .expect("a named local class must carry a source-name segment")
        .clone()
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
    use crate::ir::{IrClass, IrEnclosure, IrLocalClassNameProvenance, IrModuleSource};

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

    #[test]
    fn named_local_and_its_member_use_checked_source_ownership() {
        let mut ir = IrFile::default();
        let owner = type_name("sample/Owner");
        let local = type_name("sample/Owner$make$Local");
        let member = type_name("sample/Owner$make$Local$Part");
        let owner_id = ir.add_class(IrClass::synthetic(owner));

        let mut local_class = IrClass::synthetic(local);
        local_class.is_local_class = true;
        local_class.enclosure = Some(IrEnclosure::File);
        let local_id = ir.add_class(local_class);
        ir.local_class_name_provenance.insert(
            local_id,
            IrLocalClassNameProvenance {
                source: IrModuleSource {
                    source: crate::fir::SourceFileId::from_raw(0),
                    package: type_name("sample"),
                },
                lexical_owner: Some(crate::ir::IrLocalClassOwner::Class(owner_id)),
                segments: vec!["make".to_string(), "Local".to_string()].into_boxed_slice(),
                ordinal: None,
            },
        );
        ir.add_class(IrClass::synthetic(member));

        assert_eq!(
            InnerClasses::new(&ir).specs,
            [
                InnerClassSpec {
                    inner: "sample/Owner$make$Local".to_string(),
                    outer: None,
                    name: Some("Local".to_string()),
                    access: 0x0019,
                },
                InnerClassSpec {
                    inner: "sample/Owner$make$Local$Part".to_string(),
                    outer: Some("sample/Owner$make$Local".to_string()),
                    name: Some("Part".to_string()),
                    access: 0x0019,
                },
            ]
        );
    }

    #[test]
    #[should_panic(expected = "a named local class must carry checked naming provenance")]
    fn named_local_without_provenance_is_rejected_instead_of_reparsed() {
        let mut ir = IrFile::default();
        let mut local = IrClass::synthetic(type_name("sample/Owner$make$Local"));
        local.is_local_class = true;
        local.enclosure = Some(IrEnclosure::File);
        ir.add_class(local);

        InnerClasses::new(&ir);
    }
}
