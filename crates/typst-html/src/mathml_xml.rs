//! Presentation MathML for an equation as a standalone XML string, for use
//! outside HTML export (PDF/UA-2 attaches it to the `Formula` tag).

use comemo::{Track, Tracked};
use ecow::{EcoString, EcoVec};
use typst_library::diag::SourceResult;
use typst_library::engine::{Engine, Route, Sink, Traced};
use typst_library::foundations::{Packed, StyleChain, Target, TargetElem};
use typst_library::introspection::{Introspector, Locator};
use typst_library::{Library, World};
use typst_utils::{LazyHash, Protected};
use typst_library::math::EquationElem;
use typst_library::math::ir::resolve_equation;
use typst_library::routines::Arenas;
use typst_library::text::SmartQuoter;

use crate::HtmlNode;
use crate::convert::Whitespace;
use crate::fragment::html_math_fragment;
use crate::mathml::convert_math_to_nodes;

/// Converts an equation to presentation MathML.
///
/// Returns `None` if the equation has no location yet. Warnings of the
/// conversion are dropped: they concern HTML export, which the user did not
/// ask for.
pub fn equation_mathml(
    elem: &Packed<EquationElem>,
    engine: &mut Engine,
    styles: StyleChain,
) -> SourceResult<Option<EcoString>> {
    equation_mathml_impl(
        elem,
        engine.world,
        engine.library,
        engine.introspector.into_raw(),
        engine.traced,
        engine.route.track(),
        styles,
    )
}

/// Memoized, so that the layout iterations of a document share the work.
#[comemo::memoize]
fn equation_mathml_impl(
    elem: &Packed<EquationElem>,
    world: Tracked<dyn World + '_>,
    library: &LazyHash<Library>,
    introspector: Tracked<dyn Introspector + '_>,
    traced: Tracked<Traced>,
    route: Tracked<Route>,
    styles: StyleChain,
) -> SourceResult<Option<EcoString>> {
    let Some(location) = elem.location() else { return Ok(None) };

    let mut sink = Sink::new();
    let mut engine = Engine {
        library,
        world,
        introspector: Protected::from_raw(introspector),
        traced,
        sink: sink.track_mut(),
        route: Route::extend(route),
    };

    let target = TargetElem::target.set(Target::Html).wrap();
    let styles = styles.chain(&target);

    let arenas = Arenas::default();
    let item = resolve_equation(
        elem,
        &mut engine,
        Locator::synthesize(location),
        &arenas,
        styles,
    )?;
    let block = elem.block.get(styles);
    let body = convert_math_to_nodes(item, &mut engine, styles, block)?;
    let body = typst_library::foundations::Content::sequence(body);

    let mut locator = Locator::synthesize(location).split();
    let nodes = html_math_fragment(
        &mut engine,
        &body,
        &mut locator,
        &mut SmartQuoter::new(),
        styles,
        Whitespace::Normal,
    )?;

    let mut buf = EcoString::new();
    buf.push_str("<math xmlns=\"http://www.w3.org/1998/Math/MathML\"");
    if block {
        buf.push_str(" display=\"block\"");
    }
    buf.push('>');
    write_nodes(&mut buf, &nodes);
    buf.push_str("</math>");
    Ok(Some(buf))
}

fn write_nodes(buf: &mut EcoString, nodes: &EcoVec<HtmlNode>) {
    for node in nodes {
        match node {
            HtmlNode::Text(text, _) => write_escaped(buf, text),
            HtmlNode::Element(element) => {
                let tag = element.tag.resolve();
                buf.push('<');
                buf.push_str(&tag);
                for (attr, value) in &element.attrs.0 {
                    buf.push(' ');
                    buf.push_str(&attr.resolve());
                    buf.push_str("=\"");
                    write_escaped(buf, value);
                    buf.push('"');
                }
                if element.children.is_empty() {
                    buf.push_str("/>");
                } else {
                    buf.push('>');
                    write_nodes(buf, &element.children);
                    buf.push_str("</");
                    buf.push_str(&tag);
                    buf.push('>');
                }
            }
            // Tags carry no content, and a laid out frame has no MathML form.
            HtmlNode::Tag(_) | HtmlNode::Frame(_) => {}
        }
    }
}

fn write_escaped(buf: &mut EcoString, text: &str) {
    for c in text.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            '"' => buf.push_str("&quot;"),
            // Control characters other than these are not allowed in XML.
            c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => {}
            c => buf.push(c),
        }
    }
}
