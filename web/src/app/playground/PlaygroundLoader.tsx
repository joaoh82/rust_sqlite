"use client";

import dynamic from "next/dynamic";

// The playground pulls in CodeMirror and (at runtime) the WASM engine —
// neither has any business running during SSR. `ssr: false` keeps this
// route's heavy client code out of the server render and off the initial
// HTML, while the static page shell around it stays server-rendered for
// SEO. The skeleton holds layout so there's no jank before hydration.
const Playground = dynamic(
  () => import("./Playground").then((m) => m.Playground),
  {
    ssr: false,
    loading: () => (
      <div className="pg-shell" aria-busy="true">
        <div className="pg-toolbar pg-toolbar-skeleton" aria-hidden="true" />
        <p className="pg-status">Loading the SQLRite WASM engine…</p>
        <div className="pg-panes">
          <div className="pg-pane pg-pane-skeleton" />
          <div className="pg-pane pg-pane-skeleton" />
        </div>
      </div>
    ),
  },
);

export default function PlaygroundLoader() {
  return <Playground />;
}
