//! Building the logical tree.
//!
//! The tree of [`Frame`]s which is split up into pages doesn't necessarily
//! represent the logical structure of the Typst document. The logical structure
//! is instead defined by the start and end [`introspection::Tag`]s and
//! additional insertions of frames by the means of [`Frame::set_parent`].
//! These inserted frames resolve to groups of kind [`GroupKind::LogicalChild`].
//!
//! This module resolves the logical structure in a pre-pass, so that the
//! complete logical tree is available when the document's content is converted.
//!
//! [`introspection::Tag`]: typst_library::introspection::Tag
//! [`FrameItem::parent`]: typst_library::layout::FrameItem

use std::num::NonZeroU16;
use std::ops::ControlFlow;

use ecow::EcoVec;
use krilla::tagging::{ArtifactType, ListNumbering, Tag, TagKind};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use typst_layout::PagedDocument;
use typst_library::diag::{
    At, ExpectInternal, SourceDiagnostic, SourceResult, assert_internal, bail, error,
    panic_internal,
};
use typst_library::format::Complete;
use typst_library::foundations::{Content, ContextElem};
use typst_library::introspection::Location;
use typst_library::layout::{
    Frame, FrameItem, FrameParent, GridCell, GridElem, GroupItem, HideElem, Inherit,
    PlaceElem, RepeatElem,
};
use typst_library::math::EquationElem;
use typst_library::model::{
    ArtifactElem, BibliographyElem, Document, EmphElem, EnumElem, FigureCaption,
    FigureElem, FootnoteElem, FootnoteEntry, HeadingElem, LinkMarker, ListElem,
    ListMarker, Numbering, Outlinable, OutlineEntry, ParElem, PdfMarkerTag,
    PdfMarkerTagKind, QuoteElem, StrongElem, TableCell, TableElem, TermsElem, TitleElem,
};
use typst_library::text::{
    HighlightElem, OverlineElem, RawElem, RawLine, StrikeElem, SubElem, SuperElem,
    UnderlineElem,
};
use typst_library::visualize::ImageElem;
use typst_syntax::Span;

use crate::PdfOptions;
use crate::tags::GroupId;
use crate::tags::context::{Ctx, FigureCtx, GridCtx, ListCtx, OutlineCtx, TableCtx};
use crate::tags::groups::{BreakOpportunity, BreakPriority, GroupKind, Groups};
use crate::tags::tree::text::TextAttr;
use crate::tags::tree::{Break, TraversalStates, Tree, Unfinished};
use crate::tags::util::{ArtifactKindExt, PropertyValCopied};
use crate::util::ValidatorsExt;

pub struct TreeBuilder<'a> {
    options: &'a PdfOptions<Complete>,

    /// Each [`FrameItem::Tag`] and each [`FrameItem::Group`] with a parent
    /// will append a progression to this tree. This list of progressions is
    /// used to determine the location in the tree when doing the actual PDF
    /// generation and inserting the marked content sequences.
    progressions: Vec<GroupId>,
    breaks: Vec<Break>,
    unfinished: Vec<Unfinished>,
    groups: Groups,
    ctx: Ctx,
    logical_children: FxHashMap<Location, SmallVec<[GroupId; 4]>>,
    errors: EcoVec<SourceDiagnostic>,

    stack: TagStack,
    /// Currently only used for table/grid cells that are broken across multiple
    /// regions, and thus can have opening/closing introspection tags that are
    /// in completely different frames, due to the logical parenting mechanism.
    unfinished_stacks: FxHashMap<Location, Vec<StackEntry>>,
}

impl<'a> TreeBuilder<'a> {
    pub fn new(document: &PagedDocument, options: &'a PdfOptions<Complete>) -> Self {
        let doc_lang = document.info().locale.custom();
        let mut groups = Groups::new();
        let doc = groups.new_virtual(
            GroupId::INVALID,
            Span::detached(),
            GroupKind::Root(doc_lang),
        );
        Self {
            options,
            progressions: vec![doc],
            breaks: Vec::new(),
            unfinished: Vec::new(),
            groups,
            ctx: Ctx::new(),
            logical_children: FxHashMap::default(),
            errors: EcoVec::new(),

            stack: TagStack::new(),
            unfinished_stacks: FxHashMap::default(),
        }
    }

    pub fn finish(self) -> Tree {
        Tree {
            prog_cursor: 0,
            progressions: self.progressions,
            break_cursor: 0,
            breaks: self.breaks,
            unfinished_cursor: 0,
            unfinished: self.unfinished,
            state: TraversalStates::new(),
            groups: self.groups,
            ctx: self.ctx,
            logical_children: self.logical_children,
            errors: self.errors,
        }
    }

    pub fn root_document(&self) -> GroupId {
        self.progressions[0]
    }

    /// The last group in the progression.
    pub fn current(&self) -> GroupId {
        *self.progressions.last().unwrap()
    }

    /// The last group on the stack or the root document.
    pub fn parent(&self) -> GroupId {
        self.stack.last().map(|e| e.id).unwrap_or(self.root_document())
    }

