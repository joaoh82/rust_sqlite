// Bottom-strip caption shown over each screen. Fades in just after
// the screen's Ken Burns starts, so the eye lands on the UI first
// and the caption arrives a beat later.

import React from "react";
import { interpolate, useCurrentFrame } from "remotion";

type Props = {
  text: string;
  /** Frame offset relative to the parent Sequence, after which the
   * caption begins fading in. Fade duration is fixed at 12 frames. */
  enterAt?: number;
};

export const Caption: React.FC<Props> = ({ text, enterAt = 12 }) => {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [enterAt, enterAt + 12], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const lift = interpolate(frame, [enterAt, enterAt + 14], [16, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  return (
    <div
      style={{
        position: "absolute",
        left: 0,
        right: 0,
        bottom: 56,
        display: "flex",
        justifyContent: "center",
        pointerEvents: "none",
        opacity,
        transform: `translateY(${lift}px)`,
      }}
    >
      <div
        style={{
          background: "rgba(20, 20, 24, 0.78)",
          color: "#e8e8ee",
          padding: "14px 26px",
          borderRadius: 12,
          fontFamily:
            "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
          fontSize: 28,
          fontWeight: 500,
          letterSpacing: 0.2,
          maxWidth: "70%",
          textAlign: "center",
          backdropFilter: "blur(6px)",
          border: "1px solid rgba(125, 211, 164, 0.22)",
          boxShadow: "0 10px 40px rgba(0,0,0,0.35)",
        }}
      >
        {text}
      </div>
    </div>
  );
};
