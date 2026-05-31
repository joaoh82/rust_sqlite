import { ImageResponse } from "next/og";
import { OG_CONTENT_TYPE, OG_SIZE, OgFrame } from "@/lib/og";

export const runtime = "edge";
export const alt = "SQLRite SQL playground — run the engine in your browser";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export default function TwitterImage() {
  return new ImageResponse(
    (
      <OgFrame
        eyebrow="playground · wasm"
        title="Run SQLRite in your browser. No install, no server."
        subtitle="The full embedded SQL + vector engine, compiled to WebAssembly. Sample datasets, HNSW vector search, CSV export."
      />
    ),
    { ...size },
  );
}
