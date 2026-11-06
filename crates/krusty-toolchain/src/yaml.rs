//! A YAML document as the project model reads it: an index-based tree whose every node keeps its
//! source span, built from a standards-conformant event parser under explicit bounds.
//!
//! The grammar is `saphyr-parser`'s (YAML 1.2). This module only decides which of YAML's features a
//! Kotlin Toolchain file may use and how large it may be, and fails closed on the rest. Where the
//! toolchain rejects a feature, the problem carries the toolchain's message and position, and
//! reading continues so every such problem in the file is reported, as the toolchain reports them:
//!
//! * anchors and aliases are refused at the `&` or `*`;
//! * a `!!` tag is refused at the tag; where any other tag is written is kept on its node
//!   ([`Document::tag`]) for the reader to refuse, never dropped silently;
//! * a second document (`---`) is refused.
//!
//! Beyond the toolchain's own rules, krusty refuses outright (as the first and only problem):
//!
//! * a syntax error, with the parser's message;
//! * a non-scalar or duplicate mapping key;
//! * more than [`Limits::bytes`] of input, [`Limits::depth`] nested collections, or
//!   [`Limits::nodes`] nodes, each with the limit named.
//!
//! Scalars keep their style, because the schema distinguishes a plain `true` from a quoted `"true"`.

use std::fmt;

use saphyr_parser::{Event, Parser, ScalarStyle, Span as ParserSpan};

/// Bounds on one YAML file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub bytes: usize,
    pub depth: usize,
    pub nodes: usize,
}

impl Limits {
    /// Generous for any hand-written build file, small enough that a hostile one cannot exhaust
    /// memory or the stack.
    pub const DEFAULT: Limits = Limits {
        bytes: 1 << 20,
        depth: 64,
        nodes: 100_000,
    };
}

/// A 1-based line and column (in characters).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

/// The source range of a node: `start` inclusive, `end` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

fn position(marker: saphyr_parser::Marker) -> Position {
    Position {
        line: marker.line(),
        column: marker.col() + 1,
    }
}

impl Span {
    /// The empty range at `position`, for a problem a parser places at one point.
    pub fn point(position: Position) -> Self {
        Span {
            start: position,
            end: position,
        }
    }

    fn from_parser(span: ParserSpan) -> Self {
        Span {
            start: position(span.start),
            end: position(span.end),
        }
    }
}

/// Index of a node in its [`Document`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    SingleQuoted,
    DoubleQuoted,
    Literal,
    Folded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Scalar { value: String, style: Style },
    Sequence(Vec<NodeId>),
    Mapping(Vec<(NodeId, NodeId)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    pub span: Span,
    /// Where the node's `!tag` is written.
    tag: Option<Position>,
    /// A quoted scalar's text between its quotes, as written: escapes unprocessed.
    spelling: Option<String>,
}

/// A parsed file. An empty file is a document whose root is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    nodes: Vec<Node>,
    root: Option<NodeId>,
}

/// A problem with a file, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlError {
    pub position: Position,
    pub message: String,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.position.line, self.position.column, self.message
        )
    }
}

/// The result of reading a file the grammar accepts: its tree, and the problems found in it (empty
/// when the file uses no refused feature).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub document: Document,
    pub problems: Vec<YamlError>,
}

/// The toolchain's wording for the YAML features it refuses (`SchemaBundle.properties`).
const ANCHORS: &str = "YAML anchors/aliases are not supported in Kotlin project files";
const SECONDARY_TAG: &str =
    "Secondary (`!!`) YAML type tags are not supported in Kotlin project files";
const DOCUMENTS: &str = "Multiple YAML documents are not supported";

impl Document {
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    /// Where the tag written on `id` is, if it has one.
    pub fn tag(&self, id: NodeId) -> Option<Position> {
        self.node(id).tag
    }