    pub fn parent_kind(&self) -> &GroupKind {
        &self.groups.get(self.parent()).kind
    }
}

#[derive(Debug)]
struct TagStack {
    items: Vec<StackEntry>,
}

impl std::ops::Deref for TagStack {
    type Target = Vec<StackEntry>;

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl std::ops::DerefMut for TagStack {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.items
    }
}

impl TagStack {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Remove all stack entries after the idx.
    fn take_unfinished_stack(&mut self, idx: usize) -> Option<Vec<StackEntry>> {
        if idx + 1 < self.items.len() {
            Some(self.items.drain(idx + 1..).collect())
        } else {
            None
        }
    }
}

#[derive(Debug, Copy, Clone)]
struct StackEntry {
    /// The location of the stack entry. If this is `None` the stack entry has
    /// to be manually popped.
    loc: Option<Location>,
    id: GroupId,
    prog_idx: u32,
}

pub fn build(
    document: &PagedDocument,
    options: &PdfOptions<Complete>,
) -> SourceResult<Tree> {
    let mut tree = TreeBuilder::new(document, options);
    for page in document.pages() {
        visit_page(&mut tree, &page.frame)?;
    }

    if let Some(last) = tree.stack.last() {
        panic_internal("tags weren't properly closed")
            .at(tree.groups.get(last.id).span)?;
    }
    assert_internal(tree.unfinished_stacks.is_empty(), "tags weren't properly closed")
        .at(Span::detached())?;
    assert_internal(
        tree.progressions.first() == tree.progressions.last(),
        "tags weren't properly closed",
    )
    .at(Span::detached())?;

    // Insert logical children into the tree.
    #[expect(clippy::iter_over_hash_type)]
    for (loc, children) in &tree.logical_children {
        let located = (tree.groups.by_loc(loc))
            .expect_internal("parent group")
            .at(Span::detached())?;

        if let Some(a11y) = options.validators().accessibility()
            && located.multiple_parents
        {
            let validator = a11y.as_str();
            let group = tree.groups.get(located.id);
            bail!(
                group.span,
                "{validator} error: ambiguous logical parent";
                hint: "please report this as a bug";
            );
        }

        for child in children {
            let child = tree.groups.get_mut(*child);

            let GroupKind::LogicalChild(inherit, logical_parent) = &mut child.kind else {
                unreachable!()
            };
            *logical_parent = located.id;

            // Move the child into its logical parent, so artifact, bbox, and
            // text attributes are inherited.
            if *inherit == Inherit::Yes {
                child.parent = located.id;
            }
        }
    }

    #[cfg(debug_assertions)]
    for group in tree.groups.list.iter().skip(1) {
        assert_ne!(group.parent, GroupId::INVALID);
    }

    Ok(tree.finish())
}

/// Prototype: a page starts with no line markers waiting for their numbers.
fn visit_page(tree: &mut TreeBuilder, frame: &Frame) -> SourceResult<()> {
    tree.groups.refs.lines.start_page();
    visit_frame(tree, frame)
}

fn visit_frame(tree: &mut TreeBuilder, frame: &Frame) -> SourceResult<()> {
    for (pos, item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                // Prototype: how far down the page this is, to match the numbers of
                // lines to their lines. Only the shift of a transform is followed.
                let outer = tree.groups.refs.lines.origin;
                tree.groups.refs.lines.origin += pos.y + group.transform.ty;
                let result = visit_group_frame(tree, group);
                tree.groups.refs.lines.origin = outer;
                result?
            }
            FrameItem::Tag(typst_library::introspection::Tag::Start(elem, flags)) => {
                tree.groups.refs.lines.tag_y = tree.groups.refs.lines.origin + pos.y;
                if flags.tagged {
                    visit_start_tag(tree, elem);
                } else {
                    // Prototype: not tagged, but it may still be linked to.
                    let enclosing = enclosing_tagged(tree);
                    alias_untagged(tree, elem, enclosing);
                }
            }
            FrameItem::Tag(typst_library::introspection::Tag::End(loc, _, flags)) => {
                if flags.tagged {
                    visit_end_tag(tree, *loc)?;
                }
            }
            FrameItem::Text(_) => (),
            FrameItem::Shape(..) => (),
            FrameItem::Image(..) => (),
            FrameItem::Link(..) => (),
        }
    }
    Ok(())
}

/// Handle children frames logically belonging to another element, because
/// [`typst_library::layout::GroupItem::parent`] has been set. All elements that
/// can have children set by this mechanism must be handled in
/// [`crate::tags::handle_start`] and must produce a located
/// [`crate::tags::groups::Group`], so the children can be inserted there.
///
/// Currently the frame parent is only set for:
/// - place elements [`PlaceElem`]
/// - footnote entries [`FootnoteEntry`]
/// - broken table/grid cells [`TableCell`]/[`GridCell`]
fn visit_group_frame(tree: &mut TreeBuilder, group: &GroupItem) -> SourceResult<()> {
    let Some(parent) = group.parent else {
        return visit_frame(tree, &group.frame);
    };

    // Push the logical child.
    let prev = tree.current();
    let stack_idx = tree.stack.len();
    let id = push_logical_child(tree, parent);
    tree.progressions.push(id);

    // Handle the group frame.
    visit_frame(tree, &group.frame)?;

    // Pop logical child.
    pop_logical_child(tree, parent, stack_idx);
    tree.progressions.push(prev);

    Ok(())
}

