# wa-rs consumer skills

Agent skills for code that **uses** [wa-rs](https://github.com/vaam-apps/wa-rs)
— an e-commerce backend (marketing, order notifications, WhatsApp OTP
login) or a CMS whose merchants connect their own number with Embedded
Signup and chat with their customers. Each skill is one job an integrator
(or their coding agent) asks for: it names the real API, shows code wa-rs
compiles and tests, lists the traps the reviews found, and says what the
library leaves to you.

Skills for working **on** wa-rs itself live in `.claude/skills/`; they are
marked `internal` and not offered by the installer.

## Install

```bash
npx skills add vaam-apps/wa-rs --list                                  # what is offered
npx skills add vaam-apps/wa-rs                                         # every skill
npx skills add vaam-apps/wa-rs -s wa-rs -s wa-rs-webhook-endpoint -s wa-rs-cms-inbox
npx skills update                                                      # re-fetch what is installed
```

The repository is public: neither the installer nor Cargo needs
credentials. Install `wa-rs` in any case: it is the map and routes to the
others. Each skill is self-contained (its
`references/` and `examples/` travel with it; links elsewhere point at
GitHub), so any subset works.

## The skills

**Start**

| Skill | Load it when |
| --- | --- |
| [`wa-rs`](wa-rs/) | anything with wa-rs: install, features, rules, which skill to load |
| [`wa-rs-setup`](wa-rs-setup/) | Meta-side setup, building the `Client`, tokens, API version, unwrapped endpoints |
| [`wa-rs-errors`](wa-rs-errors/) | matching errors, retries, job queues around sends |
| [`wa-rs-testing`](wa-rs-testing/) | testing your code without Meta or a database |

**Messaging**

| Skill | Load it when |
| --- | --- |
| [`wa-rs-send-messages`](wa-rs-send-messages/) | text, media, location, contacts, reactions, read receipts |
| [`wa-rs-interactive-messages`](wa-rs-interactive-messages/) | buttons, lists, CTA links, location requests, Flows, carousels |
| [`wa-rs-media`](wa-rs-media/) | uploads, verified downloads, template header handles |

**Templates and authentication**

| Skill | Load it when |
| --- | --- |
| [`wa-rs-templates`](wa-rs-templates/) | creating and managing templates, following their review |
| [`wa-rs-send-templates`](wa-rs-send-templates/) | sending a template with its parameters |
| [`wa-rs-otp-login`](wa-rs-otp-login/) | login or phone verification with WhatsApp codes |

**Onboarding merchants**

| Skill | Load it when |
| --- | --- |
| [`wa-rs-embedded-signup`](wa-rs-embedded-signup/) | the "Connect WhatsApp" flow and its callback |
| [`wa-rs-token-vault`](wa-rs-token-vault/) | merchants' tokens, key rotation, acting as a merchant |
| [`wa-rs-phone-numbers`](wa-rs-phone-numbers/) | registration, PIN, business profile, webhook subscriptions |

**Webhooks and chat**

| Skill | Load it when |
| --- | --- |
| [`wa-rs-webhook-endpoint`](wa-rs-webhook-endpoint/) | the endpoint Meta calls, on axum or any framework |
| [`wa-rs-webhook-events`](wa-rs-webhook-events/) | what each event means and what to do with it |
| [`wa-rs-live-updates`](wa-rs-live-updates/) | sinks, fan-out, SSE, background workers |
| [`wa-rs-cms-inbox`](wa-rs-cms-inbox/) | the merchant ↔ customer inbox of a CMS |
| [`wa-rs-groups-and-calling`](wa-rs-groups-and-calling/) | blocking a customer, group chats, WhatsApp calls |

**Business features**

| Skill | Load it when |
| --- | --- |
| [`wa-rs-marketing`](wa-rs-marketing/) | campaigns, opt-ins and opt-outs, analytics, QR codes |
| [`wa-rs-commerce`](wa-rs-commerce/) | catalogs, product messages, carts |
| [`wa-rs-documents`](wa-rs-documents/) | invoices, receipts, vouchers rendered with Typst |
| [`wa-rs-flows`](wa-rs-flows/) | WhatsApp Flows and their data endpoint |

**Operations**

| Skill | Load it when |
| --- | --- |
| [`wa-rs-storage`](wa-rs-storage/) | memory, Postgres or Redis stores; your own adapter |
| [`wa-rs-production`](wa-rs-production/) | secrets, logs, limits, versions, several instances |

## Versioning: a skill is true of one wa-rs commit

wa-rs has no releases (`publish = false`; you depend on a git `rev`). So
each `SKILL.md` names the commit it was verified against, under its title:

```markdown
> **Verified against wa-rs 6909be3b54768abc3d5f9b04543a49f32b072669 (2026-09-25).**
```

"Verified" means: every Rust block is an excerpt of a file wa-rs compiles
and tests at that commit (the skill's own `examples/*.rs`, or
`crates/wa-rs/examples/*.rs`), and every Rust name the prose uses exists
there. The rules:

1. **Your `Cargo.toml` pins you, not the skill.** Same commit as the
   stamp: trust the skill. Another commit: trust the code (rustdoc,
   source) wherever they disagree.
2. **A newer skill on an older wa-rs is the dangerous case**: it describes
   API your revision lacks. Bump the `rev`, or use the skill's git history
   at the stamp nearest your revision.
3. **A corrected claim stays visible**, struck through with the commit or
   date it stopped being true, in the skill it belongs to: upgraders see
   what changed.
4. **Stamps move one skill at a time**: re-verifying one skill against a
   newer commit restamps that skill only.

## How wa-rs keeps them true

`just ci` runs, on every change:

- `crates/wa-rs/tests/skills.rs` (in `just test`): every
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
  here and in the `wa-rs` hub.
- `just skills-check`: every stamp's commit exists and is an ancestor of
  the checked-out commit.

What no check can prove: that the prose's *semantics* are right (a real
constant with a wrong value, a real method called on the wrong type
through a variable, `inbox.publish()`). Reviews do that.

To see what the installer offers from a checkout (developer skills must
not appear): `npx -y skills add <path-to-checkout> --list`.

## Do not hand-edit installed copies

`npx skills` records a hash of what it installed; a local edit reads as
drift and the next `update` overwrites it. Send the fix here instead.
