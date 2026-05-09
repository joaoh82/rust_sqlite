import { ImageResponse } from "next/og";
import { OG_CONTENT_TYPE, OG_SIZE, OgFrame } from "@/lib/og";

export const runtime = "edge";
export const alt = "SQLRite docs — getting started";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export default function OgImage() {
  return new ImageResponse(
    (
      <OgFrame
        eyebrow="docs · getting started"
        title="From cargo install to a persistent on-disk database in ten minutes."
        subtitle="REPL, transactions, JOINs, prepared statements, vector + BM25 search, MCP, and six language SDKs."
      />
    ),
    { ...size },
  );
}