fn push_logical_child(tree: &mut TreeBuilder, parent: FrameParent) -> GroupId {
    let id = tree.groups.new_virtual(
        match parent.inherit {
            Inherit::Yes => GroupId::INVALID,
            Inherit::No => tree.current(),
        },
        Span::detached(),
        GroupKind::LogicalChild(parent.inherit, GroupId::INVALID),
    );

    tree.logical_children.entry(parent.location).or_default().push(id);

    push_stack_entry(tree, None, id);
    if let Some(stack) = tree.unfinished_stacks.remove(&parent.location) {
        tree.stack.extend(stack);
    }
    // Move to the top of the stack, including the pushed on unfinished stack.
    tree.stack.last().unwrap().id
}

fn pop_logical_child(tree: &mut TreeBuilder, parent: FrameParent, stack_idx: usize) {
    if let Some(stack) = tree.stack.take_unfinished_stack(stack_idx) {
        tree.unfinished_stacks.insert(parent.location, stack);
        tree.unfinished.push(Unfinished {
            prog_idx: tree.progressions.len() as u32,
            group_to_close: tree.stack[stack_idx].id,
        });
    }
    tree.stack.pop().expect("stack entry");
}

fn visit_start_tag(tree: &mut TreeBuilder, elem: &Content) {
    let enclosing = enclosing_tagged(tree);

    let group_id = progress_tree_start(tree, elem);
    tree.progressions.push(group_id);

    alias_untagged(tree, elem, enclosing);
}

/// Prototype: the location of the nearest enclosing element that has a tag.
fn enclosing_tagged(tree: &TreeBuilder) -> Option<Location> {
    (tree.stack.iter().rev()).find_map(|entry| {
        let loc = entry.loc?;
        (tree.groups.refs.tag_locs.get(&loc) == Some(&entry.id)).then_some(loc)
    })
}

/// Prototype: located content that has no tag of its own stands for the element that
/// encloses it, so that a link to it can still lead to a tag.
fn alias_untagged(tree: &mut TreeBuilder, elem: &Content, enclosing: Option<Location>) {
    if let Some(loc) = elem.location()
        && let Some(enclosing) = enclosing
    {
        let refs = &mut tree.groups.refs;
        if !refs.tag_locs.contains_key(&loc) && !refs.alias.contains_key(&loc) {
            refs.alias.insert(loc, enclosing);
        }
    }
}

fn visit_end_tag(tree: &mut TreeBuilder, loc: Location) -> SourceResult<()> {
    let group = progress_tree_end(tree, loc)?;
    tree.progressions.push(group);
    Ok(())
}

