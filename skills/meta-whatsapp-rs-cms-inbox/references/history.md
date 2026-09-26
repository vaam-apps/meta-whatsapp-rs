# What changed in the inbox, and when

> **Verified against meta-whatsapp-rs a0361269ea95d7c4a6101622364f3c3ff160ddb4 (2026-09-27).** Source: the CHANGELOG and the history of `crates/meta-whatsapp-rs/src/inbox.rs`.

On a pin older than a fix below, the inbox behaves as struck through.

~~`update_status` matched on the message id alone~~: until 4b47bf7. ~~A `wa_id` conversation replied
without `+`~~: until 2b2679a (on an older pin, `send` with `Recipient::phone` and the `+`). ~~Echoes and
history are not recorded~~: until a3582b8. ~~Synced history opens the window, is unread, keeps its
placeholders~~: until 6d50701. ~~A revoke deletes any message of its number; one before its message is
lost~~: until a9593f3 (all 2026-09-24). ~~A tombstone moves the summary; a revoked placeholder is
filled~~: until af5b1f8 (2026-09-25). ~~U+0000 becomes U+FFFD~~: until PR #7 (2026-09-25). ~~Calls,
standby, handovers and identity links not recorded~~: until roadmap L7. 4b47bf7, 6d50701, a9593f3,
af5b1f8 and PR #7 change the `ConversationStore` contract.
