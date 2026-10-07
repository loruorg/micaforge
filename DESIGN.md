---
name: Micaforge
description: Cookieless analytics that counts machine readers as an audience, drawn as a continuous-trace recording station
colors:
  soot-ground: "#13110e"
  soot-panel: "#191612"
  soot-raised: "#201c17"
  soot-well: "#0c0b09"
  paper-ink: "#e9e3d6"
  paper-ink-2: "#b4ac9c"
  paper-ink-3: "#8c8474"
  stylus-white: "#f5f0e4"
  shellac: "#c98a46"
  shellac-hi: "#e3ba7e"
  shellac-dim: "#a76b33"
  oxide: "#c4694f"
  oxide-deep: "#b4553f"
  verdigris: "#74a388"
  verdigris-deep: "#5e8c72"
  chart-ground: "#dddad0"
  chart-panel: "#e7e4db"
  chart-ink: "#1a1714"
  chart-shellac: "#7e4e1c"
typography:
  display:
    fontFamily: "Trench Slab, Iowan Old Style, Palatino Linotype, serif"
    fontSize: "clamp(2.15rem, 4.4vw, 4.6rem)"
    fontWeight: 600
    lineHeight: 0.99
    letterSpacing: "-0.026em"
  headline:
    fontFamily: "Trench Slab, Iowan Old Style, Palatino Linotype, serif"
    fontSize: "1.75rem"
    fontWeight: 600
    lineHeight: 1.06
    letterSpacing: "-0.018em"
  title:
    fontFamily: "Trench Slab, Iowan Old Style, Palatino Linotype, serif"
    fontSize: "1.375rem"
    fontWeight: 600
    lineHeight: 1.15
  body:
    fontFamily: "system-ui, -apple-system, Segoe UI, Roboto, Helvetica Neue, Arial, sans-serif"
    fontSize: "0.9375rem"
    fontWeight: 400
    lineHeight: 1.58
  data:
    fontFamily: "Tabular, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "0.8125rem"
    fontWeight: 400
    fontFeature: "tnum 1, lnum 1"
  label:
    fontFamily: "Tabular, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "0.6875rem"
    fontWeight: 500
    letterSpacing: "0.09em"
rounded:
  none: "0"
  sm: "2px"
  md: "3px"
spacing:
  "1": "0.25rem"
  "2": "0.5rem"
  "3": "0.75rem"
  "4": "1rem"
  "5": "1.25rem"
  "6": "1.5rem"
  "7": "1.75rem"
  "8": "2rem"
  "10": "2.5rem"
  "12": "3rem"
  "16": "4rem"
  "24": "6rem"
components:
  button-key:
    backgroundColor: "{colors.soot-raised}"
    textColor: "{colors.paper-ink}"
    rounded: "{rounded.sm}"
    padding: "7px 1rem"
    typography: "{typography.data}"
  button-key-hover:
    backgroundColor: "{colors.soot-panel}"
    textColor: "{colors.paper-ink}"
    rounded: "{rounded.sm}"
    padding: "7px 1rem"
  button-primary:
    backgroundColor: "{colors.shellac}"
    textColor: "{colors.soot-ground}"
    rounded: "{rounded.sm}"
    padding: "9px 1.25rem"
  button-primary-hover:
    backgroundColor: "{colors.shellac-hi}"
    textColor: "{colors.soot-ground}"
    rounded: "{rounded.sm}"
    padding: "9px 1.25rem"
  button-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.paper-ink-2}"
    rounded: "{rounded.sm}"
    padding: "7px 0.5rem"
  panel:
    backgroundColor: "{colors.soot-panel}"
    textColor: "{colors.paper-ink}"
    rounded: "{rounded.md}"
    padding: "1.25rem"
  readout-value:
    textColor: "{colors.paper-ink}"
    typography: "{typography.headline}"
  input-well:
    backgroundColor: "{colors.soot-well}"
    textColor: "{colors.paper-ink-2}"
    rounded: "{rounded.sm}"
    padding: "7px 0.75rem"
---

# Design System: Micaforge

## Overview

