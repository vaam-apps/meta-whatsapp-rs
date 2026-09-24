# wa-rs consumer skills

Agent skills for code that **uses** [wa-rs](https://github.com/vaam-apps/wa-rs)
— the e-commerce backend (marketing, order notifications, WhatsApp OTP login)
and the CMS (merchants onboard their own number with Embedded Signup and chat
with customers in-app). They tell a coding agent how the real API is shaped,
which traps the reviews found, and what the library deliberately leaves to you.

Skills for working **on** wa-rs itself live in `.claude/skills/` instead.

## Install

```bash
npx skills add vaam-apps/wa-rs                      # every skill
npx skills add vaam-apps/wa-rs --skill wa-rs        # just the map
npx skills update                                   # re-fetch what is installed
npx skills ls                                       # what is installed, from where
```

The repository is private: the installer (and Cargo, for the crate itself)
needs GitHub credentials that can read it.

Start with `wa-rs`: it is the map and routes to the others.

| Skill | Load it when |
| --- | --- |
| [`wa-rs`](wa-rs/) | Anything with wa-rs: crates, features, building a `Client`, errors and retries, recipients |
| [`wa-rs-embedded-signup`](wa-rs-embedded-signup/) | A merchant connects their WhatsApp number (Embedded Signup, token vault, per-tenant clients) |
| [`wa-rs-webhooks`](wa-rs-webhooks/) | Receiving Meta's webhooks: endpoint, signatures, events, dedup, sinks, SSE |
| [`wa-rs-cms-inbox`](wa-rs-cms-inbox/) | The merchant ↔ customer inbox: conversations, the 24-hour window, replies, live updates |
| [`wa-rs-messaging`](wa-rs-messaging/) | Sending anything: text, media, interactive, products, templates, marketing, opt-outs |
| [`wa-rs-templates-otp`](wa-rs-templates-otp/) | Creating/managing templates, and WhatsApp OTP login |
| [`wa-rs-documents`](wa-rs-documents/) | Invoices, receipts and vouchers rendered with Typst and sent as documents/images |

## Versioning: a skill is true of one wa-rs commit

wa-rs has no releases yet (`publish = false`; consumers depend on it by git
revision). So each `SKILL.md` names the **commit and date it was verified
against**, directly under its title:

```markdown
> **Verified against wa-rs 91431ae (2026-09-24).**
```

"Verified" means every type, function, argument and enum variant the skill
names was checked against the source at that commit, and the key snippets
were compiled against it. It does not mean the prose is true of any other
commit.

The rules:

1. **Your `Cargo.toml` pins you, not the skill.** Compare the `rev` you depend
   on with the skill's stamp. Same commit: trust the skill. Different commit:
   **trust the code** — read the rustdoc (`cargo doc -p wa-rs --open`) or the
   source for anything the skill tells you, and prefer what the code says.
2. **A newer skill on an older wa-rs is the dangerous case.** It will describe
   API your pinned revision does not have. Either bump the `rev`, or read the
   skill's git history for the version stamped nearest your revision.
3. **When a skill is corrected, the old claim stays visible**: struck through,
   with the date and the reason, so a reader can tell which belief was wrong
   and when it stopped being true.
4. **A stamp may move forward one skill at a time.** Re-verifying one skill
   against a newer wa-rs and restamping only that skill is expected; the
   stamps do not have to agree with each other.

Inside wa-rs, a change to a public API is not done until `docs/`, the rustdoc
and these skills agree with it (see `AGENTS.md`). The pull request says what
happened to each.

## Do not hand-edit installed copies

`npx skills` records a hash of what it installed; a local edit reads as drift
and the next `update` overwrites it. Send the fix to this repository instead.
