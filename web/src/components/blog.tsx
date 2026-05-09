type Post = {
  num: string;
  title: string;
  desc: string;
  href: string;
};

const POSTS: Post[] = [
  {
    num: "Part 0",
    title: "Overview",
    desc:
      "What this whole series is about, and why rebuilding SQLite is the right way to learn it.",
    href: "https://medium.com/the-polyglot-programmer/what-would-sqlite-would-look-like-if-written-in-rust-part-0-4fc192368984",
  },
  {
    num: "Part 1",
    title: "Setting up the CLI and REPL",
    desc:
      "From cargo new to a SQLite-style prompt with history and bracket matching.",
    href: "https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-1-4a84196c217d",
  },
  {
    num: "Part 2",
    title: "Statements and meta commands",
    desc:
      "Hand-rolling the parser surface and the typed error path that replaces every panic.",
    href: "https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-2-55b30824de0c",
  },
  {
    num: "Part 3",
    title: "B-Trees and database design",
    desc:
      "Why every embedded database leans on this one data structure.",
    href: "https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-3-edd2eefda473",
  },
];

export function Blog() {
  return (
    <section id="blog">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">09 · written in public</span>
          <div>
            <h2>Read the series.</h2>
            <p className="sub">
              SQLRite is a learning project as much as a database. Each phase is
              paired with a long-form post on the design choices behind it.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="blog-list">
            {POSTS.map((p) => (
              <a
                className="blog-item"
                key={p.num}
                href={p.href}
                target="_blank"
                rel="noreferrer"
              >
                <span className="num">{p.num.toUpperCase()}</span>
                <h3>{p.title}</h3>
                <p className="dim" style={{ fontSize: 14 }}>
                  {p.desc}
                </p>
                <span className="arrow">read on medium →</span>
              </a>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