    /// The text of scalar `id` as written in the file: a quoted scalar's text between its quotes,
    /// with escapes unprocessed, and otherwise its value.
    pub fn spelling(&self, id: NodeId) -> &str {
        let node = self.node(id);
        match (&node.spelling, &node.kind) {
            (Some(spelling), _) => spelling,
            (None, NodeKind::Scalar { value, .. }) => value,
            (None, _) => "",
        }
    }

    /// Parse `text` under `limits`.
    pub fn parse(text: &str, limits: Limits) -> Result<Parsed, YamlError> {
        if text.len() > limits.bytes {
            return Err(YamlError {
                position: Position { line: 1, column: 1 },
                message: format!(
                    "the file is {} bytes; krusty-toolchain reads build files of at most {} bytes",
                    text.len(),
                    limits.bytes
                ),
            });
        }
        Builder {
            lines: text.lines().collect(),
            limits,
            nodes: Vec::new(),
            open: Vec::new(),
            root: None,
            documents: 0,
            problems: Vec::new(),
        }
        .build(text)
    }
}

/// A collection still receiving children.
enum Open {
    Sequence(NodeId, Vec<NodeId>),
    /// The mapping, its finished entries, and a key waiting for its value.
    Mapping(NodeId, Vec<(NodeId, NodeId)>, Option<NodeId>),
}

struct Builder<'a> {
    lines: Vec<&'a str>,
    limits: Limits,
    nodes: Vec<Node>,
    open: Vec<Open>,
    root: Option<NodeId>,
    documents: usize,
    problems: Vec<YamlError>,
}

fn refuse(span: ParserSpan, message: impl Into<String>) -> YamlError {
    YamlError {
        position: Span::from_parser(span).start,
        message: message.into(),
    }
}

/// The legacy content-start annotation reader's property positions.
struct Properties {
    anchor: Option<Position>,
    tag: Option<Position>,
}

/// Property positions and content coordinates for the parser's accepted node.
#[derive(Debug, PartialEq, Eq)]
struct SourceProperties {
    anchor: Option<Position>,
    tag: Option<Position>,
    content: Position,
}

impl SourceProperties {
    fn span(&self, span: ParserSpan) -> Span {
        Span {
            start: self.content,
            ..Span::from_parser(span)
        }
    }
}

