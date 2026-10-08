// A showcase for typst-canvas. It uses no packages, so it works offline. Colors that must follow
// the Emacs theme come from `text.fill`; fixed colors are bright, or transparent tints.

#let violet = rgb("#7048e8")
#let blue = rgb("#1c7ed6")
#let teal = rgb("#0ca678")
#let orange = rgb("#f76707")
#let tint(color) = color.transparentize(82%)

#set document(title: "typst-canvas showcase")
#set page(
  paper: "a5",
  margin: (x: 1.5cm, top: 1.7cm, bottom: 1.6cm),
  header: context {
    if counter(page).get().first() > 1 {
      set text(size: 7.5pt)
      emph[Typst, live in Emacs]
      h(1fr)
      counter(page).display()
    }
  },
)
#set text(size: 10pt)
#set par(justify: true, leading: 0.62em)
#set heading(numbering: "1.")
#show heading.where(level: 1): it => block(above: 1.3em, below: 0.8em, {
  if it.numbering != none {
    text(fill: blue, counter(heading).display())
    h(0.4em)
  }
  it.body
})
#show link: it => if type(it.dest) == str { underline(text(fill: blue, it)) } else { it }
#show raw.where(block: true): block.with(fill: tint(blue), inset: 8pt, radius: 4pt, width: 100%)
#show figure.caption: set text(size: 8.5pt)

#block(
  width: 100%,
  inset: (x: 14pt, y: 16pt),
  radius: 6pt,
  fill: gradient.linear(violet, blue, teal, angle: 15deg),
)[
  #set text(fill: white)
  #text(size: 21pt, weight: "bold")[Typst, live in Emacs]
  #v(-6pt)
  #text(size: 10.5pt)[Compiled on a background thread, painted into canvas images]
]

#outline(indent: auto)

= Introduction

This page comes from a Typst file that Emacs compiles while you type. A Rust module runs the
compiler on a background thread, rasterizes the pages, and copies the pixels into canvas
images#footnote[Canvas images are new in Emacs 32. Rust writes their pixels directly.]. Click
any word to jump to its source, and watch the caret follow the cursor. Learn more about the
language at #link("https://typst.app/docs")[typst.app/docs].

= Equations

Maxwell's equations, in differential form:
$ nabla dot bold(E) = rho / epsilon_0, quad nabla dot bold(B) = 0 $
$ nabla times bold(E) = - (partial bold(B)) / (partial t), quad
  nabla times bold(B) = mu_0 bold(J) + mu_0 epsilon_0 (partial bold(E)) / (partial t) $

The Fourier transform of a function $f$:
$ hat(f)(xi) = integral_(-oo)^(oo) f(x) e^(-2 pi i x xi) dif x $

#block(breakable: false)[
  A tridiagonal matrix and a piecewise function:
  $ A = mat(2, -1, 0; -1, 2, -1; 0, -1, 2), quad det A = 4, quad
    |x| = cases(x & "if" x >= 0, -x & "otherwise") $
]

= Figures

#let data = (("Mon", 12), ("Tue", 19), ("Wed", 15), ("Thu", 24), ("Fri", 21), ("Sat", 9), ("Sun", 6))
#let peak = calc.max(..data.map(((_, value)) => value))

#figure(
  grid(
    columns: data.len(),
    column-gutter: 10pt,
    row-gutter: 4pt,
    align: center + bottom,
    ..data.map(((_, value)) => {
      text(size: 7pt)[#value]
      v(-2pt)
      rect(
        width: 18pt,
        height: value / peak * 2.6cm,
        radius: (top: 3pt),
        fill: gradient.linear(teal, blue, angle: 90deg),
      )
    }),
    ..data.map(((day, _)) => text(size: 7.5pt, day)),
  ),
  caption: [Edits per day, drawn from a data array with a loop.],
)

#let plot(functions, width: 100%, height: 3cm, domain: (0, 4 * calc.pi), samples: 120) = layout(size => {
  let width = if type(width) == ratio { size.width * width } else { width }
  let (start, end) = domain
  let x-of(x) = (x - start) / (end - start) * width
  let y-of(y) = height / 2 - y * height / 2.3
  context {
    let ink = text.fill
    box(width: width, height: height, {
      place(line(start: (0pt, height / 2), end: (width, height / 2), stroke: 0.6pt + ink))
      place(line(start: (0pt, 0pt), end: (0pt, height), stroke: 0.6pt + ink))
      for (f, color) in functions {
        let points = range(samples + 1).map(i => {
          let x = start + (end - start) * i / samples
          (x-of(x), y-of(f(x)))
        })
        place(curve(
          stroke: 1.4pt + color,
          curve.move(points.first()),
          ..points.slice(1).map(point => curve.line(point)),
        ))
      }
    })
  }
})

#figure(
  plot(((x => calc.sin(x), blue), (x => calc.exp(-x / 5) * calc.cos(2 * x), orange))),
  caption: [$sin x$ and a damped $e^(-x\/5) cos 2x$, plotted with `curve`.],
)

= Data

#table(
  columns: (auto, 1fr, auto),
  stroke: none,
  inset: (x: 6pt, y: 5pt),
  align: (left, left, right),
  fill: (_, y) => if y == 0 { blue } else if calc.even(y) { tint(blue) },
  table.header(..([*Stage*], [*Where*], [*Time*]).map(cell => text(fill: white, cell))),
  [Edit], [Emacs sends the buffer text], [0.1 ms],
  [Compile], [Typst, incremental], [1 ms],
  [Render], [`typst-render`, pages in parallel], [8 ms],
  [Notify], [A byte through a pipe], [0.1 ms],
  [Present], [Copy into the canvas], [1 ms],
  table.hline(),
)

= Code

#block(breakable: false)[
The defun that copies a page into its canvas:

```rust
// Called from Lisp, after the thread rendered the page.
fn present_page(
    session: &Session,
    index: usize,
    canvas: Value,
) -> Result<bool> {
    let image = session.output().pages.get(index).cloned();
    let Some(image) = image else { return Ok(false) };
    canvas.with_canvas_data(|data| {
        data.buffer.copy_from_slice(&image.pixels);
        true
    })
}
```
]

= Two columns

#columns(2, gutter: 14pt)[
  The compile thread only ever sees the newest text. If you type faster than it compiles, it
  skips the stale versions#footnote[A one-element slot with a condition variable.], so the
  preview never lags behind by more than one compile.

  Pages whose frames did not change keep their images. A typical edit re-renders one page,
  unless it reflows the pages after it.

  #colbreak()

  Theme matching sets the page and text colors as standard library styles, not as `#set`
  rules in your source. Source positions stay valid, so click-to-jump keeps
  working#footnote[Even while an error keeps the last good render.].

  Each page is one image on its own line, so Emacs scrolls through them like through lines
  of text.
]

= Conclusion

Everything above is plain Typst: no packages, no external tools. Edit any of it, and the
preview follows a few milliseconds later.

#lorem(70)
