//! The C preprocessor (C11 §6.10) that cinterop runs headers through.
//!
//! It is a complete preprocessor for what system and library headers use: object- and
//! function-like macros with `#`, `##`, `__VA_ARGS__` and `__VA_OPT__`; the conditional
//! directives and their integer constant expressions; `#include`, `#include_next` and
//! `#pragma once`; and the `__has_include` family. Macro expansion follows the standard's
//! rescanning rule with per-token hide sets (Prosser's algorithm), so recursive and
//! self-referential macros (`#define errno errno`) stop where the standard says.
//!
//! It also keeps every macro definition, with the file it was made in: cinterop turns a header's
//! constant macros into Kotlin constants, and the header filter decides which files count.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::lexer::{tokenize, Kind, Location, Token};

/// Where a header is looked for.
#[derive(Clone, Debug)]
pub(crate) enum IncludeDir {
    /// A directory on disk: the target's sysroot, or a library's own include directory.
    Disk(PathBuf),
    /// The compiler's own headers (`stddef.h`, `stdarg.h`, …), which krusty carries instead of a
    /// C compiler's resource directory. See `builtin_headers.rs`.
    Builtin(&'static [(&'static str, &'static str)]),
}

/// A preprocessing failure: where, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreprocessError {
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) message: String,
}

impl std::fmt::Display for PreprocessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}: {}", self.file, self.line, self.message)
    }
}

/// One macro definition.
#[derive(Clone, Debug)]
pub(crate) struct Macro {
    /// `None` for an object-like macro; the parameter names for a function-like one.
    pub(crate) params: Option<Vec<Rc<str>>>,
    pub(crate) variadic: bool,
    pub(crate) body: Vec<Token>,
    /// The file the definition is in; `None` for a predefined or command-line macro.
    pub(crate) file: Option<Rc<str>>,
}

/// One open conditional group.
struct Conditional {
    /// Whether the group's current branch is being emitted.
    active: bool,
    /// Whether some branch of the group was taken already (later `#elif`s are then skipped).
    taken: bool,
    seen_else: bool,
}

/// An open source file: where it was found, for `#include_next`.
struct OpenFile {
    /// Index into the search path of the directory the file was found in; `None` for the
    /// main file or one found relative to its includer.
    found_in: Option<usize>,
    path: Rc<str>,
    conditional_depth: usize,
}

pub(crate) struct Preprocessor {
    include_dirs: Vec<IncludeDir>,
    pub(crate) macros: HashMap<Rc<str>, Macro>,
    /// Macro names in definition order (the last definition wins), for a stable constant order.
    pub(crate) macro_order: Vec<Rc<str>>,
    once: HashSet<String>,
    queue: VecDeque<Token>,
    files: Vec<OpenFile>,
    conditionals: Vec<Conditional>,
    counter: u64,
    /// Files whose contents were included, in first-inclusion order.
    pub(crate) included: Vec<Rc<str>>,
}

/// The value of a `#if` expression: C's `intmax_t`/`uintmax_t` arithmetic (C11 §6.10.1p4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Value {
    bits: u64,
    unsigned: bool,
}

impl Preprocessor {
    pub(crate) fn new(include_dirs: Vec<IncludeDir>) -> Self {
        Self {
            include_dirs,
            macros: HashMap::new(),
            macro_order: Vec::new(),
            once: HashSet::new(),
            queue: VecDeque::new(),
            files: Vec::new(),
            conditionals: Vec::new(),
            counter: 0,
            included: Vec::new(),
        }
    }

    /// Define the macros a source of `#define` lines defines, as predefined (no file).
    pub(crate) fn predefine(&mut self, source: &str) -> Result<(), PreprocessError> {
        let tokens = tokenize("<predefined>", source).map_err(|error| PreprocessError {
            file: error.file,
            line: error.line,
            message: error.message,
        })?;
        let mut index = 0;
        while index < tokens.len() {
            let end = tokens[index + 1..]
                .iter()
                .position(|token| token.at_line_start)
                .map_or(tokens.len(), |at| index + 1 + at);
            let line = &tokens[index..end];
            if line.len() >= 2 && line[0].is("#") && line[1].is("define") {
                self.define(&line[2..], None)?;
            } else if line.len() >= 3 && line[0].is("#") && line[1].is("undef") {
                self.macros.remove(&line[2].text);
            } else {
                return Err(error_at(
                    &line[0],
                    "only #define and #undef may be predefined",
                ));
            }
            index = end;
        }
        Ok(())
    }

    /// Preprocess the file at `path` (with `source` as its text) to the tokens the parser reads.
    pub(crate) fn run(&mut self, path: &str, source: &str) -> Result<Vec<Token>, PreprocessError> {
        self.push_file(path, source, None)?;
        let mut out = Vec::new();
        while let Some(token) = self.next_expanded()? {
            out.push(token);
        }
        Ok(out)
    }

    fn push_file(
        &mut self,
        path: &str,
        source: &str,
        found_in: Option<usize>,
    ) -> Result<(), PreprocessError> {
        let mut tokens = tokenize(path, source).map_err(|error| PreprocessError {
            file: error.file,
            line: error.line,
            message: error.message,
        })?;
        let path: Rc<str> = Rc::from(path);
        if !self.included.contains(&path) {
            self.included.push(path.clone());
        }
        let location = Rc::new(Location {
            file: path.clone(),
            line: tokens.last().map_or(1, |token| token.location.line),
        });
        tokens.push(Token {
            kind: Kind::FileEnd,
            text: Rc::from(""),
            at_line_start: true,
            space_before: false,
            location,
            hide: Rc::from(Vec::new()),
        });
        self.files.push(OpenFile {
            found_in,
            path,
            conditional_depth: self.conditionals.len(),
        });
        for token in tokens.into_iter().rev() {
            self.queue.push_front(token);
        }
        Ok(())
    }