fn progress_tree_start(tree: &mut TreeBuilder, elem: &Content) -> GroupId {
    // Artifacts
    #[expect(clippy::redundant_pattern_matching)]
    if let Some(_) = elem.to_packed::<HideElem>() {
        push_artifact(tree, elem, ArtifactType::Other)
    } else if let Some(artifact) = elem.to_packed::<ArtifactElem>() {
        let kind = artifact.kind.val().to_krilla();
        // Prototype: PDF/UA-2 wants an artifact that only means something next to
        // real content to be an `Artifact` structure element (8.3.2). A line number
        // is one. Its frame has the line's marker as its logical parent.
        if kind == ArtifactType::LineNumber && tree.pdf20() {
            push_line_number(tree, elem, kind)
        } else {
            push_artifact(tree, elem, kind)
        }
    } else if let Some(_) = elem.to_packed::<RepeatElem>() {
        push_artifact(tree, elem, ArtifactType::Layout)

    // Elements
    } else if let Some(tag) = elem.to_packed::<PdfMarkerTag>() {
        match &tag.kind {
            PdfMarkerTagKind::OutlineBody => {
                let id = tree.ctx.outlines.push(OutlineCtx::new());
                push_group(tree, elem, GroupKind::Outline(id, None))
            }
            PdfMarkerTagKind::Bibliography(numbered) => {
                let numbering =
                    if *numbered { ListNumbering::Decimal } else { ListNumbering::None };
                let id = tree.ctx.lists.push(ListCtx::new());
                push_group(tree, elem, GroupKind::List(id, numbering, None))
            }
            PdfMarkerTagKind::BibEntry => {
                push_group(tree, elem, GroupKind::BibEntry(None))
            }
            PdfMarkerTagKind::ListItemLabel => {
                push_group(tree, elem, GroupKind::ListItemLabel(None))
            }
            PdfMarkerTagKind::ListItemBody => {
                push_group(tree, elem, GroupKind::ListItemBody(None))
            }
            PdfMarkerTagKind::TermsItemLabel => {
                push_group(tree, elem, GroupKind::TermsItemLabel(None))
            }
            PdfMarkerTagKind::TermsItemBody => {
                push_group(tree, elem, GroupKind::TermsItemBody(None, None))
            }
            PdfMarkerTagKind::Label => push_tag(tree, elem, Tag::Lbl),
            PdfMarkerTagKind::EquationNumber => {
                if tree.pdf20() {
                    push_tag(tree, elem, Tag::Lbl)
                } else {
                    no_progress(tree)
                }
            }
        }
    } else if let Some(link) = elem.to_packed::<LinkMarker>() {
        // Prototype: the link with a footnote's number stands for the footnote.
        let footnote = (tree.stack.iter().rev()).find_map(|entry| {
            let GroupKind::LogicalParent(parent) = &tree.groups.get(entry.id).kind else {
                return None;
            };
            parent.to_packed::<FootnoteElem>().and(entry.loc)
        });
        if let Some(footnote) = footnote
            && let Some(link_loc) = elem.location()
        {
            let refs = &mut tree.groups.refs;
            if refs.footnotes.insert(footnote) {
                refs.alias.insert(footnote, link_loc);
            }
        }
        push_group(tree, elem, GroupKind::Link(link.clone(), None))
    } else if let Some(_) = elem.to_packed::<typst_library::model::ParLineMarker>() {
        // The number of this line is inserted here, see `push_line_number`.
        // The number is itself a line of text and so has a marker of its own, which
        // is not one to hang a number on.
        let in_number = tree.stack.iter().any(|entry| {
            matches!(
                &tree.groups.get(entry.id).kind,
                GroupKind::Standard(tag, _)
                    if matches!(tree.groups.tags.get(*tag), TagKind::Artifact(_))
            )
        });
        if tree.pdf20()
            && !in_number
            && let Some(loc) = elem.location()
        {
            tree.groups.refs.lines.add_marker(loc);
            push_located(tree, elem, GroupKind::LogicalParent(elem.clone()))
        } else {
            no_progress(tree)
        }
    } else if let Some(_) = elem.to_packed::<BibliographyElem>() {
        // Prototype: PDF/UA-2 wants the section that holds a bibliography to say so
        // with an ARIA role (8.2.5.31). Typst has no sections otherwise.
        if tree.pdf20() {
            let role = Some("doc-bibliography".to_string());
            push_tag(tree, elem, Tag::Section.with_aria_role(role))
        } else {
            no_progress(tree)
        }
    } else if let Some(_) = elem.to_packed::<TitleElem>() {
        push_tag(tree, elem, Tag::Title)
    } else if let Some(entry) = elem.to_packed::<OutlineEntry>() {
        push_group(tree, elem, GroupKind::OutlineEntry(entry.clone(), None))
    } else if let Some(list) = elem.to_packed::<ListElem>() {
        // Prototype: PDF/UA-2 wants the value closest to the labels (8.2.5.25).
        // Older versions keep the value Typst has always written.
        let numbering = if tree.pdf20() {
            let depth = tree.list_depth(is_bullet_numbering);
            bullet_numbering(
                list.marker.get_ref(typst_library::foundations::StyleChain::default()),
                depth,
            )
        } else {
            ListNumbering::Circle
        };
        let id = tree.ctx.lists.push(ListCtx::new());
        push_group(tree, elem, GroupKind::List(id, numbering, None))
    } else if let Some(list) = elem.to_packed::<EnumElem>() {
        let numbering = if tree.pdf20() {
            let depth = tree.list_depth(|numbering| !is_bullet_numbering(numbering));
            enum_numbering(
                list.numbering
                    .get_ref(typst_library::foundations::StyleChain::default()),
                depth,
            )
        } else {
            ListNumbering::Decimal
        };
        let id = tree.ctx.lists.push(ListCtx::new());
        push_group(tree, elem, GroupKind::List(id, numbering, None))
    } else if let Some(_) = elem.to_packed::<TermsElem>() {
        // Prototype: krilla writes this as `None` before PDF 2.0.
        let numbering = ListNumbering::Description;
        let id = tree.ctx.lists.push(ListCtx::new());
        push_group(tree, elem, GroupKind::List(id, numbering, None))
    } else if let Some(figure) = elem.to_packed::<FigureElem>() {
        let lang = figure.locale;
        let bbox = tree.ctx.new_bbox();
        let group_id = tree.groups.list.next_id();
        let figure_id = tree.ctx.figures.push(FigureCtx::new(group_id, figure.clone()));
        push_group(tree, elem, GroupKind::Figure(figure_id, bbox, lang))
    } else if let Some(_) = elem.to_packed::<FigureCaption>() {
        let bbox = tree.ctx.new_bbox();
        push_group(tree, elem, GroupKind::FigureCaption(bbox, None))
    } else if let Some(image) = elem.to_packed::<ImageElem>() {
        let lang = image.locale;
        let bbox = tree.ctx.new_bbox();
        push_group(tree, elem, GroupKind::Image(image.clone(), bbox, lang))
    } else if let Some(equation) = elem.to_packed::<EquationElem>() {
        let lang = equation.locale;
        let bbox = tree.ctx.new_bbox();
        push_group(tree, elem, GroupKind::Formula(equation.clone(), bbox, lang))
    } else if let Some(table) = elem.to_packed::<TableElem>() {
        let group_id = tree.groups.list.next_id();
        let table_id = tree.ctx.tables.next_id();
        tree.ctx.tables.push(TableCtx::new(group_id, table_id, table.clone()));
        let bbox = tree.ctx.new_bbox();
        push_group(tree, elem, GroupKind::Table(table_id, bbox, None))
    } else if let Some(cell) = elem.to_packed::<TableCell>() {
        // Only repeated table headers and footer cells are laid out multiple
        // times. Mark duplicate headers as artifacts, since they have no
        // semantic meaning in the tag tree, which doesn't use page breaks for
        // it's semantic structure.
        let kind = if cell.is_repeated.val() {
            GroupKind::Artifact(ArtifactType::PaginationOther)
        } else {
            let tag = tree.groups.tags.push(Tag::TD);
            GroupKind::TableCell(cell.clone(), tag, None)
        };
        push_located(tree, elem, kind)
    } else if let Some(grid) = elem.to_packed::<GridElem>() {
        let group_id = tree.groups.list.next_id();
        let id = tree.ctx.grids.push(GridCtx::new(group_id, grid));
        push_group(tree, elem, GroupKind::Grid(id, None))
    } else if let Some(cell) = elem.to_packed::<GridCell>() {
        // The grid cells are collected into a grid to ensure proper reading
        // order even when using rowspans, which may be laid out later than
        // other cells in the same row.
        let kind = if !matches!(tree.parent_kind(), GroupKind::Grid(..)) {
            // If there is no grid parent, this means a grid layouter is used
            // internally.
            GroupKind::Transparent
        } else if cell.is_repeated.val() {
            // Only repeated grid headers and footer cells are laid out multiple
            // times. Mark duplicate headers as artifacts, since they have no
            // semantic meaning in the tag tree, which doesn't use page breaks
            // for it's semantic structure.
            GroupKind::Artifact(ArtifactType::PaginationOther)
        } else {
            GroupKind::GridCell(cell.clone(), None)
        };
        push_located(tree, elem, kind)
    } else if let Some(heading) = elem.to_packed::<HeadingElem>() {
        let level = heading.level().try_into().unwrap_or(NonZeroU16::MAX);
        let title = heading.body.plain_text().to_string();
        if title.is_empty()
            && let Some(accessibility) = tree.options.validators().accessibility()
        {
            let contains_context = heading.body.traverse(&mut |c| {
                if c.is::<ContextElem>() {
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            });
            let validator = accessibility.as_str();
            tree.errors.push(if contains_context.is_break() {
                error!(
                    heading.span(),
                    "{validator} error: heading title could not be determined";
                    hint: "this seems to be caused by a context expression within the \
                           heading";
                    hint: "consider wrapping the entire heading in a context expression \
                           instead";
                )
            } else {
                error!(heading.span(), "{validator} error: heading title is empty")
            });
        }
        push_tag(tree, elem, Tag::Hn(level, Some(title)))
    } else if let Some(_) = elem.to_packed::<FootnoteElem>() {
        push_located(tree, elem, GroupKind::LogicalParent(elem.clone()))
    } else if let Some(_) = elem.to_packed::<FootnoteEntry>() {
        let note = Tag::Note.with_note_type(Some(krilla::tagging::NoteType::Footnote));
        let id = push_tag(tree, elem, note);
        // Prototype: keep every part of a footnote that runs on to another page.
        if let Some(loc) = elem.location() {
            tree.groups.refs.note_parts.entry(loc).or_default().push(id);
        }
        id
    } else if let Some(quote) = elem.to_packed::<QuoteElem>() {
        // TODO: should the attribution be handled somehow?
        if quote.block.val() {
            push_tag(tree, elem, Tag::BlockQuote)
        } else {
            push_tag(tree, elem, Tag::InlineQuote)
        }
    } else if let Some(raw) = elem.to_packed::<RawElem>() {
        if raw.block.val() {
            push_group(tree, elem, GroupKind::CodeBlock(None))
        } else {
            push_tag(tree, elem, Tag::Code)
        }
    } else if let Some(_) = elem.to_packed::<RawLine>() {
        // If the raw element is inline, the content can be inserted directly.
        if matches!(tree.parent_kind(), GroupKind::CodeBlock(..)) {
            push_group(tree, elem, GroupKind::CodeBlockLine(None))
        } else {
            no_progress(tree)
        }
    } else if let Some(place) = elem.to_packed::<PlaceElem>() {
        if place.float.val() {
            push_located(tree, elem, GroupKind::LogicalParent(elem.clone()))
        } else {
            no_progress(tree)
        }
    } else if let Some(_) = elem.to_packed::<ParElem>() {
        push_weak(tree, elem, GroupKind::Par(None))

    // Text attributes
    } else if let Some(_strong) = elem.to_packed::<StrongElem>() {
        push_text_attr(tree, elem, TextAttr::Strong)
    } else if let Some(_emph) = elem.to_packed::<EmphElem>() {
        push_text_attr(tree, elem, TextAttr::Emph)
    } else if let Some(sub) = elem.to_packed::<SubElem>() {
        push_text_attr(tree, elem, TextAttr::SubScript(sub.clone()))
    } else if let Some(sup) = elem.to_packed::<SuperElem>() {
        push_text_attr(tree, elem, TextAttr::SuperScript(sup.clone()))
    } else if let Some(highlight) = elem.to_packed::<HighlightElem>() {
        push_text_attr(tree, elem, TextAttr::Highlight(highlight.clone()))
    } else if let Some(underline) = elem.to_packed::<UnderlineElem>() {
        push_text_attr(tree, elem, TextAttr::Underline(underline.clone()))
    } else if let Some(overline) = elem.to_packed::<OverlineElem>() {
        push_text_attr(tree, elem, TextAttr::Overline(overline.clone()))
    } else if let Some(strike) = elem.to_packed::<StrikeElem>() {
        push_text_attr(tree, elem, TextAttr::Strike(strike.clone()))
    } else {
        no_progress(tree)
    }
}

/// Whether a list with this numbering is a bullet list rather than a numbered one.
fn is_bullet_numbering(numbering: ListNumbering) -> bool {
    matches!(
        numbering,
        ListNumbering::Disc
            | ListNumbering::Circle
            | ListNumbering::Square
            | ListNumbering::Unordered
    )
}

/// The `ListNumbering` closest to the marker of a bullet list at a depth.
fn bullet_numbering(marker: &ListMarker, depth: usize) -> ListNumbering {
    let ListMarker::Content(markers) = marker else {
        return ListNumbering::Unordered;
    };
    let Some(marker) = markers.get(depth % markers.len().max(1)) else {
        return ListNumbering::Unordered;
    };
    match marker.plain_text().trim() {
        "\u{2022}" | "\u{25CF}" => ListNumbering::Disc,
        "\u{25E6}" | "\u{25CB}" => ListNumbering::Circle,
        "\u{25AA}" | "\u{25A0}" => ListNumbering::Square,
        _ => ListNumbering::Unordered,
    }
}

/// The `ListNumbering` closest to the numbering of a numbered list at a depth.
fn enum_numbering(numbering: &Numbering, depth: usize) -> ListNumbering {
    use codex::numeral_systems::NamedNumeralSystem as System;
    let Numbering::Pattern(pattern) = numbering else {
        return ListNumbering::Ordered;
    };
    // As when the pattern is applied: a level beyond the last piece uses the last.
    let piece = (pattern.pieces.iter())
        .chain(pattern.pieces.last().into_iter().cycle())
        .nth(depth);
    match piece.map(|(_, system)| *system) {
        Some(System::Arabic) => ListNumbering::Decimal,
        Some(System::LowerRoman) => ListNumbering::LowerRoman,
        Some(System::UpperRoman) => ListNumbering::UpperRoman,
        Some(System::LowerLatin) => ListNumbering::LowerAlpha,
        Some(System::UpperLatin) => ListNumbering::UpperAlpha,
        _ => ListNumbering::Ordered,
    }
}

/// Prototype: the number of a line is drawn in the margin after everything else in its
/// column, but belongs to its line. It becomes an `Artifact` structure element that is
/// a logical child of the line's marker, as a footnote entry is of its footnote. Layout
/// cannot say which line a number belongs to without disturbing the line counter, so
/// the two are matched by where they are on the page.
fn push_line_number(
    tree: &mut TreeBuilder,
    elem: &Content,
    kind: ArtifactType,
) -> GroupId {
    let Some(marker) = tree.groups.refs.lines.take_marker() else {
        return push_tag(tree, elem, Tag::Artifact(kind));
    };

    let child = tree.groups.new_virtual(
        tree.current(),
        Span::detached(),
        GroupKind::LogicalChild(Inherit::No, GroupId::INVALID),
    );
    tree.logical_children.entry(marker).or_default().push(child);

    let loc = elem.location().expect("elem to have a location");
    let tag = tree.groups.tags.push(Tag::Artifact(kind));
    let id = tree
        .groups
        .new_virtual(child, elem.span(), GroupKind::Standard(tag, None));
    remember_location(tree, id, loc);
    push_stack_entry(tree, Some(loc), id)
}

fn no_progress(tree: &TreeBuilder) -> GroupId {
    tree.current()
}

fn push_tag(tree: &mut TreeBuilder, elem: &Content, tag: impl Into<TagKind>) -> GroupId {
    let id = tree.groups.tags.push(tag.into());
    push_group(tree, elem, GroupKind::Standard(id, None))
}

fn push_text_attr(tree: &mut TreeBuilder, elem: &Content, attr: TextAttr) -> GroupId {
    push_group(tree, elem, GroupKind::TextAttr(attr))
}

fn push_artifact(tree: &mut TreeBuilder, elem: &Content, ty: ArtifactType) -> GroupId {
    push_group(tree, elem, GroupKind::Artifact(ty))
}

fn push_group(tree: &mut TreeBuilder, elem: &Content, kind: GroupKind) -> GroupId {
    let loc = elem.location().expect("elem to have a location");
    let span = elem.span();
    let parent = tree.current();
    let id = tree.groups.new_virtual(parent, span, kind);
    remember_location(tree, id, loc);
    push_stack_entry(tree, Some(loc), id)
}

/// Prototype: note which element a group belongs to, so that its tag can get an id.
fn remember_location(tree: &mut TreeBuilder, id: GroupId, loc: Location) {
    let group = tree.groups.get_mut(id);
    group.loc = Some(loc);
    let has_tag = !matches!(
        group.kind,
        GroupKind::Root(..)
            | GroupKind::Artifact(..)
            | GroupKind::LogicalParent(..)
            | GroupKind::LogicalChild(..)
            | GroupKind::TextAttr(..)
            | GroupKind::Transparent
            | GroupKind::TableCell(..)
    );
    if has_tag {
        tree.groups.refs.tag_locs.entry(loc).or_insert(id);
    }
}

fn push_located(tree: &mut TreeBuilder, elem: &Content, kind: GroupKind) -> GroupId {
    let loc = elem.location().expect("elem to have a location");
    let span = elem.span();
    let parent = tree.current();
    let id = tree.groups.new_located(loc, parent, span, kind);
    remember_location(tree, id, loc);
    push_stack_entry(tree, Some(loc), id)
}

fn push_weak(tree: &mut TreeBuilder, elem: &Content, kind: GroupKind) -> GroupId {
    let loc = elem.location().expect("elem to have a location");
    let span = elem.span();
    let parent = tree.current();
    let id = tree.groups.new_weak(parent, span, kind);
    // Prototype: a paragraph is left out when it turns out empty, in which case the id
    // made for it here leads nowhere. That case is not handled.
    remember_location(tree, id, loc);
    push_stack_entry(tree, Some(loc), id)
}

fn push_stack_entry(
    tree: &mut TreeBuilder,
    loc: Option<Location>,
    id: GroupId,
) -> GroupId {
    let prog_idx = tree.progressions.len() as u32;
    let entry = StackEntry { loc, id, prog_idx };
    tree.stack.push(entry);
    id
}

fn progress_tree_end(tree: &mut TreeBuilder, loc: Location) -> SourceResult<GroupId> {
    if tree.stack.pop_if(|e| e.loc == Some(loc)).is_some() {
        // The tag nesting was properly closed.
        return Ok(tree.parent());
    }

    // Search for an improperly nested starting tag, that is being closed.
    let Some(stack_idx) = (tree.stack.iter().enumerate())
        .rev()
        .find_map(|(i, e)| (e.loc == Some(loc)).then_some(i))
    else {
        // The start tag isn't in the tag stack, just ignore the end tag.
        return Ok(no_progress(tree));
    };

    let entry = tree.stack[stack_idx];
    let outer = tree.groups.get(entry.id);

    // There are overlapping tags in the tag tree. Figure out whether breaking
    // up the current tag stack is semantically ok, and how to do it.
    let is_pdf_ua = tree.options.validators().accessibility().is_some();
    let mut inner_break_priority = Some(BreakPriority::MAX);
    let mut inner_non_breakable_span = Span::detached();
    let mut inner_non_breakable_in_pdf_ua = false;
    for e in tree.stack.iter().skip(stack_idx + 1) {
        let group = tree.groups.get(e.id);
        let opportunity = tree.groups.breakable(&group.kind);
        let Some(priority) = opportunity.get(is_pdf_ua) else {
            if inner_non_breakable_span.is_detached() {
                inner_non_breakable_span = group.span;
            }
            if let BreakOpportunity::NoPdfUa(_) = opportunity {
                inner_non_breakable_in_pdf_ua = true;
            }
            inner_break_priority = None;
            continue;
        };

        if let Some(inner) = &mut inner_break_priority {
            *inner = (*inner).min(priority);
        }
    }

    let outer_break_opportunity = tree.groups.breakable(&outer.kind);
    let outer_break_priority = outer_break_opportunity.get(is_pdf_ua);

    match (outer_break_priority, inner_break_priority) {
        (Some(outer_priority), Some(inner_priority)) => {
            // Prefer splitting up the inner groups.
            if inner_priority >= outer_priority {
                Ok(split_inner_groups(tree, outer.parent, stack_idx))
            } else {
                Ok(split_outer_group(tree, outer.parent, stack_idx))
            }
        }
        (Some(_), None) => Ok(split_outer_group(tree, outer.parent, stack_idx)),
        (None, Some(_)) => Ok(split_inner_groups(tree, outer.parent, stack_idx)),
        (None, None) => {
            let non_breakable_span = inner_non_breakable_span.or(outer.span);

            let non_breakable_in_pdf_ua = inner_non_breakable_in_pdf_ua
                || matches!(outer_break_opportunity, BreakOpportunity::NoPdfUa(_));

            if non_breakable_in_pdf_ua {
                let validator =
                    tree.options.format.standard.v.config.validators().to_comma_list();
                bail!(
                    non_breakable_span,
                    "{validator} error: invalid document structure, \
                     this element's PDF tag would be split up";
                    hint: "this is probably caused by paragraph grouping";
                    hint: "maybe you've used a `parbreak`, `colbreak`, or `pagebreak`";
                );
            } else {
                bail!(
                    non_breakable_span,
                    "invalid document structure, \
                     this element's PDF tag would be split up";
                    hint: "please report this as a bug";
                );
            }
        }
    }
}

/// Consider the following introspection tags:
/// ```txt
/// start a
///   start b
///     start c
/// end   a
///     end   c
///   end   b
/// ```
/// This will split the inner groups, producing the following tag tree:
/// ```yml
/// - a:
///   - b:
///     - c:
/// - b:
///   - c:
/// ```
fn split_inner_groups(
    tree: &mut TreeBuilder,
    mut parent: GroupId,
    stack_idx: usize,
) -> GroupId {
    // Since the broken groups won't be visited again in any future progression,
    // they'll need to be closed when this progression is visited.
    let num_closed = (tree.stack.len() - stack_idx) as u16;
    tree.breaks.push(Break {
        prog_idx: tree.progressions.len() as u32,
        num_closed,
        num_opened: num_closed - 1,
    });

    // Remove the closed entry.
    tree.stack.remove(stack_idx);

    // Duplicate all broken entries.
    for entry in tree.stack.iter_mut().skip(stack_idx) {
        let new_id = tree.groups.break_group(entry.id, parent);
        *entry = StackEntry {
            loc: entry.loc,
            id: new_id,
            prog_idx: tree.progressions.len() as u32,
        };
        parent = new_id;
    }

    // We're now in a new duplicated group
    tree.parent()
}

/// Consider the following introspection tags:
/// ```txt
/// OPEN a
///   OPEN b
///     OPEN c
/// END  a
///     END  c
///   END  b
/// ```
/// This will split the outer group, producing the following tag tree:
/// ```yml
/// - a:
/// - b:
///   - a:
///   - c:
///     - a:
/// ```
fn split_outer_group(
    tree: &mut TreeBuilder,
    parent: GroupId,
    stack_idx: usize,
) -> GroupId {
    let prev = tree.current();

    // Remove the closed entry;
    let outer = tree.stack.remove(stack_idx);

    // Move the nested group out of the outer entry.
    tree.groups.get_mut(tree.stack[stack_idx].id).parent = parent;

    let mut entry_iter = tree.stack.iter().skip(stack_idx).peekable();
    while let Some(entry) = entry_iter.next() {
        let next_entry = entry_iter.peek().map(|e| e.id);

        let nested = tree.groups.break_group(outer.id, entry.id);

        // Move all children of the stack entry into the nested group.
        for (id, group) in tree.groups.list.ids().zip(tree.groups.list.iter_mut()).rev() {
            // Avoid searching *all* groups! The children of this group are guaranteed to be
            // created after the outer group and thus have a higher ID.
            if id == outer.id {
                break;
            }

            // Don't move the nested group into itself, or the next stack entry
            // into the nested group.
            if group.parent == entry.id && id != nested && Some(id) != next_entry {
                group.parent = nested;
            }
        }

        // Update progressions to jump into the inner entry instead.
        let prev = entry.id;
        for prog in &mut tree.progressions[entry.prog_idx as usize..] {
            if *prog == prev {
                *prog = nested;
            }
        }

        // Either update an existing break, or insert a new one.
        let mut break_idx = Some(tree.breaks.len());
        for (i, brk) in tree.breaks.iter_mut().enumerate().rev() {
            if brk.prog_idx == entry.prog_idx {
                brk.num_closed += 1;
                brk.num_opened += 1;
                break_idx = None;
                break;
            } else if brk.prog_idx < entry.prog_idx {
                break_idx = Some(i + 1);
                break;
            }
        }
        if let Some(idx) = break_idx {
            // Insert a break to close the previous broken group, and enter
            // the new group.
            let brk = Break {
                prog_idx: entry.prog_idx,
                num_closed: 1,
                num_opened: 2,
            };
            tree.breaks.insert(idx, brk);
        }
    }

    // We're still in the same group, but the outer group has been split up.
    debug_assert_eq!(tree.parent(), prev);

    tree.parent()
}

// Prototype: kept at the end of the file, because a test of Typst's names a line
// number further up.
impl TreeBuilder<'_> {
    fn pdf20(&self) -> bool {
        self.options.version() >= krilla::configure::PdfVersion::Pdf20
    }

    /// How many lists whose numbering matches enclose the current position.
    fn list_depth(&self, matches: impl Fn(ListNumbering) -> bool) -> usize {
        (self.stack.iter())
            .filter(|entry| match &self.groups.get(entry.id).kind {
                GroupKind::List(_, numbering, _) => matches(*numbering),
                _ => false,
            })
            .count()
    }
}
