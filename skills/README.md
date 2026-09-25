# meta-whatsapp-rs consumer skills

Agent skills for code that **uses** [meta-whatsapp-rs](https://github.com/vaam-apps/meta-whatsapp-rs)
— an e-commerce backend (marketing, order notifications, WhatsApp OTP
login) or a CMS whose merchants connect their own number with Embedded
Signup and chat with their customers. Each skill is one job an integrator
(or their coding agent) asks for: it names the real API, shows code meta-whatsapp-rs
compiles and tests, lists the traps the reviews found, and says what the
library leaves to you.

Skills for working **on** meta-whatsapp-rs itself live in `.claude/skills/`; they are
marked `internal` and not offered by the installer.

## Install

```bash
npx skills add vaam-apps/meta-whatsapp-rs --list    # what is offered
npx skills add vaam-apps/meta-whatsapp-rs           # every skill
npx skills add vaam-apps/meta-whatsapp-rs -s meta-whatsapp-rs -s meta-whatsapp-rs-webhook-endpoint -s meta-whatsapp-rs-cms-inbox
npx skills update                                   # re-fetch what is installed
```

The repository is public: neither the installer nor Cargo needs
credentials. Install `meta-whatsapp-rs` in any case: it is the map and routes to the
others. Each skill is self-contained (its
`references/` and `examples/` travel with it; links elsewhere point at
GitHub), so any subset works.

Skills installed before the project was renamed (named `wa-rs` and
`wa-rs-*`) name crates and paths that no longer exist, and
`npx skills update` cannot refresh them: the repository has no skills by
those names any more. Remove them (`npx skills list` shows them;
`npx skills remove <name> …`), then add the ones above.

## The skills

**Start**

| Skill | Load it when |
| --- | --- |
| [`meta-whatsapp-rs`](meta-whatsapp-rs/) | anything with meta-whatsapp-rs: install, features, rules, which skill to load |
| [`meta-whatsapp-rs-setup`](meta-whatsapp-rs-setup/) | Meta-side setup, building the `Client`, tokens, API version, unwrapped endpoints |
| [`meta-whatsapp-rs-errors`](meta-whatsapp-rs-errors/) | matching errors, retries, job queues around sends |
| [`meta-whatsapp-rs-testing`](meta-whatsapp-rs-testing/) | testing your code without Meta or a database |

**Messaging**

| Skill | Load it when |
| --- | --- |
| [`meta-whatsapp-rs-send-messages`](meta-whatsapp-rs-send-messages/) | text, media, location, contacts, reactions, read receipts |
| [`meta-whatsapp-rs-interactive-messages`](meta-whatsapp-rs-interactive-messages/) | buttons, lists, CTA links, location requests, Flows, carousels |
| [`meta-whatsapp-rs-media`](meta-whatsapp-rs-media/) | uploads, verified downloads, template header handles |

**Templates and authentication**

| Skill | Load it when |
| --- | --- |
| [`meta-whatsapp-rs-templates`](meta-whatsapp-rs-templates/) | creating and managing templates, following their review |
| [`meta-whatsapp-rs-send-templates`](meta-whatsapp-rs-send-templates/) | sending a template with its parameters |
| [`meta-whatsapp-rs-otp-login`](meta-whatsapp-rs-otp-login/) | login or phone verification with WhatsApp codes |

**Onboarding merchants**

| Skill | Load it when |
| --- | --- |
| [`meta-whatsapp-rs-embedded-signup`](meta-whatsapp-rs-embedded-signup/) | the "Connect WhatsApp" flow and its callback; Solution Partner credit lines |
| [`meta-whatsapp-rs-token-vault`](meta-whatsapp-rs-token-vault/) | merchants' tokens, key rotation, acting as a merchant |
| [`meta-whatsapp-rs-phone-numbers`](meta-whatsapp-rs-phone-numbers/) | registration, PIN, business profile, webhook subscriptions |

**Webhooks and chat**

| Skill | Load it when |
| --- | --- |
| [`meta-whatsapp-rs-webhook-endpoint`](meta-whatsapp-rs-webhook-endpoint/) | the endpoint Meta calls, on axum or any framework |
| [`meta-whatsapp-rs-webhook-events`](meta-whatsapp-rs-webhook-events/) | what each event means and what to do with it |
| [`meta-whatsapp-rs-live-updates`](meta-whatsapp-rs-live-updates/) | sinks, fan-out, SSE, background workers |
| [`meta-whatsapp-rs-cms-inbox`](meta-whatsapp-rs-cms-inbox/) | the merchant ↔ customer inbox of a CMS |
| [`meta-whatsapp-rs-groups-and-calling`](meta-whatsapp-rs-groups-and-calling/) | blocking a customer, group chats, WhatsApp calls |

**Business features**

| Skill | Load it when |
| --- | --- |
| [`meta-whatsapp-rs-marketing`](meta-whatsapp-rs-marketing/) | campaigns, opt-ins and opt-outs, analytics, QR codes |
| [`meta-whatsapp-rs-commerce`](meta-whatsapp-rs-commerce/) | catalogs, product messages, carts |
| [`meta-whatsapp-rs-documents`](meta-whatsapp-rs-documents/) | invoices, receipts, vouchers rendered with Typst |
| [`meta-whatsapp-rs-flows`](meta-whatsapp-rs-flows/) | WhatsApp Flows and their data endpoint |

**Operations**

| Skill | Load it when |
| --- | --- |
| [`meta-whatsapp-rs-storage`](meta-whatsapp-rs-storage/) | memory, Postgres or Redis stores; your own adapter |
| [`meta-whatsapp-rs-production`](meta-whatsapp-rs-production/) | secrets, logs, limits, versions, several instances |

## Versioning: a skill is true of one meta-whatsapp-rs commit

meta-whatsapp-rs has no releases (`publish = false`; you depend on a git `rev`). So
each `SKILL.md` names the commit it was verified against, under its title:

```markdown
> **Verified against meta-whatsapp-rs 6d04f3da9c504cffac32f7dbe05869adcaf1957e (2026-09-25).**
```

"Verified" means: every Rust block is an excerpt of a file meta-whatsapp-rs compiles
and tests at that commit (the skill's own `examples/*.rs`, or
`crates/meta-whatsapp-rs/examples/*.rs`), and every Rust name the prose uses exists
there. The rules:

1. **Your `Cargo.toml` pins you, not the skill.** Same commit as the
   stamp: trust the skill. Another commit: trust the code (rustdoc,
   source) wherever they disagree. A stamp may name a pull request's
   commit that was squash-merged: the `main` commit that carries it is the
   one listing it, found with
   `git log origin/main --grep "Squashed-commit: <sha>"`, and GitHub still
   shows the stamped commit itself at `/commit/<sha>`.
2. **A newer skill on an older meta-whatsapp-rs is the dangerous case**: it describes
   API your revision lacks. Bump the `rev`, or use the skill's git history
   at the stamp nearest your revision.
3. **A corrected claim stays visible**, struck through with the commit or
   date it stopped being true, in the skill it belongs to: upgraders see
   what changed.
4. **Stamps move one skill at a time**: re-verifying one skill against a
   newer commit restamps that skill only.

## How meta-whatsapp-rs keeps them true

`just ci` runs, on every change:

- `crates/meta-whatsapp-rs/tests/skills.rs` (in `just test`): every
  `skills/*/examples/*.rs` compiles and its tests pass, and hides no code
  that is never compiled (no block comments, `macro_rules!` or `cfg`
  but the tests' `#[cfg(test)]`, and in the crate's own examples only the
  `postgres` arms); every Rust block is a verbatim excerpt of a compiled
  file, and of its compiled code only: never lines inside a string (raw,
  byte or C strings included) or a block comment, nor an item under a
  `cfg` that `--all-features` never enables (such as the
  `#[cfg(not(feature = …))]` arms of the crate's own examples, or their
  `#[cfg(test)]` items) or under a `#[cfg_attr(…, cfg(…))]`,
  and every fence carries a known language, so no Rust escapes the check
  as an `rs` or `rust,ignore` fence, a `~~~` one or an unlabeled block;
  frontmatter parses the way
  the `npx skills` CLI parses it (quoted descriptions, `name` = directory,
  consumer skills never `internal`, developer skills always); the
  installer finds no other `SKILL.md` (a root one would hide every
  skill); relative links resolve and stay inside the skill; links into
  this repository and their anchors exist; every backticked Rust name
  exists in `crates/` (`skills/.allowlist` lists the placeholders and
  other crates' names), and the last segment of a path must be a variant,
  field or item of the type before it, not just of the same file; every
  skill and every `references/*.md` is stamped under its title (and any
  other stamp is well-formed), every skill is short, and listed
  here and in the `meta-whatsapp-rs` hub.
- `just skills-check`: every stamp's commit is in the checked-out
  commit's history, as an ancestor or as a pull request's commit that a
  squash commit on main lists in its Squashed-commit lines.

What no check can prove: that the prose's *semantics* are right (a real
constant with a wrong value, a real method called on the wrong type
through a variable, `inbox.publish()`). Reviews do that.

To see what the installer offers from a checkout (developer skills must
not appear): `npx -y skills add <path-to-checkout> --list`.

## Do not hand-edit installed copies

`npx skills` records a hash of what it installed; a local edit reads as
drift and the next `update` overwrites it. Send the fix here instead.