    /// The next token from the input, before macro expansion, with directives processed.
    fn next_raw(&mut self) -> Result<Option<Token>, PreprocessError> {
        loop {
            let Some(token) = self.queue.pop_front() else {
                return Ok(None);
            };
            if token.kind == Kind::FileEnd {
                let file = self.files.pop().expect("a FileEnd closes an open file");
                if self.conditionals.len() != file.conditional_depth {
                    return Err(error_at(&token, "unterminated conditional directive"));
                }
                continue;
            }
            if token.at_line_start && token.is("#") && token.hide.is_empty() {
                let line = self.take_line();
                self.directive(&token, line)?;
                continue;
            }
            if !self.active() {
                continue;
            }
            return Ok(Some(token));
        }
    }

    fn active(&self) -> bool {
        self.conditionals
            .iter()
            .all(|conditional| conditional.active)
    }

    /// The rest of the current line.
    fn take_line(&mut self) -> Vec<Token> {
        let mut line = Vec::new();
        while let Some(token) = self.queue.front() {
            if token.at_line_start || token.kind == Kind::FileEnd {
                break;
            }
            line.push(self.queue.pop_front().expect("peeked"));
        }
        line
    }

    fn directive(&mut self, hash: &Token, line: Vec<Token>) -> Result<(), PreprocessError> {
        let Some(name) = line.first() else {
            return Ok(()); // the null directive
        };
        let name_text = name.text.clone();
        let rest = &line[1..];
        // Conditionals are tracked even inside skipped groups, so nesting stays balanced.
        match &*name_text {
            "if" | "ifdef" | "ifndef" => {
                let outer = self.active();
                let condition = outer
                    && match &*name_text {
                        "if" => self.condition(name, rest)?,
                        "ifdef" => self.defined_operand(name, rest)?,
                        _ => !self.defined_operand(name, rest)?,
                    };
                self.conditionals.push(Conditional {
                    active: condition,
                    taken: condition || !outer,
                    seen_else: false,
                });
                return Ok(());
            }
            "elif" | "elifdef" | "elifndef" | "else" => {
                let depth = self.conditionals.len();
                if depth == 0 || depth <= self.files.last().map_or(0, |file| file.conditional_depth)
                {
                    return Err(error_at(name, &format!("#{name_text} without #if")));
                }
                let outer = self.conditionals[..depth - 1]
                    .iter()
                    .all(|conditional| conditional.active);
                let current = &self.conditionals[depth - 1];
                if current.seen_else {
                    return Err(error_at(name, &format!("#{name_text} after #else")));
                }
                let taken = current.taken;
                let condition = if taken || !outer {
                    false
                } else {
                    match &*name_text {
                        "else" => true,
                        "elif" => self.condition(name, rest)?,
                        "elifdef" => self.defined_operand(name, rest)?,
                        _ => !self.defined_operand(name, rest)?,
                    }
                };
                let current = &mut self.conditionals[depth - 1];
                current.active = condition;
                current.taken = taken || condition;
                current.seen_else = &*name_text == "else";
                return Ok(());
            }
            "endif" => {
                let depth = self.conditionals.len();
                if depth == 0 || depth <= self.files.last().map_or(0, |file| file.conditional_depth)
                {
                    return Err(error_at(name, "#endif without #if"));
                }
                self.conditionals.pop();
                return Ok(());
            }
            _ => {}
        }
        if !self.active() {
            return Ok(());
        }
        match &*name_text {
            "define" => {
                let file = self.files.last().map(|file| file.path.clone());
                self.define(rest, file)
            }
            "undef" => {
                if let Some(target) = rest.first() {
                    self.macros.remove(&target.text);
                }
                Ok(())
            }
            "include" | "include_next" | "import" => {
                self.include(name, rest, &*name_text == "include_next")
            }
            "pragma" => {
                if rest.first().is_some_and(|token| token.is("once")) {
                    if let Some(file) = self.files.last() {
                        self.once.insert(file.path.to_string());
                    }
                }
                Ok(())
            }
            "error" => Err(error_at(hash, &format!("#error {}", spelling(rest).trim()))),
            // A warning, a line marker or an identification string changes nothing cinterop reads.
            "warning" | "line" | "ident" | "sccs" | "assert" | "unassert" => Ok(()),
            _ if name.kind == Kind::Number => Ok(()), // `# 12 "file"` line markers
            other => Err(error_at(name, &format!("unknown directive #{other}"))),
        }
    }

    fn defined_operand(&self, directive: &Token, rest: &[Token]) -> Result<bool, PreprocessError> {
        let Some(name) = rest.first().filter(|token| token.kind == Kind::Identifier) else {
            return Err(error_at(directive, "expected a macro name"));
        };
        Ok(self.is_defined(&name.text))
    }

    fn is_defined(&self, name: &str) -> bool {
        self.macros.contains_key(name) || builtin_macro(name)
    }

    fn define(&mut self, line: &[Token], file: Option<Rc<str>>) -> Result<(), PreprocessError> {
        let Some(name) = line.first().filter(|token| token.kind == Kind::Identifier) else {
            return Err(PreprocessError {
                file: file.as_deref().unwrap_or("<predefined>").to_string(),
                line: line.first().map_or(0, |token| token.location.line),
                message: "#define needs a macro name".to_string(),
            });
        };
        let mut body_start = 1;
        let mut params = None;
        let mut variadic = false;
        if line
            .get(1)
            .is_some_and(|token| token.is("(") && !token.space_before)
        {
            let mut names = Vec::new();
            let mut index = 2;
            loop {
                let Some(token) = line.get(index) else {
                    return Err(error_at(name, "unterminated macro parameter list"));
                };
                index += 1;
                if token.is(")") {
                    break;
                }
                if token.is(",") {
                    continue;
                }
                if token.is("...") {
                    variadic = true;
                    names.push(Rc::from("__VA_ARGS__"));
                } else if token.kind == Kind::Identifier {
                    if line.get(index).is_some_and(|next| next.is("...")) {
                        // A GNU named variadic parameter: `args...`.
                        variadic = true;
                        index += 1;
                    }
                    names.push(token.text.clone());
                } else {
                    return Err(error_at(token, "invalid macro parameter"));
                }
            }
            params = Some(names);
            body_start = index;
        }
        let mut body = line[body_start..].to_vec();
        if let Some(first) = body.first_mut() {
            first.space_before = false;
        }
        let key = name.text.clone();
        self.macro_order.push(key.clone());
        self.macros.insert(
            key,
            Macro {
                params,
                variadic,
                body,
                file,
            },
        );
        Ok(())
    }

