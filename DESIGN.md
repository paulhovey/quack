---
name: quack
description: The marketing site drawn as a field guide plate. The TUI's own ASCII duck, labelled with field marks, on plates over a sky-wash pond, in light and dark.
colors:
  paper: "#ffffff"
  wash: "#f1fafd"
  ink: "#1d2a3a"
  ink-2: "#46596d"
  rule: "#d5dee6"
  duck: "#ffd60a"
  duck-soft: "#fff4b8"
  duck-line: "#e8cf55"
  duck-ink: "#c99d00"
  duck-glyph: "#e8b800"
  beak: "#ff9f43"
  beak-ink: "#b25300"
  sky: "#5ec8e5"
  sky-soft: "#d6f2fa"
  sky-ink: "#086a86"
  leaf: "#8fd694"
  leaf-ink: "#22743a"
  pink: "#ff8fab"
  pink-ink: "#b0244c"
  pink-soft: "#fff2f5"
  on-tint: "#1d2a3a"
  night: "#1d2a3a"
  night-2: "#c9d4df"
  term: "#16191f"
  term-text: "#d8dee9"
  term-dim: "#8a94a6"
typography:
  display:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "clamp(4.5rem, 13vw, 9.5rem)"
    fontWeight: 800
    lineHeight: 0.86
    letterSpacing: "-0.045em"
  headline-lead:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "clamp(1.7rem, 3.4vw, 2.6rem)"
    fontWeight: 700
    lineHeight: 1.08
    letterSpacing: "-0.02em"
  headline:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "2.25rem / 3rem (sm) / 3.75rem (lg)"
    fontWeight: 800
    lineHeight: 1.02
    letterSpacing: "-0.03em"
  title:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: 800
    lineHeight: 1.25
  title-mark:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "1.25rem"
    fontWeight: 700
    lineHeight: 1.25
  body-lead:
    fontFamily: "Atkinson Hyperlegible Next Variable, system-ui, sans-serif"
    fontSize: "1.25rem"
    fontWeight: 400
    lineHeight: 1.625
  body:
    fontFamily: "Atkinson Hyperlegible Next Variable, system-ui, sans-serif"
    fontSize: "1.125rem"
    fontWeight: 400
    lineHeight: 1.625
  caption:
    fontFamily: "Atkinson Hyperlegible Next Variable, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.43
  label-tab:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 700
    lineHeight: 1
  mono:
    fontFamily: "JetBrains Mono Variable, ui-monospace, Menlo, Consolas, monospace"
    fontSize: "14px"
    fontWeight: 600
    lineHeight: 1.625
    fontFeature: "liga 0, calt 0"
rounded:
  sm: "4px"
  md: "6px"
  lg: "8px"
  xl: "12px"
  2xl: "16px"
  3xl: "24px"
  full: "9999px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "16px"
  lg: "24px"
  xl: "32px"
  2xl: "48px"
  3xl: "56px"
  4xl: "80px"
  container: "1280px"
  water: "10px"
components:
  plate:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.3xl}"
    padding: "80px 48px"
  plate-notch:
    backgroundColor: "{colors.beak}"
    rounded: "{rounded.md}"
    width: "56px"
    height: "8px"
  install-box:
    backgroundColor: "{colors.duck-soft}"
    textColor: "{colors.ink}"
    typography: "{typography.mono}"
    rounded: "{rounded.xl}"
    padding: "12px 16px"
  button-copy:
    backgroundColor: "{colors.duck}"
    textColor: "{colors.on-tint}"
    padding: "0 16px"
  pill-tab:
    textColor: "{colors.ink-2}"
    rounded: "{rounded.full}"
    padding: "4px 12px"
  pill-tab-selected:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.paper}"
    rounded: "{rounded.full}"
    padding: "4px 12px"
  plucked-switch:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.full}"
    width: "36px"
    height: "20px"
  plucked-switch-on:
    backgroundColor: "{colors.beak}"
  thumb-tab:
    textColor: "{colors.on-tint}"
    typography: "{typography.label-tab}"
    rounded: "{rounded.xl}"
    padding: "12px 12px 12px 16px"
    width: "160px"
  theme-toggle:
    textColor: "{colors.ink}"
    rounded: "{rounded.full}"
    size: "40px"
  chip-failure-selected:
    backgroundColor: "{colors.pink-ink}"
    textColor: "{colors.paper}"
    rounded: "{rounded.full}"
    padding: "6px 12px"
  chip-handled-selected:
    backgroundColor: "{colors.leaf-ink}"
    textColor: "{colors.paper}"
    rounded: "{rounded.full}"
    padding: "6px 12px"
  name-chip:
    backgroundColor: "{colors.wash}"
    textColor: "{colors.ink}"
    typography: "{typography.mono}"
    rounded: "{rounded.md}"
    padding: "4px 10px"
  terminal-panel:
    backgroundColor: "{colors.term}"
    textColor: "{colors.term-text}"
    typography: "{typography.mono}"
    rounded: "{rounded.xl}"
  footer:
    backgroundColor: "{colors.night}"
    textColor: "{colors.night-2}"
    padding: "48px 32px"
