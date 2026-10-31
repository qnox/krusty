//! kotlinc's `InlineScopesGenerator` (`-Xuse-inline-scopes-numbers`): the debug names an inlined
//! body's locals take when every inline scope of a method is numbered.
//!
//! Without numbers an inlined local is suffixed with `$iv` once per enclosing inlining. With them
//! each inline marker variable (`$i$f$…`, `$i$a$…`) opens a scope numbered in the order the method
//! inlines it, and every local of that scope ends with `\<scope>`. A marker also records the line
//! of its call site (`$i$f$f\3\12`) and a lambda's marker the scope it was declared in
//! (`$i$a$-f-K$g$1\4\12\3`, `0` for the method itself). The numbers are per method: each method
//! being generated owns one [`InlineScopes`], whose count of already numbered scopes offsets the
//! scopes of each body inlined into it.
//!
//! A body is numbered from its own variable table. A body kotlinc compiled with numbers declares
//! one unnumbered marker, its own, and its nested scopes are renumbered from their numbers. Any
//! other body was compiled with `$iv` suffixes, and the scope tree is read from those.

use std::collections::{HashMap, HashSet};

use crate::jvm::method_node::{LabelId, LocalVariable, MethodNode, Node};

/// `INLINE_SCOPE_NUMBER_SEPARATOR`.
const SEPARATOR: char = '\\';
/// `INLINE_FUN_VAR_SUFFIX`.
const INLINED_LOCAL_SUFFIX: &str = "$iv";
/// `LOCAL_VARIABLE_NAME_PREFIX_INLINE_FUNCTION`.
const FUNCTION_MARKER_PREFIX: &str = "$i$f$";
/// `LOCAL_VARIABLE_NAME_PREFIX_INLINE_ARGUMENT`.
const LAMBDA_MARKER_PREFIX: &str = "$i$a$";

/// The numbering state of the method being generated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InlineScopes {
    /// `inlinedScopes`: how many scopes the method has numbered so far.
    pub inlined: i32,
    /// `currentCallSiteLineNumber`: the line of the call whose body is numbered next.
    pub call_site_line: i32,
}

/// `JvmAbi.isFakeLocalVariableForInline`.
pub(crate) fn is_marker(name: &str) -> bool {
    name.starts_with(FUNCTION_MARKER_PREFIX) || name.starts_with(LAMBDA_MARKER_PREFIX)
}

/// The numbers a local's name ends with (`getInlineScopeInfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScopeInfo {
    scope: i32,
    call_site_line: Option<i32>,
    surrounding_scope: Option<i32>,
}

/// kotlinc's `toIntOrNull`: an optional sign and decimal digits that fit an `Int`.
fn int(text: &str) -> Option<i32> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `getInlineScopeInfo`: the up to three numbers after the first separator. A name without one is
/// read whole, so it has numbers only when it is itself a number.
fn scope_info(name: &str) -> Option<ScopeInfo> {
    let numbers = name.split_once(SEPARATOR).map_or(name, |(_, after)| after);
    let parts: Vec<&str> = numbers.split(SEPARATOR).collect();
    if parts.len() > 3 {
        return None;
    }
    Some(ScopeInfo {
        scope: int(parts[0])?,
        call_site_line: parts.get(1).copied().and_then(int),
        surrounding_scope: parts.get(2).copied().and_then(int),
    })
}

/// `dropInlineScopeInfo`.
fn without_scope_info(name: &str) -> &str {
    name.split_once(SEPARATOR)
        .map_or(name, |(before, _)| before)
}

/// The number of `$iv` suffixes a name ends with (`getInlineDepth`).
fn inline_depth(name: &str) -> i32 {
    let mut rest = name;
    let mut depth = 0;
    while let Some(shorter) = rest.strip_suffix(INLINED_LOCAL_SUFFIX) {
        rest = shorter;
        depth += 1;
    }
    depth
}

