//! The class-file version a writer emits: the `-jvm-target` major version and the preview marking
//! `-Xjvm-enable-preview` adds to every class.

use super::ACC_INTERFACE;

/// The class-file minor version that marks a class as compiled with preview features.
pub const PREVIEW_MINOR_VERSION: u16 = 0xFFFF;

/// Major 52 = Java 8, matching kotlinc's default JVM target.
pub const MAJOR_JAVA8: u16 = 52;

/// The flag on an `InnerClasses` row that names a class with identity (`ACC_SYNCHRONIZED`'s bit,
/// `ACC_SUPER`'s on the class itself).
pub const ACC_IDENTITY: u16 = 0x0020;

/// The first class-file major version (JDK 28) whose `InnerClasses` rows carry [`ACC_IDENTITY`]
/// under `-Xjvm-enable-preview`.
const IDENTITY_FLAG_MAJOR_VERSION: u16 = 72;

/// The class-file version a writer targets: the `-jvm-target` major version and whether
/// `-Xjvm-enable-preview` marks the class as compiled with preview features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClassFileVersion {
    pub major: u16,
    pub preview: bool,
}

impl Default for ClassFileVersion {
    fn default() -> Self {
        Self {
            major: MAJOR_JAVA8,
            preview: false,
        }
    }
}

impl ClassFileVersion {
    /// The minor version written to the class file.
    pub fn minor(self) -> u16 {
        if self.preview {
            PREVIEW_MINOR_VERSION
        } else {
            0
        }
    }

    /// An `InnerClasses` row's flags as written: under preview from JDK 28 on, kotlinc gives every
    /// non-interface class (an annotation is an interface) the identity flag, whichever class file
    /// declared it.
    pub fn inner_class_access(self, access: u16) -> u16 {
        if self.preview && self.major >= IDENTITY_FLAG_MAJOR_VERSION && access & ACC_INTERFACE == 0
        {
            access | ACC_IDENTITY
        } else {
            access
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::{
        ClassWriter, InnerClassSpec, ACC_ABSTRACT, ACC_FINAL, ACC_PUBLIC, ACC_STATIC,
    };

    /// One class referencing the nested classes `A$Concrete` (flags 0x19) and `A$Face` (an
    /// interface), written under `major` with or without the preview marking: its minor version
    /// and the flags of its `InnerClasses` rows.
    fn preview_class(major: u16, preview: bool) -> (u16, Vec<(String, u16)>) {
        let mut cw = ClassWriter::new("Use", "java/lang/Object");
        cw.set_major(major);
        cw.set_preview(preview);
        for (inner, name, access) in [
            (
                "A$Concrete",
                "Concrete",
                ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
            ),
            (
                "A$Face",
                "Face",
                ACC_PUBLIC | ACC_STATIC | ACC_INTERFACE | ACC_ABSTRACT,
            ),
        ] {
            cw.add_inner_class(InnerClassSpec {
                inner: inner.to_string(),
                outer: Some("A".to_string()),
                name: Some(name.to_string()),
                access,
            });
            cw.add_field(ACC_PUBLIC, name, &format!("LA${name};"));
        }
        let bytes = cw.finish();
        let minor = u16::from_be_bytes([bytes[4], bytes[5]]);
        let info = crate::jvm::classreader::parse_class(&bytes).expect("parse class");
        let rows = info
            .inner_classes
            .iter()
            .map(|row| (row.inner.clone(), row.access))
            .collect();
        (minor, rows)
    }

    #[test]
    fn preview_marks_the_minor_version_and_from_major_72_the_non_interface_inner_class_rows() {
        let concrete = ACC_PUBLIC | ACC_STATIC | ACC_FINAL;
        let face = ACC_PUBLIC | ACC_STATIC | ACC_INTERFACE | ACC_ABSTRACT;
        let rows = |concrete: u16| {
            vec![
                ("A$Concrete".to_string(), concrete),
                ("A$Face".to_string(), face),
            ]
        };
        assert_eq!(
            preview_class(72, true),
            (0xFFFF, rows(concrete | ACC_IDENTITY))
        );
        assert_eq!(preview_class(71, true), (0xFFFF, rows(concrete)));
        assert_eq!(preview_class(72, false), (0, rows(concrete)));
    }
}
