import { Architecture } from "@/components/architecture";
import { Benchmarks } from "@/components/benchmarks";
import { Blog } from "@/components/blog";
import { CTAStrip } from "@/components/cta-strip";
import { Desktop } from "@/components/desktop";
import { Features } from "@/components/features";
import { Footer } from "@/components/footer";
import { Hero } from "@/components/hero";
import { Nav } from "@/components/nav";
import { Roadmap } from "@/components/roadmap";
import { SDKShowcase } from "@/components/sdk-showcase";
import { SQLRef } from "@/components/sql-ref";

export default function Home() {
  return (
    <>
      <Nav />
      <Hero />
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
