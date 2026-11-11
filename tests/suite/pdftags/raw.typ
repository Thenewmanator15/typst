--- raw-tags-basic-block pdftags ---
#set pdf(standard: "ua-1")
```rs
const PDF_STANDARD: &str = "PDF/UA-1";

fn main() {
    println!("hello {PDF_STANDARD}");
}
```

--- raw-tags-basic-block-ua-2 pdftags ---
// PDF/UA-2 does not allow a `P` inside `Code`, so each line is a `Sub`.
#set pdf(standard: "ua-2")
```rs
fn main() {
    println!("hello");
}
```

--- raw-tags-basic-inline pdftags ---
#set pdf(standard: "ua-1")
Some `inline raw` text and the rust ```rs fn``` keyword.
