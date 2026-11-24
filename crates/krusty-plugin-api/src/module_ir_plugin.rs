use crate::ir::ModuleIr;

/// A kotlinc plugin that transforms IR: it reads the module krusty built from checked code and
/// answers with the module it leaves. The compiler holds one only through this trait, so a plugin
/// that runs in a JVM beside krusty and one linked into it look the same to the compiler.
pub trait ModuleIrPlugin {
    /// Runs the plugin's IR generation over `module` and returns the module it leaves.
    ///
    /// A plugin that cannot answer for a module refuses it by what went wrong, rather than
    /// returning the module without the IR it could not express.
    fn transform(&mut self, module: &ModuleIr) -> Result<ModuleIr, ModuleIrRefusal>;
}

/// Why a [`ModuleIrPlugin`] did not return a module, in the words the build reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleIrRefusal {
    pub message: String,
}

impl std::fmt::Display for ModuleIrRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModuleIrRefusal {}

#[cfg(test)]
mod tests {
    use super::*;

    struct Refuses;

    impl ModuleIrPlugin for Refuses {
        fn transform(&mut self, _: &ModuleIr) -> Result<ModuleIr, ModuleIrRefusal> {
            Err(ModuleIrRefusal {
                message: "no".to_string(),
            })
        }
    }

    #[test]
    fn a_plugin_is_held_through_the_trait_and_its_refusal_reads_as_its_message() {
        let mut plugin: Box<dyn ModuleIrPlugin> = Box::new(Refuses);
        let refusal = plugin
            .transform(&ModuleIr { files: Vec::new() })
            .unwrap_err();
        assert_eq!(refusal.to_string(), "no");
    }
}