---

# Design System: quack

## Overview

**Creative North Star: "The Field Guide"**

The site is a plate from a field guide to one bird, the terminal duck. The specimen is the ASCII duck that quack's TUI draws on its own welcome screen, set large in mono, and every capability is a labelled field mark with an orange leader line running to a part of the duck. Each section after the hero is another plate: a hairline-framed page with a coloured notch at its top corner and a matching coloured thumb tab down the right edge of the viewport that doubles as the navigation.

The palette is the TUI's own inks on white paper. Duck yellow, orange and sky do nearly all the work, leaf green and pink join as tab colours and as the grinder's pass and fail inks, and navy ink carries the text. The hero sits on paper and ends in a single striped water line; below it the plates float on a pale sky wash, the pond. In light mode, dark surfaces exist only where the real product appears: terminal panels and config files drawn the way the TUI draws them, and the footer. Dark mode turns the paper and the pond to dusk; the TUI's inks stay as they are.

Density is generous and editorial: big extrabold grotesque headings, a highly legible humanist body face, and mono reserved for things you would type or the terminal would print. Motion is rationed. The leader lines draw once on load; everything else that moves is the product moving (the TUI duck's animation and the grinder's gears).

**Key Characteristics:**
- Plates with hairline frames and a tab-colour notch; no numbers or captions at the foot.
- The TUI's three inks (duck yellow, orange, sky) plus leaf and pink, each light ink paired with a darker `-ink` for text.
- One water line, under the hero; the pond wash below it.
- Thumb-index tabs with level labels on the right edge as navigation (xl and up).
- Light and dark, following the OS by default, with a toggle in the hero header.
- Mono only for the duck, commands, config, terminal output and dates.
- One authored motion moment: the leader lines drawing in.

## Colors

The TUI's own inks on white paper, with navy ink for text and a dark terminal for the product itself.

### Primary
- **Duck Yellow** (`duck`): the duck's colour. Fills the install Copy button, the highlight behind "No leaks.", text selection, the first and sixth thumb tabs, and the duck glyph in dark terminal panels. Its pale partner **Duck Cream** (`duck-soft`) is the install command's field and the eval table's header band; **Duck Line** (`duck-line`) borders quack's profile chips. **Duck Gold** (`duck-ink`) is the `_(o)` of the wordmark on paper, and **Duck Glyph** (`duck-glyph`) the body of the large welcome duck.
- **Beak Orange** (`beak`): leader lines, their anchor dots and end dots, the beak glyph in the duck, mobile field-mark dashes and the grinder and install tabs. **Burnt Beak** (`beak-ink`) is its text-safe form: the `$` prompt in the install box, the wordmark's `>`, the disclosure chevron.
- **Sky** (`sky`): water. The striped water line, the duck's waves, the scrollbar thumb, tab and accent colour inside terminal panels. **Sky Mist** (`sky-soft`) fills the water line's alternate stripes and selected session tabs. **Deep Sky** (`sky-ink`) is the link colour, the hover colour for header links and the focus ring.

### Secondary
- **Leaf** (`leaf`) and **Deep Leaf** (`leaf-ink`): the field-marks tab; in the grinder, quack's side ("a profile per model family", the "handled" chips).
- **Pink** (`pink`) and **Deep Pink** (`pink-ink`): the measured tab; in the grinder, the generic harness's side (its jittering gears, the failure chips). **Pink Blush** (`pink-soft`) fills the generic harness's dashed machine.

