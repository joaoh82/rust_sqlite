// Pre-loaded sample datasets for the playground. Each one is a complete
// setup script (DDL + seed rows) plus a sample query that drops into the
// editor when the dataset loads. The scripts only use SQL the engine
// actually supports today — notably:
//   * No aggregates over JOIN results (rejected by the engine — SQLR issue
//     #6), so the northwind JOIN demo is projection-only.
//   * `vec_distance_*` lives in ORDER BY / WHERE, never the SELECT list
//     (SQLR docs/supported-sql.md), so the movies KNN query projects plain
//     columns and ranks in ORDER BY.
//   * Booleans are stored as 0/1 INTEGERs to avoid leaning on boolean
//     literal support.

export type Dataset = {
  id: string;
  label: string;
  /** One-line description shown in the dataset picker / status line. */
  blurb: string;
  /** SQLRite features this dataset is meant to show off (UI pills). */
  features: string[];
  /** DDL + seed data, run on a fresh DB when the dataset is selected. */
  setup: string;
  /** Query dropped into the editor after the dataset loads. */
  sampleQuery: string;
};

const POKEMON: Dataset = {
  id: "pokemon",
  label: "Pokémon",
  blurb: "151 first-gen creatures (a 24-row sample) — filters, ORDER BY, GROUP BY.",
  features: ["WHERE", "GROUP BY", "aggregates"],
  setup: `-- Pokémon — a single-table dataset for SELECT / WHERE / GROUP BY.
CREATE TABLE pokemon (
  id         INTEGER PRIMARY KEY,
  name       TEXT,
  type1      TEXT,
  type2      TEXT,
  hp         INTEGER,
  attack     INTEGER,
  defense    INTEGER,
  speed      INTEGER,
  generation INTEGER,
  legendary  INTEGER   -- 0 / 1
);

INSERT INTO pokemon (name, type1, type2, hp, attack, defense, speed, generation, legendary) VALUES
  ('Bulbasaur',  'Grass',    'Poison', 45, 49, 49, 45, 1, 0),
  ('Charmander', 'Fire',     NULL,     39, 52, 43, 65, 1, 0),
  ('Charizard',  'Fire',     'Flying', 78, 84, 78,100, 1, 0),
  ('Squirtle',   'Water',    NULL,     44, 48, 65, 43, 1, 0),
  ('Blastoise',  'Water',    NULL,     79, 83,100, 78, 1, 0),
  ('Pikachu',    'Electric', NULL,     35, 55, 40, 90, 1, 0),
  ('Raichu',     'Electric', NULL,     60, 90, 55,110, 1, 0),
  ('Jigglypuff', 'Normal',   'Fairy',115, 45, 20, 20, 1, 0),
  ('Geodude',    'Rock',     'Ground', 40, 80,100, 20, 1, 0),
  ('Onix',       'Rock',     'Ground', 35, 45,160, 70, 1, 0),
  ('Gengar',     'Ghost',    'Poison', 60, 65, 60,110, 1, 0),
  ('Onix',       'Rock',     'Ground', 35, 45,160, 70, 1, 0),
  ('Eevee',      'Normal',   NULL,     55, 55, 50, 55, 1, 0),
  ('Vaporeon',   'Water',    NULL,    130, 65, 60, 65, 1, 0),
  ('Jolteon',    'Electric', NULL,     65, 65, 60,130, 1, 0),
  ('Snorlax',    'Normal',   NULL,    160,110, 65, 30, 1, 0),
  ('Dragonite',  'Dragon',   'Flying', 91,134, 95, 80, 1, 0),
  ('Mewtwo',     'Psychic',  NULL,    106,110, 90,130, 1, 1),
  ('Mew',        'Psychic',  NULL,    100,100,100,100, 1, 1),
  ('Articuno',   'Ice',      'Flying', 90, 85,100, 85, 1, 1),
  ('Zapdos',     'Electric', 'Flying', 90, 90, 85,100, 1, 1),
  ('Moltres',    'Fire',     'Flying', 90,100, 90, 90, 1, 1),
  ('Machamp',    'Fighting', NULL,     90,130, 80, 55, 1, 0),
  ('Alakazam',   'Psychic',  NULL,     55, 50, 45,120, 1, 0);`,
  sampleQuery: `-- Strongest non-legendary attackers.
SELECT name, type1, attack, speed
FROM pokemon
WHERE legendary = 0
ORDER BY attack DESC
LIMIT 8;

-- Try a single-table aggregate too (run it on its own):
-- SELECT type1, COUNT(*) AS n, AVG(hp) AS avg_hp
-- FROM pokemon GROUP BY type1 ORDER BY n DESC;`,
};

