import { SITE } from "./site";

export const OG_SIZE = { width: 1200, height: 630 } as const;
export const OG_CONTENT_TYPE = "image/png";

type OgFrameProps = {
  eyebrow: string;
  title: string;
  subtitle?: string;
};

// Shared visual for every per-route OG image. ImageResponse renders this JSX
// to a 1200×630 PNG via @vercel/og's flat layout engine, so styles must stay
// inline and any flex container with multiple children needs an explicit
// `display: "flex"` (the renderer doesn't infer it the way browsers do).
export function OgFrame({ eyebrow, title, subtitle }: OgFrameProps) {
  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        display: "flex",
        flexDirection: "column",
        justifyContent: "space-between",
        padding: "72px",
        background:
          "linear-gradient(135deg, #07090c 0%, #0d1117 55%, #131a24 100%)",
        color: "#e6edf3",
        fontFamily: "Inter, system-ui, sans-serif",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            width: 56,
            height: 56,
            borderRadius: 12,
            background: "#f97316",
          }}
        >
          {/* Inline SVG so the OG renderer doesn't need to fetch a dynamic
              font subset for the brand mark (Satori's glyph fetcher 400s on
              less-common chars and uses a tofu fallback otherwise). */}
          <svg
            width={28}
            height={28}
            viewBox="0 0 24 24"
            xmlns="http://www.w3.org/2000/svg"
          >
            <path d="M7 5l12 7-12 7z" fill="#0d1117" />
          </svg>
        </div>
        <div
          style={{
            display: "flex",
            flexDirection: "column",
          }}
        >
          <span
            style={{
              fontSize: 28,
              fontWeight: 600,
              letterSpacing: -0.5,
            }}
          >
            sqlrite
          </span>
          <span
            style={{
              fontSize: 14,
              color: "#8b949e",
              fontFamily: "JetBrains Mono, monospace",
            }}
          >
            v{SITE.version} · embedded SQL in Rust
          </span>
        </div>
      </div>

      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 20,
        }}
      >
        <span
          style={{
            fontSize: 16,
            fontWeight: 500,
            letterSpacing: 1.5,
            textTransform: "uppercase",
            color: "#f97316",
            fontFamily: "JetBrains Mono, monospace",
          }}
        >
          {eyebrow}
        </span>
        <h1
          style={{
            fontSize: 68,
            fontWeight: 600,
            lineHeight: 1.05,
            letterSpacing: -1.5,
            margin: 0,
            maxWidth: 1000,
          }}
        >
          {title}
        </h1>
        {subtitle ? (
          <p
            style={{
              fontSize: 28,
              lineHeight: 1.35,
              color: "#8b949e",
              margin: 0,
              maxWidth: 1000,
            }}
          >
            {subtitle}
          </p>
        ) : null}
      </div>

      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          fontSize: 18,
          color: "#8b949e",
          fontFamily: "JetBrains Mono, monospace",
        }}
      >
        <span>sqlritedb.com</span>
        <span>github.com/joaoh82/rust_sqlite</span>
      </div>
    </div>
  );
}
