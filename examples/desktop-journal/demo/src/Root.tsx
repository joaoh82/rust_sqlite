// The composition registry. Just one composition (`Demo`) for now;
// each Composition entry tells Remotion the dimensions, frame rate,
// and total length so the renderer knows what to produce.

import { Composition } from "remotion";
import { Demo, DEMO_FPS, DEMO_HEIGHT, DEMO_WIDTH, totalFrames } from "./Demo";

export const Root: React.FC = () => {
  return (
    <>
      <Composition
        id="Demo"
        component={Demo}
        durationInFrames={totalFrames()}
        fps={DEMO_FPS}
        width={DEMO_WIDTH}
        height={DEMO_HEIGHT}
      />
    </>
  );
};