    fn include(
        &mut self,
        directive: &Token,
        rest: &[Token],
        next: bool,
    ) -> Result<(), PreprocessError> {
        let mut operand = rest.to_vec();
        if !operand
            .first()
            .is_some_and(|token| token.kind == Kind::StringLiteral || token.is("<"))
        {
            operand = self.expand_list(operand)?;
        }
        let (name, quoted) = header_name(&operand)
            .ok_or_else(|| error_at(directive, "#include expects \"FILENAME\" or <FILENAME>"))?;
        let (path, source, found_in) = self
            .find_header(&name, quoted, next)
            .ok_or_else(|| error_at(directive, &format!("'{name}' file not found")))?;
        if self.once.contains(&path) {
            return Ok(());
        }
        if self.files.len() > 200 {
            return Err(error_at(directive, "#include nested too deeply"));
        }
        self.push_file(&path, &source, found_in)
    }

    /// Find a header: next to the including file for a quoted name, then along the search path;
    /// for `#include_next`, only past the directory the including file was found in.
    fn find_header(
        &self,
        name: &str,
        quoted: bool,
        next: bool,
    ) -> Option<(String, String, Option<usize>)> {
        let current = self.files.last();
        if Path::new(name).is_absolute() {
            return std::fs::read_to_string(name)
                .ok()
                .map(|source| (name.to_string(), source, None));
        }
        if quoted && !next {
            if let Some(file) = current {
                if let Some(directory) = Path::new(&*file.path).parent() {
                    let candidate = directory.join(name);
                    if let Ok(source) = std::fs::read_to_string(&candidate) {
                        return Some((
                            candidate.to_string_lossy().into_owned(),
                            source,
                            file.found_in,
                        ));
                    }
                }
            }
        }
        let start = if next {
            current
                .and_then(|file| file.found_in)
                .map_or(0, |index| index + 1)
        } else {
            0
        };
        for (index, directory) in self.include_dirs.iter().enumerate().skip(start) {
            match directory {
                IncludeDir::Disk(root) => {
                    let candidate = root.join(name);
                    if let Ok(source) = std::fs::read_to_string(&candidate) {
                        return Some((
                            candidate.to_string_lossy().into_owned(),
                            source,
                            Some(index),
                        ));
                    }
                }
                IncludeDir::Builtin(files) => {
                    if let Some((file, source)) = files.iter().find(|(file, _)| *file == name) {
                        return Some((
                            format!("<builtin:{index}>/{file}"),
                            source.to_string(),
                            Some(index),
                        ));
                    }
                }
            }
        }
        None
    }

    // ---- macro expansion ----------------------------------------------------------------------

    /// The next fully macro-expanded token.
    fn next_expanded(&mut self) -> Result<Option<Token>, PreprocessError> {
        loop {
            let Some(token) = self.next_raw()? else {
                return Ok(None);
            };
            if token.kind != Kind::Identifier {
                return Ok(Some(token));
            }
            if token.is("_Pragma") {
                // `_Pragma ( string-literal )` is a pragma, which nothing here reads.
                self.skip_parenthesized_operand(&token)?;
                continue;
            }
            if self.expand_one(&token)? {
                continue;
            }
            return Ok(Some(token));
        }
    }

    fn skip_parenthesized_operand(&mut self, at: &Token) -> Result<(), PreprocessError> {
        let Some(open) = self.next_raw()? else {
            return Err(error_at(at, "expected '('"));
        };
        if !open.is("(") {
            return Err(error_at(at, "expected '('"));
        }
        let mut depth = 1;
        while depth > 0 {
            let Some(token) = self.next_raw()? else {
                return Err(error_at(at, "unterminated operand"));
            };
            if token.is("(") {
                depth += 1;
            } else if token.is(")") {
                depth -= 1;
            }
        }
        Ok(())
    }

    /// Expand `token` if it names a macro (or a builtin one), pushing the result back onto the
    /// input to be rescanned. Whether it did.
    fn expand_one(&mut self, token: &Token) -> Result<bool, PreprocessError> {
        let name = token.text.clone();
        if token.is_hidden(&name) {
            return Ok(false);
        }
        if let Some(replacement) = self.builtin_expansion(token)? {
            self.queue.push_front(replacement);
            return Ok(true);
        }
        let Some(definition) = self.macros.get(&name).cloned() else {
            return Ok(false);
        };
        let Some(params) = &definition.params else {
            let hide = with_hidden(&token.hide, &name);
            let body = self.substitute_object(&definition, token, &hide);
            for replacement in body.into_iter().rev() {
                self.queue.push_front(replacement);
            }
            return Ok(true);
        };
        // A function-like macro's name not followed by `(` is an ordinary identifier.
        if !self.peek_is_open_paren()? {
            return Ok(false);
        }
        self.next_raw()?; // `(`
        let (arguments, close) =
            self.collect_arguments(token, params.len(), definition.variadic)?;
        let hide = with_hidden(&intersect(&token.hide, &close.hide), &name);
        let body = self.substitute(&definition, params, &arguments, &hide)?;
        let mut body = body;
        if let Some(first) = body.first_mut() {
            first.space_before = token.space_before || token.at_line_start;
        }
        for replacement in body.into_iter().rev() {
            self.queue.push_front(replacement);
        }
        Ok(true)
    }

