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

Known shortcuts: links split over two lines and footnotes that run on to another page are
handled but not covered by a test document; a paragraph that turns out empty would leave a
reference to nothing; term lists still fail PDF/UA-2 (8.2.5.25). `tests/src/run.rs` has a
local change so that the test suite runs on Windows without Developer Mode.
