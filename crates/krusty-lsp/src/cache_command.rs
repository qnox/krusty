//! `krusty-lsp cache clean [--all] [-deps-cache-dir <dir>]`: prune the dependency cache without
//! starting a language server.

use std::path::PathBuf;

pub(crate) fn run(args: &[String]) {
    let (all, root) = parse(args).unwrap_or_else(|error| {
        eprintln!("krusty-lsp: {error}");
        std::process::exit(2);
    });
    let root = root.unwrap_or_else(|| {
        krusty_lsp::deps_cache::default_cache_root(&|key| std::env::var(key).ok())
    });
    match krusty_lsp::deps_cache::clean(&root, all) {
        Ok(freed) => println!("krusty-lsp: freed {freed} bytes from {}", root.display()),
        Err(error) => {
            eprintln!("krusty-lsp: cache clean failed: {error}");
            std::process::exit(1);
        }
    }
}

fn parse(args: &[String]) -> Result<(bool, Option<PathBuf>), String> {
    if args.first().map(String::as_str) != Some("clean") {
        return Err("usage: cache clean [--all] [-deps-cache-dir <dir>]".to_string());
    }
    let mut all = false;
    let mut root = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--all" => {
                all = true;
                index += 1;
            }
            "-deps-cache-dir" => {
                let path = args
                    .get(index + 1)
                    .filter(|path| !path.starts_with('-'))
                    .ok_or_else(|| "-deps-cache-dir requires a value".to_string())?;
                root = Some(PathBuf::from(path));
                index += 2;
            }
            option => return Err(format!("unknown cache option '{option}'")),
        }
    }
    Ok((all, root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_command_rejects_missing_values_and_unknown_options() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parse(&args(&["clean", "--all", "-deps-cache-dir", "/cache"])).unwrap(),
            (true, Some(PathBuf::from("/cache")))
        );
        assert!(parse(&args(&["clean", "-deps-cache-dir"])).is_err());
        assert!(parse(&args(&["clean", "--unknown"])).is_err());
    }
}
