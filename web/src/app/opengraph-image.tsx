import { ImageResponse } from "next/og";
import { OG_CONTENT_TYPE, OG_SIZE, OgFrame } from "@/lib/og";

export const runtime = "edge";
export const alt = "SQLRite — embedded SQL database, built in Rust";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export default function OgImage() {
  return new ImageResponse(
    (
      <OgFrame
        eyebrow="embedded · open source"
        title="An embedded SQL database, built from scratch in Rust."
        subtitle="Single-file engine. B-tree, WAL, transactions, JOINs, vector + full-text search, six SDKs."
      />
    ),
    { ...size },
  );
}