/// `computeNewVariableName`.
fn numbered_name(
    name: &str,
    scope: i32,
    call_site_line: Option<i32>,
    surrounding_scope: Option<i32>,
) -> String {
    let stripped = name.replace(INLINED_LOCAL_SUFFIX, "");
    let mut numbered = without_scope_info(&stripped).to_string();
    numbered.push(SEPARATOR);
    numbered.push_str(&scope.to_string());
    if let Some(line) = call_site_line {
        numbered.push(SEPARATOR);
        numbered.push_str(&line.to_string());
        if let Some(surrounding) = surrounding_scope {
            numbered.push(SEPARATOR);
            numbered.push_str(&surrounding.to_string());
        }
    }
    numbered
}

/// `updateCallSiteLineNumber`: a marker's call-site line moved into the caller's line space.
pub(crate) fn with_call_site_line(name: &str, map: impl FnOnce(i32) -> i32) -> String {
    let Some(info) = scope_info(name) else {
        return name.to_string();
    };
    let Some(line) = info.call_site_line else {
        return name.to_string();
    };
    let mapped = map(line);
    if mapped == line {
        return name.to_string();
    }
    let mut renamed = format!(
        "{}{SEPARATOR}{}{SEPARATOR}{mapped}",
        without_scope_info(name),
        info.scope
    );
    if let Some(surrounding) = info.surrounding_scope {
        renamed.push(SEPARATOR);
        renamed.push_str(&surrounding.to_string());
    }
    renamed
}

/// The scope a root body's `this` is renamed into (`calculateNewNameUsingScopeNumbers`): kotlinc
/// renames every name starting with `this` to the declaration-site `this_`, keeping its scope.
pub(crate) fn declaration_site_name(name: &str) -> String {
    if !name.starts_with("this") {
        return name.to_string();
    }
    match scope_info(name) {
        Some(info) => format!("this_{SEPARATOR}{}", info.scope),
        None => "this_".to_string(),
    }
}

/// The names of the markers a numbered body declares inside its own scope: every marker but the
/// first to start, which is the body's own and already has its call site.
pub(crate) fn nested_markers(node: &MethodNode) -> HashSet<String> {
    let label_index = label_indices(node);
    let mut markers: Vec<&LocalVariable> = node
        .local_variables
        .iter()
        .filter(|local| is_marker(&local.name))
        .collect();
    markers.sort_by_key(|local| label_index.get(&local.start).copied());
    markers
        .into_iter()
        .skip(1)
        .map(|local| local.name.clone())
        .collect()
}

/// `MethodInliner.updateCallSiteLineNumbers`: once a body is inlined, the call-site lines its
/// nested `markers` recorded in the body's own line space become the caller's, through `map`. A
/// line nothing maps becomes `-1`.
pub(crate) fn map_call_site_lines(
    node: &mut MethodNode,
    markers: &HashSet<String>,
    mut map: impl FnMut(u16) -> Option<u16>,
) {
    if markers.is_empty() {
        return;
    }
    for local in &mut node.local_variables {
        if is_marker(&local.name) && markers.contains(&local.name) {
            local.name = with_call_site_line(&local.name, |line| {
                u16::try_from(line)
                    .ok()
                    .and_then(&mut map)
                    .map_or(-1, i32::from)
            });
        }
    }
}

/// One scope of a body: the root (the body itself) or the scope a marker variable opens.
struct ScopeNode {
    marker: Option<usize>,
    scope: i32,
    nesting: i32,
    parent: Option<usize>,
    call_site_line: Option<i32>,
    surrounding_scope: Option<i32>,
    variables: Vec<usize>,
}

/// Which of kotlinc's three renamers numbers a body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scheme {
    /// `addInlineScopesInfoFromScopeNumbers`: the body was compiled with numbers.
    FromScopeNumbers,
    /// `addInlineScopesInfoFromIVSuffixes`: the body was compiled with `$iv` suffixes.
    FromSuffixes,
    /// `addInlineScopesInfoFromIVSuffixesWhenRegeneratingAnonymousObject`.
    RegeneratingObject,
}

