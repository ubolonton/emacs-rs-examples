// Slides about typst-canvas, for `M-x typst-canvas-present'. They set
// their own colors, so they look the same in every Emacs theme. Fonts
// are the ones embedded in Typst, so the slides need no system fonts.

#let night = rgb("#0c1222")
#let panel = rgb("#18213a")
#let ink = rgb("#e8ecf6")
#let muted = rgb("#8d97b3")
#let violet = rgb("#8b6cff")
#let blue = rgb("#3d9bff")
#let teal = rgb("#1fc7a0")
#let orange = rgb("#ff9248")
#let accent = gradient.linear(violet, blue, teal)

#set document(title: "typst-canvas")
#set page(
  paper: "presentation-16-9",
  fill: night,
  margin: (x: 2.4cm, top: 2cm, bottom: 1.6cm),
  footer: context {
    set text(size: 12pt, fill: muted)
    [typst-canvas]
    h(1fr)
    counter(page).display("1 / 1", both: true)
  },
)
#set text(font: "Libertinus Serif", size: 22pt, fill: ink)
#set par(leading: 0.7em)
#show raw: set text(font: "DejaVu Sans Mono", size: 0.8em)
#show math.equation: set text(font: "New Computer Modern Math")

// A slide: a title with a gradient rule under it, then the body.
#let slide(title, body) = {
  pagebreak(weak: true)
  text(size: 34pt, weight: "bold", title)
  v(-0.5em)
  box(width: 3.2cm, height: 4pt, radius: 2pt, fill: accent)
  v(0.8em)
  body
}

// A rounded panel with a colored top edge.
#let card(color, title, body) = block(
  width: 100%,
  height: 100%,
  inset: (x: 14pt, top: 12pt, bottom: 12pt),
  radius: 8pt,
  fill: panel,
  stroke: (top: 3pt + color),
  {
    text(size: 18pt, weight: "bold", fill: color, title)
    v(-0.4em)
    set text(size: 15pt, fill: ink)
    body
  },
)

// Title slide.
#set page(footer: none)
#let faded(amount) = gradient.linear(
  ..(violet, blue, teal).map(color => color.transparentize(amount)),
  angle: 30deg,
)
#place(top + right, dx: 3.4cm, dy: -3.2cm, circle(radius: 6.5cm, fill: faded(85%)))
#place(top + right, dx: 1.2cm, dy: -1.4cm, circle(radius: 3.2cm, fill: faded(55%)))
#v(2.6cm)
#text(size: 60pt, weight: "bold")[typst-canvas]
#v(-1.1em)
#box(width: 7cm, height: 6pt, radius: 3pt, fill: accent)
#v(0.3em)
#text(size: 26pt)[Live Typst preview inside Emacs 32]
#v(0.2em)
#text(size: 17pt, fill: muted)[
  Compiled on a background thread · painted into canvas images ·
  no PDF viewer, no browser
]
#place(bottom + left, text(size: 13pt, fill: muted)[
  SPC next · DEL previous · 3 RET slide 3 · q quit
])

#set page(footer: context {
  set text(size: 12pt, fill: muted)
  [typst-canvas]
  h(1fr)
  counter(page).display("1 / 1", both: true)
})

#slide[From key press to pixels][
  #let stage(color, name, detail, time) = box(
    width: 100%,
    height: 3.4cm,
    inset: 10pt,
    radius: 8pt,
    fill: panel,
    stroke: (bottom: 3pt + color),
    align(center + horizon, {
      text(size: 18pt, weight: "bold", fill: color, name)
      linebreak()
      text(size: 13pt, fill: muted, detail)
      linebreak()
      text(size: 20pt, time)
    }),
  )
  #let arrow = align(horizon, text(size: 26pt, fill: muted)[→])
  #v(0.6em)
  #grid(
    columns: (1fr, auto, 1fr, auto, 1fr, auto, 1fr, auto, 1fr),
    column-gutter: 8pt,
    align: horizon,
    stage(violet, [Edit], [buffer text], [0.1 ms]), arrow,
    stage(blue, [Compile], [incremental], [1 ms]), arrow,
    stage(teal, [Render], [in parallel], [7 ms]), arrow,
    stage(orange, [Notify], [one byte], [0.1 ms]), arrow,
    stage(violet, [Present], [canvas copy], [5 ms]),
  )
  #v(1em)
  A Rust thread only ever works on the newest text. Pages whose frames
  did not change keep their images, so a key press usually re-renders
  one page.
]

#slide[Fast enough to type][
  #let bar(label, value, color, max: 680) = grid(
    columns: (8cm, 1fr),
    align: horizon,
    text(size: 17pt, label),
    box(width: 100%, {
      box(width: value / max * 100%, height: 22pt, radius: 4pt, fill: color)
      h(8pt)
      text(size: 16pt)[#value ms]
    }),
  )
  Median time from an edit to the screen, 4-page document:
  #v(0.4em)
  #stack(
    spacing: 12pt,
    bar([One char], 22, teal),
    bar([Reflow 3 pages], 30, teal),
    bar([One char, unoptimized], 148, orange.transparentize(30%)),
    bar([Reflow, unoptimized], 551, orange.transparentize(30%)),
  )
  #v(0.6em)
  #text(size: 17pt, fill: muted)[
    Unoptimized: the crate at `opt-level` 0, pages rendered one by one.
  ]
]

#slide[Math as you type][
  #grid(
    columns: (1fr, 1fr),
    column-gutter: 1.2cm,
    [
      Put point in an equation, and it shows rendered right below its
      source line, at the size of the buffer text.

      #v(0.3em)
      #text(size: 17pt, fill: muted)[
        It is cut out of the laid-out page: your `#set` and `#let`
        rules apply, and no extra compile runs. While the text does not
        compile, it shows dimmed.
      ]
    ],
    block(width: 100%, inset: 16pt, radius: 8pt, fill: panel, {
      set text(size: 20pt)
      $ integral_(-oo)^oo e^(-x^2) dif x = sqrt(pi) $
      $ sum_(n=1)^oo 1 / n^2 = pi^2 / 6 $
      $ e^(i pi) + 1 = 0 $
    }),
  )
]

#slide[What you get][
  #grid(
    columns: (1fr, 1fr, 1fr),
    rows: (4.2cm, 4.2cm),
    gutter: 14pt,
    card(violet, [Live preview])[Every key press, without blocking Emacs.],
    card(blue, [Click to jump])[A click on a page goes to its source.],
    card(teal, [Caret])[The cursor shows on the page, and stays in view.],
    card(orange, [Theme colors])[Pages take the colors of your theme.],
    card(blue, [Errors])[Flymake marks them. The last good pages stay.],
    card(violet, [Slides])[This deck: one page at a time, live.],
  )
]

#slide[Try it][
  #grid(
    columns: (1.15fr, 1fr),
    column-gutter: 1.2cm,
    block(width: 100%, inset: 16pt, radius: 8pt, fill: panel)[
      #set text(size: 20pt)
      ```elisp
      (require 'typst-canvas)
      ;; In a Typst buffer:
      M-x typst-canvas-mode
      ;; Watch it type by itself:
      M-x typst-canvas-demo
      ;; Present the pages:
      M-x typst-canvas-present
      ```
    ],
    align(horizon, {
      set text(size: 21pt)
      table(
        columns: 2,
        stroke: none,
        inset: (x: 6pt, y: 7pt),
        fill: (_, y) => if calc.odd(y) { panel },
        [`SPC` `n` `→`], [next slide],
        [`DEL` `p` `←`], [previous slide],
        [`3` `RET`], [slide 3],
        [`q`], [quit],
      )
    }),
  )
]
