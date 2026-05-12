import type { Metadata, Viewport } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
import { PostHogPageView, PostHogProvider } from "@posthog/next";
import { SITE } from "@/lib/site";
import "./globals.css";

// PostHog is gated on NEXT_PUBLIC_POSTHOG_KEY so preview deploys that haven't
// configured the secret still build/serve. The env is inlined at build time, so
// redeploy after setting the secret in Vercel.
const POSTHOG_ENABLED = Boolean(process.env.NEXT_PUBLIC_POSTHOG_KEY);

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  // The site is dark-only; surface that to mobile browsers so address bars
  // and status bars adopt the page background instead of flashing white.
  themeColor: "#0b0c0e",
  colorScheme: "dark",
};

const inter = Inter({
  subsets: ["latin"],
  weight: ["400", "500", "600"],
  variable: "--font-inter",
  display: "swap",
});

const jetbrainsMono = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["400", "500"],
  variable: "--font-jetbrains-mono",
  display: "swap",
});

// Header copy is the primary on-page SEO surface — see web/seo/keywords.md
// for the keyword strategy and per-page rationale.
const DEFAULT_TITLE =
  "SQLRite — an embedded SQL + vector database in Rust";
const DEFAULT_DESCRIPTION =
  "SQLRite is an embedded SQL + vector database in Rust. SQLite-style single-file format, WAL transactions, HNSW vector search, BM25 full-text, and six language SDKs.";

export const metadata: Metadata = {
  metadataBase: new URL(SITE.url),
  title: {
    default: DEFAULT_TITLE,
    template: "%s · SQLRite",
  },
  description: DEFAULT_DESCRIPTION,
  applicationName: "SQLRite",
  authors: [{ name: "Joao Henrique Machado Silva", url: SITE.socials.github }],
  keywords: [
    "SQLRite",
    "embedded database in Rust",
    "embedded SQL + vector database",
    "SQLite alternative",
    "SQLite-compatible Rust crate",
    "Rust database",
    "embedded vector search Rust",
    "HNSW",
    "BM25 full-text search",
    "WAL",
    "single-file database",
    "MCP server for SQLite",
  ],
  openGraph: {
    type: "website",
    siteName: "SQLRite",
    locale: "en_US",
    url: SITE.url,
    title: DEFAULT_TITLE,
    description: DEFAULT_DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: SITE.twitterHandle,
    creator: SITE.twitterHandle,
    title: DEFAULT_TITLE,
    description: DEFAULT_DESCRIPTION,
  },
  robots: {
    index: true,
    follow: true,
    googleBot: {
      index: true,
      follow: true,
      "max-image-preview": "large",
      "max-snippet": -1,
      "max-video-preview": -1,
    },
  },
  alternates: {
    canonical: "/",
  },
  // Search-engine ownership tokens. Fill these in once Google Search Console
  // and Bing Webmaster Tools verification is complete (the values are short
  // opaque strings issued by each tool).
  verification: {
    google: "cp64_cUV17FdoMHKQuAKddZ7RVMcgLpELFIfrxDpCOo",
    other: { "msvalidate.01": "CCCE26885E00B1C873F188104E8B283D" },
  },
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html
      lang="en"
      className={`${inter.variable} ${jetbrainsMono.variable}`}
      suppressHydrationWarning
    >
      <body>
        {POSTHOG_ENABLED ? (
          <PostHogProvider clientOptions={{ api_host: "/ingest" }}>
            <PostHogPageView />
            {children}
          </PostHogProvider>
        ) : (
          children
        )}
      </body>
    </html>
  );
}
