//! Phase 8d — hybrid retrieval worked example. Combines Phase 8b's
//! BM25 lexical index with Phase 7d's vector cosine similarity into a
//! single ORDER BY, showing where each shape wins and why fusing the
//! two beats either alone.
//!
//! Run with: `cargo run --example hybrid-retrieval`
//!
//! Corpus: 6 hand-written 1-sentence "tech blurbs", each with a
//! pre-baked 4-dim embedding `[systems, scripting, database, web]`.
//! Real RAG would call an embedding model; the math is identical.
//! Vectors are hand-set so each query's expected ranking is obvious
//! by inspection — no surprises from a neural net's latent space.
//!
//! See `README.md` for the narrative explanation.

use sqlrite::{Connection, Result};

// (name, body, embedding) per doc.
const CORPUS: &[(&str, &str, [f32; 4])] = &[
    (
        "doc1",
        "rust is a systems programming language",
        [0.9, 0.0, 0.0, 0.0],
    ),
    (
        "doc2",
        "python is great for data science",
        [0.0, 0.9, 0.4, 0.0],
    ),
    (
        "doc3",
        "sqlite is an embedded database engine",
        [0.0, 0.0, 0.9, 0.0],
    ),
    (
        "doc4",
        "postgres is a powerful relational database server",
        [0.1, 0.0, 0.9, 0.5],
    ),
    (
        "doc5",
        "javascript runs in browsers and on servers",
        [0.0, 0.7, 0.0, 0.8],
    ),
    (
        "doc6",
        "redis caches data in memory for fast lookups",
        [0.0, 0.0, 0.6, 0.5],
    ),
];

fn main() -> Result<()> {
    let mut conn = Connection::open_in_memory()?;
    conn.execute(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, name TEXT, body TEXT, embedding VECTOR(4));",
    )?;
    for (name, body, vec) in CORPUS {
        conn.execute(&format!(
            "INSERT INTO docs (name, body, embedding) VALUES \
             ('{name}', '{body}', [{}, {}, {}, {}]);",
            vec[0], vec[1], vec[2], vec[3]
        ))?;
    }
    conn.execute("CREATE INDEX docs_fts ON docs USING fts (body);")?;

    // Same query, three rankings — see README for what each shape sees.
    let body_query = "small embedded database";
    let vector_query = [0.0, 0.0, 0.9, 0.2]; // semantic intent: "database, lightly web-ish"
    let q_str = vec_lit(&vector_query);

    println!("Corpus:");
    for (name, body, vec) in CORPUS {
        println!("  {name}: \"{body}\"  embedding={vec:?}");
    }
    println!("\nQuery body:   '{body_query}'");
    println!("Query vector: {vector_query:?}\n");

    println!("===  1. Pure BM25 (lexical) ===");
    println!(
        "WHERE  fts_match(body, '{body_query}')\n\
         ORDER BY bm25_score(body, '{body_query}') DESC  LIMIT 3"
    );
    print_top(
        &mut conn,
        &format!(
            "SELECT name, body FROM docs \
             WHERE fts_match(body, '{body_query}') \
             ORDER BY bm25_score(body, '{body_query}') DESC LIMIT 3;"
        ),
    )?;

    println!("===  2. Pure vector (semantic) ===");
    println!("ORDER BY vec_distance_cosine(embedding, {q_str}) ASC  LIMIT 3");
    print_top(
        &mut conn,
        &format!(
            "SELECT name, body FROM docs \
             ORDER BY vec_distance_cosine(embedding, {q_str}) ASC LIMIT 3;"
        ),
    )?;

    println!("===  3. Hybrid (50% BM25 + 50% inverted cosine) ===");
    println!(
        "WHERE  fts_match(body, '{body_query}')\n\
         ORDER BY 0.5*bm25_score(...) + 0.5*(1.0 - vec_distance_cosine(...)) DESC  LIMIT 3"
    );
    print_top(
        &mut conn,
        &format!(
            "SELECT name, body FROM docs \
             WHERE fts_match(body, '{body_query}') \
             ORDER BY 0.5 * bm25_score(body, '{body_query}') \
                    + 0.5 * (1.0 - vec_distance_cosine(embedding, {q_str})) DESC \
             LIMIT 3;"
        ),
    )?;
    Ok(())
}

fn vec_lit(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| format!("{x}")).collect();
    format!("[{}]", parts.join(", "))
}

fn print_top(conn: &mut Connection, sql: &str) -> Result<()> {
    let stmt = conn.prepare(sql)?;
    let mut rows = stmt.query()?;
    let mut rank = 1;
    while let Some(row) = rows.next()? {
        let name: String = row.get_by_name("name")?;
        let body: String = row.get_by_name("body")?;
        println!("  {rank}. {name}  \"{body}\"");
        rank += 1;
    }
    println!();
    Ok(())
}
