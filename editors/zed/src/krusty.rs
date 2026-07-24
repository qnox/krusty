//! Zed extension for `krusty-lsp`.

use zed_extension_api::{self as zed, settings::LspSettings, LanguageServerId, Result};

const SERVER_BINARY: &str = "krusty-lsp";

struct KrustyExtension;

impl zed::Extension for KrustyExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let command = binary
            .as_ref()
            .and_then(|binary| binary.path.clone())
            .or_else(|| worktree.which(SERVER_BINARY))
            .ok_or_else(|| {
                format!(
                    "{SERVER_BINARY} not found in PATH; set \
                     lsp.{id}.binary.path in settings.json",
                    id = language_server_id.as_ref()
                )
            })?;

        let mut args = binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();
        if !args.iter().any(|argument| argument == "--stdio") {
            args.insert(0, "--stdio".to_string());
        }

        let mut env = worktree.shell_env();
        if let Some(overrides) = binary.and_then(|binary| binary.env) {
            for (key, value) in overrides {
                env.retain(|(existing, _)| existing != &key);
                env.push((key, value));
            }
        }

        Ok(zed::Command { command, args, env })
    }
}

zed::register_extension!(KrustyExtension);
