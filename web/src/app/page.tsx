import type { Metadata } from "next";
import { Architecture } from "@/components/architecture";
import { Benchmarks } from "@/components/benchmarks";
import { Blog } from "@/components/blog";
import { CTAStrip } from "@/components/cta-strip";
import { Desktop } from "@/components/desktop";
import { Features } from "@/components/features";
import { Footer } from "@/components/footer";
import { Hero } from "@/components/hero";
import { Nav } from "@/components/nav";
import { PlaygroundCard } from "@/components/playground-card";
import { Roadmap } from "@/components/roadmap";
import { SDKShowcase } from "@/components/sdk-showcase";
import { SQLRef } from "@/components/sql-ref";
import { SITE } from "@/lib/site";

// SEO targeting: primary "embedded SQL + vector database in Rust",
// secondary "SQLite alternative" / "Rust database" / "HNSW vector search" /
// "BM25 full-text" / "MCP server". See web/seo/keywords.md.
const TITLE = "SQLRite — an embedded SQL + vector database in Rust";
const DESCRIPTION =
  "SQLRite is an embedded SQL + vector database in Rust — a SQLite alternative with WAL transactions, HNSW vector search, BM25 full-text, and six language SDKs.";

export const metadata: Metadata = {
  title: { absolute: TITLE },
  description: DESCRIPTION,
  alternates: { canonical: "/" },
  // openGraph and twitter aren't deep-merged with the parent layout in Next
  // 15 — providing either at the page level fully replaces the layout's
  // version, so site-wide fields (siteName, twitter card, etc.) have to be
  // restated here.
  openGraph: {
    type: "website",
    siteName: "SQLRite",
    locale: "en_US",
    url: SITE.url,
    title: TITLE,
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: SITE.twitterHandle,
    creator: SITE.twitterHandle,
    title: TITLE,
    description: DESCRIPTION,
  },
};

// JSON-LD describing SQLRite as a SoftwareApplication. Helps search engines
// surface a rich card (name + description + repo + license) and gives LLM
// crawlers a structured handle on what this project is.
const softwareApplicationJsonLd = {
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  name: "SQLRite",
  description: DESCRIPTION,
  applicationCategory: "DeveloperApplication",
  applicationSubCategory: "DatabaseApplication",
  operatingSystem: "macOS, Windows, Linux, Web (WASM)",
  softwareVersion: SITE.version,
  url: SITE.url,
  downloadUrl: SITE.releasesLatest,
  codeRepository: SITE.repo,
  programmingLanguage: "Rust",
  license: "https://opensource.org/licenses/MIT",
  offers: {
    "@type": "Offer",
    price: "0",
    priceCurrency: "USD",
  },
  author: {
    "@type": "Person",
    name: "Joao Henrique Machado Silva",
    url: SITE.socials.github,
  },
};

export default function Home() {
  return (
    <>
      <script
        type="application/ld+json"
        // Stringified ahead of render — the script's content is static, so
        // this is not a user-controlled injection vector.
        dangerouslySetInnerHTML={{
          __html: JSON.stringify(softwareApplicationJsonLd),
        }}
      />
      <Nav />
      <Hero />
      <PlaygroundCard />
      <Features />
      <Architecture />
      <Roadmap />
      <SDKShowcase />
      <SQLRef />
      <Benchmarks />
      <Desktop />
      <Blog />
      <CTAStrip />
      <Footer />
    </>
  );
}
