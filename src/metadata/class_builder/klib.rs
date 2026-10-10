//! The KLIB carrier: a class in a `PackageFragment`, with the `KlibMetadataProtoBuf` annotation,
//! file and constant extensions.

use crate::metadata::protobuf::Pb;
use crate::metadata::type_encoder::{StringTable, TypeParameters};

use super::carrier::{AnnotationSite, ClassCarrier, ConstructorSlot};
use super::declarations::FnMeta;

/// A class written into a KLIB fragment for the file `source_file`.
pub(crate) struct KlibCarrier<'a> {
    /// The file declaring the class, recorded on the class (`classFile` = 175) and on each member
    /// with a source (`functionFile` = 172, `propertyFile` = 176).
    pub(crate) source_file: &'a str,
    /// Parallel to the declaration's properties: a `const val`'s value (`compileTimeValue` = 173),
    /// which a JVM class file keeps in a `ConstantValue` attribute instead.
    pub(crate) constants: &'a [Option<crate::ir::IrConst>],
}

impl ClassCarrier for KlibCarrier<'_> {
    fn annotation_field(&self, site: AnnotationSite) -> u32 {
        match site {
            // `KlibMetadataProtoBuf.{class,constructor,parameter,enumEntry,function,property}Annotation`
            AnnotationSite::Class
            | AnnotationSite::Constructor
            | AnnotationSite::ValueParameter
            | AnnotationSite::EnumEntry
            | AnnotationSite::Function
            | AnnotationSite::Property => 170,
            AnnotationSite::Getter => 177,
            AnnotationSite::Setter => 178,
            AnnotationSite::BackingField => 181,
        }
    }

    fn constructor_extensions(&self, _: &mut StringTable<'_>, _: ConstructorSlot) -> Pb {
        Pb::new()
    }

    fn property_extensions(&self, _: &mut StringTable<'_>, _: usize) -> Pb {
        Pb::new()
    }

    fn property_trailer(&self, st: &mut StringTable<'_>, index: usize) -> Pb {
        let mut trailer = Pb::new();
        // The file interns before the constant, as for a top-level property.
        trailer.field_varint(176, u64::from(st.local(self.source_file))); // propertyFile = 176
        if let Some(Some(constant)) = self.constants.get(index) {
            let value = crate::metadata::builder::constant_value_pb(st, constant);
            trailer.field_message(173, &value); // compileTimeValue = 173
        }
        trailer
    }

    fn function_extensions(&self, _: &mut StringTable<'_>, _: usize) -> Pb {
        Pb::new()
    }

    fn function_trailer(&self, st: &mut StringTable<'_>, function: &FnMeta) -> Pb {
        let mut trailer = Pb::new();
        if function.has_source {
            trailer.field_varint(172, u64::from(st.local(self.source_file))); // functionFile = 172
        }
        trailer
    }

    fn class_extensions(&self, _: &mut StringTable<'_>, _: &TypeParameters) -> Pb {
        Pb::new()
    }

    fn class_trailer(&self, st: &mut StringTable<'_>) -> Pb {
        let mut trailer = Pb::new();
        trailer.field_varint(175, u64::from(st.local(self.source_file))); // classFile = 175
        trailer
    }
}