    fn substitute_object(
        &self,
        definition: &Macro,
        token: &Token,
        hide: &Rc<[Rc<str>]>,
    ) -> Vec<Token> {
        let mut body: Vec<Token> = definition
            .body
            .iter()
            .map(|part| Token {
                hide: hide.clone(),
                at_line_start: false,
                location: token.location.clone(),
                ..part.clone()
            })
            .collect();
        body = paste_all(body).unwrap_or_default();
        if let Some(first) = body.first_mut() {
            first.space_before = token.space_before || token.at_line_start;
        }
        body
    }

    /// Whether the next token (skipping nothing but the end of a line) is `(`.
    fn peek_is_open_paren(&mut self) -> Result<bool, PreprocessError> {
        // Directives between a macro name and its `(` are not supported; the next raw token
        // decides, as it does for every compiler on real headers.
        Ok(match self.queue.front() {
            None => false,
            Some(token) if token.kind == Kind::FileEnd => false,
            Some(token) if token.at_line_start && token.is("#") && token.hide.is_empty() => false,
            Some(token) => token.is("("),
        })
    }

    /// Read the arguments of an invocation whose `(` was consumed, up to the matching `)`.
    fn collect_arguments(
        &mut self,
        name: &Token,
        count: usize,
        variadic: bool,
    ) -> Result<(Vec<Vec<Token>>, Token), PreprocessError> {
        let mut arguments: Vec<Vec<Token>> = vec![Vec::new()];
        let mut depth = 0;
        loop {
            let Some(token) = self.next_raw()? else {
                return Err(error_at(
                    name,
                    &format!("unterminated invocation of macro '{}'", name.text),
                ));
            };
            if token.is("(") {
                depth += 1;
            } else if token.is(")") {
                if depth == 0 {
                    // `F()` for a one-parameter macro passes one empty argument.
                    if count == 0 && arguments.len() == 1 && arguments[0].is_empty() {
                        arguments.clear();
                    }
                    if arguments.len() < count && variadic && arguments.len() + 1 == count {
                        arguments.push(Vec::new());
                    }
                    if arguments.len() != count {
                        return Err(error_at(
                            name,
                            &format!(
                                "macro '{}' takes {count} arguments, {} given",
                                name.text,
                                arguments.len()
                            ),
                        ));
                    }
                    return Ok((arguments, token));
                }
                depth -= 1;
            } else if token.is(",") && depth == 0 && !(variadic && arguments.len() == count) {
                arguments.push(Vec::new());
                continue;
            }
            let mut token = token;
            token.at_line_start = false;
            arguments
                .last_mut()
                .expect("one argument at least")
                .push(token);
        }
    }

    fn substitute(
        &mut self,
        definition: &Macro,
        params: &[Rc<str>],
        arguments: &[Vec<Token>],
        hide: &Rc<[Rc<str>]>,
    ) -> Result<Vec<Token>, PreprocessError> {
        let parameter = |token: &Token| {
            (token.kind == Kind::Identifier)
                .then(|| params.iter().position(|param| *param == token.text))
                .flatten()
        };
        let body = &definition.body;
        let variadic_empty =
            definition.variadic && arguments.last().is_none_or(|argument| argument.is_empty());
        let mut out: Vec<Token> = Vec::new();
        let mut index = 0;
        while index < body.len() {
            let token = &body[index];
            // `#param`: the argument's spelling as a string literal.
            if token.is("#") {
                if let Some(param) = body.get(index + 1).and_then(parameter) {
                    out.push(Token {
                        kind: Kind::StringLiteral,
                        text: Rc::from(stringize(&arguments[param])),
                        ..token.clone()
                    });
                    index += 2;
                    continue;
                }
            }
            if token.is("__VA_OPT__") && definition.variadic {
                // `__VA_OPT__ ( content )`: content when the variable arguments are non-empty.
                let mut depth = 0;
                let mut end = index + 1;
                while end < body.len() {
                    if body[end].is("(") {
                        depth += 1;
                    } else if body[end].is(")") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    end += 1;
                }
                if !variadic_empty {
                    let inner = Macro {
                        body: body[index + 2..end].to_vec(),
                        ..definition.clone()
                    };
                    out.extend(self.substitute(&inner, params, arguments, hide)?);
                }
                index = end + 1;
                continue;
            }
            let pasted_left = out
                .last()
                .is_some_and(|last: &Token| last.is("##") && last.hide.is_empty());
            let pasted_right = body.get(index + 1).is_some_and(|next| next.is("##"));
            if let Some(param) = parameter(token) {
                let argument = &arguments[param];
                // GNU `, ## __VA_ARGS__`: the comma vanishes when the arguments are empty, and
                // is kept, unpasted, when they are not.
                if pasted_left
                    && definition.variadic
                    && param == params.len() - 1
                    && out.len() >= 2
                    && out[out.len() - 2].is(",")
                {
                    out.pop();
                    if argument.is_empty() {
                        out.pop();
                    } else {
                        let mut expanded = self.expand_list(argument.clone())?;
                        if let Some(first) = expanded.first_mut() {
                            first.space_before = true;
                        }
                        out.extend(expanded);
                    }
                    index += 1;
                    continue;
                }
                let replacement = if pasted_left || pasted_right {
                    argument.clone()
                } else {
                    self.expand_list(argument.clone())?
                };
                let mut replacement = replacement;
                if let Some(first) = replacement.first_mut() {
                    first.space_before = token.space_before;
                }
                if replacement.is_empty() && (pasted_left || pasted_right) {
                    // An empty operand of `##` is a placemarker.
                    out.push(Token {
                        kind: Kind::Other,
                        text: Rc::from(""),
                        ..token.clone()
                    });
                } else {
                    out.extend(replacement);
                }
                index += 1;
                continue;
            }
            let mut copy = token.clone();
            // Mark the body's own `##` so a `##` that arrived in an argument is not an operator.
            if copy.is("##") {
                copy.hide = Rc::from(Vec::new());
            }
            out.push(copy);
            index += 1;
        }
        let pasted = paste_all(out).map_err(|message| PreprocessError {
            file: body
                .first()
                .map_or(String::new(), |token| token.location.file.to_string()),
            line: body.first().map_or(0, |token| token.location.line),
            message,
        })?;
        Ok(pasted
            .into_iter()
            .filter(|token| !(token.kind == Kind::Other && token.text.is_empty()))
            .map(|token| Token {
                hide: union(&token.hide, hide),
                at_line_start: false,
                ..token
            })
            .collect())
    }