impl InlineScopes {
    /// `addInlineScopesInfo`: number the scopes of `node`, a body about to be inlined into the
    /// method. `regenerating` is a method of an anonymous object being regenerated, whose scopes do
    /// not count toward the method's.
    pub(crate) fn number(&mut self, node: &mut MethodNode, regenerating: bool) {
        if node.local_variables.is_empty() {
            return;
        }
        let unnumbered = node
            .local_variables
            .iter()
            .filter(|local| is_marker(&local.name) && !local.name.contains(SEPARATOR))
            .count();
        let scheme = match (regenerating, unnumbered) {
            (true, 0) => return,
            (true, _) => Scheme::RegeneratingObject,
            (false, 1) => Scheme::FromScopeNumbers,
            (false, _) => Scheme::FromSuffixes,
        };
        let numbered = self.rename(node, scheme);
        if scheme != Scheme::RegeneratingObject {
            self.inlined += numbered;
        }
    }

    /// `VariableRenamer.renameVariables`: build the scope tree, then rename each scope's marker and
    /// locals. Returns how many scopes the body has.
    fn rename(&self, node: &mut MethodNode, scheme: Scheme) -> i32 {
        let label_index = label_indices(node);
        let label_lines = label_lines(node);
        let locals = &node.local_variables;
        let index_of = |label: LabelId| label_index.get(&label).copied();
        let mut order: Vec<usize> = (0..locals.len()).collect();
        // `sortedBy` is stable and orders a label outside the instructions (`null`) first.
        order.sort_by_key(|&local| index_of(locals[local].start));
        let contains = |outer: &LocalVariable, inner: &LocalVariable| -> bool {
            let (Some(start), Some(end), Some(inner_start), Some(inner_end)) = (
                index_of(outer.start),
                index_of(outer.end),
                index_of(inner.start),
                index_of(inner.end),
            ) else {
                return false;
            };
            start < inner_start && end >= inner_end
        };

        let root_nesting = match scheme {
            Scheme::RegeneratingObject => 0,
            Scheme::FromScopeNumbers | Scheme::FromSuffixes => -1,
        };
        let mut scopes = vec![ScopeNode {
            marker: None,
            scope: 0,
            nesting: root_nesting,
            parent: None,
            call_site_line: None,
            surrounding_scope: None,
            variables: Vec::new(),
        }];
        let mut unmatched = Vec::new();
        let mut seen = 0;
        let mut current = 0;
        for &local in &order {
            let variable = &locals[local];
            while let (Some(parent), Some(marker)) =
                (scopes[current].parent, scopes[current].marker)
            {
                if contains(&locals[marker], variable) {
                    break;
                }
                current = parent;
            }
            let name = variable.name.as_str();
            if is_marker(name) {
                seen += 1;
                let parent_nesting = scopes[current].nesting;
                scopes.push(ScopeNode {
                    marker: Some(local),
                    scope: seen,
                    nesting: parent_nesting,
                    parent: Some(current),
                    call_site_line: None,
                    surrounding_scope: None,
                    variables: std::mem::take(&mut unmatched),
                });
                current = scopes.len() - 1;
                let start_line = label_lines.get(&variable.start).copied();
                self.describe(&mut scopes, current, name, scheme, start_line);
                continue;
            }
            let is_root = scopes[current].parent.is_none();
            let skipped =
                scheme == Scheme::RegeneratingObject && !name.contains(INLINED_LOCAL_SUFFIX);
            if is_root && skipped {
                continue;
            }
            let belongs = match scheme {
                Scheme::FromScopeNumbers => match scope_info(name) {
                    Some(info) => info.scope == scopes[current].nesting,
                    None => !is_root,
                },
                Scheme::FromSuffixes | Scheme::RegeneratingObject => {
                    !is_root && inline_depth(name) == scopes[current].nesting
                }
            };
            if belongs {
                scopes[current].variables.push(local);
            } else {
                unmatched.push(local);
            }
        }

        let mut renamed: Vec<(usize, String)> = Vec::new();
        for scope in &scopes[1..] {
            let number = scope.scope + self.inlined;
            let marker = scope.marker.expect("only the root scope has no marker");
            renamed.push((
                marker,
                numbered_name(
                    &locals[marker].name,
                    number,
                    scope.call_site_line,
                    scope.surrounding_scope,
                ),
            ));
            for &variable in &scope.variables {
                renamed.push((
                    variable,
                    numbered_name(&locals[variable].name, number, None, None),
                ));
            }
        }
        for (local, name) in renamed {
            node.local_variables[local].name = name;
        }
        seen
    }

