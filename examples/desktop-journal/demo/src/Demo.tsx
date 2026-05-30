// Main composition: stitches seven screenshots together with Ken
// Burns pans and bottom captions. Each "screen" gets its own
// <Sequence> so timeline editing in remotion studio shows them as
// discrete blocks, and so the per-screen `Caption` component can
// reference frame 0 of its own sub-timeline (cleaner than computing
// global offsets).
//
// Layout target: 1920×1080 (standard 16:9). The screenshots are
// macOS retina captures, typically 2440×something — `objectFit:
// contain` inside Pan keeps them centered with letterboxing on the
// sides, which reads cleanly against the dark journal palette.

import React from "react";
import { AbsoluteFill, Sequence, staticFile } from "remotion";
import { Caption } from "./Caption";
import { Pan } from "./Pan";

export const DEMO_WIDTH = 1920;
export const DEMO_HEIGHT = 1080;
export const DEMO_FPS = 30;

type Screen = {
  src: string;
  caption: string;
  /** Per-screen duration in seconds. */
  seconds: number;
  /** Optional pan / zoom tweaks. */
  startScale?: number;
  endScale?: number;
  startOffset?: [number, number];
  endOffset?: [number, number];
};

const SCREENS: Screen[] = [
  {
    src: "01-empty.png",
    caption: "SQLRite Journal — local-first markdown daily notes.",
    seconds: 3.0,
  },
  {
    src: "02-new-entry.png",
    caption: "Write in markdown. ⌘S saves to a single .sqlrite file.",
    seconds: 3.2,
  },
  {
    src: "03-markdown-preview.png",
    caption: "Toggle Preview for a rendered view of your entry.",
    seconds: 3.0,
  },
  {
    src: "04-list-and-tags.png",
    caption: "Tag chips group entries — click to filter.",
    seconds: 3.0,
  },
  {
    src: "05-fts-highlight.png",
    caption: "Phase 8 BM25 full-text search with hit highlighting.",
    seconds: 3.4,
  },
  {
    src: "06-settings.png",
    caption: "Paste your Anthropic key — never sent to the webview.",
    seconds: 3.0,
  },
  {
    src: "07-ask-panel.png",
    caption: "Ask my journal. Read-only SELECT — your data, your SQL.",
    seconds: 3.4,
  },
];

const SEGMENT_TRANSITION_FRAMES = 8; // overlap for cross-fade

function screenFrames(s: Screen): number {
  return Math.round(s.seconds * DEMO_FPS);
}

export function totalFrames(): number {
  // Each screen contributes its own length minus the overlap from
  // the next (except the last). Add an intro fade-in pad.
  const sum = SCREENS.reduce((acc, s) => acc + screenFrames(s), 0);
  const overlap = SEGMENT_TRANSITION_FRAMES * (SCREENS.length - 1);
  return sum - overlap + 18; // 18-frame outro fade
}

export const Demo: React.FC = () => {
  // Walk the screens, placing each Sequence starting `frame` frames
  // in. Cross-fade is "free" because Pan paints over the previous
  // Sequence's last frames within the overlap window.
  let frame = 0;
  return (
    <AbsoluteFill style={{ background: "#1e1e22" }}>
      {SCREENS.map((s, i) => {
        const dur = screenFrames(s);
        const from = frame;
        const node = (
          <Sequence
            key={s.src}
            from={from}
            durationInFrames={dur + SEGMENT_TRANSITION_FRAMES}
          >
            <Pan
              src={staticFile(s.src)}
              durationInFrames={dur}
              startScale={s.startScale ?? 1.0}
              endScale={s.endScale ?? 1.06}
              startOffset={s.startOffset}
              endOffset={s.endOffset}
            />
            <Caption text={s.caption} />
          </Sequence>
        );
        // Advance frame cursor by dur minus the overlap so the next
        // sequence starts cross-fading before this one ends.
        frame += dur - (i < SCREENS.length - 1 ? SEGMENT_TRANSITION_FRAMES : 0);
        return node;
      })}
    </AbsoluteFill>
  );
};