    /// Fully macro-expand a token list on its own (an argument, a `#if` line, an include).
    fn expand_list(&mut self, tokens: Vec<Token>) -> Result<Vec<Token>, PreprocessError> {
        // Expansion reads from the queue; run it on a private one. The open files stay, so
        // `__has_include_next` in an `#if` continues from the directory its file came from.
        let saved_queue = std::mem::take(&mut self.queue);
        let saved_conditionals = std::mem::take(&mut self.conditionals);
        self.queue = tokens
            .into_iter()
            .map(|mut token| {
                token.at_line_start = false;
                token
            })
            .collect();
        let mut out = Vec::new();
        let result = loop {
            let Some(token) = self.queue.pop_front() else {
                break Ok(());
            };
            if token.kind == Kind::Identifier {
                match self.expand_one(&token) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => break Err(error),
                }
            }
            out.push(token);
        };
        self.queue = saved_queue;
        self.conditionals = saved_conditionals;
        result.map(|()| out)
    }

    /// The replacement for a builtin macro (`__LINE__`, `__has_include(…)`, …), consuming its
    /// operand from the input; `None` when `token` names none.
    fn builtin_expansion(&mut self, token: &Token) -> Result<Option<Token>, PreprocessError> {
        let number = |value: String| Token {
            kind: Kind::Number,
            text: Rc::from(value),
            hide: Rc::from(Vec::new()),
            at_line_start: false,
            ..token.clone()
        };
        Ok(Some(match &*token.text {
            "__LINE__" => number(token.location.line.to_string()),
            "__FILE__" => Token {
                kind: Kind::StringLiteral,
                text: Rc::from(format!("{:?}", &*token.location.file)),
                ..number(String::new())
            },
            "__COUNTER__" => {
                self.counter += 1;
                number((self.counter - 1).to_string())
            }
            "__has_include" | "__has_include_next" => {
                let operand = self.builtin_operand(token)?;
                let operand = if operand
                    .first()
                    .is_some_and(|first| first.kind == Kind::StringLiteral || first.is("<"))
                {
                    operand
                } else {
                    self.expand_list(operand)?
                };
                let (name, quoted) = header_name(&operand)
                    .ok_or_else(|| error_at(token, "__has_include expects a header name"))?;
                let found = self
                    .find_header(&name, quoted, &*token.text == "__has_include_next")
                    .is_some();
                number(u8::from(found).to_string())
            }
            // Attributes, builtins and language features: cinterop reads declarations, never
            // code that would use them, so every header's fallback spelling is the one to take.
            "__has_attribute"
            | "__has_c_attribute"
            | "__has_cpp_attribute"
            | "__has_declspec_attribute"
            | "__has_builtin"
            | "__has_feature"
            | "__has_extension"
            | "__has_warning"
            | "__is_identifier" => {
                let operand = self.builtin_operand(token)?;
                let answer = &*token.text == "__is_identifier"
                    && operand
                        .first()
                        .is_some_and(|name| name.kind == Kind::Identifier);
                number(u8::from(answer).to_string())
            }
            _ => return Ok(None),
        }))
    }

    /// The parenthesized operand of a builtin, from the input.
    fn builtin_operand(&mut self, token: &Token) -> Result<Vec<Token>, PreprocessError> {
        let open = self.queue.pop_front();
        if !open.as_ref().is_some_and(|open| open.is("(")) {
            return Err(error_at(
                token,
                &format!("missing '(' after {}", token.text),
            ));
        }
        let mut operand = Vec::new();
        let mut depth = 0;
        loop {
            let Some(next) = self.queue.pop_front() else {
                return Err(error_at(token, &format!("unterminated {}", token.text)));
            };
            if next.is("(") {
                depth += 1;
            } else if next.is(")") {
                if depth == 0 {
                    return Ok(operand);
                }
                depth -= 1;
            }
            operand.push(next);
        }
    }

    // ---- #if expressions ------------------------------------------------------------------------

    fn condition(&mut self, directive: &Token, line: &[Token]) -> Result<bool, PreprocessError> {
        // `defined X` and `defined(X)` are read before expansion, so `X` is never expanded.
        let mut tokens = Vec::with_capacity(line.len());
        let mut index = 0;
        while index < line.len() {
            let token = &line[index];
            if token.is("defined") {
                let (name, consumed) = if line.get(index + 1).is_some_and(|next| next.is("(")) {
                    (line.get(index + 2), 4)
                } else {
                    (line.get(index + 1), 2)
                };
                let Some(name) = name.filter(|name| name.kind == Kind::Identifier) else {
                    return Err(error_at(token, "'defined' needs a macro name"));
                };
                tokens.push(Token {
                    kind: Kind::Number,
                    text: Rc::from(if self.is_defined(&name.text) {
                        "1"
                    } else {
                        "0"
                    }),
                    ..token.clone()
                });
                index += consumed;
                continue;
            }
            tokens.push(token.clone());
            index += 1;
        }
        let expanded = self.expand_list(tokens)?;
        // An identifier left after expansion (including a `defined` a macro produced) is 0.
        let mut values = Vec::with_capacity(expanded.len());
        let mut index = 0;
        while index < expanded.len() {
            let token = &expanded[index];
            if token.is("defined") {
                let (name, consumed) = if expanded.get(index + 1).is_some_and(|next| next.is("(")) {
                    (expanded.get(index + 2), 4)
                } else {
                    (expanded.get(index + 1), 2)
                };
                let defined = name.is_some_and(|name| self.is_defined(&name.text));
                values.push(Token {
                    kind: Kind::Number,
                    text: Rc::from(if defined { "1" } else { "0" }),
                    ..token.clone()
                });
                index += consumed;
                continue;
            }
            if token.kind == Kind::Identifier {
                values.push(Token {
                    kind: Kind::Number,
                    text: Rc::from("0"),
                    ..token.clone()
                });
            } else {
                values.push(token.clone());
            }
            index += 1;
        }
        let mut parser = Expression {
            tokens: &values,
            at: 0,
            directive,
        };
        let value = parser.conditional()?;
        if parser.at != values.len() {
            return Err(error_at(
                values.get(parser.at).unwrap_or(directive),
                "token is not a valid binary operator in a preprocessor subexpression",
            ));
        }
        Ok(value.bits != 0)
    }
}

