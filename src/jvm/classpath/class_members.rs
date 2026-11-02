//! The one gate through which a class's member declarations are read.
//!
//! A class file's member metadata is decoded on first use (see
//! [`crate::jvm::metadata::LazyMembers`]). Reading it through the classpath makes a member message
//! that does not decode the classpath's terminal load error, the same diagnostic an undecodable
//! class header produces when the class is read.

use std::sync::Arc;

use super::Classpath;
use crate::jvm::classreader::{ClassInfo, ReadError};
use crate::jvm::metadata::{FunctionName, KotlinMembers};
use crate::types::{Ty, TypeName, Visibility};

impl Classpath {
    /// The functions, properties, and constructors `class` declares. When they do not decode, the
    /// failure is recorded and the lookup sees none, as it sees nothing once any class load fails.
    pub fn class_members<'a>(&self, class: &'a ClassInfo) -> &'a KotlinMembers {
        match class.kotlin_members() {
            Ok(members) => members,
            Err(error) => {
                self.record_class_load_error(class.this_class, ReadError::BadKotlinMetadata(error));
                KotlinMembers::none()
            }
        }
    }
}

impl Classpath {
    /// The metadata-declared RETURN type of the PROPERTY getter realized by JVM method
    /// `jvm_name`/`jvm_desc`, with full structure when the metadata generic signature carries it and
    /// the bare classifier otherwise. Function returns travel in [`MetadataCallFacts::declared_ret`]
    /// from the already descriptor-aligned callable; a getter is not a metadata function, so this
    /// deliberately separate fallback matches its `JvmPropertySignature` without repeating function
    /// alignment. Together they carry collection identity that JVM signatures erase at every depth.
    pub fn metadata_property_ret_ty_name(
        &self,
        internal: TypeName,
        jvm_name: &str,
        jvm_desc: &str,
    ) -> Option<Ty> {
        let ci = self.find_name(internal)?;
        self.class_members(&ci)
            .class_properties
            .iter()
            .find(|property| {
                property
                    .getter
                    .as_ref()
                    .is_some_and(|getter| getter.name == jvm_name && getter.desc == jvm_desc)
            })
            .and_then(|property| {
                property
                    .generic_sig
                    .as_ref()
                    .map(|gsig| gsig.ret)
                    .or_else(|| property.ret_class.map(super::kotlin_type_name_to_ty))
            })
    }

    /// The names of the `Package.function`s `class` declares, without their signatures. A failure is
    /// recorded as [`Self::class_members`] records it.
    fn package_function_names<'a>(&self, class: &'a ClassInfo) -> &'a [FunctionName] {
        match class.meta.members.package_function_names() {
            Ok(names) => names,
            Err(error) => {
                self.record_class_load_error(class.this_class, ReadError::BadKotlinMetadata(error));
                &[]
            }
        }
    }

    /// The function names `owner`'s statics are declared under, merged across a multifile facade's
    /// parts exactly as [`Self::meta_functions_name`] merges their signatures: a facade with no
    /// functions of its own takes its parts' functions, and a part's private function is never
    /// offered for the facade's forwarder of the same JVM name.
    pub(super) fn facade_function_names(&self, owner: TypeName) -> Vec<FunctionName> {
        let declaring = self.function_declaring_classes(owner);
        declaring
            .classes
            .iter()
            .flat_map(|class| self.package_function_names(class))
            .filter(|name| !(declaring.merged_parts && name.visibility == Visibility::Private))
            .cloned()
            .collect()
    }

    /// Whether [`Self::meta_functions_name`] of `owner` holds a function named `kotlin_name`,
    /// answered from the function names alone.
    pub(crate) fn facade_declares_function(&self, owner: TypeName, kotlin_name: &str) -> bool {
        self.function_declaring_classes(owner)
            .classes
            .iter()
            .flat_map(|class| self.package_function_names(class))
            .any(|name| name.kotlin_name == kotlin_name)
    }

    /// The class files whose `Package.function`s `owner` exposes: `owner` itself, or, for a
    /// multifile facade without functions of its own, its parts.
    fn function_declaring_classes(&self, owner: TypeName) -> FunctionDeclaringClasses {
        let Some(class) = self.find_name(owner) else {
            return FunctionDeclaringClasses::default();
        };
        if !self.package_function_names(&class).is_empty() {
            return FunctionDeclaringClasses {
                classes: vec![class],
                merged_parts: false,
            };
        }
        let parts: Vec<_> = class
            .meta
            .multifile_parts
            .iter()
            .filter_map(|part| self.find(part))
            .collect();
        FunctionDeclaringClasses {
            merged_parts: !parts.is_empty(),
            classes: parts,
        }
    }
}

#[derive(Default)]
struct FunctionDeclaringClasses {
    classes: Vec<Arc<ClassInfo>>,
    /// Whether `classes` are a facade's parts, whose private functions the facade never forwards.
    merged_parts: bool,
}

#[cfg(test)]
mod tests {
    use super::super::test_support::test_temp_dir;
    use super::Classpath;
    use crate::jvm::classfile::{ClassWriter, MetadataStamp};
    use crate::libraries::PlatformInitializationError;

    /// A class whose header decodes but whose one member function carries an invalid type
    /// parameter variance.
    fn class_with_malformed_member(internal: &str) -> Vec<u8> {
        let malformed_member = [
            0x00, // empty StringTableTypes prefix
            0x4a, 0x0a, // Class.function
            0x10, 0x00, // Function.name = d2[0]
            0x22, 0x06, // Function.type_parameter
            0x08, 0x00, // id
            0x10, 0x00, // name
            0x20, 0x03, // invalid variance enum
        ];
        let d1 = malformed_member
            .into_iter()
            .map(char::from)
            .collect::<String>();
        let mut class = ClassWriter::new(internal, "java/lang/Object");
        class.set_kotlin_metadata(
            1,
            MetadataStamp {
                version: [2, 2, 0],
                pre_release: false,
            },
            0,
            &[d1],
            &["broken".to_string()],
        );
        class.finish()
    }

    #[test]
    fn malformed_member_metadata_is_reported_only_when_a_lookup_reads_the_members() {
        let directory = test_temp_dir("lazy-member-metadata");
        std::fs::create_dir(directory.join("lazy")).expect("create package");
        std::fs::write(
            directory.join("lazy/Members.class"),
            class_with_malformed_member("lazy/Members"),
        )
        .expect("write class");
        let classpath = Classpath::new(vec![directory.clone()]);

        let class = classpath
            .find("lazy/Members")
            .expect("the class header decodes");
        assert_eq!(classpath.validate_lazy_class_loads(), Ok(()));

        let members = classpath.class_members(&class);
        assert!(members.class_functions.is_empty());
        assert_eq!(
            classpath.validate_lazy_class_loads(),
            Err(PlatformInitializationError {
                message: "cannot load classpath declaration lazy/Members: invalid Kotlin metadata: InvalidVariance(3)".to_string(),
            })
        );

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove classpath directory");
    }
}
