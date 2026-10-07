#set page(paper: "a5", numbering: "1")
#set heading(numbering: "1.1")
#set par(justify: true)

#align(center)[
  #text(size: 20pt, weight: "bold")[Live Typst in Emacs]

  _Rendered into canvas images by a Rust module_
]

#outline()

= Introduction

Each edit sends the buffer text to a background thread. The thread compiles it with the `typst`
crate, rasterizes the pages, and tells Emacs through a pipe. Only pages that changed are copied
into their canvases.

#lorem(80)

= Math

The Gaussian integral:
$ integral_(-oo)^oo e^(-x^2) dif x = sqrt(pi) $

And a matrix:
$ A = mat(1, 2; 3, 4), quad det A = -2 $

#lorem(60)

= Tables and code

#table(
  columns: 3,
  table.header[*Step*][*Thread*][*Lisp*],
  [1], [compile], [keep editing],
  [2], [render pages], [keep editing],
  [3], [notify], [copy changed pages],
)

```rust
fn present(canvas: Value, image: &PageImage) -> Result<bool> {
    canvas.with_canvas_data(|data| data.buffer.copy_from_slice(&image.pixels))
}
```

#lorem(120)

= Conclusion

#lorem(150)
