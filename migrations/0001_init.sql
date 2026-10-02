-- Initial schema for system-design-transparent.
--
-- This file is intentionally commented: the schema itself is part of what the
-- project teaches. Every index below exists because a specific query needs it.

-- Trigram matching lets search tolerate typos ("postgress", "idempotancy").
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-------------------------------------------------------------------------------
-- Accounts & sessions
-------------------------------------------------------------------------------

CREATE TABLE users (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    username      TEXT        NOT NULL CHECK (char_length(username) BETWEEN 3 AND 32),
    -- Argon2id PHC string; the salt and parameters live inside the string.
    password_hash TEXT        NOT NULL,
    is_admin      BOOLEAN     NOT NULL DEFAULT false,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Usernames are unique case-insensitively: "Alice" and "alice" are the same person.
CREATE UNIQUE INDEX users_username_lower_key ON users (lower(username));

-- Server-side sessions. The cookie holds a random token; the database only
-- stores its SHA-256 hash, so a leaked database dump cannot be replayed as
-- valid session cookies.
CREATE TABLE sessions (
    token_hash BYTEA       PRIMARY KEY,
    user_id    BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
-- Used by the background cleanup job: DELETE ... WHERE expires_at < now().
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-------------------------------------------------------------------------------
-- Content
-------------------------------------------------------------------------------

-- Tags are the main taxonomy. They are declared in content/tags.toml and are
-- also what vote rounds are organised around.
CREATE TABLE tags (
    slug        TEXT PRIMARY KEY CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$'),
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT ''
);

-- Posts are authored as Markdown files in content/posts and synced into this
-- table on startup. Git is the CMS; Postgres is the read model.
CREATE TABLE posts (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    slug            TEXT        NOT NULL UNIQUE,
    title           TEXT        NOT NULL,
    summary         TEXT        NOT NULL,
    level           TEXT        NOT NULL CHECK (level IN ('beginner', 'intermediate', 'advanced')),
    -- Tag slugs. An array + GIN index is simpler than a join table for a
    -- small, read-heavy taxonomy, and supports "has all of these tags" via @>.
    tags            TEXT[]      NOT NULL DEFAULT '{}',
    body_md         TEXT        NOT NULL,
    -- Markdown stripped to plain text: what search indexes and snippets use.
    body_text       TEXT        NOT NULL,
    body_html       TEXT        NOT NULL,
    toc_html        TEXT        NOT NULL DEFAULT '',
    reading_minutes INT         NOT NULL,
    -- SHA-256 of the source file, so unchanged files are skipped on sync.
    content_hash    TEXT        NOT NULL,
    published_at    TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL,
    -- Set to false when the source file disappears. Soft-delete keeps
    -- likes/saves intact if the file comes back (e.g. after a rename revert).
    is_published    BOOLEAN     NOT NULL DEFAULT true,
    -- Denormalised counters, maintained in the same transaction as the
    -- reaction rows. Avoids COUNT(*) on every list page.
    like_count      INT         NOT NULL DEFAULT 0,
    upvote_count    INT         NOT NULL DEFAULT 0,
    save_count      INT         NOT NULL DEFAULT 0,
    -- Weighted full-text document: title (A) > summary (B) > tags (B) > body (C).
    -- Computed by the sync job rather than a generated column because it
    -- includes the tags array, and array_to_string() is not IMMUTABLE.
    search_vector   TSVECTOR    NOT NULL
);

CREATE INDEX posts_search_vector_idx ON posts USING GIN (search_vector);
CREATE INDEX posts_tags_idx          ON posts USING GIN (tags);
CREATE INDEX posts_title_trgm_idx    ON posts USING GIN (title gin_trgm_ops);
CREATE INDEX posts_published_at_idx  ON posts (published_at DESC) WHERE is_published;

-- One table for all three reaction kinds. The composite primary key makes
-- "like twice" impossible and lets INSERT ... ON CONFLICT DO NOTHING tell us
-- whether the row was actually new.
CREATE TABLE post_reactions (
    user_id    BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    post_id    BIGINT      NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    kind       TEXT        NOT NULL CHECK (kind IN ('like', 'upvote', 'save')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, post_id, kind)
);

-- "My saved posts, newest first".
CREATE INDEX post_reactions_user_kind_idx ON post_reactions (user_id, kind, created_at DESC);

-------------------------------------------------------------------------------
-- Topic vote game
-------------------------------------------------------------------------------

-- A round is a time-boxed event (e.g. 3 days or a week). An admin picks a few
-- tags; each tag gets its own poll where anyone can suggest the next article.
CREATE TABLE vote_rounds (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    title       TEXT        NOT NULL,
    description TEXT        NOT NULL DEFAULT '',
    starts_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    ends_at     TIMESTAMPTZ NOT NULL,
    -- Set by the background finaliser once winners have been recorded.
    closed_at   TIMESTAMPTZ,
    created_by  BIGINT      REFERENCES users (id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX vote_rounds_ends_at_idx ON vote_rounds (ends_at DESC);

CREATE TABLE vote_polls (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    round_id             BIGINT NOT NULL REFERENCES vote_rounds (id) ON DELETE CASCADE,
    tag_slug             TEXT   NOT NULL REFERENCES tags (slug),
    winner_suggestion_id BIGINT,
    UNIQUE (round_id, tag_slug)
);

CREATE TABLE suggestions (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    poll_id           BIGINT      NOT NULL REFERENCES vote_polls (id) ON DELETE CASCADE,
    title             TEXT        NOT NULL CHECK (char_length(title) BETWEEN 5 AND 120),
    details           TEXT        NOT NULL DEFAULT '' CHECK (char_length(details) <= 1000),
    -- Voting is anonymous and IP-based. We never store raw IPs, only a keyed
    -- hash, which is enough to enforce "one vote per network" limits.
    author_ip_hash    BYTEA       NOT NULL,
    author_user_id    BIGINT      REFERENCES users (id) ON DELETE SET NULL,
    vote_count        INT         NOT NULL DEFAULT 0,
    is_hidden         BOOLEAN     NOT NULL DEFAULT false,
    fulfilled_post_id BIGINT      REFERENCES posts (id) ON DELETE SET NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Leaderboard for a poll.
CREATE INDEX suggestions_poll_votes_idx ON suggestions (poll_id, vote_count DESC, created_at);
-- Exact duplicate suggestions in the same poll are rejected.
CREATE UNIQUE INDEX suggestions_poll_title_key ON suggestions (poll_id, lower(title));
-- Per-IP submission limits.
CREATE INDEX suggestions_poll_author_idx ON suggestions (poll_id, author_ip_hash);

ALTER TABLE vote_polls
    ADD CONSTRAINT vote_polls_winner_fk
    FOREIGN KEY (winner_suggestion_id) REFERENCES suggestions (id) ON DELETE SET NULL;

CREATE TABLE suggestion_votes (
    suggestion_id BIGINT      NOT NULL REFERENCES suggestions (id) ON DELETE CASCADE,
    -- Denormalised so "how many votes has this IP used in this poll" is a
    -- single index lookup.
    poll_id       BIGINT      NOT NULL REFERENCES vote_polls (id) ON DELETE CASCADE,
    voter_ip_hash BYTEA       NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (suggestion_id, voter_ip_hash)
);

CREATE INDEX suggestion_votes_poll_voter_idx ON suggestion_votes (poll_id, voter_ip_hash);
