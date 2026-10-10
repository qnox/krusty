//! `show modules`: the modules, as a table or as plain names.

use crate::module::ModuleHeader;

/// The modules sorted by name, in UTF-16 order as the toolchain sorts them.
fn sorted(headers: &[ModuleHeader]) -> Vec<&ModuleHeader> {
    let mut sorted: Vec<&ModuleHeader> = headers.iter().collect();
    sorted.sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
    sorted
}

/// `show modules --format=plain`: the module names, sorted.
pub fn module_names(headers: &[ModuleHeader]) -> Vec<&str> {
    sorted(headers)
        .into_iter()
        .map(|header| header.name.as_str())
        .collect()
}

/// `show modules`: the modules sorted by name, one row each with the module's name, product type
/// and the first line of its description, in a table with rounded borders.
pub fn modules_table(headers: &[ModuleHeader]) -> String {
    let rows: Vec<[&str; 3]> = sorted(headers)
        .iter()
        .map(|header| {
            let description = header.description.as_deref().unwrap_or("");
            let short = description.split('\n').next().unwrap_or("");
            [header.name.as_str(), header.product.name(), short]
        })
        .collect();
    table(["Name", "Type", "Short description"], &rows)
}

/// A table with a header row and rounded borders, every row separated by a rule.
fn table<const N: usize>(header: [&str; N], rows: &[[&str; N]]) -> String {
    let mut widths = header.map(|cell| cell.chars().count());
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let rule = |left: &str, middle: &str, right: &str| {
        let segments: Vec<String> = widths.iter().map(|width| "─".repeat(width + 2)).collect();
        format!("{left}{}{right}\n", segments.join(middle))
    };
    let line = |cells: &[&str; N]| {
        let padded: Vec<String> = cells
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!(" {cell}{} ", " ".repeat(width - cell.chars().count())))
            .collect();
        format!("│{}│\n", padded.join("│"))
    };
    let mut out = rule("╭", "┬", "╮");
    out.push_str(&line(&header));
    for row in rows {
        out.push_str(&rule("├", "┼", "┤"));
        out.push_str(&line(row));
    }
    out.push_str(&rule("╰", "┴", "╯"));
    out
}
