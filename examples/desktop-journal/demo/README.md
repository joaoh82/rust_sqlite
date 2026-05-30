# demo/ — Remotion composition for the SQLRite Journal demo video

Renders `examples/desktop-journal/docs/demo.mp4` (1080p H.264) and
`examples/desktop-journal/docs/demo.gif` (720p, 18 seconds — the
first chunk of the composition, fits comfortably in a README).

## How it's built

Each of the seven app states is captured once as a PNG screenshot
under `../docs/screenshots/`. The Remotion composition (`src/Demo.tsx`)
walks the screen list, gives each one a Ken Burns slow zoom via
[`src/Pan.tsx`](src/Pan.tsx), and overlays a fade-in caption strip
via [`src/Caption.tsx`](src/Caption.tsx). Consecutive screens
overlap for 8 frames so the cross-fade is automatic.

Why composition over real screen recording? Two reasons. (1) The
output is deterministic — re-render after any UI change by replacing
the affected screenshot. (2) Captions and animation timings are in
source, not in a video editor — so the file diff is reviewable.

## Render

```bash
cd examples/desktop-journal/demo
npm install
npm run render            # produces ../docs/demo.mp4 and ../docs/demo.gif
# or:
npm run studio            # opens Remotion Studio for live preview
npm run render:mp4        # MP4 only
npm run render:gif        # GIF only — width 960, first 18s
```

The screenshots live at `../docs/screenshots/` and are loaded via
Remotion's `staticFile()` from `public/`. The npm scripts symlink
them in pre-render — or you can copy them in manually:

```bash
mkdir -p public
cp ../docs/screenshots/*.png public/
```

## Replacing a screenshot

1. Re-capture the screen (Cmd+Shift+4 around the journal window).
2. Save it over the matching file in `../docs/screenshots/` keeping
   the same name (`01-empty.png` etc.) — the composition reads names
   from `src/Demo.tsx` (`SCREENS` array).
3. Re-run `npm run render`.

## Adjusting the composition

- **Reorder / add screens** — edit the `SCREENS` array in `src/Demo.tsx`.
- **Tweak caption text** — same array, `caption` field.
- **Change the Ken Burns pan** — pass `startScale` / `endScale` /
  `startOffset` / `endOffset` per screen in `SCREENS`.
- **Change duration** — `seconds` field per screen; total frames is
  computed in `totalFrames()`.