impl Builder<'_> {
    fn build(mut self, text: &str) -> Result<Parsed, YamlError> {
        for event in Parser::new_from_str(text) {
            let (event, span) = event.map_err(|error| YamlError {
                position: position(*error.marker()),
                message: error.info().to_string(),
            })?;
            match event {
                Event::StreamStart | Event::StreamEnd | Event::DocumentEnd | Event::Nothing => {}
                Event::DocumentStart(_) => {
                    self.documents += 1;
                    if self.documents > 1 {
                        self.problems.push(refuse(span, DOCUMENTS));
                        break;
                    }
                }
                Event::Alias(_) => {
                    self.problems.push(refuse(span, ANCHORS));
                    // The alias stands for a value; an empty scalar keeps the tree's shape.
                    let id = self.push(
                        NodeKind::Scalar {
                            value: String::new(),
                            style: Style::Plain,
                        },
                        Span::from_parser(span),
                        None,
                    )?;
                    self.attach(id)?;
                }
                Event::Scalar(value, style, anchor, tag) => {
                    let properties = self.properties(
                        anchor,
                        tag.map(|tag| format!("{}{}", tag.handle, tag.suffix)),
                        span,
                    );
                    let style = match style {
                        ScalarStyle::Plain => Style::Plain,
                        ScalarStyle::SingleQuoted => Style::SingleQuoted,
                        ScalarStyle::DoubleQuoted => Style::DoubleQuoted,
                        ScalarStyle::Literal => Style::Literal,
                        ScalarStyle::Folded => Style::Folded,
                    };
                    let spelling = match style {
                        Style::SingleQuoted | Style::DoubleQuoted => {
                            Some(self.between_quotes(properties.span(span)))
                        }
                        _ => None,
                    };
                    let id = self.push(
                        NodeKind::Scalar {
                            value: value.into_owned(),
                            style,
                        },
                        properties.span(span),
                        properties.tag,
                    )?;
                    self.nodes[id.0 as usize].spelling = spelling;
                    self.attach(id)?;
                }
                Event::SequenceStart(anchor, tag) => {
                    let properties = self.properties(
                        anchor,
                        tag.map(|tag| format!("{}{}", tag.handle, tag.suffix)),
                        span,
                    );
                    let id = self.push(
                        NodeKind::Sequence(Vec::new()),
                        properties.span(span),
                        properties.tag,
                    )?;
                    self.open(Open::Sequence(id, Vec::new()), properties.span(span))?;
                }
                Event::MappingStart(anchor, tag) => {
                    let properties = self.properties(
                        anchor,
                        tag.map(|tag| format!("{}{}", tag.handle, tag.suffix)),
                        span,
                    );
                    let id = self.push(
                        NodeKind::Mapping(Vec::new()),
                        properties.span(span),
                        properties.tag,
                    )?;
                    self.open(Open::Mapping(id, Vec::new(), None), properties.span(span))?;
                }
                Event::SequenceEnd | Event::MappingEnd => self.close(span)?,
            }
        }
        Ok(Parsed {
            document: Document {
                nodes: self.nodes,
                root: self.root,
            },
            problems: self.problems,
        })
    }

    /// Keep lexical property positions separate from content. Parser 0.2 events start at the
    /// content token; 0.3 events include its preceding properties. Both refer to the same source.
    fn properties(
        &mut self,
        anchor: usize,
        tag: Option<String>,
        span: ParserSpan,
    ) -> SourceProperties {
        let span = Span::from_parser(span);
        let mut found = self.find_properties(span, anchor != 0, tag.is_some());
        if anchor != 0 {
            self.problems.push(YamlError {
                position: found.anchor.unwrap_or(span.start),
                message: ANCHORS.to_string(),
            });
        }
        if let Some(tag) = tag {
            // Prefix spans were checked above. This is only the preexisting legacy fallback.
            let at = found.tag.unwrap_or(span.start);
            if tag.starts_with("!!") || tag.starts_with("tag:yaml.org,2002:") {
                self.problems.push(YamlError {
                    position: at,
                    message: SECONDARY_TAG.to_string(),
                });
                found.tag = None;
            } else {
                found.tag = Some(at);
            }
        }
        found
    }

    /// Locate only properties authenticated by the parser, without interpreting their names or
    /// scalar contents. This annotates accepted YAML; syntax and property validity belong upstream.
    fn find_properties(&self, span: Span, anchor: bool, tag: bool) -> SourceProperties {
        let mut found = SourceProperties {
            anchor: None,
            tag: None,
            content: span.start,
        };
        if !anchor && !tag {
            return found;
        }
        let blank = |c: char| matches!(c, ' ' | '\t');
        let flow = |c: char| matches!(c, ',' | '[' | ']' | '{' | '}');
        let mut source = self
            .lines
            .iter()
            .enumerate()
            .skip(span.start.line - 1)
            .take(span.end.line - span.start.line + 1)
            .flat_map(|(line, text)| {
                text.chars()
                    .chain(std::iter::once('\n'))
                    .enumerate()
                    .map(move |(column, c)| {
                        (
                            Position {
                                line: line + 1,
                                column: column + 1,
                            },
                            c,
                        )
                    })
            })
            .skip_while(|(at, _)| *at < span.start)
            .take_while(|(at, _)| *at < span.end)
            .peekable();
        let prefix = source
            .peek()
            .is_some_and(|(_, c)| (*c == '&' && anchor) || (*c == '!' && tag));
        if prefix {
            // The span includes properties. Traverse their validated lexical boundaries, never
            // searching for a quote inside a tag URI or an anchor name.
            loop {
                while source.peek().is_some_and(|(_, c)| blank(*c) || *c == '\n') {
                    source.next();
                }
                if source.peek().is_some_and(|(_, c)| *c == '#') {
                    for (_, c) in source.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                    continue;
                }
                let Some(&(at, indicator)) = source.peek() else {
                    found.content = span.end;
                    break;
                };
                match indicator {
                    '&' if anchor && found.anchor.is_none() => found.anchor = Some(at),
                    '!' if tag && found.tag.is_none() => found.tag = Some(at),
                    _ => {
                        found.content = at;
                        break;
                    }
                }
                source.next();
                let verbatim = indicator == '!' && source.peek().is_some_and(|(_, c)| *c == '<');
                if verbatim {
                    // A verbatim tag can contain flow indicators and apostrophes.
                    for (_, c) in source.by_ref() {
                        if c == '>' {
                            break;
                        }
                    }
                } else {
                    while source
                        .peek()
                        .is_some_and(|(_, c)| !blank(*c) && *c != '\n' && !flow(*c))
                    {
                        source.next();
                    }
                }
            }
            assert!(
                found.anchor.is_some() == anchor && found.tag.is_some() == tag,
                "a property-inclusive parser span contains its authenticated properties"
            );
            return found;
        }
        // Preserve the old content-start reader, including its existing comment-position
        // limitation. New prefix-span navigation must not reinterpret preceding scalar contents.
        let preceding = self.preceding_properties(span.start);
        SourceProperties {
            anchor: preceding.anchor.filter(|_| anchor),
            tag: preceding.tag.filter(|_| tag),
            content: span.start,
        }
    }

    fn preceding_properties(&self, content: Position) -> Properties {
        let mut found = Properties {
            anchor: None,
            tag: None,
        };
        let mut line = content.line;
        let mut column = content.column; // 1-based; scanning the characters before it
        for _ in 0..2 {
            // Skip whitespace (and line breaks) backwards.
            loop {
                let characters: Vec<char> = self
                    .lines
                    .get(line.wrapping_sub(1))
                    .map(|text| text.chars().collect())
                    .unwrap_or_default();
                let before = &characters[..(column - 1).min(characters.len())];
                match before.iter().rposition(|c| !c.is_whitespace()) {
                    Some(last) => {
                        // The token ends at `last`; find its start.
                        let start = before[..=last]
                            .iter()
                            .rposition(|c| c.is_whitespace())
                            .map_or(0, |space| space + 1);
                        let token_position = Position {
                            line,
                            column: start + 1,
                        };
                        match before[start] {
                            '&' if found.anchor.is_none() => found.anchor = Some(token_position),
                            '!' if found.tag.is_none() => found.tag = Some(token_position),
                            _ => return found,
                        }
                        column = start + 1;
                        break;
                    }
                    None if line > 1 => {
                        line -= 1;
                        column = usize::MAX;
                    }
                    None => return found,
                }
            }
        }
        found
    }

    /// The source text of a quoted scalar spanning `span`, without its quotes.
    fn between_quotes(&self, span: Span) -> String {
        let mut text = String::new();
        for line in span.start.line..=span.end.line {
            let source = self.lines.get(line - 1).copied().unwrap_or("");
            let from = if line == span.start.line {
                span.start.column
            } else {
                1
            };
            let to = if line == span.end.line {
                span.end.column - 1
            } else {
                source.chars().count()
            };
            if line != span.start.line {
                text.push('\n');
            }
            text.extend(
                source
                    .chars()
                    .skip(from - 1)
                    .take(to.saturating_sub(from - 1)),
            );
        }
        let mut chars = text.chars();
        chars.next();
        chars.next_back();
        chars.as_str().to_string()
    }

    fn push(
        &mut self,
        kind: NodeKind,
        mut span: Span,
        tag: Option<Position>,
    ) -> Result<NodeId, YamlError> {
        if self.nodes.len() >= self.limits.nodes {
            return Err(YamlError {
                position: span.start,
                message: format!(
                    "krusty-toolchain reads build files of at most {} YAML nodes",
                    self.limits.nodes
                ),
            });
        }
        let id = NodeId(self.nodes.len() as u32);
        // A tagged node starts at its tag, as the toolchain places it.
        if let Some(tag) = tag {
            span.start = tag;
        }
        self.nodes.push(Node {
            kind,
            span,
            tag,
            spelling: None,
        });
        Ok(id)
    }

    fn open(&mut self, collection: Open, span: Span) -> Result<(), YamlError> {
        if self.open.len() >= self.limits.depth {
            return Err(YamlError {
                position: span.start,
                message: format!(
                    "krusty-toolchain reads build files nested at most {} collections deep",
                    self.limits.depth
                ),
            });
        }
        self.open.push(collection);
        Ok(())
    }

    /// Hand a finished node to the collection it belongs to, or make it the root.
    /// An empty value has no text of its own; the toolchain places it where the item or entry
    /// holding it starts: at the `-` of a sequence item, or at the key of a mapping entry.
    fn place_empty(&mut self, id: NodeId) {
        let node = &self.nodes[id.0 as usize];
        let empty = matches!(&node.kind, NodeKind::Scalar { value, style: Style::Plain } if value.is_empty());
        if !empty {
            return;
        }
        let start = match self.open.last() {
            Some(Open::Sequence(..)) => self.previous_token(node.span.start),
            Some(Open::Mapping(_, _, Some(key))) => Some(self.nodes[key.0 as usize].span.start),
            _ => None,
        };
        if let Some(start) = start {
            let node = &mut self.nodes[id.0 as usize];
            node.span = Span { start, end: start };
        }
    }

    /// The position of the last character before `position` that is not whitespace.
    fn previous_token(&self, position: Position) -> Option<Position> {
        let mut line = position.line;
        let mut column = position.column;
        while line >= 1 {
            let characters: Vec<char> = self.lines.get(line - 1)?.chars().collect();
            let before = &characters[..(column - 1).min(characters.len())];
            if let Some(last) = before.iter().rposition(|c| !c.is_whitespace()) {
                return Some(Position {
                    line,
                    column: last + 1,
                });
            }
            line -= 1;
            column = usize::MAX;
        }
        None
    }

    fn attach(&mut self, id: NodeId) -> Result<(), YamlError> {
        self.place_empty(id);
        match self.open.last_mut() {
            None => self.root = Some(id),
            Some(Open::Sequence(_, items)) => items.push(id),
            Some(Open::Mapping(_, entries, pending)) => match pending.take() {
                Some(key) => entries.push((key, id)),
                None => {
                    let key = &self.nodes[id.0 as usize];
                    let at = key.span.start;
                    let NodeKind::Scalar { value, .. } = &key.kind else {
                        return Err(YamlError {
                            position: at,
                            message: "a mapping key must be a scalar".to_string(),
                        });
                    };
                    let duplicate = entries.iter().any(|(key, _)| {
                        matches!(&self.nodes[key.0 as usize].kind, NodeKind::Scalar { value: other, .. } if other == value)
                    });
                    if duplicate {
                        return Err(YamlError {
                            position: at,
                            message: format!("duplicate key `{value}`"),
                        });
                    }
                    *pending = Some(id);
                }
            },
        }
        Ok(())
    }

    fn close(&mut self, span: ParserSpan) -> Result<(), YamlError> {
        let (id, kind) = match self
            .open
            .pop()
            .expect("an end event closes an open collection")
        {
            Open::Sequence(id, items) => (id, NodeKind::Sequence(items)),
            Open::Mapping(id, entries, _) => (id, NodeKind::Mapping(entries)),
        };
        let event = Span::from_parser(span);
        // A flow collection's end event starts at its closing bracket, where it ends. A block
        // collection's is empty and stands at the next token, past blank lines and comments: it
        // ends with its last entry.
        let last = match &kind {
            NodeKind::Sequence(items) => items.last().copied(),
            NodeKind::Mapping(entries) => entries.last().map(|&(_, value)| value),
            NodeKind::Scalar { .. } => None,
        };
        let end = match last {
            Some(last) if event.start == event.end => self.nodes[last.0 as usize].span.end,
            _ if event.start == event.end => event.end,
            _ => Position {
                line: event.start.line,
                column: event.start.column + 1,
            },
        };
        let node = &mut self.nodes[id.0 as usize];
        node.kind = kind;
        node.span.end = end;
        self.attach(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Parsed, YamlError> {
        Document::parse(text, Limits::DEFAULT)
    }

    fn scalar(document: &Document, id: NodeId) -> (&str, Style) {
        match &document.node(id).kind {
            NodeKind::Scalar { value, style } => (value, *style),
            other => panic!("not a scalar: {other:?}"),
        }
    }

    fn problems(text: &str) -> Vec<String> {
        parse(text)
            .unwrap()
            .problems
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_quoted_scalar_keeps_its_spelling_between_its_quotes() {
        let text = "\"q\\\"\\u00e9\": 'it''s'\nplain: !t \"a\n  b\"\n";
        let document = parse(text).unwrap().document;
        let NodeKind::Mapping(entries) = &document.node(document.root().unwrap()).kind else {
            panic!("root is a mapping");
        };
        let spellings: Vec<(&str, &str)> = entries
            .iter()
            .map(|&(key, value)| (document.spelling(key), document.spelling(value)))
            .collect();
        assert_eq!(spellings, [(r#"q\"\u00e9"#, "it''s"), ("plain", "a\n  b")]);
    }

    #[test]
    fn property_tokens_are_outside_quoted_spelling_and_keep_their_own_positions() {
        let at = |line, column| Position { line, column };
        for (text, spelling, tag, problems) in [
            (
                "!<tag:example.org,2026:a'b> 'it''s'\n",
                "it''s",
                at(1, 1),
                vec![],
            ),
            (
                "&a\"quote !t 'it''s'\n",
                "it''s",
                at(1, 10),
                vec!["1:1: YAML anchors/aliases are not supported in Kotlin project files"],
            ),
            (
                "!t &a'quote \"é\\u00e9\"\n",
                "é\\u00e9",
                at(1, 1),
                vec!["1:4: YAML anchors/aliases are not supported in Kotlin project files"],
            ),
        ] {
            let parsed = parse(text).unwrap();
            assert_eq!(
                parsed
                    .problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                problems,
                "{text:?}"
            );
            let id = parsed.document.root().unwrap();
            assert_eq!(parsed.document.spelling(id), spelling, "{text:?}");
            assert_eq!(parsed.document.tag(id), Some(tag), "{text:?}");
            assert_eq!(parsed.document.node(id).span.start, tag, "{text:?}");
        }
        assert_eq!(
            problems("&a !!str 'value'\n"),
            [
                "1:1: YAML anchors/aliases are not supported in Kotlin project files",
                "1:4: Secondary (`!!`) YAML type tags are not supported in Kotlin project files",
            ]
        );
    }

    #[test]
    fn a_hash_in_a_quoted_key_does_not_change_the_following_property_positions() {
        let parsed = parse("\"key # text\": !t \"value\"\n").unwrap();
        assert_eq!(parsed.problems, []);
        let document = parsed.document;
        let NodeKind::Mapping(entries) = &document.node(document.root().unwrap()).kind else {
            panic!("a mapping");
        };
        assert_eq!(entries.len(), 1);
        let (key, value) = entries[0];
        assert_eq!(document.spelling(key), "key # text");
        assert_eq!(document.spelling(value), "value");
        assert_eq!(
            document.tag(value),
            Some(Position {
                line: 1,
                column: 15
            })
        );
        assert_eq!(
            document.node(value).span,
            Span {
                start: Position {
                    line: 1,
                    column: 15
                },
                end: Position {
                    line: 1,
                    column: 25
                }
            }
        );
        assert_eq!(
            problems("\"key # text\": &a !!str \"value\"\n"),
            [
                "1:15: YAML anchors/aliases are not supported in Kotlin project files",
                "1:18: Secondary (`!!`) YAML type tags are not supported in Kotlin project files",
            ]
        );
    }

    #[test]
    fn comments_keep_raw_spelling_and_new_prefix_spans_keep_property_coordinates() {
        let at = |line, column| Position { line, column };
        // Exact coordinates below describe 0.3's property-inclusive spans. The legacy 0.2
        // comment-position limitation is unchanged; whole raw spelling is checked in either case.
        for (text, spelling, anchor, tag, content, end) in [
            (
                "&α # anchor\n!<tag:example.org,2026:t> # tag\n\"a\n  b\"\n",
                "a\n  b",
                at(1, 1),
                at(2, 1),
                at(3, 1),
                at(4, 5),
            ),
            (
                "!t # tag\n  &é # anchor\n  'a\n    b'\n",
                "a\n    b",
                at(2, 3),
                at(1, 1),
                at(3, 3),
                at(4, 7),
            ),
        ] {
            let document = parse(text).unwrap().document;
            assert_eq!(document.spelling(document.root().unwrap()), spelling);
            let builder = Builder {
                lines: text.lines().collect(),
                limits: Limits::DEFAULT,
                nodes: Vec::new(),
                open: Vec::new(),
                root: None,
                documents: 0,
                problems: Vec::new(),
            };
            let span = Span {
                start: at(1, 1),
                end,
            };
            let properties = builder.find_properties(span, true, true);
            assert_eq!(
                properties,
                SourceProperties {
                    anchor: Some(anchor),
                    tag: Some(tag),
                    content
                }
            );
            assert_eq!(
                builder.between_quotes(Span {
                    start: content,
                    end
                }),
                spelling
            );
        }
    }

    #[test]
    fn old_content_spans_and_new_property_spans_describe_the_same_quoted_source() {
        // These are the external parser 0.2 and 0.3 span conventions for this whole input.
        let at = |line, column| Position { line, column };
        let text = "plain: !t \"a\n  b\"\n";
        let builder = Builder {
            lines: text.lines().collect(),
            limits: Limits::DEFAULT,
            nodes: Vec::new(),
            open: Vec::new(),
            root: None,
            documents: 0,
            problems: Vec::new(),
        };
        for start in [at(1, 11), at(1, 8)] {
            let span = Span {
                start,
                end: at(2, 5),
            };
            let properties = builder.find_properties(span, false, true);
            assert_eq!(
                properties,
                SourceProperties {
                    anchor: None,
                    tag: Some(at(1, 8)),
                    content: at(1, 11)
                }
            );
            assert_eq!(
                builder.between_quotes(Span {
                    start: properties.content,
                    ..span
                }),
                "a\n  b"
            );
        }
    }

    #[test]
    fn a_module_file_becomes_a_spanned_tree() {
        let parsed = parse("product: jvm/app\nmodules:\n  - ./libs/*\n  - \"true\"\n").unwrap();
        assert_eq!(parsed.problems, []);
        let document = parsed.document;
        let NodeKind::Mapping(entries) = &document.node(document.root().unwrap()).kind else {
            panic!("root is a mapping");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(scalar(&document, entries[0].0), ("product", Style::Plain));
        assert_eq!(scalar(&document, entries[0].1), ("jvm/app", Style::Plain));
        let NodeKind::Sequence(items) = &document.node(entries[1].1).kind else {
            panic!("modules is a sequence");
        };
        assert_eq!(
            document.node(items[0]).span.start,
            Position { line: 3, column: 5 }
        );
        assert_eq!(scalar(&document, items[1]), ("true", Style::DoubleQuoted));
        assert_eq!(
            document.node(items[1]).span.start,
            Position { line: 4, column: 5 }
        );
    }

    /// A block collection ends where its last entry ends, as the toolchain marks it, not at the
    /// next line; a flow collection ends after its closing bracket.
    #[test]
    fn a_collection_ends_where_its_text_ends() {
        let end = |text: &str| {
            let document = parse(text).unwrap().document;
            document.node(document.root().unwrap()).span.end
        };
        assert_eq!(
            end("modules: []\n"),
            Position {
                line: 1,
                column: 12
            }
        );
        assert_eq!(
            end("a:\n  b: 1\n\n# done\n"),
            Position { line: 2, column: 7 }
        );
        assert_eq!(end("- x\n- [y, z]   \n"), Position { line: 2, column: 9 });
        assert_eq!(end("{a: 1}\n"), Position { line: 1, column: 7 });
    }

    #[test]
    fn an_empty_file_has_no_root() {
        assert_eq!(parse("").unwrap().document.root(), None);
        assert_eq!(parse("# only a comment\n").unwrap().document.root(), None);
    }

    /// Positions and messages recorded from the toolchain's `kotlin show modules` on a `module.yaml`
    /// holding each text.
    #[test]
    fn refused_features_are_reported_as_the_toolchain_reports_them() {
        assert_eq!(
            problems("product: jvm/lib\ndescription: &x hi\n"),
            ["2:14: YAML anchors/aliases are not supported in Kotlin project files"]
        );
        assert_eq!(
            problems("product: &p jvm/lib\ndescription: *p\n"),
            [
                "1:10: YAML anchors/aliases are not supported in Kotlin project files",
                "2:14: YAML anchors/aliases are not supported in Kotlin project files"
            ]
        );
        assert_eq!(
            problems("product: jvm/lib\n---\ndescription: x\n"),
            ["2:1: Multiple YAML documents are not supported"]
        );
        assert_eq!(
            problems("product: !!str jvm/lib\n"),
            ["1:10: Secondary (`!!`) YAML type tags are not supported in Kotlin project files"]
        );
    }

    #[test]
    fn krusty_refuses_what_it_does_not_model() {
        for (text, expected) in [
            ("a: 1\na: 2\n", "2:1: duplicate key `a`"),
            ("? [a]\n: 1\n", "1:3: a mapping key must be a scalar"),
        ] {
            assert_eq!(parse(text).unwrap_err().to_string(), expected, "{text:?}");
        }
    }

    #[test]
    fn a_syntax_error_names_its_position() {
        let error = parse("a: [1, 2\n").unwrap_err();
        assert_eq!(error.position.line, 2);
    }

    #[test]
    fn where_a_local_tag_is_written_is_kept() {
        let parsed =
            parse("product: !foo jvm/lib\naction: !com.example.Generate\n  dir: x\n").unwrap();
        assert_eq!(parsed.problems, []);
        let document = &parsed.document;
        let Some(NodeKind::Mapping(entries)) = document.root().map(|top| &document.node(top).kind)
        else {
            panic!("a mapping");
        };
        let tags: Vec<Option<Position>> = entries
            .iter()
            .flat_map(|&(key, value)| [document.tag(key), document.tag(value)])
            .collect();
        assert_eq!(
            tags,
            [
                None,
                Some(Position {
                    line: 1,
                    column: 10
                }),
                None,
                Some(Position { line: 2, column: 9 }),
            ]
        );
    }

    #[test]
    fn every_limit_is_enforced_with_its_name() {
        let limits = Limits {
            bytes: 32,
            depth: 2,
            nodes: 5,
        };
        assert_eq!(
            Document::parse(&"a: 1\n".repeat(10), limits)
                .unwrap_err()
                .to_string(),
            "1:1: the file is 50 bytes; krusty-toolchain reads build files of at most 32 bytes"
        );
        assert_eq!(
            Document::parse("a: {b: {c: 1}}", limits)
                .unwrap_err()
                .to_string(),
            "1:8: krusty-toolchain reads build files nested at most 2 collections deep"
        );
        assert_eq!(
            Document::parse("[1, 2, 3, 4, 5]", limits)
                .unwrap_err()
                .to_string(),
            "1:14: krusty-toolchain reads build files of at most 5 YAML nodes"
        );
    }
}
