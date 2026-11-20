//! The frontend's view of a convention plugin ([`krusty_plugin_api::ConventionPlugin`]): the
//! annotations its options configured, published through the [`IrPlugin`] accessor for its
//! convention.

use crate::plugins::cli::PluginOption;
use crate::plugins::IrPlugin;
use crate::types::TypeName;
use krusty_plugin_api::{Convention, ConventionPlugin};

pub(crate) struct ConventionInstance {
    name: &'static str,
    convention: Convention,
    annotations: Vec<TypeName>,
}

impl ConventionInstance {
    pub(crate) fn new(plugin: &ConventionPlugin, options: &[PluginOption]) -> Self {
        Self {
            name: plugin.name,
            convention: plugin.convention,
            annotations: (plugin.annotations)(options),
        }
    }

    fn annotations_for(&self, convention: Convention) -> &[TypeName] {
        if self.convention == convention {
            &self.annotations
        } else {
            &[]
        }
    }
}

impl IrPlugin for ConventionInstance {
    fn name(&self) -> &str {
        self.name
    }

    fn open_by_default_annotations(&self) -> &[TypeName] {
        self.annotations_for(Convention::OpenByDefault)
    }

    fn no_arg_constructor_annotations(&self) -> &[TypeName] {
        self.annotations_for(Convention::NoArgConstructor)
    }

    fn sam_with_receiver_annotations(&self) -> &[TypeName] {
        self.annotations_for(Convention::SamWithReceiver)
    }

    fn assignment_annotations(&self) -> &[TypeName] {
        self.annotations_for(Convention::Assignment)
    }
}