/// Whether `name` is a builtin macro, for `defined` and `#ifdef`.
fn builtin_macro(name: &str) -> bool {
    matches!(
        name,
        "__LINE__"
            | "__FILE__"
            | "__COUNTER__"
            | "__has_include"
            | "__has_include_next"
            | "__has_attribute"
            | "__has_c_attribute"
            | "__has_cpp_attribute"
            | "__has_declspec_attribute"
            | "__has_builtin"
            | "__has_feature"
            | "__has_extension"
            | "__has_warning"
            | "__is_identifier"
    )
}

fn error_at(token: &Token, message: &str) -> PreprocessError {
    PreprocessError {
        file: token.location.file.to_string(),
        line: token.location.line,
        message: message.to_string(),
    }
}

fn with_hidden(hide: &Rc<[Rc<str>]>, name: &Rc<str>) -> Rc<[Rc<str>]> {
    if hide.iter().any(|hidden| hidden == name) {
        return hide.clone();
    }
    let mut names = hide.to_vec();
    names.push(name.clone());
    Rc::from(names)
}

fn intersect(left: &Rc<[Rc<str>]>, right: &Rc<[Rc<str>]>) -> Rc<[Rc<str>]> {
    Rc::from(
        left.iter()
            .filter(|name| right.contains(name))
            .cloned()
            .collect::<Vec<_>>(),
    )
}

fn union(left: &Rc<[Rc<str>]>, right: &Rc<[Rc<str>]>) -> Rc<[Rc<str>]> {
    if right.is_empty() {
        return left.clone();
    }
    let mut names = left.to_vec();
    for name in right.iter() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    Rc::from(names)
}

/// Spell tokens back as text, one space where space was.
pub(crate) fn spelling(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 && (token.space_before || token.at_line_start) {
            out.push(' ');
        }
        out.push_str(&token.text);
    }
    out
}

/// `#argument`: the argument's spelling as a string literal (C11 §6.10.3.2).
fn stringize(argument: &[Token]) -> String {
    let mut out = String::from("\"");
    for character in spelling(argument).chars() {
        if character == '"' || character == '\\' {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('"');
    out
}

/// Apply every `##` in `tokens` (C11 §6.10.3.3): each joins its neighbours into one token.
fn paste_all(tokens: Vec<Token>) -> Result<Vec<Token>, String> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut iter = tokens.into_iter().peekable();
    while let Some(token) = iter.next() {
        if token.is("##") && token.hide.is_empty() && !out.is_empty() {
            let Some(right) = iter.next() else {
                return Err("'##' cannot appear at the end of a macro expansion".to_string());
            };
            let left = out.pop().expect("checked non-empty");
            out.push(paste(&left, &right)?);
            continue;
        }
        out.push(token);
    }
    Ok(out)
}

fn paste(left: &Token, right: &Token) -> Result<Token, String> {
    let text = format!("{}{}", left.text, right.text);
    if left.text.is_empty() {
        return Ok(right.clone());
    }
    if right.text.is_empty() {
        return Ok(left.clone());
    }
    let tokens = tokenize(&left.location.file, &text).map_err(|error| error.message)?;
    let [token] = tokens.as_slice() else {
        return Err(format!(
            "pasting \"{}\" and \"{}\" does not give a valid preprocessing token",
            left.text, right.text
        ));
    };
    Ok(Token {
        kind: token.kind,
        text: token.text.clone(),
        ..left.clone()
    })
}

/// A header name from `"name"` or `< name >` tokens, and whether it was quoted.
fn header_name(tokens: &[Token]) -> Option<(String, bool)> {
    let first = tokens.first()?;
    if first.kind == Kind::StringLiteral {
        return Some((first.text.trim_matches('"').to_string(), true));
    }
    if first.is("<") {
        let end = tokens.iter().position(|token| token.is(">"))?;
        let mut name = String::new();
        for (index, token) in tokens[1..end].iter().enumerate() {
            if index > 0 && token.space_before {
                name.push(' ');
            }
            name.push_str(&token.text);
        }
        return Some((name, false));
    }
    None
}

// ---- #if expression evaluation ------------------------------------------------------------------

struct Expression<'a> {
    tokens: &'a [Token],
    at: usize,
    directive: &'a Token,
}

