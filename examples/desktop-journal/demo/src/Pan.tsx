// Ken Burns slow-zoom + pan wrapper around a single screenshot. Each
// child renders for `durationInFrames` frames; the image gently grows
// from `startScale` to `endScale` over that interval, optionally
// drifting toward an end position.
//
// Why this exists separately from the screen components: every screen
// shares the same Ken Burns rhythm; defining it once keeps the
// per-screen code to "what image + what caption."

import React from "react";
import { Img, interpolate, useCurrentFrame } from "remotion";

type Props = {
  src: string;
  startScale?: number;
  endScale?: number;
  /** Pixel offset at frame 0 (relative to scaled image center). */
  startOffset?: [number, number];
  /** Pixel offset at the last frame. */
  endOffset?: [number, number];
  /** Duration of this segment, in frames. Used to drive the easing
   * curves. */
  durationInFrames: number;
};

export const Pan: React.FC<Props> = ({
  src,
  startScale = 1.0,
  endScale = 1.06,
  startOffset = [0, 0],
  endOffset = [0, 0],
  durationInFrames,
}) => {
  const frame = useCurrentFrame();
  const t = Math.min(frame, durationInFrames);

  const scale = interpolate(t, [0, durationInFrames], [startScale, endScale], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const x = interpolate(t, [0, durationInFrames], [startOffset[0], endOffset[0]], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const y = interpolate(t, [0, durationInFrames], [startOffset[1], endOffset[1]], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: "#1e1e22",
        overflow: "hidden",
      }}
    >
      <Img
        src={src}
        style={{
          width: "100%",
          height: "100%",
          objectFit: "contain",
          transform: `translate(${x}px, ${y}px) scale(${scale})`,
          transformOrigin: "center center",
        }}
      />
    </div>
  );
};
