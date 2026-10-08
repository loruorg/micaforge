---
name: Micaforge
description: Open-source, cookie-free web analytics that shows the traffic AI sends you and the AI crawlers that read your site
colors:
  page: "#fafafa"
  panel: "#ffffff"
  raised: "#f4f4f5"
  well-2: "#f6f6f7"
  rule: "#e8e8eb"
  rule-strong: "#d4d4d8"
  ink-strong: "#09090b"
  ink: "#18181b"
  ink-2: "#52525b"
  ink-3: "#71717a"
  ai-orange: "#f25a1d"
  ai-orange-ink: "#c2410c"
  ai-orange-lo: "#fff1ea"
  people-blue: "#2f6bff"
  people-blue-ink: "#1d4fd7"
  people-blue-2: "#82a6ff"
  people-blue-3: "#c9d8ff"
  danger: "#d92d20"
  danger-deep: "#b42318"
  success: "#067647"
  band-dark: "#09090b"
typography:
  display:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "clamp(2.6rem, 6.6vw, 4.75rem)"
    fontWeight: 650
    lineHeight: 1
    letterSpacing: "-0.04em"
  headline:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "clamp(1.875rem, 3.4vw, 2.75rem)"
    fontWeight: 650
    lineHeight: 1.08
    letterSpacing: "-0.035em"
  title:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "1.375rem"
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: "-0.02em"
  body:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "0.9375rem"
    fontWeight: 400
    lineHeight: 1.6
  data:
    fontFamily: "Geist Mono, ui-monospace, SFMono-Regular, Menlo, monospace"
    fontSize: "0.8125rem"
    fontWeight: 400
    fontFeature: "tnum 1, lnum 1"
  label:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "0.75rem"
    fontWeight: 500
rounded:
  sm: "6px"
  md: "10px"
  lg: "16px"
  pill: "999px"
spacing:
  "1": "0.25rem"
  "2": "0.5rem"
  "3": "0.75rem"
  "4": "1rem"
  "5": "1.25rem"
  "6": "1.5rem"
  "8": "2rem"
  "10": "2.5rem"
  "12": "3rem"
  "16": "4rem"
  "24": "6rem"
components:
  button-primary:
    backgroundColor: "{colors.ink-strong}"
    textColor: "{colors.panel}"
    rounded: "{rounded.sm}"
    padding: "0 1.25rem"
    height: "44px"
  button-primary-hover:
    backgroundColor: "#27272a"
    textColor: "{colors.panel}"
  button-ghost:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.ink-strong}"
    rounded: "{rounded.sm}"
    padding: "0 1.25rem"
    height: "44px"
  button-ghost-hover:
    backgroundColor: "{colors.raised}"
  panel:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.ink}"
    rounded: "{rounded.md}"
    padding: "1.25rem"
  feature-frame:
    backgroundColor: "{colors.panel}"
    rounded: "{rounded.lg}"
    padding: "20px"
  input:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.ink}"
    rounded: "{rounded.sm}"
    padding: "0.5rem 0.75rem"
    height: "40px"
  badge-ai:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.ai-orange-ink}"
    rounded: "4px"
    padding: "0 5px"
  segment-selected:
    backgroundColor: "{colors.ink-strong}"
    textColor: "{colors.panel}"
    rounded: "{rounded.sm}"
---

# Design System: Micaforge

## Overview

**Creative North Star: "The Morning Report"**

Micaforge is the analytics a small SaaS or developer-tool team opens with their first coffee and after every ship. The design is the category standard done carefully: a light page, white panels, near-black type and controls, and charts that are read in a second. It sits beside Plausible, PostHog and Vercel in craft, and it refuses two things: the report wall of GA4, and decoration that needs explaining. Nothing in the interface is a metaphor. A button says what it does, a chart says what it measures, a label is the word a person would use.

There is one theme. There is no dark mode and no theme switch: the product is read at a desk in daylight, and one well-tuned light palette is easier to keep right than two. The one idea the eye should learn is a colour pair. **Blue is people. Orange is AI.** It holds on the homepage, in every chart, in table share bars, in badges, in the brand mark and in the emails.