    /// `computeInlineScopeInfo` of the scope `at`, just opened by the marker `name`, which starts
    /// where `start_line` is in effect.
    fn describe(
        &self,
        scopes: &mut [ScopeNode],
        at: usize,
        name: &str,
        scheme: Scheme,
        start_line: Option<u16>,
    ) {
        let scope = scopes[at].scope;
        let lambda = name.starts_with(LAMBDA_MARKER_PREFIX);
        match scheme {
            Scheme::FromScopeNumbers => {
                let info = scope_info(name);
                scopes[at].nesting = info.map_or(0, |info| info.scope);
                scopes[at].call_site_line = Some(if scope == 1 {
                    self.call_site_line
                } else {
                    info.and_then(|info| info.call_site_line).unwrap_or(0)
                });
                if lambda {
                    scopes[at].surrounding_scope = Some(if scope == 1 {
                        0
                    } else {
                        match info.and_then(|info| info.surrounding_scope) {
                            Some(surrounding) => surrounding + self.inlined + 1,
                            None => -1,
                        }
                    });
                }
            }
            Scheme::FromSuffixes | Scheme::RegeneratingObject => {
                let parent_depth = scopes[at].nesting;
                scopes[at].nesting = if lambda {
                    inline_depth(name)
                } else {
                    parent_depth + 1
                };
                // A body compiled without numbers does not record its call sites: the line in
                // effect where the marker starts stands in for each nested one.
                scopes[at].call_site_line = Some(if scheme == Scheme::FromSuffixes && scope == 1 {
                    self.call_site_line
                } else {
                    start_line.map_or(0, i32::from)
                });
                if lambda {
                    scopes[at].surrounding_scope = Some(self.surrounding_scope(scopes, at));
                }
            }
        }
    }

    /// `computeSurroundingScopeNumber`: the nearest enclosing scope at the lambda's own depth.
    fn surrounding_scope(&self, scopes: &[ScopeNode], at: usize) -> i32 {
        if scopes[at].scope == 1 {
            return 0;
        }
        let depth = scopes[at].nesting;
        let mut next = scopes[at].parent;
        while let Some(scope) = next {
            if scopes[scope].parent.is_none() {
                break;
            }
            if scopes[scope].nesting == depth {
                return scopes[scope].scope + self.inlined;
            }
            next = scopes[scope].parent;
        }
        0
    }
}

/// `getLabelToIndexMap`: each label's position among the body's labels.
fn label_indices(node: &MethodNode) -> HashMap<LabelId, usize> {
    node.nodes
        .iter()
        .filter_map(|entry| match entry {
            Node::Label(label) => Some(*label),
            _ => None,
        })
        .enumerate()
        .map(|(index, label)| (label, index))
        .collect()
}