### Neutral
- **Paper** (`paper`): plate and hero background.
- **Pond Wash** (`wash`): the ground the plates sit on below the water line; hover fill for unselected pill tabs; model-name chips.
- **Field Ink** (`ink`): all primary text, 2px instrument borders, the selected pill tab.
- **Tint Ink** (`on-tint`): text on a light-ink fill (thumb tabs, the Copy button, the "No leaks." highlight, the skip link, selection). Field Ink's light value, fixed in both modes.
- **Night** (`night`) and **Night Text** (`night-2`): the footer ground and its text, with white links.
- **Slate Ink** (`ink-2`): secondary text, plate paragraphs, captions, field-mark notes.
- **Hairline** (`rule`): plate frames, table row dividers, inline code borders.
- **Terminal** (`term`), **Terminal Text** (`term-text`), **Terminal Dim** (`term-dim`): the TUI's dark theme, used for every rendering of the product (session panel, config files, before/after code).

### Dark mode
`mode-watcher` puts `dark` on `<html>`: from the OS preference by default, or from the sun/moon toggle in the hero header, which it remembers. Tailwind's `dark:` variant follows that class, and `:root.dark` in `layout.css` swaps the tokens, so components never need their own dark styles. The bright inks (duck, beak, sky, leaf, pink), the terminal and `on-tint` keep their values; everything else changes:

| Token | Light | Dark |
|---|---|---|
| paper | #ffffff | #17212d |
| wash | #f1fafd | #0e161f |
| ink | #1d2a3a | #e8eef4 |
| ink-2 | #46596d | #a6b4c2 |
| rule | #d5dee6 | #2f3e4e |
| duck-soft | #fff4b8 | #3a330c |
| duck-line | #e8cf55 | #6e5d12 |
| duck-ink, duck-glyph | #c99d00, #e8b800 | #ffd60a |
| beak-ink | #b25300 | #ffb070 |
| sky-soft | #d6f2fa | #123647 |
| sky-ink | #086a86 | #7fd3ec |
| leaf-ink | #22743a | #9fe0a4 |
| pink-ink | #b0244c | #ff9db5 |
| pink-soft | #fff2f5 | #33161f |
| night | #1d2a3a | #0a1017 |