impl Expression<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn error(&self, message: &str) -> PreprocessError {
        error_at(self.peek().unwrap_or(self.directive), message)
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.peek().is_some_and(|token| token.is(text)) {
            self.at += 1;
            return true;
        }
        false
    }

    fn conditional(&mut self) -> Result<Value, PreprocessError> {
        let condition = self.binary(0)?;
        if !self.eat("?") {
            return Ok(condition);
        }
        let then = self.conditional()?;
        if !self.eat(":") {
            return Err(self.error("expected ':' in conditional expression"));
        }
        let otherwise = self.conditional()?;
        let unsigned = then.unsigned || otherwise.unsigned;
        let chosen = if condition.bits != 0 { then } else { otherwise };
        Ok(Value {
            bits: chosen.bits,
            unsigned,
        })
    }

    fn binary(&mut self, min_precedence: u8) -> Result<Value, PreprocessError> {
        let mut left = self.unary()?;
        loop {
            let Some(operator) = self.peek().map(|token| token.text.clone()) else {
                return Ok(left);
            };
            let Some(precedence) = precedence(&operator) else {
                return Ok(left);
            };
            if precedence < min_precedence {
                return Ok(left);
            }
            self.at += 1;
            let right = self.binary(precedence + 1)?;
            left = self.apply(&operator, left, right)?;
        }
    }

    fn apply(&self, operator: &str, left: Value, right: Value) -> Result<Value, PreprocessError> {
        let unsigned = left.unsigned || right.unsigned;
        let (l, r) = (left.bits, right.bits);
        let boolean = |value: bool| Value {
            bits: u64::from(value),
            unsigned: false,
        };
        let arithmetic = |bits: u64| Value { bits, unsigned };
        Ok(match operator {
            "*" => arithmetic(l.wrapping_mul(r)),
            "/" | "%" => {
                if r == 0 {
                    return Err(self.error("division by zero in preprocessor expression"));
                }
                let bits = if unsigned {
                    if operator == "/" {
                        l / r
                    } else {
                        l % r
                    }
                } else if operator == "/" {
                    (l as i64).wrapping_div(r as i64) as u64
                } else {
                    (l as i64).wrapping_rem(r as i64) as u64
                };
                arithmetic(bits)
            }
            "+" => arithmetic(l.wrapping_add(r)),
            "-" => arithmetic(l.wrapping_sub(r)),
            "<<" => Value {
                bits: l.wrapping_shl(r as u32),
                unsigned: left.unsigned,
            },
            ">>" => Value {
                bits: if left.unsigned {
                    l.wrapping_shr(r as u32)
                } else {
                    (l as i64).wrapping_shr(r as u32) as u64
                },
                unsigned: left.unsigned,
            },
            "<" | ">" | "<=" | ">=" => {
                let ordering = if unsigned {
                    l.cmp(&r)
                } else {
                    (l as i64).cmp(&(r as i64))
                };
                boolean(match operator {
                    "<" => ordering.is_lt(),
                    ">" => ordering.is_gt(),
                    "<=" => ordering.is_le(),
                    _ => ordering.is_ge(),
                })
            }
            "==" => boolean(l == r),
            "!=" => boolean(l != r),
            "&" => arithmetic(l & r),
            "^" => arithmetic(l ^ r),
            "|" => arithmetic(l | r),
            "&&" => boolean(l != 0 && r != 0),
            "||" => boolean(l != 0 || r != 0),
            "," => right,
            _ => return Err(self.error(&format!("unexpected operator '{operator}'"))),
        })
    }

    fn unary(&mut self) -> Result<Value, PreprocessError> {
        let Some(token) = self.peek().cloned() else {
            return Err(self.error("expected value in expression"));
        };
        self.at += 1;
        match &*token.text {
            "(" => {
                let value = self.conditional()?;
                if !self.eat(")") {
                    return Err(self.error("expected ')' in preprocessor expression"));
                }
                Ok(value)
            }
            "+" => self.unary(),
            "-" => {
                let value = self.unary()?;
                Ok(Value {
                    bits: value.bits.wrapping_neg(),
                    ..value
                })
            }
            "~" => {
                let value = self.unary()?;
                Ok(Value {
                    bits: !value.bits,
                    ..value
                })
            }
            "!" => {
                let value = self.unary()?;
                Ok(Value {
                    bits: u64::from(value.bits == 0),
                    unsigned: false,
                })
            }
            _ if token.kind == Kind::Number => integer_literal(&token.text)
                .map(|(bits, unsigned)| Value { bits, unsigned })
                .ok_or_else(|| error_at(&token, &format!("invalid integer '{}'", token.text))),
            _ if token.kind == Kind::CharLiteral => Ok(Value {
                bits: char_literal(&token.text).unwrap_or(0),
                unsigned: false,
            }),
            _ => Err(error_at(
                &token,
                &format!("invalid token '{}' in preprocessor expression", token.text),
            )),
        }
    }
}

fn precedence(operator: &str) -> Option<u8> {
    Some(match operator {
        "," => 0,
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "<" | ">" | "<=" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        _ => return None,
    })
}

/// An integer constant's value and whether its type is unsigned (C11 §6.4.4.1): an explicit `u`
/// suffix, or a value only `uintmax_t` can hold.
pub(crate) fn integer_literal(text: &str) -> Option<(u64, bool)> {
    let text = text.replace('\'', "");
    let lower = text.to_ascii_lowercase();
    let digits_end = lower
        .rfind(|character: char| !matches!(character, 'u' | 'l' | 'z'))
        .map_or(0, |at| at + 1);
    let (digits, suffix) = lower.split_at(digits_end);
    if !suffix
        .chars()
        .all(|character| matches!(character, 'u' | 'l' | 'z'))
    {
        return None;
    }
    let (radix, body) = if let Some(hex) = digits.strip_prefix("0x") {
        (16, hex)
    } else if let Some(binary) = digits.strip_prefix("0b") {
        (2, binary)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, &digits[1..])
    } else {
        (10, digits)
    };
    if body.is_empty() {
        return (digits == "0").then_some((0, suffix.contains('u')));
    }
    let value = u64::from_str_radix(body, radix).ok()?;
    let unsigned = suffix.contains('u') || (value > i64::MAX as u64);
    Some((value, unsigned))
}

