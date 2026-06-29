# The Reader's Field Guide

This file exercises the **Markdown** path of the reader. It mixes headings,
emphasis, inline `code`, thematic breaks, and enough plain prose to spill across
more than one page so the pagination and page-turning can be tested too.

## Inline styles

A paragraph can contain **bold text**, *italic text*, and `inline code` mixed
freely with ordinary words. On this panel only **bold** has a distinct face;
*italic* and `code` currently fall back to the regular font, but the styles are
preserved in the document model so a future font change can render them
properly without touching the parser.

Bold can also run across **several words in a row**, and it can sit right next
to *an italic run* so the wrapper has to keep adjacent runs of different styles
on the same line and advance the cursor correctly between them.

---

## Headings and breaks

The line above is a thematic break, drawn as a thin horizontal rule. Headings
come in levels one through six, though only the first couple of levels show up
in a typical book. Each heading gets a little breathing room above and below it
so the page does not feel cramped.

### A third-level heading

Below a heading, the body text resumes as normal. The reader does not yet do
anything clever with heading levels beyond rendering them bold, but the level
is carried through the model, so a table of contents or an outline view could
be built on top of it later.

## Why so much text?

A reader is only interesting once there is more than a screenful to read. The
remaining paragraphs exist purely to push the content past the bottom margin so
that a second and probably third page are created.

Reading on electronic paper is a deliberately slow medium. There is no scroll,
no infinite feed, no notification sliding in from the top. There is a page, and
then the next page, and the small satisfying act of asking for it. The screen
holds its last image with no power at all, which is why a book can sit half
finished on a shelf for a week and open exactly where it was left.

That persistence is the whole point of saving your place to the card. Close
this file, open something else, come back tomorrow, and the reader should drop
you back on the page you were reading. If it does, the progress-saving path is
working; if it does not, that is the first place to look.

Now tap the right edge of the screen and keep going until you reach the end.