Motion is a few useful moments, not a layer: a chart draws in when it first appears, the hero figures tick the last stretch to their value, a live feed shows visits arriving, and the three homepage feature visuals build once when scrolled to. Everything is complete without motion, and nothing moves under reduced motion.

**Key Characteristics:**
- Light only: #fafafa page in the app, white page with #fafafa bands on the site, white panels
- Near-black primary buttons; orange is never a button colour
- Blue for people and orange for AI, everywhere, and never as the only cue (line style differs too)
- Geist for words, Geist Mono for code, paths and identifiers, tabular figures in tables
- Hairline edges and soft offset shadows; no glow, no gradient decoration
- Plain words: "AI crawlers", "AI answers", "Dashboard", "Realtime"

## Colors

Zinc neutrals with one accent pair that carries meaning.

### Primary
- **AI Orange** (#f25a1d): AI traffic. The orange series in every chart, the AI band in the hero chart, the dot in the brand mark, AI rows' share bars (on #fff1ea), the "AI" badge border. **AI Orange Ink** (#c2410c, 5.2:1 on white) is the only orange used for text.

### Secondary
- **People Blue** (#2f6bff): people. The blue series in every chart, default table share bars (#edf2ff), the focus ring. Lighter steps #82a6ff and #c9d8ff split people by channel. **People Blue Ink** (#1d4fd7) for blue text.

### Tertiary
- **Danger** (#d92d20) and **Danger Deep** (#b42318 for text on its own wash): spoofed crawlers, robots.txt violations, "Not credited" verdicts, destructive actions.
- **Success** (#067647): verified identity, rises, "Cited".

### Neutral
- **Page** (#fafafa): the app background and the site's bands and footer.
- **Panel** (#ffffff): panels, cards, inputs, the marketing page.
- **Raised** (#f4f4f5): hover rows, active navigation, icon tiles, segmented-control tracks.
- **Rule** (#e8e8eb) and **Rule Strong** (#d4d4d8): hairlines and control borders.
- **Ink Strong** (#09090b): headings, figures, primary buttons. **Ink** (#18181b): body. **Ink 2** (#52525b): secondary text. **Ink 3** (#71717a, 4.8:1): the dimmest text allowed.
- **Band Dark** (#09090b): the one dark band on the homepage (open source), with white and #a1a1aa text.

### Named Rules
**The Two Audiences Rule.** Blue means a person and orange means AI, on every surface. Never use orange for a generic accent, a link, a checkbox or a call to action, and never use blue for an AI series.

**The Not By Colour Alone Rule.** The people series is a smooth line and the AI series is a stepped or dashed line; tables carry a label or badge as well as a colour.

## Typography

**Display and Body Font:** Geist (variable, self-hosted, with system-ui fallback)
**Data Font:** Geist Mono (variable, self-hosted)

**Character:** One modern grotesk at several weights does all the talking; the mono face appears only for things that are literally code or data.

### Hierarchy
- **Display** (650, clamp 2.6rem to 4.75rem, line-height 1, -0.04em): the homepage h1 only.
- **Headline** (650, clamp 1.875rem to 2.75rem, 1.08, -0.035em): section headings on the site.
- **Title** (600, 1.375rem, 1.2, -0.02em): feature titles, panel titles in the app (the app uses 1.375rem for panel titles and 1.625 to 2.25rem 600 for metric figures, -0.03em).
- **Body** (400, 15px UI / 17px marketing, 1.6): measure 54 to 62ch for marketing copy.
- **Label** (500, 12px; table headers 12px ink-3; small uppercase 11px with 0.04em only for genuine column or scale labels).

### Named Rules
**The No Eyebrow Rule.** Nothing sits above a heading as a kicker. The heading carries itself.

**The Mono Means Data Rule.** Geist Mono is for snippets, paths, hostnames and identifiers. It is never a costume for "technical".

## Layout

The site uses a 1180px max wrap with clamp(16px, 4vw, 32px) side padding. Sections breathe at clamp(4rem, 9vw, 7.5rem) vertically; feature rows are two columns (5fr text / 7fr visual, flipped to 7fr / 5fr when the visual leads) that stack under 960px. The header is sticky at 64px with a translucent white background and a burger drawer under 960px.

The app is a 232px left navigation grouped by question (Overview, AI traffic, Audience, Behavior, Health, Workspace) and a sticky 56px top bar with the site, who is online (people and AI crawlers), filters and export. The navigation becomes a drawer under 1080px. Wide tables scroll inside their own container; the page never scrolls sideways at 390px.

Spacing is a 4px scale (tokens --mf-1 to --mf-32), with more space above a heading than below it.

## Elevation & Depth

Surfaces separate by a hairline and a soft, offset shadow. Panels use the extra-small shadow; homepage feature frames and the hero preview lift a little more; menus and the mobile drawer use the large shadow.

### Shadow Vocabulary
- **xs** (`0 1px 2px rgba(16,24,40,.05)`): panels, buttons, inputs.
- **md** (`0 1px 2px rgba(16,24,40,.04), 0 4px 12px rgba(16,24,40,.06)`): homepage feature frames, the sign-in card.
- **lg** (`0 2px 4px rgba(16,24,40,.04), 0 12px 32px rgba(16,24,40,.10)`): dropdown menus, the navigation drawer.
- **hero** (`0 1px 2px rgba(16,24,40,.04), 0 24px 64px -12px rgba(16,24,40,.14)`): the homepage dashboard preview only.

### Named Rules
**The No Glow Rule.** Every shadow has an offset and a blur. No zero-offset coloured halos, no glowing borders.

## Shapes

6px on controls, 10px on panels, 16px on homepage feature frames and the sign-in card, 14px on the hero preview, and full pills only for small status badges ("Demo data", verdict chips). The brand mark is a 24-unit tile with 6.5 corners: a white traffic line ending in the orange dot.

## Components

- **Buttons.** Primary is near-black with white text (44px on the site, 32 to 40px in the app); ghost is white with a strong hairline. Hover changes fill only. In the sign-in card the one action runs full width.
- **Segmented controls** (audience switch, date range). A raised track; the selected segment is white with an xs shadow (audience) or near-black (range).
- **Metric strip.** One strip divided by hairlines, label above, figure, delta below in success or danger with a sign. The selected metric gets a 2px ink underline.
- **Tables.** Tabular figures in Geist, 12px ink-3 headers, hairline rows, share drawn as a soft bar behind the row (blue by default, orange in AI tables).
- **Charts.** Grid lines #f1f1f3 / #e4e4e7, 11px ink-3 axis labels, people blue smooth, AI orange stepped; stacked areas use the blue steps with orange on top.
- **Inputs.** White, strong hairline, 40px, focus ring = blue border plus a 3px blue wash.
- **Badges.** "AI" badge: orange-ink text on white with a #fed7c3 border. "Demo data": pill with ink-3 text and a hairline. Verdicts: pill on the danger or success wash.
- **States.** Every panel has loading (soft skeleton bars), empty (a sentence naming what would fill it) and error (the problem and the recovery) states.
- **Emails.** Light card on #f4f4f5 with a dark-mode enhancement, the tile-and-dot mark drawn in table cells, near-black button, the code in a large mono panel; no images, no tracking pixel.

## Do's and Don'ts

**Do**
- Label every illustrative figure "Demo data"; never invent a customer, metric or logo
- Use the colour pair for meaning and pair it with a second cue
- Write the plain word: AI crawlers, AI answers, visitors, crawls, Dashboard
- Keep motion to a first-appearance moment, complete without it, off under reduced motion
- Give every empty state the sentence that says what would fill it

**Don't**
- No theme switch, no dark mode
- No orange buttons, links or generic accents
- No eyebrow above a heading, no gradient text, no glow
- No metaphors in the interface (no "pens", "record", "drum", "station")
- No grid of identical cards as a page's structure; the feature list is one hairline grid
- Never name a real company as violating robots.txt in demo material