/// A character constant's value (C11 §6.4.4.4), for the escapes headers use.
pub(crate) fn char_literal(text: &str) -> Option<u64> {
    let start = text.find('\'')?;
    let inner = &text[start + 1..text.len() - 1];
    let mut chars = inner.chars();
    let first = chars.next()?;
    if first != '\\' {
        return Some(u64::from(u32::from(first)));
    }
    let escape = chars.next()?;
    Some(match escape {
        'n' => 10,
        't' => 9,
        'r' => 13,
        '0'..='7' => u64::from_str_radix(&inner[1..], 8).ok()?,
        'x' => u64::from_str_radix(&inner[2..], 16).ok()?,
        'a' => 7,
        'b' => 8,
        'f' => 12,
        'v' => 11,
        'e' => 27,
        other => u64::from(u32::from(other)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preprocess(source: &str) -> String {
        let mut preprocessor = Preprocessor::new(Vec::new());
        let tokens = preprocessor.run("t.h", source).expect("preprocesses");
        spelling(&tokens)
    }

    #[test]
    fn object_and_function_like_macros_expand_and_rescan() {
        assert_eq!(
            preprocess("#define N 4\n#define SQ(x) ((x) * (x))\nint a[SQ(N + 1)];"),
            "int a[((4 + 1) * (4 + 1))];"
        );
    }

    #[test]
    fn a_self_referential_macro_stops_as_the_standard_says() {
        assert_eq!(
            preprocess(
                "#define errno (*__errno_location())\n#define foo foo\nint x = errno + foo;"
            ),
            "int x = (*__errno_location()) + foo;"
        );
        // From C11 §6.10.3.5 example 3.
        assert_eq!(
            preprocess(
                "#define x 3\n#define f(a) f(x * (a))\n#undef x\n#define x 2\n#define g f\n\
                 #define z z[0]\nf(y+1) + f(f(z)) % g(z);"
            ),
            "f(2 * (y+1)) + f(2 * (f(2 * (z[0])))) % f(2 * (z[0]));"
        );
    }

    #[test]
    fn stringizing_pasting_and_variadic_arguments() {
        assert_eq!(
            preprocess(
                "#define str(s) # s\n#define cat(a, b) a ## b\n\
                 #define call(f, ...) f(__VA_ARGS__)\n#define opt(f, ...) f(1 __VA_OPT__(,) __VA_ARGS__)\n\
                 #define gnu(fmt, args...) p(fmt, ## args)\n\
                 str(a \"b\") cat(x, 1) call(g, 1, 2) call(h) opt(k) opt(k, 2) gnu(\"%d\") gnu(\"%d\", 3)"
            ),
            "\"a \\\"b\\\"\" x1 g(1, 2) h() k(1) k(1, 2) p(\"%d\") p(\"%d\", 3)"
        );
    }

    #[test]
    fn conditionals_take_one_branch_and_evaluate_intmax_arithmetic() {
        assert_eq!(
            preprocess(
                "#define A 2\n#if defined(A) && A * 3 == 6\nyes\n#elif 1\nno\n#else\nno\n#endif\n\
                 #if -1 < 0u\nunsigned_wrong\n#else\nunsigned_right\n#endif\n\
                 #ifdef B\nno\n#elifndef B\nnot_b\n#endif\n#if (1 ? 2 : 3) == 2 && UNDEFINED == 0\nz\n#endif"
            ),
            "yes unsigned_right not_b z"
        );
    }

    #[test]
    fn an_error_directive_in_an_active_branch_fails() {
        let mut preprocessor = Preprocessor::new(Vec::new());
        let error = preprocessor
            .run(
                "t.h",
                "#if 0\n#error skipped\n#endif\n#error \"stop\" here\n",
            )
            .expect_err("fails");
        assert_eq!(
            error,
            PreprocessError {
                file: "t.h".to_string(),
                line: 4,
                message: "#error \"stop\" here".to_string(),
            }
        );
    }

    #[test]
    fn includes_search_builtin_directories_in_order_and_include_next_continues() {
        const FIRST: &[(&str, &str)] = &[("a.h", "#pragma once\nfirst\n#include_next <a.h>\n")];
        const SECOND: &[(&str, &str)] = &[("a.h", "second\n#define FROM_A 1\n")];
        let mut preprocessor = Preprocessor::new(vec![
            IncludeDir::Builtin(FIRST),
            IncludeDir::Builtin(SECOND),
        ]);
        let tokens = preprocessor
            .run(
                "t.h",
                "#include <a.h>\n#include <a.h>\n#if __has_include(<a.h>) && !__has_include(<b.h>)\nFROM_A\n#endif\n",
            )
            .expect("preprocesses");
        assert_eq!(spelling(&tokens), "first second 1");
        assert_eq!(
            preprocessor.macros["FROM_A"].file.as_deref(),
            Some("<builtin:1>/a.h")
        );
    }

    #[test]
    fn has_include_next_in_a_condition_searches_after_its_own_directory() {
        const FIRST: &[(&str, &str)] = &[(
            "a.h",
            "#if __has_include_next(<a.h>)\n#include_next <a.h>\n#else\nlast\n#endif\n",
        )];
        let mut preprocessor = Preprocessor::new(vec![IncludeDir::Builtin(FIRST)]);
        let tokens = preprocessor
            .run("t.h", "#include <a.h>\n")
            .expect("preprocesses");
        assert_eq!(spelling(&tokens), "last");
    }

    #[test]
    fn integer_literals_carry_their_signedness() {
        assert_eq!(integer_literal("0x1fULL"), Some((31, true)));
        assert_eq!(integer_literal("010"), Some((8, false)));
        assert_eq!(integer_literal("0"), Some((0, false)));
        assert_eq!(
            integer_literal("18446744073709551615"),
            Some((u64::MAX, true))
        );
        assert_eq!(integer_literal("1.5"), None);
    }
}
