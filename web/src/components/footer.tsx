import Link from "next/link";
import { SITE } from "@/lib/site";
import { GithubIcon, LinkedinIcon, TwitterIcon } from "./icons";

export function Footer() {
  return (
    <footer>
      <div className="wrap">
        <div className="foot-grid">
          <div className="foot-col">
            <div className="brand" style={{ marginBottom: 14 }}>
              <span className="brand-mark">▸</span>
              <span>sqlrite</span>
            </div>
            <p className="dim" style={{ fontSize: 13, maxWidth: 340 }}>
              Simple embedded database modeled off SQLite, built in Rust. By{" "}
              <a
                href={SITE.socials.github}
                style={{ color: "var(--color-fg)" }}
              >
                @joaoh82
              </a>{" "}
              and contributors.
            </p>
            <div
              aria-label="Author social links"
              style={{
                display: "flex",
                gap: 10,
                marginTop: 18,
                alignItems: "center",
              }}
            >
              <a
                href={SITE.socials.github}
                target="_blank"
                rel="noreferrer"
                aria-label="GitHub profile"
                title="GitHub"
                className="social-link"
              >
                <GithubIcon size={16} />
              </a>
              <a
                href={SITE.socials.linkedin}
                target="_blank"
                rel="noreferrer"
                aria-label="LinkedIn profile"
                title="LinkedIn"
                className="social-link"
              >
                <LinkedinIcon size={16} />
              </a>
              <a
                href={SITE.socials.twitter}
                target="_blank"
                rel="noreferrer"
                aria-label="Twitter / X profile"
                title="Twitter / X"
                className="social-link"
              >
                <TwitterIcon size={14} />
              </a>
            </div>
          </div>
          <div className="foot-col">
            <h3>Project</h3>
            <a href={SITE.repo}>GitHub</a>
            <Link href="/docs">Documentation</Link>
            <a href={SITE.docsRs}>Rust API docs</a>
            <a href={SITE.releases}>Releases</a>
          </div>
          <div className="foot-col">
            <h3>Community</h3>
            <a href={SITE.discord}>Discord</a>
            <a href={`${SITE.repo}/discussions`}>Discussions</a>
            <a href={`${SITE.repo}/issues`}>Issues</a>
            <a href="https://github.com/sponsors/joaoh82">Sponsor</a>
          </div>
          <div className="foot-col">
            <h3>Reading</h3>
            <a href={SITE.medium}>Series on Medium</a>
            <a href="https://cstack.github.io/db_tutorial/">
              DB tutorial (inspiration)
            </a>
            <a href={SITE.archDoc}>Architecture</a>
            <a href={SITE.roadmapDoc}>Roadmap</a>
          </div>
        </div>
        <div className="foot-bottom">
          <span>© 2026 sqlrite contributors · MIT licensed</span>
          <span>v{SITE.version}</span>
        </div>
      </div>
    </footer>
  );
}