**Creative North Star: "The Smoked Drum Record"**

Before there were dashboards there were recording stations: a drum wrapped in paper
blackened over a lamp, a stylus resting against it, and a clock turning the drum under the
pen. The pen did not draw a line onto the paper. It scratched the soot away, and the line
you read was the bright paper underneath, with a small ridge of displaced soot pushed up
along one edge of the cut. Micaforge is that instrument. The interface is the record; the
data is what the pens wrote; and the product's whole argument is that there are two pens,
one for the people reading a site and one for the machines, writing on the same paper.

The register is an instrument, not an app. It is dense where a working screen should be
dense and quiet where a reader needs to think. Nothing floats, nothing glows, and nothing
bounces when you point at it. Depth comes from the scratch: a one-pixel ridge in the
surface's own colour, cast in one direction, doing the job a drop shadow would do in a
softer world. That single decision is why there is no glow anywhere in this product, and it
is the detail to protect above all others.

The anti-reference is explicit and was named before a line was written: the category's
near-black dashboard with a neon accent, glowing card borders and a grid of same-size stat
cards. Micaforge refuses all four. A metric is never a card. Colour never carries meaning
alone. The accent is a varnish, not a highlighter.

**Key Characteristics:**
- Warm lampblack ground, never a cool blue-charcoal
- Bright paper scratched out of dark, edged by a ridge of displaced soot
- One accent (shellac amber), with series differentiated by value rather than hue
- Square corners; 2px on controls, 3px on panels, and nothing rounder
- Ruling lives inside a panel, clipped to it; there is no page-wide grid anywhere
- Two pens: the machine pen steps, the human pen is written
- Genuine data in a data face; prose in system-ui; display in a slab serif

## Colors

A lampblack ground with the paper showing through, varnished with one amber. The palette
was chosen against computed contrast ratios, not by eye, and every value names a physical
material rather than a UI role.

### Primary
- **Shellac** (`#c98a46`): the fixative varnish, and the only accent. It marks the machine
  pen, the active channel in the rail, the one committed action on a screen, and the
  Citation Gap figure. 6.5:1 on the ground. Its lighter step **Shellac Hi** (`#e3ba7e`) is
  the pressed and hovered state and the focus ring.

### Secondary
- **Oxide** (`#c4694f`): the red annotation pen. Negative verdicts, destructive controls,
  and the Extracted state. 4.9:1, so it is safe as text.
- **Verdigris** (`#74a388`): the green annotation pen. Positive verdicts and the
  Compounding state. 6.6:1.

These two are the only hues besides shellac, and they exist because a station operator
carried two annotation pens. They mark verdicts. They are never decoration and never a
fourth and fifth brand colour.

### Neutral
- **Soot Ground** (`#13110e`): the blackened drum. The page, the rail, the top bar.
- **Soot Panel** (`#191612`): a sheet of the instrument's paper. Every panel surface.
- **Soot Raised** (`#201c17`): a pressed key, a hovered row, an active channel.
- **Soot Well** (`#0c0b09`): sunken wells. Inputs, code blocks, the perforation holes.
- **Paper Ink** (`#e9e3d6`): body and heading text. 14.7:1.
- **Paper Ink 2** (`#b4ac9c`): secondary text and table bodies. 8.4:1.
- **Paper Ink 3** (`#8c8474`): the dimmest text permitted anywhere. 5.1:1.
- **Stylus White** (`#f5f0e4`): the human trace itself. 16.6:1.

### The paper medium
The light theme is not an inverted dark theme; it is the same record transcribed onto chart
paper. The ground is a toned mineral grey (`#dddad0`), deliberately not the warm cream that
every "tasteful" light theme defaults to, and the traces invert: the human pen becomes dark
ink on light paper.

### Named Rules

**The Value Ramp Rule.** Categorical series are five lightness steps of one hue
(`#f2d9ae` → `#8a552a`), never five different hues. A chart that needs six colours has too
many series. This is why every chart survives greyscale printing and every common form of
colour blindness.

