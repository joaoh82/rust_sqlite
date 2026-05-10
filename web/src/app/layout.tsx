import type { Metadata } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
import { SITE } from "@/lib/site";
import "./globals.css";

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

const DEFAULT_TITLE = "SQLRite — embedded SQL database, built in Rust";
const DEFAULT_DESCRIPTION =
  "An embedded SQL database modeled after SQLite, built from scratch in Rust. Single-file format, real B-tree, WAL, transactions, vector search, full-text search, and six language SDKs.";

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
    "Rust SQLite",
    "embedded database",
    "embedded SQL",
    "Rust database",
    "vector search",
    "HNSW",
    "BM25",
    "full-text search",
    "WAL",
    "B-tree",
    "MCP",
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
      <body>{children}</body>
    </html>
  );
}
