import { SITE } from "@/lib/site";

export function CTAStrip() {
  return (
    <section className="cta-strip">
      <div className="wrap">
        <span className="eyebrow">join in</span>
        <h2 style={{ marginTop: 18 }}>
          What I cannot create, I do not understand.
        </h2>
        <p>
          SQLRite is open source under MIT. Pull requests, issues, and database
          trivia all welcome.
        </p>
        <div className="cta-row">
          <a className="btn btn-primary" href={SITE.repo}>
            Star on GitHub
          </a>
          <a className="btn" href={SITE.discord}>
            Join the Discord
          </a>
        </div>
      </div>
    </section>
  );
}