**The Two Pens Rule.** The machine pen steps (a crawler fetches in discrete jumps); the
human pen is written (a reader arrives continuously). Channel identity is carried by the
LINE'S OWN CHARACTER before it is carried by colour, in the plot and in the legend swatch
alike. Colour is never the only encoding of anything.

**The No Glow Rule.** There is no glow, halo, bloom or zero-offset coloured shadow anywhere
in this product. Depth is the scratch ridge: `0 1px 0` in the ground's own colour, cast in
one direction. If an element needs to lift, it shifts value.

## Typography

**Display Font:** Trench Slab (self-hosted woff2, with Iowan Old Style and Palatino as
fallbacks)
**Body Font:** system-ui
**Data Font:** Tabular (self-hosted woff2, with ui-monospace as fallback)

**Character:** A slab serif with weight and a slightly mechanical joint, chosen against
rendered specimens rather than by reputation, so headings and big figures read as stamped
into the sheet rather than set in a UI font. Beneath it, system-ui does the ordinary work
of an interface without pretending to be a brand. Tabular is a true tabular-figure
monospace and appears only where the content is genuinely data.

**The Data Face Rule.** Tabular is for timestamps, counts in columns, paths, user-agent
strings, status codes, code, and identifiers. It is never a costume for "technical". A
monospace worn by eyebrows, captions and running labels is the single loudest tell that
nobody chose the typography, and this product does not do it.

**Three small-text treatments, deliberately distinct.** The rail's wordmark is the display
face; the channel-bank headings are the data face in tracked caps because they name a real
hardware grouping; the channel names themselves are plain UI sentence case. One costume on
every small string would read as a template.

**No eyebrows.** There is no kicker label above any heading anywhere in this product. A
heading carries its own weight.

Scale: 11px (instrument labels only) · 12 · 13 (dense table body) · 15 (UI body) · 17 ·
22 · 28 · 36 · 48 · 88 (display ceiling). Body measure is capped at 68ch.

## Layout

**The station spine.** A 232px fixed rail carries the wordmark, the station code set
vertically against the paper edge, the channel banks, and the medium switch. It becomes a
drawer below 1080px. It is not a flush column of links: routes are grouped the way a
recorder's channel bank is grouped, and the three routes that exist nowhere else in the
category (agent traffic, citation gap, policy) are their own bank.

**Rhythm.** A 4px base scale. One spacing rhythm throughout, with more space above a
heading than below it. Density alternates deliberately: a dense ledger is followed by airier
verdict rows, then a dense table again.

**Breakpoints.** 1080px (rail becomes a drawer), 1000px (two-column blocks stack), 760px
and 700px (readouts reflow to two columns, panels lose their outer padding), 620px (the
drum reduces its margins and its annotation set), 560px (live readout units drop).

**Wide content scrolls inside its own container.** Tables and records scroll horizontally
within an `overflow-x: auto` region. The page body never scrolls sideways.

## Elevation & Depth

**Tonal, with one directional cut.** There is no ambient shadow system. Surfaces separate by
value (`ground` → `panel` → `raised`), by a self-coloured 1px edge at 8.5% ink, and by an
inner top highlight (`inset 0 1px 0`) that reads as light catching a rounded lip.

The one real shadow in the system is the **scratch ridge**: `0 1px 0` of the well colour,
cast downward only, in the ground's own tone. Every trace in every chart is drawn twice, the
ridge first and the bright line over it. In the paper medium the ridge inverts to a light
value, because displaced material catches light differently on a light substrate.

`--mf-lift` (a tight two-layer shadow) exists for the one element that genuinely floats: the
rail when it becomes a drawer. It is not available as a card decoration.

## Shapes

**Square, because paper and instrument panels are square.** 2px on controls, 3px on panels,
0 on rules and traces. The only fully rounded shape in the product is the caliper handle.

**The pin feed.** Primary record panels carry a 22px left gutter of sprocket perforations at
a 16px pitch, drawn as a radial-gradient of well-coloured holes against a dashed edge.
Content is padded clear of the gutter by a full step, so the cut can never crop a word.

