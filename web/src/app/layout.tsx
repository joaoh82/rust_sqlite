import type { Metadata } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
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

export const metadata: Metadata = {
  title: "SQLRite — embedded SQL database, built in Rust",
  description:
    "An embedded SQL database modeled after SQLite, built from scratch in Rust. Single-file format, real B-tree, WAL, transactions, vector search, full-text search, and six language SDKs.",
  metadataBase: new URL("https://sqlrite.dev"),
  openGraph: {
    title: "SQLRite — embedded SQL database, built in Rust",
    description:
      "Single-file embedded SQL engine in Rust — B-tree, WAL, transactions, HNSW vector search, BM25 full-text, six language SDKs.",
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: "SQLRite — embedded SQL database, built in Rust",
    description:
      "Single-file embedded SQL engine in Rust — B-tree, WAL, transactions, HNSW vector search, BM25 full-text, six language SDKs.",
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