/// `getLabelToLineNumberMap`: the line in effect at each label, `0` before the first.
fn label_lines(node: &MethodNode) -> HashMap<LabelId, u16> {
    let mut lines = HashMap::new();
    let mut current = 0;
    for entry in &node.nodes {
        match entry {
            Node::Line { line, .. } => current = *line,
            Node::Label(label) => {
                lines.insert(*label, current);
            }
            Node::Insn(_) => {}
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body with a label at each offset its tables name, `lines` starting at their offsets and
    /// `locals` as `(start, length, name)`, as the class reader produces it.
    fn body(lines: &[(u16, u16)], locals: &[(u16, u16, &str)]) -> MethodNode {
        let mut offsets: Vec<u16> = lines.iter().map(|&(offset, _)| offset).collect();
        for &(start, length, _) in locals {
            offsets.extend([start, start + length]);
        }
        offsets.sort_unstable();
        offsets.dedup();
        let mut node = MethodNode::new(0, "f", "()V");
        let mut labels = HashMap::new();
        for offset in offsets {
            let label = node.new_label();
            labels.insert(offset, label);
            node.nodes.push(Node::Label(label));
            if let Some(&(_, line)) = lines.iter().find(|&&(at, _)| at == offset) {
                node.nodes.push(Node::Line { line, start: label });
            }
        }
        node.local_variables = locals
            .iter()
            .map(|&(start, length, name)| LocalVariable {
                name: name.to_string(),
                desc: "I".to_string(),
                start: labels[&start],
                end: labels[&(start + length)],
                slot: 0,
            })
            .collect();
        node
    }

    fn names(node: &MethodNode) -> Vec<&str> {
        node.local_variables
            .iter()
            .map(|local| local.name.as_str())
            .collect()
    }

    /// `inline fun outer(g: (Int) -> Int) { val o = 3; return twice { x -> g(x) + o } }` as kotlinc
    /// compiles it without numbers, inlined at line 2 of a method that numbered nothing yet.
    #[test]
    fn a_body_compiled_with_suffixes_is_numbered_from_its_scope_tree() {
        let mut node = body(
            &[(8, 3), (12, 5), (18, 3), (37, 5), (49, 3), (74, 5), (75, 3)],
            &[
                (18, 19, "$i$a$-twice-LibKt$outer$1"),
                (15, 22, "x"),
                (49, 19, "$i$a$-twice-LibKt$outer$1"),
                (46, 22, "x"),
                (12, 63, "$i$f$twice"),
                (39, 36, "a$iv"),
                (8, 68, "$i$f$outer"),
                (10, 66, "o"),
                (0, 76, "g"),
            ],
        );
        let mut scopes = InlineScopes {
            inlined: 0,
            call_site_line: 2,
        };
        scopes.number(&mut node, false);
        assert_eq!(
            names(&node),
            [
                "$i$a$-twice-LibKt$outer$1\\3\\5\\1",
                "x\\3",
                "$i$a$-twice-LibKt$outer$1\\4\\5\\1",
                "x\\4",
                "$i$f$twice\\2\\3",
                "a\\2",
                "$i$f$outer\\1\\2",
                "o\\1",
                // A parameter starts before the body's own marker and joins its scope.
                "g\\1",
            ]
        );
        assert_eq!(scopes.inlined, 4);
        assert_eq!(
            nested_markers(&node),
            HashSet::from([
                "$i$f$twice\\2\\3".to_string(),
                "$i$a$-twice-LibKt$outer$1\\3\\5\\1".to_string(),
                "$i$a$-twice-LibKt$outer$1\\4\\5\\1".to_string(),
            ])
        );
    }

    /// A lambda's node compiled with numbers declares only its own marker unnumbered. Inlined after
    /// four scopes, its own scope is the fifth and its locals follow it.
    #[test]
    fn a_body_compiled_with_numbers_keeps_its_scope_tree() {
        let mut node = body(
            &[(0, 2)],
            &[
                (2, 10, "$i$a$-outer-MainKt$box$r$1"),
                (4, 8, "z"),
                (0, 12, "y"),
            ],
        );
        let mut scopes = InlineScopes {
            inlined: 4,
            call_site_line: 4,
        };
        scopes.number(&mut node, false);
        assert_eq!(
            names(&node),
            ["$i$a$-outer-MainKt$box$r$1\\5\\4\\0", "z\\5", "y\\5"]
        );
        assert_eq!(scopes.inlined, 5);
    }

    #[test]
    fn a_call_site_line_moves_into_the_callers_lines() {
        assert_eq!(
            with_call_site_line("$i$f$twice\\2\\3", |line| line + 1),
            "$i$f$twice\\2\\4"
        );
        assert_eq!(
            with_call_site_line("$i$a$-twice-K$1\\3\\5\\1", |_| -1),
            "$i$a$-twice-K$1\\3\\-1\\1"
        );
        assert_eq!(with_call_site_line("x\\3", |_| 9), "x\\3");
    }

    #[test]
    fn this_becomes_the_declaration_site_this_of_its_scope() {
        assert_eq!(declaration_site_name("this\\2"), "this_\\2");
        assert_eq!(declaration_site_name("this"), "this_");
        assert_eq!(declaration_site_name("$this$map\\2"), "$this$map\\2");
    }
}
