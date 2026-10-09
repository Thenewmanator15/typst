use krilla::tagging::{self as kt, Node, Tag, TagGroup, TagKind};

use crate::tags::resolve::{ElementKind, element_kind};

pub struct Accumulator {
    pub nesting: ElementKind,
    buf: Vec<Node>,
    // An intermediate `Span` node to collect marked content sequences.
    // Grouping elements may not contain marked content sequences directly, so
    // they are wrapped into a `Span`.
    grouping_span: Option<Vec<Node>>,
    /// Prototype: PDF 2.0 does not allow spans and other inline elements directly in a
    /// grouping element, so there they are collected into a paragraph instead.
    pdf20: bool,
}

impl Accumulator {
    /// Create a new accumulator.
    fn new(nesting: ElementKind, pdf20: bool) -> Self {
        Self {
            nesting,
            buf: Vec::new(),
            grouping_span: None,
            pdf20,
        }
    }

    /// Create a new accumulator.
    pub fn root(pdf20: bool) -> Self {
        Self::new(ElementKind::Grouping, pdf20)
    }

    /// Create a new nested accumulator for the children of an element with the
    /// given tag. This will flush any intermediate grouping span, to ensure
    /// correct ordering of nested groups.
    ///
    /// Prototype: unless the element will join that span itself, as a link in
    /// loose text does in PDF 2.0. Flushing then would end the paragraph in the
    /// middle of a sentence.
    pub fn nest(&mut self, nesting: ElementKind, tag: &TagKind) -> Self {
        let joins = self.pdf20 && self.nesting == ElementKind::Grouping && is_phrase(tag);
        if !joins {
            self.flush_grouping_span();
        }
        Self::new(nesting, self.pdf20)
    }

    /// Flush any intermediate grouping span into the nodes array.
    fn flush_grouping_span(&mut self) {
        if let Some(span_nodes) = self.grouping_span.take() {
            // Line numbers with no text between them need no paragraph around them.
            if span_nodes.iter().all(is_artifact_element) {
                self.buf.extend(span_nodes);
                return;
            }
            let tag: TagKind = if self.pdf20 {
                Tag::P.into()
            } else {
                Tag::Span.with_placement(Some(kt::Placement::Block)).into()
            };
            let group = TagGroup::with_children(tag, span_nodes);
            self.buf.push(group.into());
        }
    }

    /// Push a node into this accumulator.
    pub fn push(&mut self, mut node: Node) {
        if self.nesting == ElementKind::Grouping {
            match &mut node {
                Node::Group(group) if self.pdf20 && is_phrase(&group.tag) => {
                    let span_nodes = self.grouping_span.get_or_insert_default();
                    span_nodes.push(node);
                }
                Node::Group(group) => {
                    self.flush_grouping_span();

                    // Ensure ILSE have block placement when inside grouping elements.
                    if element_kind(&group.tag) == ElementKind::Inline {
                        group.tag.set_placement(Some(kt::Placement::Block));
                    }

                    self.buf.push(node);
                }
                Node::Leaf(_) => {
                    let span_nodes = self.grouping_span.get_or_insert_default();
                    span_nodes.push(node);
                }
            }
        } else {
            self.buf.push(node);
        }
    }

    /// Reserve additional capacity inside the node buffer.
    pub fn reserve(&mut self, additional: usize) {
        self.buf.reserve(additional);
    }

    /// Push multiple nodes into this accumulator.
    pub fn extend(&mut self, nodes: impl ExactSizeIterator<Item = Node>) {
        self.buf.reserve(nodes.len());
        for node in nodes {
            self.push(node);
        }
    }

    // Finish accumulating and return the nodes.
    pub fn finish(mut self) -> Vec<Node> {
        self.flush_grouping_span();
        self.buf
    }
}

/// Prototype: inline elements that PDF 2.0 does not allow directly in a grouping element.
/// An element that was given block placement stays as it is.
fn is_phrase(tag: &TagKind) -> bool {
    matches!(
        tag,
        TagKind::Span(_)
            | TagKind::Em(_)
            | TagKind::Strong(_)
            | TagKind::InlineQuote(_)
            | TagKind::Code(_)
            | TagKind::Link(_)
            | TagKind::Reference(_)
            // The number of a line, which sits between the lines of loose text
            // and must not split them into a paragraph each.
            | TagKind::Artifact(_)
    ) && tag.placement() != Some(kt::Placement::Block)
}

fn is_artifact_element(node: &Node) -> bool {
    matches!(node, Node::Group(group) if matches!(group.tag, TagKind::Artifact(_)))
}
