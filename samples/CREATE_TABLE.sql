CREATE TABLE contacts (
    id INTEGER PRIMARY KEY,
    first_name TEXT NOT NULL,
    last_name TEXT NOT NULl,
    email TEXT NOT NULL UNIQUE
);

CREATE TABLE artists (
    id INTEGER PRIMARY KEY,
    first_name TEXT NOT NULL,
    last_name TEXT NOT NULl,
    email TEXT NOT NULL UNIQUE
);

CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT UNIQUE,
    email TEXT,
);

CREATE TABLE players (
    name TEXT PRIMARY KEY,
    email TEXT,
);