const NORTHWIND: Dataset = {
  id: "northwind",
  label: "Northwind (slim)",
  blurb: "Classic orders schema — multi-table INNER JOINs across 4 tables.",
  features: ["JOIN", "multi-table", "ORDER BY"],
  setup: `-- Northwind (slimmed) — customers, products, orders, order_items.
-- Showcases SQLRite's multi-table JOINs. (Aggregates over JOIN results
-- aren't supported yet, so the JOIN demo projects columns directly.)
CREATE TABLE customers (
  id      INTEGER PRIMARY KEY,
  name    TEXT,
  country TEXT
);
CREATE TABLE products (
  id       INTEGER PRIMARY KEY,
  name     TEXT,
  category TEXT,
  price    REAL
);
CREATE TABLE orders (
  id          INTEGER PRIMARY KEY,
  customer_id INTEGER,
  order_date  TEXT
);
CREATE TABLE order_items (
  id         INTEGER PRIMARY KEY,
  order_id   INTEGER,
  product_id INTEGER,
  quantity   INTEGER
);

INSERT INTO customers (name, country) VALUES
  ('Alfreds Futterkiste',   'Germany'),
  ('Around the Horn',       'UK'),
  ('Berglunds snabbköp',    'Sweden'),
  ('Blondel père et fils',  'France'),
  ('Ernst Handel',          'Austria');

INSERT INTO products (name, category, price) VALUES
  ('Chai',            'Beverages',  18.00),
  ('Chang',           'Beverages',  19.00),
  ('Aniseed Syrup',   'Condiments', 10.00),
  ('Chef Anton''s Mix','Condiments', 22.00),
  ('Pavlova',         'Confections', 17.45),
  ('Tofu',            'Produce',     23.25),
  ('Konbu',           'Seafood',      6.00);

INSERT INTO orders (customer_id, order_date) VALUES
  (1, '2024-01-15'),
  (2, '2024-01-18'),
  (3, '2024-02-02'),
  (1, '2024-02-20'),
  (5, '2024-03-09');

INSERT INTO order_items (order_id, product_id, quantity) VALUES
  (1, 1, 10), (1, 5, 3),
  (2, 2, 6),  (2, 7, 20),
  (3, 3, 12), (3, 6, 4),
  (4, 4, 8),
  (5, 1, 15), (5, 2, 5), (5, 5, 2);`,
  sampleQuery: `-- Order lines joined across 4 tables (no aggregates).
-- NB: SQLRite's ORDER BY takes a single sort key for now.
SELECT c.name AS customer, o.order_date, p.name AS product, oi.quantity
FROM order_items oi
JOIN orders o     ON oi.order_id = o.id
JOIN customers c  ON o.customer_id = c.id
JOIN products p   ON oi.product_id = p.id
ORDER BY o.order_date
LIMIT 20;

-- Single-table aggregate (run separately):
-- SELECT category, COUNT(*) AS n, AVG(price) AS avg_price
-- FROM products GROUP BY category ORDER BY avg_price DESC;`,
};

