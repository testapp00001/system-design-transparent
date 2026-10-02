# Contributing

Thank you for helping make backend knowledge less dependent on luck. The most valuable
contributions are **articles** and **corrections to articles**.

## Writing or editing an article

1. Copy [`content/posts/_template.md`](content/posts/_template.md) to
   `content/posts/<your-slug>.md`. The file name is the URL: lowercase words separated by `-`.
2. Fill in the frontmatter:

   ```toml
   +++
   title = "Retries, timeouts and idempotency"
   summary = "One or two sentences (20–320 characters) shown in lists and search results."
   tags = ["reliability", "api-design"]     # must exist in content/tags.toml
   level = "intermediate"                    # beginner | intermediate | advanced
   date = 2026-10-02                         # first published
   updated = 2026-11-15                      # optional: last meaningful update
   draft = false                             # optional: true hides it from the site
   +++
   ```

3. If the article covers a topic on the roadmap, link it in `content/roadmap.toml` with
   `post = "<your-slug>"`.
4. Check everything is valid: `cargo run -- check-content` (CI runs the same check).
5. Run the site (`cargo run`) and read your article in all three themes.
6. Open a pull request. Small fixes (typos, a wrong number, a broken link) are just as welcome.

## What makes a good article here

Readers are working developers who know the basics (HTTP, SQL, a web framework) but may never have
operated a system at scale. Aim to give them the mental model a senior colleague would.

- **Start with the problem**, not the solution. "Why does this exist?" before "how does it work?".
- **Explain the mechanism** concretely: what actually happens, step by step, with a small diagram
  (ASCII in a code block is fine) or a code/SQL snippet.
- **Show the trade-offs.** Every technique has a cost. Say when *not* to use it.
- **Be practical.** Include defaults, rules of thumb, commands, and the mistakes people commonly make.
- **Be accurate.** Claims about a specific product or company must link to a primary source —
  official documentation or the company's own engineering blog/talk. Prefer "Discord reported
  roughly X" over unsourced precise numbers. If unsure, leave it out.
- **Write simply.** Many readers are not native English speakers. Short sentences, defined terms,
  no unexplained jargon or memes.
- **Be timeless where possible.** Focus on principles that age well; date-stamp anything that
  changes quickly (versions, prices, defaults) with `updated`.

A typical structure (adapt freely):

```markdown
Intro paragraph: the situation and the problem.

## The problem
## How it works
## Trade-offs / when not to use it
## In practice (defaults, checklists, code)
## Common mistakes
## Further reading
```

Useful Markdown features: tables, footnotes, `> [!NOTE]` / `> [!TIP]` / `> [!WARNING]` callouts,
and `{#custom-id}` after a heading to give it a stable anchor.

## Proposing topics

Use the vote rounds on the site, or open an issue. To add a topic to the roadmap without writing
it yet, add a `[[section.topic]]` entry without a `post` field.

## Code contributions

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test        # uses DATABASE_URL from .env; the role must be allowed to create databases
```

Keep the codebase small and readable — it doubles as a teaching example. Prefer plain SQL and
standard library solutions over new dependencies, and comment the *why* of anything non-obvious.