Pairings that invert (Field Ink fill with Paper text on the selected pill tab; `-ink` fills with Paper text on selected chips) stay legible in both modes because both sides swap. The `theme-color` meta follows the mode (#ffffff, #17212d).

### Named Rules
**The Ink-on-Tint Rule.** The light inks (duck, beak, sky, leaf, pink) are fills, strokes and terminal colours, never text on paper. Coloured text on a light ground uses the matching `-ink` (beak-ink, sky-ink, leaf-ink, pink-ink); text on a light-ink fill is Tint Ink (`on-tint`), never Field Ink, which turns light in dark mode.

**The Yellow Belongs to the Duck Rule.** Yellow appears on the duck, on the install command that gets you the duck, behind the tagline's one highlighted phrase, and in selection. It is never decorative fill for an ordinary card or section.

**The Pass-and-Fail Pair Rule.** Leaf means quack's handling and pink means the generic harness's failure. Keep that pairing wherever the two are compared.

## Typography

**Display Font:** Bricolage Grotesque Variable (with system-ui)
**Body Font:** Atkinson Hyperlegible Next Variable (with system-ui)
**Label/Mono Font:** JetBrains Mono Variable (with ui-monospace, Menlo, Consolas), ligatures off

**Character:** A heavy, slightly quirky grotesque for names and headings against a body face built for legibility; the mono is the product's own voice, used only where the product speaks.

### Hierarchy
- **Display** (800, clamp(4.5rem, 13vw, 9.5rem), 0.86, -0.045em): the "quack" wordmark in the hero, once per page.
- **Headline Lead** (700, clamp(1.7rem, 3.4vw, 2.6rem), 1.08): the hero tagline, one line per phrase.
- **Headline** (800, 2.25rem to 3.75rem, 1.02, -0.03em): plate titles, balanced, max 56rem wide.
- **Title** (800, 1.5rem; 1.875rem for the grinder's detail and the evals subhead): card and group headings inside a plate.
- **Title Mark** (700, 1.25rem): field-mark terms and the "Where to find it" label.
- **Body Lead** (400, 1.25rem, 1.625): the paragraph under a plate title, 65ch max.
- **Body** (400, 1.125rem, 1.625): running text, 46 to 70ch.
- **Caption** (400, 0.875rem): notes under the install box and the eval table, the chart credit.
- **Label Tab** (700, 16px, 1): thumb tabs, set level.
- **Mono** (600, 14 to 15px): commands, config, model and profile names, eval dates (tabular numbers), terminal panels.

### Named Rules
**The Mono Is Evidence Rule.** Mono is for the duck, things you type, things the terminal prints and dated results. Headings, labels and prose never use it.

**The Bare Title Rule.** A plate opens with its title and intro paragraph and ends with its content. Nothing sits above the title (no eyebrow, kicker or small-caps label) and nothing labels the plate at its foot (no "Plate N" or caption). Anything a reader must know, such as "the model names are examples", goes in the intro.

## Layout

The page is a stack of plates. The hero is full-width paper on a two-column grid (5fr text, 7fr specimen) inside a 1280px container, and ends in the 10px water line. Below it, `main` is Pond Wash, and each plate is a section with 24px (40px from md) vertical breathing room holding a 1280px framed page. Inside a plate, padding is 16px, 32px from sm and 48px from lg horizontally, and 56px (80px from md) top and bottom; the title block takes 40px (56px from md) below it.

Content grids inside plates are two-column at lg, usually 3fr to 2fr or 1fr to 2.2fr, with 24 to 48px gaps; the field-marks plate uses CSS columns (1, 2 at md, 3 at xl). From xl the plates, the hero and the footer keep 160px clear on the right for the fixed thumb tabs. Below xl the tabs disappear. Below lg the hero stacks with a small duck beside the wordmark, and field marks become a list with short orange dashes instead of leader lines.

Breakpoints are Tailwind's (sm 640, md 768, lg 1024, xl 1280), plus 420px for terminal text size.

### Named Rules
**The Pond Rule.** There is one water line, at the foot of the hero. Everything after it floats on the wash as white plates. Don't repeat the water line between plates.

## Elevation & Depth

Plates are flat: depth comes from white paper on the pale wash and a 1px hairline frame. Instruments, the things you use or read evidence from, are drawn in 2px Field Ink borders. Three of them also lift with a soft, diffuse navy shadow, larger the more product they show: the install box, quack's side of the grinder, and the terminal session panel. Thumb tabs carry a small edge shadow so they read as tabs coming off the page.

### Shadow Vocabulary
- **Install lift** (`box-shadow: 0 6px 18px -8px rgba(29,42,58,0.35)`): the install command box.
- **Card lift** (`box-shadow: 0 10px 30px -12px rgba(29,42,58,0.3)`): quack's side of the grinder comparison.
- **Terminal lift** (`box-shadow: 0 18px 40px -18px rgba(29,42,58,0.55)`): the session terminal panel.
- **Tab edge** (`box-shadow: -2px 2px 8px -3px rgba(10,17,25,0.35)`): thumb tabs.

In dark mode the navy lifts all but vanish against the dusk paper; the 2px borders, which turn light, carry the instruments instead.

### Named Rules
**The Hairline Plate, Inked Instrument Rule.** Plates get a 1px Hairline frame and no shadow; tools and evidence inside them get a 2px Field Ink border. Shadows are soft and spread from below, never hard offsets.

## Shapes

Corners are soft and graded by scale: plates 24px, cards and the grinder detail 16px, the install box, terminal panels and session tabs 12px, thumb tabs 12px, code blocks 8px, name chips and the plate notch 6px, inline code and the focus ring 4px. Pills (install method tabs, failure and handled chips) are fully round, and so are the field-mark anchor dots and the group-colour bars in the field-marks plate. The plate notch is a 56 by 8px tab of the plate's colour hanging from the top edge, 40px in from the right, rounded only at the bottom. The grinder's generic machine is the one dashed border, in pink, because it is the broken one.

## Components

### Plate
The page unit. Paper, 24px corners, 1px Hairline frame, a tab-colour notch top right, then its children; no foot caption. Tab colours follow the plate order: quack duck, grinder beak, in the wild sky, field marks leaf, measured pink, teams duck, install beak.

### Specimen and field marks (signature)
The welcome duck from quack-tui, frame for frame (16 steps of 128ms; reduced motion holds step 0), in bold mono with an orange beak and sky waves. Its parts carry `data-part` anchors. On lg and up, four field marks sit two above and two below it, and an SVG overlay measures each mark's orange dot to its duck part and draws a 2px orange line ending in a 3.5px dot, short of the glyph by its height times the mark's `clear` factor (at least 16px). The beak and water use 0.62; the eye (1.7) and body (1.5) sit among other glyphs, the cap and the bill, so their lines stop further out. Lines draw once on load (0.9s, cubic-bezier(0.16, 1, 0.3, 1), staggered 120ms). Each mark is a Title Mark term plus a Slate Ink note, linking to its account in the grinder.

### Install box
Pill tabs above (Linux & macOS, Windows, From source; selected: Field Ink with paper text; others Slate Ink, wash on hover) and, to their right, the **Plucked: no hosted services** switch (36 by 20px, 2px ink track, beak fill when on). Then a 2px Field Ink box with 12px corners: a Duck Cream mono command field with a burnt-orange `$` (`>` for PowerShell), and a Duck Yellow Copy button behind a 2px ink divider that turns to "Copied" for 2s and nudges down 1px when pressed. Plucked switches the command to `sh -s -- --plucked`, `-Plucked` or `--from-source --plucked`. URLs break after each `/`; other words never break.

On load the box detects the visitor's platform (client hints, then the user agent): Windows selects the PowerShell tab, and under the box a Caption line offers the release archive (full or plucked, following the switch) for that OS with a download icon, plus "other platforms". When the CPU is known (client hints; Linux user agents) one archive is offered; otherwise both (macOS lists Apple silicon first). Phones and unknown platforms get no archive line.

### Thumb tabs (navigation)
Fixed to the right edge, vertically centred, xl and up only. Each plate's tab is its colour, 160px wide, with a level 16px Label Tab in Tint Ink and 12px corners on the left. Inactive tabs sit 16px tucked off the edge; the tab for the plate in view slides fully out and shows an 8px Tint Ink dot before its label, and hover or focus brings a tab most of the way out (300ms ease-out).

### Theme toggle
A 40px round button with a Hairline border in the hero header, beside the GitHub link: a moon in light mode, a sun in dark. The icons switch on the `dark` class rather than on script state, so the right one shows before hydration. It is a toggle (`aria-pressed`) labelled "Dark mode".

### Chips
Mono name chips (6px corners, 1px border: wash and Hairline for models, Duck Cream and a darker yellow for quack's profiles). Round failure chips in the grinder carry a warning icon, 2px pink-ink borders and fill solid pink-ink when selected; quack's matching chips do the same in leaf-ink with a check.

### Terminal panel
The TUI drawn in HTML: Terminal ground, Terminal Text, a 2px ink border, 12px corners, the TUI's colour classes (sky for reads and accents, duck for edits and warnings, dim for metadata), a blinking caret and a status line with sky or duck mode badges. Config files use the same panel with a dim file-name bar.

### Grinder comparison
Two 16px-cornered cards side by side: the generic grinder (pink, jittering fast gears, dashed machine) and quack (2px ink border with card lift, sky machine with slow gears), then a detail card with before and after terminal blocks.

### Links and focus
Links are Deep Sky, semibold, underline offset 0.2em at 2px thickness. Focus is a 3px Deep Sky outline, 3px offset, 4px corners, everywhere.

## Do's and Don'ts

### Do:
- **Do** put every new section on a Plate: paper, 24px corners, Hairline frame, a notch in its tab colour and a thumb tab.
- **Do** use tokens for every colour so dark mode follows; a hex in a component won't change with the mode.
- **Do** draw the duck from the TUI's own glyphs (`_(o)>` or the welcome duck), in mono, with an orange beak and sky water.
- **Do** use the `-ink` variant whenever a hue colours text on a light ground.
- **Do** show the product in Terminal panels, drawn the way quack draws it.
- **Do** keep icons as 16px line SVGs (Lucide) outside terminal and duck renderings; inside them, the TUI's own glyphs (✓ ✗ ▶ ╭ ╮) are native.
- **Do** honour reduced motion: the duck holds still, the gears stop, the leader lines appear without drawing.

### Don't:
- **Don't** label a plate above its title (eyebrow, kicker) or at its foot ("Plate N", captions).
- **Don't** add a second water line or a second authored motion moment.
- **Don't** use any mascot or logo other than the ASCII duck the TUI draws.
- **Don't** set light inks (duck, beak, sky, leaf, pink) as text on paper or wash, or Field Ink on a light-ink fill (use `on-tint`).
- **Don't** use hard offset shadows; lifts are soft and diffuse, in navy.
- **Don't** use mono for headings, labels or prose.