const MOVIES: Dataset = {
  id: "movies",
  label: "Movies (vector search)",
  blurb: "12 films with 4-dim embeddings + an HNSW index — cosine KNN search.",
  features: ["VECTOR(4)", "HNSW", "cosine KNN"],
  setup: `-- Movies — the vector-search demo. Each film carries a hand-made
-- 4-dim "taste" embedding over the axes [sci-fi, romance, action, comedy].
-- The HNSW index makes the ORDER BY vec_distance_cosine query an
-- approximate-nearest-neighbour probe instead of a full scan — the same
-- machinery the Python agent + notes examples use for RAG, here entirely
-- in your browser tab.
CREATE TABLE movies (
  id        INTEGER PRIMARY KEY,
  title     TEXT,
  genre     TEXT,
  year      INTEGER,
  embedding VECTOR(4)
);

CREATE INDEX idx_movies_embedding
  ON movies USING hnsw (embedding) WITH (metric = 'cosine');

INSERT INTO movies (title, genre, year, embedding) VALUES
  ('The Matrix',                'sci-fi / action', 1999, [0.80, 0.05, 0.50, 0.05]),
  ('Blade Runner',              'sci-fi',          1982, [0.90, 0.10, 0.20, 0.00]),
  ('Interstellar',              'sci-fi',          2014, [0.85, 0.20, 0.15, 0.05]),
  ('Arrival',                   'sci-fi',          2016, [0.90, 0.25, 0.05, 0.05]),
  ('Die Hard',                  'action',          1988, [0.10, 0.00, 0.90, 0.15]),
  ('Mad Max: Fury Road',        'action',          2015, [0.20, 0.05, 0.95, 0.05]),
  ('John Wick',                 'action',          2014, [0.05, 0.00, 0.90, 0.10]),
  ('Notting Hill',              'romance',         1999, [0.00, 0.90, 0.05, 0.30]),
  ('Pride & Prejudice',         'romance',         2005, [0.05, 0.95, 0.00, 0.10]),
  ('La La Land',                'romance / comedy',2016, [0.10, 0.80, 0.05, 0.50]),
  ('Superbad',                  'comedy',          2007, [0.00, 0.10, 0.05, 0.95]),
  ('The Grand Budapest Hotel',  'comedy',          2014, [0.10, 0.20, 0.10, 0.90]);`,
  sampleQuery: `-- Nearest neighbours (cosine) to a sci-fi / action taste vector.
-- HNSW-probed via the index. vec_distance_cosine is allowed in ORDER BY
-- and WHERE but not in the SELECT projection (recompute client-side if
-- you need the score).
SELECT title, genre, year
FROM movies
ORDER BY vec_distance_cosine(embedding, [0.85, 0.05, 0.40, 0.05])
LIMIT 5;

-- Swap the query vector for a rom-com lean and re-run:
-- ORDER BY vec_distance_cosine(embedding, [0.05, 0.85, 0.05, 0.45])`,
};

export const DATASETS: Dataset[] = [POKEMON, NORTHWIND, MOVIES];

/** Default editor contents on a cold first visit (no hash, no saved DB). */
export const WELCOME_SQL = `-- Welcome to the SQLRite playground — the full engine, compiled to
-- WebAssembly, running entirely in this browser tab. No server.
--
-- Run with the Run button or Cmd/Ctrl+Enter. Pick a sample dataset from
-- the toolbar to get a schema + data + an example query in one click.
CREATE TABLE greetings (id INTEGER PRIMARY KEY, lang TEXT, text TEXT);
INSERT INTO greetings (lang, text) VALUES
  ('en', 'Hello'),
  ('pt', 'Olá'),
  ('ja', 'こんにちは'),
  ('rust', 'println!("hi")');

SELECT id, lang, text FROM greetings ORDER BY lang;`;

export function findDataset(id: string | null | undefined): Dataset | undefined {
  if (!id) return undefined;
  return DATASETS.find((d) => d.id === id);
}
