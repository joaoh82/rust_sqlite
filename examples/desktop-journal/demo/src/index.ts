// Remotion entry point — registers the composition tree with the
// Remotion runtime. Both `remotion studio` (live preview) and
// `remotion render` (offline render) call this.
import { registerRoot } from "remotion";
import { Root } from "./Root";

registerRoot(Root);
