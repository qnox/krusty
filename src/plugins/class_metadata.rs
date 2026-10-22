//! Extension fields a compiler plugin adds to a class's own metadata record.
//!
//! kotlinc lets a plugin extend the serialized class message (`FirMetadataSerializerPlugin`): each
//! plugin registers the extension field numbers it owns, and the metadata serializer calls every
//! plugin while it writes the record. krusty does the same. Nothing is computed or stored ahead of
//! the write: the target's metadata writer asks the registered plugins, in registration order, at
//! the moment it serializes the class, and each plugin reads the checked class it is asked about.
//! A plugin owns its field numbers; two plugins claiming one number is a registration error, and
//! a plugin writing a number it did not claim is a compiler bug.

use super::{IrPlugin, PluginHost};
use crate::ir::{IrClass, IrFile};

/// Where a target's metadata writer takes plugin extension fields: the class record it is
/// serializing, in that record's own encoding.
pub trait ClassMetadataSink {
    /// Append `value`, interned in the record's string table, as one element of the repeated field
    /// `field`.
    fn repeated_string(&mut self, field: u32, value: &str);
}

/// The class record a plugin is writing into. It exposes only the field shapes plugin extensions
/// use, each checked against the fields the plugin owns.
pub struct ClassMetadataExtensions<'w> {
    record: &'w mut dyn ClassMetadataSink,
    plugin: &'w str,
    owned: &'static [u32],
}

impl ClassMetadataExtensions<'_> {
    /// Append `value` as one element of the repeated string-reference field `field`.
    pub fn repeated_string(&mut self, field: u32, value: &str) {
        assert!(
            self.owned.contains(&field),
            "plugin `{}` wrote class metadata field {field}, which it does not own",
            self.plugin
        );
        self.record.repeated_string(field, value);
    }
}

/// The registered plugins' view of one class whose metadata a target is about to write.
#[derive(Clone, Copy)]
pub struct PluginClassMetadata<'a> {
    pub(crate) plugins: &'a PluginHost,
    pub(crate) ir: &'a IrFile,
    pub(crate) class: &'a IrClass,
}

impl PluginClassMetadata<'_> {
    /// Ask every plugin, in registration order, for its fields of this class and append them to
    /// `record`.
    pub(crate) fn write(&self, record: &mut dyn ClassMetadataSink) {
        for plugin in &self.plugins.plugins {
            let mut out = ClassMetadataExtensions {
                record: &mut *record,
                plugin: plugin.name(),
                owned: plugin.class_metadata_fields(),
            };
            plugin.write_class_metadata(self.ir, self.class, &mut out);
        }
    }
}

/// Reject `plugin` when it claims a class metadata field a registered plugin already owns.
pub(super) fn assert_disjoint_fields(registered: &[Box<dyn IrPlugin>], plugin: &dyn IrPlugin) {
    for field in plugin.class_metadata_fields() {
        if let Some(owner) = registered
            .iter()
            .find(|other| other.class_metadata_fields().contains(field))
        {
            panic!(
                "plugins `{}` and `{}` both claim class metadata field {field}",
                owner.name(),
                plugin.name()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin owning `fields` that writes its name to each of them, then to `extra`.
    struct Claims(&'static str, &'static [u32], Option<u32>);

    impl IrPlugin for Claims {
        fn name(&self) -> &str {
            self.0
        }

        fn class_metadata_fields(&self) -> &'static [u32] {
            self.1
        }

        fn write_class_metadata(
            &self,
            _: &IrFile,
            _: &IrClass,
            out: &mut ClassMetadataExtensions<'_>,
        ) {
            for &field in self.1.iter().chain(&self.2) {
                out.repeated_string(field, self.0);
            }
        }
    }

    #[derive(Default)]
    struct Recorded(Vec<(u32, String)>);

    impl ClassMetadataSink for Recorded {
        fn repeated_string(&mut self, field: u32, value: &str) {
            self.0.push((field, value.to_string()));
        }
    }

    fn written(host: &PluginHost) -> Vec<(u32, String)> {
        let ir = IrFile::default();
        let class = crate::plugins::synthetic_class("demo/C");
        let mut record = Recorded::default();
        PluginClassMetadata {
            plugins: host,
            ir: &ir,
            class: &class,
        }
        .write(&mut record);
        record.0
    }

    #[test]
    fn plugins_write_their_fields_in_registration_order() {
        let mut host = PluginHost::new();
        host.register(Box::new(Claims("later", &[9], None)));
        host.register(Box::new(Claims("earlier", &[8], None)));
        assert_eq!(
            written(&host),
            vec![(9, "later".to_string()), (8, "earlier".to_string())]
        );
    }

    #[test]
    #[should_panic(expected = "plugins `first` and `second` both claim class metadata field 9")]
    fn two_plugins_cannot_own_one_class_metadata_field() {
        let mut host = PluginHost::new();
        host.register(Box::new(Claims("first", &[8, 9], None)));
        host.register(Box::new(Claims("second", &[9], None)));
    }

    #[test]
    #[should_panic(expected = "plugin `stray` wrote class metadata field 7, which it does not own")]
    fn a_plugin_writes_only_the_fields_it_owns() {
        let mut host = PluginHost::new();
        host.register(Box::new(Claims("stray", &[8], Some(7))));
        written(&host);
    }
}
