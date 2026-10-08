# PDF/UA-2 prototype

This branch is an experiment, not a contribution. The code on it was written by Claude (an
AI assistant), and Typst does not accept AI-written contributions, so it is not offered as
a pull request. It exists to check, with veraPDF, what Typst would have to pass to krilla
for PDF/UA-2.

It adds a `ua-2` value for `--pdf-standard` and builds against
https://github.com/Thenewmanator15/krilla/tree/ua2-on-typst-pin, which is krilla at the
commit Typst pins (`7772dbe`) with the PDF/UA-2 commits described in
https://github.com/LaurenzV/krilla/issues/449 replayed on it.

```
cargo build --release -p typst-cli
target/release/typst compile --pdf-standard ua-2 document.typ
verapdf --flavour ua2 document.pdf
```

What it does, what it was checked with and what it does not show are written up at
https://thenewmanator15.github.io/typst-pdf-ua2/.

Equations get presentation MathML, which PDF/UA-2 requires (8.2.5.29.1). It is made by the
converter that HTML export already has (`typst-html/src/mathml.rs`), written out as XML and
attached to the `Formula` tag as an associated file. With it, an equation no longer needs
`alt` for PDF/UA-2. Because making MathML costs about as much as laying an equation out, it
is only made when `ua-2` is asked for: with `--pdf-standard` on the command line, where the
CLI sets an internal style, `EquationElem::mathml_wanted`, or with a
`set pdf(standard: "ua-2")` rule at the start of the document, which a format can now
answer for (`Format::with_mathml`). Together with
PDF/A-4 the standard has to be `a-4f`, because PDF/A-4 only allows attached files that are
PDF/A themselves.

In PDF 2.0, a link to a place in the same document is tagged `Reference` rather than `Link`
(8.2.5.20), a list of terms has the `ListNumbering` `Description` (8.2.5.25), and a figure
and its caption are wrapped in an `Aside` (8.2.5.27). ISO 32005 does not allow an `Aside` in
a table cell, a block quote or another `Aside`, so there the wrapper is a `Sect`, which is
allowed but a poorer fit. An outline filler other than the default `repeat` is made an
artifact (8.2.5.8).

After reading the whole of clause 8 of ISO 14289-2, also in PDF 2.0: the number of a
numbered equation is an `Lbl` (8.2.5.16), a footnote has the `NoteType` `Footnote`
(8.2.5.14.2), the `ListNumbering` of a list follows its marker or numbering pattern
(8.2.5.25), and the bibliography is a `Sect` with the ARIA role `doc-bibliography`
(8.2.5.31), for which `BibliographyElem` is now `Tagged`. With `ua-2`, heading levels
may skip, since PDF/UA-2 does not ask for them to follow on (8.2.5.12).

Not done: line numbers are plain artifacts, not `Artifact` structure elements (8.3.2);
headings are not grouped into `Sect` elements (8.2.5.5, a recommendation); and when an
equation is given `alt`, its number is not labelled, because tags inside an element with
an alternative description are left out.

Known shortcuts: links split over two lines and footnotes that run on to another page are
handled but not covered by a test document; a paragraph that turns out empty would leave a
reference to nothing. `tests/src/run.rs` has a
local change so that the test suite runs on Windows without Developer Mode.