**Panel ruling.** A panel may carry the instrument's own 24px ruling, masked to fade at its
top and bottom edges and clipped to the panel's own border radius. This is the one grid in
the product and it never leaves a panel. A page-wide grid background is forbidden.

**Bespoke iconography.** Icons are drawn on a 16×16 grid with a 1.4 stroke, square caps,
mitred joins, and orthogonal or 45-degree geometry only. Every glyph carries exactly one
filled 1.4-square **registration tick**, the fiducial mark an instrument panel uses to say
where zero is. That tick is the invented detail that makes the set this product's own. A new
glyph with no honest place for it is the wrong glyph for this set.

## Components

**Instrument keys, not a button hierarchy.** `key` is the everyday control, `primary` is the
one committed action on a screen, `quiet` is a text control, `danger` is destructive. Two
`key`s side by side is the normal pairing. The filled-primary-beside-outlined-ghost couplet
is not in this system. Keys change state by fill and tone; they never lift, scale or bounce.

**The readout strip.** Metrics live on one continuous strip divided by ruling, never in a
grid of cards. Each channel is a three-row grid (`18px auto 18px`) so the label row, the
figure and the footnote sit on a shared baseline across every channel: a long label cannot
shove its neighbour's figure out of line. Deltas carry a caret AND a sign, so direction
survives with no colour at all.

**The Drum.** The signature artifact. A time axis with fiducial notches on real calendar
boundaries, one or more channels, and annotations written in the right margin with a leader
line back to the moment they describe. `stacked` gives each channel its own band; `mirrored`
opposes two about a centre rule, and the asymmetry about that rule IS the Citation Gap.

Two rules govern it. **Per-channel gain**: channels two orders of magnitude apart are not
forced onto one scale, and each pen's full-scale value is printed beside it in the readout,
so the magnitude is never left to the eye. **Honest clipping**: the ceiling comes from a
robust quantile so a single spike cannot flatten the rest of the window, and any sample
above the ceiling is marked and its true value stated, never silently cropped.

**The caliper.** The range control, drawn as two jaws with the measured interval between
them. Dragging across the record itself sets a custom interval.

**The audience switch.** Three channel keys on one bank (Readers / Machines / Both), each
carrying its pen's own line character. This is the control that makes the product what it
is, and it gets a position that says so.

**The tabulation sheet.** Share is drawn as ink laid down BEHIND the row's label, running
the width of the row, in the trace colour matching the audience being shown. It is not a
separate progress bar bolted beside the row.

**States are part of the component.** Every panel has a loading state (the waiting sheet),
an empty state (a blank stretch of ruled paper, saying what would fill it), and an error
state (naming the problem and the recovery). A screen that only handles the happy path is
not finished.

**Demonstration data is labelled.** Any figure that is illustrative rather than measured
carries a visible stamp. The product has no customers; nothing may imply otherwise.

## Do's and Don'ts

**Do**
- Reach for a value shift before you reach for any shadow
- Encode every series by line character or label as well as by colour
- Put ruling inside a panel, clipped to it
- Use the data face only where the content is genuinely data
- Give every empty state the sentence that says what would fill it
- State a scale as a number when the reader would otherwise have to guess it
- Say less; short lines, terse labels, no filler

**Don't**
- No glow, halo, bloom, or zero-offset coloured shadow. Ever.
- No kicker or eyebrow label above a heading. Ever.
- No grid of same-size icon-heading-text cards as a page's structure, and no nested cards
- No page-wide grid or graph-paper background
- No gradient decoration, no gradient-filled text, no blue-to-purple anything
- No hover lift, scale or bounce; no growing-underline hover
- No tinted pill chip as the default wrapper for a piece of metadata
- No icon inside a filled tile, chip or rounded square, logos included
- No emoji or unicode glyph standing in for an icon
- No em dash in prose or in a code comment; a bare `—` is permitted only as a null-value
  glyph in a table cell, and then it carries an accessible label
- Never gate content's existence on an animation; content is visible by default
- Never let colour be the only carrier of meaning
- Never invent a customer, a metric, a benchmark or a logo
