//! [`ConversationStore`] on Redis (feature `redis`, roadmap L8).
//!
//! # Layout
//!
//! Every key starts with `{prefix}{conversations}` (`B` below; `wa:` is the
//! default prefix, the key/value store's): `{conversations}` is a hash tag,
//! so all of a store's keys live in one slot and every script may touch
//! any of them. Variable parts are length-prefixed where another follows
//! (`<len>:<value>`, `lp()` below), so no two records spell one key.
//! Times are 16 hex digits, the unix microseconds with the sign bit
//! flipped (`ts16`): their byte order is their time order, so a sorted set
//! of `ts16 + id` members, all of score 0, pages by `(time, id)` with
//! `ZRANGEBYLEX`, in the byte order of the id.
//!
//! | Key | Type | Holds |
//! | --- | --- | --- |
//! | `B:numbers` | set | the business numbers with records (for purges of every number) |
//! | `B:id:<id>` | string | the number message `id` is stored under: today's rule, an id once per store (`OPEN_QUESTIONS.md` #33) |
//! | `B:m:lp(pn)<id>` | hash | a message: `c` contact, `d` direction, `k` kind, `t` text, `p` payload (JSON), `s` status, `ts`, `sa` status time, `e` error (JSON), `snd` sender |
//! | `B:h:lp(pn)<contact>` | sorted set | a conversation's history, `ts16 + id` |
//! | `B:mt:<pn>` | sorted set | a number's messages, `ts16 + id` (purge by age) |
//! | `B:s:lp(pn)<contact>` | hash | a summary: `at`, `id` (the latest message), `t` preview, `in` last inbound, `u` unread |
//! | `B:l:<pn>` | sorted set | a number's conversations, `ts16(at) + contact` |
//! | `B:snd:lp(pn)<sender>` | set | the ids of the messages a sender sent on a number (erasure) |
//! | `B:w:lp(pn)<id>` | hash | a window event: `c`, `k` kind, `at` |
//! | `B:wi:lp(pn)<contact>`, `B:wt:<pn>` | sorted sets | window events by conversation, by number: `ts16 + id` |
//! | `B:o:lp(pn)<contact>` | hash | thread ownership: `o` owner, `r` role, `a` app, `s` since |
//! | `B:ot:<pn>` | sorted set | ownership records, `ts16(since) + contact` |
//! | `B:p:lp(pn)<contact>` | hash | a synced contact: `fn`, `fi`, `ph`, `u`, `pu`, `un`, `at`; a kept removal has `at` and `rm` only |
//! | `B:pl:<pn>` | sorted set | a number's synced contacts (score 0, by contact) |
//! | `B:pr:<pn>` | sorted set | a number's kept removals, `ts16 + contact` |
//! | `B:pi:lp(pn)<id>` | set | the contacts whose BSUID, parent BSUID or phone number is `id` |
//! | `B:k:lp(pn)<id>` | set | the identity links naming `id`: `lp(previous) + current` |
//! | `B:ka:<pn>` | hash | each link's time, by `lp(previous) + current` |
//!
//! # Atomicity
//!
//! Every write is one Lua script, run atomically by Redis: a message, its
//! indexes and its conversation's summary change together, and so does
//! everything an erasure or a purge of one number deletes. A status update
//! (and a revoke of a stored message) is a compare-and-swap: read the
//! status, decide with [`DeliveryStatus::supersedes`] in Rust (the rule
//! lives in `meta-whatsapp-core`), then a script that applies it only if the
//! status is still the one read. A purge of every number runs one script
//! per number. Lua compares strings with the C library's `strcoll`, which
//! follows the server's locale: the scripts never order strings with `<`,
//! only byte by byte (`before`).
//!
//! What an operator needs to know (eviction, Cluster, the scripts' cost) is
//! on [`RedisConversationStore`]'s own docs.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{AppId, MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    ErasureMode, IdentityLink, Purged, Retention, StoredContact, StoredMessage, ThreadOwnership,
    WindowEvent, WindowEventKind,
};
use redis::Script;
use redis::aio::{ConnectionLike, ConnectionManager};
use time::OffsetDateTime;

use crate::store::codec::{
    direction_str, json_text, owner_str, parse_direction, parse_json, parse_owner, parse_status,
    status_str, utf8,
};

/// More compare-and-swap rounds than there are status ranks: reaching it
/// means something outside this adapter keeps rewriting the record.
const MAX_STATUS_ROUNDS: usize = 16;

/// How many messages of an `append_synced` batch one script stores: a
/// history chunk of thousands is a few scripts, none blocking Redis long.
const SYNCED_PER_SCRIPT: usize = 256;

/// The fields each message takes in an append script's arguments.
const MESSAGE_ARGS: usize = 13;

/// The key builders and helpers every script starts with. `ARGV[1]` is
/// always the base (`{prefix}{conversations}`), `KEYS[1]` the store's
/// `B:numbers` key, so that Redis (and a Cluster) routes the script to the
/// slot of the keys it builds from the base.
const PRELUDE: &str = r"
local B = ARGV[1]
local function lp(s) return #s .. ':' .. s end
local k_numbers = B .. ':numbers'
local function k_id(id) return B .. ':id:' .. id end
local function k_m(pn, id) return B .. ':m:' .. lp(pn) .. id end
local function k_h(pn, c) return B .. ':h:' .. lp(pn) .. c end
local function k_mt(pn) return B .. ':mt:' .. pn end
local function k_s(pn, c) return B .. ':s:' .. lp(pn) .. c end
local function k_l(pn) return B .. ':l:' .. pn end
local function k_snd(pn, s) return B .. ':snd:' .. lp(pn) .. s end
local function k_w(pn, id) return B .. ':w:' .. lp(pn) .. id end
local function k_wi(pn, c) return B .. ':wi:' .. lp(pn) .. c end
local function k_wt(pn) return B .. ':wt:' .. pn end
local function k_o(pn, c) return B .. ':o:' .. lp(pn) .. c end
local function k_ot(pn) return B .. ':ot:' .. pn end
local function k_p(pn, c) return B .. ':p:' .. lp(pn) .. c end
local function k_pl(pn) return B .. ':pl:' .. pn end
local function k_pr(pn) return B .. ':pr:' .. pn end
local function k_pi(pn, id) return B .. ':pi:' .. lp(pn) .. id end
local function k_k(pn, id) return B .. ':k:' .. lp(pn) .. id end
local function k_ka(pn) return B .. ':ka:' .. pn end

-- Whether `a` sorts before `b` in byte order (never the locale's).
local function before(a, b)
  if a == b then return false end
  local n = math.min(#a, #b)
  for i = 1, n do
    local x, y = string.byte(a, i), string.byte(b, i)
    if x ~= y then return x < y end
  end
  return #a < #b
end

-- A link member, `lp(previous) .. current`, back to its two sides.
local function split(m)
  local colon = string.find(m, ':', 1, true)
  local len = tonumber(string.sub(m, 1, colon - 1))
  return string.sub(m, colon + 1, colon + len), string.sub(m, colon + len + 1)
end

-- Store a message row and its indexes, unless its id is stored (on any
-- number: today's rule). `t`, `sa`, `e` and `snd` are false when absent.
local function insert_row(pn, id, c, d, k, t, p, s, ts, sa, e, snd)
  if redis.call('EXISTS', k_id(id)) == 1 then return false end
  redis.call('SET', k_id(id), pn)
  redis.call('SADD', k_numbers, pn)
  local mk = k_m(pn, id)
  redis.call('HSET', mk, 'c', c, 'd', d, 'k', k, 'p', p, 's', s, 'ts', ts)
  if t then redis.call('HSET', mk, 't', t) end
  if sa then redis.call('HSET', mk, 'sa', sa) end
  if e then redis.call('HSET', mk, 'e', e) end
  if snd then
    redis.call('HSET', mk, 'snd', snd)
    redis.call('SADD', k_snd(pn, snd), id)
  end
  redis.call('ZADD', k_h(pn, c), 0, ts .. id)
  redis.call('ZADD', k_mt(pn), 0, ts .. id)
  return true
end

-- Delete message `id` of number `pn` and its indexes. Returns its
-- conversation's contact, or false when it was not stored.
local function drop_message(pn, id)
  local mk = k_m(pn, id)
  local f = redis.call('HMGET', mk, 'c', 'ts', 'snd')
  if not f[1] then return false end
  redis.call('DEL', mk)
  if redis.call('GET', k_id(id)) == pn then redis.call('DEL', k_id(id)) end
  redis.call('ZREM', k_h(pn, f[1]), f[2] .. id)
  redis.call('ZREM', k_mt(pn), f[2] .. id)
  if f[3] then redis.call('SREM', k_snd(pn, f[3]), id) end
  return f[1]
end

-- Fold a stored message into its conversation's summary: the latest by
-- (ts, id); `opens` (a live inbound message) moves the window and counts
-- as unread. `t` is false when the message has no text.
local function summarize(pn, c, ts, id, t, opens)
  local sk = k_s(pn, c)
  local cur = redis.call('HMGET', sk, 'at', 'id', 'in')
  if not cur[1] then
    redis.call('HSET', sk, 'at', ts, 'id', id, 'u', opens and '1' or '0')
    if t then redis.call('HSET', sk, 't', t) end
    if opens then redis.call('HSET', sk, 'in', ts) end
    redis.call('ZADD', k_l(pn), 0, ts .. c)
    return
  end
  if before(cur[1] .. cur[2], ts .. id) then
    redis.call('ZREM', k_l(pn), cur[1] .. c)
    redis.call('ZADD', k_l(pn), 0, ts .. c)
    redis.call('HSET', sk, 'at', ts, 'id', id)
    if t then redis.call('HSET', sk, 't', t) else redis.call('HDEL', sk, 't') end
  end
  if opens then
    if (not cur[3]) or before(cur[3], ts) then redis.call('HSET', sk, 'in', ts) end
    redis.call('HINCRBY', sk, 'u', 1)
  end
end

-- Make conversation `c`'s summary follow its latest remaining message that
-- is not a tombstone (`revoked`), or drop it when none is left. Its window
-- and unread count stay.
local function resummarize(pn, c, revoked)
  local sk = k_s(pn, c)
  local old = redis.call('HGET', sk, 'at')
  if not old then return end
  local hk = k_h(pn, c)
  local start = 0
  while true do
    local members = redis.call('ZREVRANGE', hk, start, start + 31)
    if #members == 0 then break end
    for _, member in ipairs(members) do
      local id = string.sub(member, 17)
      local f = redis.call('HMGET', k_m(pn, id), 'k', 't')
      if f[1] and f[1] ~= revoked then
        local ts = string.sub(member, 1, 16)
        redis.call('ZREM', k_l(pn), old .. c)
        redis.call('ZADD', k_l(pn), 0, ts .. c)
        redis.call('HSET', sk, 'at', ts, 'id', id)
        if f[2] then redis.call('HSET', sk, 't', f[2]) else redis.call('HDEL', sk, 't') end
        return
      end
    end
    start = start + 32
  end
  redis.call('ZREM', k_l(pn), old .. c)
  redis.call('DEL', sk)
end

-- Delete synced contact `c` (or its kept removal) and its indexes.
-- Returns whether there was one.
local function drop_contact(pn, c)
  local pk = k_p(pn, c)
  local f = redis.call('HMGET', pk, 'at', 'rm', 'u', 'pu', 'ph')
  if not f[1] then return false end
  if f[2] then
    redis.call('ZREM', k_pr(pn), f[1] .. c)
  else
    redis.call('ZREM', k_pl(pn), c)
    for i = 3, 5 do
      if f[i] and f[i] ~= '' then redis.call('SREM', k_pi(pn, f[i]), c) end
    end
  end
  redis.call('DEL', pk)
  return true
end
";

/// A script: the prelude, then `body`.
fn script(body: &str) -> Script {
    Script::new(&format!("{PRELUDE}\n{body}"))
}

/// `append` / `append_synced`. `ARGV[2]`: `1` for live messages, `ARGV[3]`
/// the count, then [`MESSAGE_ARGS`] per message.
static APPEND: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local live = ARGV[2] == '1'
local n = tonumber(ARGV[3])
local out = {}
local i = 4
for j = 1, n do
  local pn, id, c, d, k = ARGV[i], ARGV[i + 1], ARGV[i + 2], ARGV[i + 3], ARGV[i + 4]
  local t = ARGV[i + 5] == '1' and ARGV[i + 6] or false
  local p, s, ts = ARGV[i + 7], ARGV[i + 8], ARGV[i + 9]
  local sa = ARGV[i + 10] ~= '' and ARGV[i + 10] or false
  local e = ARGV[i + 11] ~= '' and ARGV[i + 11] or false
  local snd = ARGV[i + 12] ~= '' and ARGV[i + 12] or false
  i = i + 13
  local stored = insert_row(pn, id, c, d, k, t, p, s, ts, sa, e, snd)
  if stored then summarize(pn, c, ts, id, t, live and d == 'inbound') end
  out[j] = stored and 1 or 0
end
return out
",
    )
});

/// A revoke's tombstone, history only, when its id is free.
static TOMBSTONE: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, id, c, d, k, at, s = ARGV[2], ARGV[3], ARGV[4], ARGV[5], ARGV[6], ARGV[7], ARGV[8]
if insert_row(pn, id, c, d, k, false, '{}', s, at, at, false, false) then return 1 end
return 0
",
    )
});

/// The status compare-and-swap: `ARGV[4]` the status read, `ARGV[5]` the
/// new one, `ARGV[6]` its time, `ARGV[7]` the error (empty: keep the
/// stored one), `ARGV[8]` the direction it is restricted to (empty: any).
static STATUS_SWAP: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local mk = k_m(ARGV[2], ARGV[3])
local f = redis.call('HMGET', mk, 's', 'd')
if f[1] ~= ARGV[4] then return 0 end
if ARGV[8] ~= '' and f[2] ~= ARGV[8] then return 0 end
redis.call('HSET', mk, 's', ARGV[5], 'sa', ARGV[6])
if ARGV[7] ~= '' then redis.call('HSET', mk, 'e', ARGV[7]) end
return 1
",
    )
});

/// `fill_media_placeholder`: `ARGV[4]` kind, `ARGV[5]` 1 when a text
/// follows in `ARGV[6]`, `ARGV[7]` payload, `ARGV[8]` the placeholder kind,
/// `ARGV[9]` the deleted status.
static FILL: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, id = ARGV[2], ARGV[3]
local mk = k_m(pn, id)
local f = redis.call('HMGET', mk, 'k', 's', 'c')
if f[1] ~= ARGV[8] or f[2] == ARGV[9] then return 0 end
redis.call('HSET', mk, 'k', ARGV[4], 'p', ARGV[7])
local has_text = ARGV[5] == '1'
if has_text then redis.call('HSET', mk, 't', ARGV[6]) else redis.call('HDEL', mk, 't') end
local sk = k_s(pn, f[3])
if redis.call('HGET', sk, 'id') == id then
  if has_text then redis.call('HSET', sk, 't', ARGV[6]) else redis.call('HDEL', sk, 't') end
end
return 1
",
    )
});

static MARK_READ: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local sk = k_s(ARGV[2], ARGV[3])
if redis.call('EXISTS', sk) == 1 then redis.call('HSET', sk, 'u', '0') end
return 1
",
    )
});

static WINDOW_EVENT: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, id, c, k, at = ARGV[2], ARGV[3], ARGV[4], ARGV[5], ARGV[6]
local wk = k_w(pn, id)
if redis.call('EXISTS', wk) == 1 then return 0 end
redis.call('HSET', wk, 'c', c, 'k', k, 'at', at)
redis.call('ZADD', k_wi(pn, c), 0, at .. id)
redis.call('ZADD', k_wt(pn), 0, at .. id)
redis.call('SADD', k_numbers, pn)
return 1
",
    )
});

/// `set_thread_owner`: `ARGV[4]` owner, `ARGV[5]`/`[6]` role (flag,
/// value), `ARGV[7]`/`[8]` app, `ARGV[9]` since. Stored unless the stored
/// record is later.
static SET_OWNER: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, c, since = ARGV[2], ARGV[3], ARGV[9]
local ok = k_o(pn, c)
local cur = redis.call('HGET', ok, 's')
if cur and before(since, cur) then return 0 end
if cur then redis.call('ZREM', k_ot(pn), cur .. c) end
redis.call('DEL', ok)
redis.call('HSET', ok, 'o', ARGV[4], 's', since)
if ARGV[5] == '1' then redis.call('HSET', ok, 'r', ARGV[6]) end
if ARGV[7] == '1' then redis.call('HSET', ok, 'a', ARGV[8]) end
redis.call('ZADD', k_ot(pn), 0, since .. c)
redis.call('SADD', k_numbers, pn)
return 1
",
    )
});

/// `put_contact`: `ARGV[4]` the sync's time, then a (flag, value) pair per
/// field: `fn`, `fi`, `ph`, `u`, `pu`, `un`. Stored unless the stored
/// record (a contact or a kept removal) is later.
static PUT_CONTACT: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, c, at = ARGV[2], ARGV[3], ARGV[4]
local pk = k_p(pn, c)
local cur = redis.call('HGET', pk, 'at')
if cur and before(at, cur) then return 0 end
drop_contact(pn, c)
redis.call('HSET', pk, 'at', at)
local names = {'fn', 'fi', 'ph', 'u', 'pu', 'un'}
for j = 1, 6 do
  if ARGV[3 + 2 * j] == '1' then
    local v = ARGV[4 + 2 * j]
    redis.call('HSET', pk, names[j], v)
    if (j == 3 or j == 4 or j == 5) and v ~= '' then redis.call('SADD', k_pi(pn, v), c) end
  end
end
redis.call('ZADD', k_pl(pn), 0, c)
redis.call('SADD', k_numbers, pn)
return 1
",
    )
});

/// `remove_contact`: the removal's key and time replace the record, unless
/// it is later. Returns 1 when a contact (not a removal) was there.
static REMOVE_CONTACT: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, c, at = ARGV[2], ARGV[3], ARGV[4]
local pk = k_p(pn, c)
local f = redis.call('HMGET', pk, 'at', 'rm')
if f[1] and before(at, f[1]) then return 0 end
local removed = (f[1] and not f[2]) and 1 or 0
drop_contact(pn, c)
redis.call('HSET', pk, 'at', at, 'rm', '1')
redis.call('ZADD', k_pr(pn), 0, at .. c)
redis.call('SADD', k_numbers, pn)
return removed
",
    )
});

static LINK: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, prev, cur, at = ARGV[2], ARGV[3], ARGV[4], ARGV[5]
local member = lp(prev) .. cur
if redis.call('HSETNX', k_ka(pn), member, at) == 0 then return 0 end
redis.call('SADD', k_k(pn, prev), member)
redis.call('SADD', k_k(pn, cur), member)
redis.call('SADD', k_numbers, pn)
return 1
",
    )
});

/// The links naming `ARGV[3]`: `member, at` pairs.
static LINKS: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn = ARGV[2]
local out = {}
for _, member in ipairs(redis.call('SMEMBERS', k_k(pn, ARGV[3]))) do
  local at = redis.call('HGET', k_ka(pn), member)
  if at then
    out[#out + 1] = member
    out[#out + 1] = at
  end
end
return out
",
    )
});

/// The closure from `ARGV[3]` over the synced contacts (not kept
/// removals: their four ids) and the links; an empty id is neither
/// followed nor found (unless it is the start).
static IDENTITIES: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, start = ARGV[2], ARGV[3]
local found = {[start] = true}
local list = {start}
local todo = {start}
local function add(v)
  if v and v ~= '' and not found[v] then
    found[v] = true
    list[#list + 1] = v
    todo[#todo + 1] = v
  end
end
local function from_contact(c)
  local f = redis.call('HMGET', k_p(pn, c), 'at', 'rm', 'u', 'pu', 'ph')
  if f[1] and not f[2] then
    add(c)
    add(f[3])
    add(f[4])
    add(f[5])
  end
end
while #todo > 0 do
  local id = table.remove(todo)
  if id ~= '' then
    from_contact(id)
    for _, c in ipairs(redis.call('SMEMBERS', k_pi(pn, id))) do from_contact(c) end
    for _, m in ipairs(redis.call('SMEMBERS', k_k(pn, id))) do
      local prev, cur = split(m)
      add(prev)
      add(cur)
    end
  end
end
return list
",
    )
});

/// `erase_all` on one number: `ARGV[3]` `delete` or `redact`, `ARGV[4]`
/// the erased kind, `ARGV[5]` the tombstone kind, `ARGV[6]` the count,
/// then the ids. Returns the seven counts of `Erased`, in field order.
static ERASE: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, mode, erased_kind, revoked = ARGV[2], ARGV[3], ARGV[4], ARGV[5]
local ids, named = {}, {}
for j = 1, tonumber(ARGV[6]) do
  local id = ARGV[6 + j]
  ids[#ids + 1] = id
  if id ~= '' then named[#named + 1] = id end
end
local counts = {0, 0, 0, 0, 0, 0, 0}
for _, id in ipairs(ids) do
  local hk = k_h(pn, id)
  for _, member in ipairs(redis.call('ZRANGE', hk, 0, -1)) do
    if drop_message(pn, string.sub(member, 17)) then counts[1] = counts[1] + 1 end
  end
  redis.call('DEL', hk)
  local sk = k_s(pn, id)
  local at = redis.call('HGET', sk, 'at')
  if at then
    redis.call('ZREM', k_l(pn), at .. id)
    redis.call('DEL', sk)
    counts[2] = counts[2] + 1
  end
  local wik = k_wi(pn, id)
  for _, member in ipairs(redis.call('ZRANGE', wik, 0, -1)) do
    if redis.call('DEL', k_w(pn, string.sub(member, 17))) == 1 then counts[3] = counts[3] + 1 end
    redis.call('ZREM', k_wt(pn), member)
  end
  redis.call('DEL', wik)
  local ok = k_o(pn, id)
  local since = redis.call('HGET', ok, 's')
  if since then
    redis.call('ZREM', k_ot(pn), since .. id)
    redis.call('DEL', ok)
    counts[4] = counts[4] + 1
  end
  if drop_contact(pn, id) then counts[5] = counts[5] + 1 end
end
-- An empty id names no one: never a contact's other ids, a link's sides
-- or a sender.
for _, id in ipairs(named) do
  for _, c in ipairs(redis.call('SMEMBERS', k_pi(pn, id))) do
    if drop_contact(pn, c) then counts[5] = counts[5] + 1 end
  end
  local kk = k_k(pn, id)
  for _, member in ipairs(redis.call('SMEMBERS', kk)) do
    if redis.call('HDEL', k_ka(pn), member) == 1 then counts[6] = counts[6] + 1 end
    local prev, cur = split(member)
    redis.call('SREM', k_k(pn, prev), member)
    redis.call('SREM', k_k(pn, cur), member)
  end
  redis.call('DEL', kk)
end
-- Their messages in conversations keyed by someone else (a group's):
-- their own conversations went above.
local touched, touched_list = {}, {}
for _, id in ipairs(named) do
  local sndk = k_snd(pn, id)
  for _, mid in ipairs(redis.call('SMEMBERS', sndk)) do
    local mk = k_m(pn, mid)
    local c = redis.call('HGET', mk, 'c')
    if c then
      local sk = k_s(pn, c)
      local latest = redis.call('HGET', sk, 'id') == mid
      if mode == 'delete' then
        drop_message(pn, mid)
        if latest and not touched[c] then
          touched[c] = true
          touched_list[#touched_list + 1] = c
        end
      else
        redis.call('HSET', mk, 'k', erased_kind, 'p', '{}')
        redis.call('HDEL', mk, 't', 'e', 'snd')
        if latest then redis.call('HDEL', sk, 't') end
      end
      counts[7] = counts[7] + 1
    end
  end
  redis.call('DEL', sndk)
end
for _, c in ipairs(touched_list) do resummarize(pn, c, revoked) end
return counts
",
    )
});

/// `purge_before` on one number: `ARGV[3]` the cutoff (`ts16`). Returns
/// the five counts of `Purged`, in field order.
static PURGE: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, cut = ARGV[2], '(' .. ARGV[3]
local counts = {0, 0, 0, 0, 0}
for _, member in ipairs(redis.call('ZRANGEBYLEX', k_mt(pn), '-', cut)) do
  if drop_message(pn, string.sub(member, 17)) then counts[1] = counts[1] + 1 end
end
redis.call('ZREMRANGEBYLEX', k_mt(pn), '-', cut)
for _, member in ipairs(redis.call('ZRANGEBYLEX', k_l(pn), '-', cut)) do
  if redis.call('DEL', k_s(pn, string.sub(member, 17))) == 1 then counts[2] = counts[2] + 1 end
end
redis.call('ZREMRANGEBYLEX', k_l(pn), '-', cut)
for _, member in ipairs(redis.call('ZRANGEBYLEX', k_wt(pn), '-', cut)) do
  local wk = k_w(pn, string.sub(member, 17))
  local c = redis.call('HGET', wk, 'c')
  if c then
    redis.call('DEL', wk)
    redis.call('ZREM', k_wi(pn, c), member)
    counts[3] = counts[3] + 1
  end
end
redis.call('ZREMRANGEBYLEX', k_wt(pn), '-', cut)
for _, member in ipairs(redis.call('ZRANGEBYLEX', k_ot(pn), '-', cut)) do
  if redis.call('DEL', k_o(pn, string.sub(member, 17))) == 1 then counts[4] = counts[4] + 1 end
end
redis.call('ZREMRANGEBYLEX', k_ot(pn), '-', cut)
for _, member in ipairs(redis.call('ZRANGEBYLEX', k_pr(pn), '-', cut)) do
  if redis.call('DEL', k_p(pn, string.sub(member, 17))) == 1 then counts[5] = counts[5] + 1 end
end
redis.call('ZREMRANGEBYLEX', k_pr(pn), '-', cut)
return counts
",
    )
});

/// A page of a sorted set, each member's record with it: `ARGV[3]` the
/// sorted set's kind (`h`, `l`, `wi`, `pl`), `ARGV[4]` its contact (unused
/// for `l` and `pl`), `ARGV[5]`/`[6]` the bounds, `ARGV[7]` the limit,
/// `ARGV[8]` `rev` for newest first. Each row is the member's id (or
/// contact) then its record's fields and values; a member whose record is
/// gone is skipped.
static PAGE: LazyLock<Script> = LazyLock::new(|| {
    script(
        r"
local pn, kind, c = ARGV[2], ARGV[3], ARGV[4]
local index, record, offset
if kind == 'h' then index, record, offset = k_h(pn, c), k_m, 17
elseif kind == 'l' then index, record, offset = k_l(pn), k_s, 17
elseif kind == 'wi' then index, record, offset = k_wi(pn, c), k_w, 17
else index, record, offset = k_pl(pn), k_p, 1 end
local members
if ARGV[8] == 'rev' then
  members = redis.call('ZREVRANGEBYLEX', index, ARGV[5], ARGV[6], 'LIMIT', 0, ARGV[7])
else
  members = redis.call('ZRANGEBYLEX', index, ARGV[5], ARGV[6], 'LIMIT', 0, ARGV[7])
end
local out = {}
for _, member in ipairs(members) do
  local id = string.sub(member, offset)
  local row = redis.call('HGETALL', record(pn, id))
  if #row > 0 then
    table.insert(row, 1, id)
    out[#out + 1] = row
  end
end
return out
",
    )
});

/// `ConversationStore` over a Redis connection. Cheap to clone.
///
/// `C` is any async redis connection that can be cloned and shared, as for
/// [`RedisKvStore`](super::RedisKvStore): [`ConnectionManager`] (the
/// default), `redis::aio::MultiplexedConnection`, or, for `rediss://`,
/// [`RedisTlsConnection`](super::RedisTlsConnection).
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use meta_whatsapp_adapters::store::RedisConversationStore;
///
/// let client = redis::Client::open("redis://127.0.0.1:6379")?;
/// let inbox = RedisConversationStore::new(client.get_connection_manager().await?);
/// # Ok(()) }
/// ```
///
/// Unread counts are by **arrival**, like the other stores; times are kept
/// in microseconds. Retention is kept by default
/// ([`with_retention`](Self::with_retention)); an erasure redacts an erased
/// person's group messages by default
/// ([`with_erasure_mode`](Self::with_erasure_mode) deletes them).
///
/// # Operating Redis for it
///
/// - **Persistence.** The history is data, not a cache: run Redis with AOF
///   (`appendonly yes`) or snapshots you can afford to lose the interval
///   of, and back it up. Nothing here expires.
/// - **Eviction policy: `noeviction`**, as for the key/value store: under
///   any other policy Redis may silently drop a message, a summary, or the
///   index that keeps an id once per store.
/// - **Cluster.** All of a store's keys share the hash tag
///   `{conversations}`: one slot, so one shard holds a store's whole inbox
///   (the scripts need it, an erasure touching the history, the indexes
///   and the summaries at once). Spread inboxes over shards with one store
///   per prefix ([`with_prefix`](Self::with_prefix)); keep an empty `{}`
///   out of the prefix. The scripts build their key names from the base
///   they are given (as job queues on Redis commonly do): a proxy that
///   refuses undeclared keys refuses them.
/// - **Cost.** Each call is one script (a purge of every number, one per
///   number). An erasure or a purge reads and deletes in one script what it
///   reaches, and Redis serves nothing else meanwhile: schedule
///   [`apply_retention`](ConversationStore::apply_retention) for quiet
///   hours on a large inbox.
#[derive(Clone)]
pub struct RedisConversationStore<C = ConnectionManager> {
    conn: C,
    prefix: Arc<str>,
    retention: Retention,
    erasure_mode: ErasureMode,
}

impl<C> fmt::Debug for RedisConversationStore<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // No connection details: they can carry credentials.
        f.debug_struct("RedisConversationStore")
            .field("prefix", &self.prefix)
            .field("retention", &self.retention)
            .field("erasure_mode", &self.erasure_mode)
            .finish_non_exhaustive()
    }
}

/// An instant as stored: 16 hex digits of its unix microseconds (floored)
/// with the sign bit flipped, so that byte order is time order.
fn ts16(t: OffsetDateTime) -> String {
    let us = t.unix_timestamp_nanos().div_euclid(1_000);
    // `OffsetDateTime` spans years ±9999, well inside `i64` microseconds.
    let us = i64::try_from(us).unwrap_or(if us < 0 { i64::MIN } else { i64::MAX });
    format!("{:016x}", us.cast_unsigned() ^ (1 << 63))
}

/// A stored instant back. `what` names the field, never the record.
fn from_ts16(s: &[u8], what: &str) -> Result<OffsetDateTime, StorageError> {
    let corrupt = || StorageError::Corrupt {
        key: what.to_owned(),
        source: serde::de::Error::custom("not a stored time"),
    };
    let text = std::str::from_utf8(s).map_err(|_| corrupt())?;
    if text.len() != 16 {
        return Err(corrupt());
    }
    let bits = u64::from_str_radix(text, 16).map_err(|_| corrupt())?;
    let us = (bits ^ (1 << 63)).cast_signed();
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(us) * 1_000).map_err(|_| corrupt())
}

fn backend(error: redis::RedisError) -> StorageError {
    StorageError::Backend(anyhow::Error::new(error))
}

/// A record's fields, from the flat `field, value` list of `HGETALL`.
struct Record(HashMap<Vec<u8>, Vec<u8>>);

impl Record {
    fn new(flat: Vec<Vec<u8>>) -> Self {
        let mut fields = HashMap::new();
        let mut it = flat.into_iter();
        while let (Some(field), Some(value)) = (it.next(), it.next()) {
            fields.insert(field, value);
        }
        Self(fields)
    }

    fn take(&mut self, field: &str) -> Option<Vec<u8>> {
        self.0.remove(field.as_bytes())
    }

    fn text(&mut self, field: &str, what: &str) -> Result<Option<String>, StorageError> {
        self.take(field).map(|v| utf8(what, v)).transpose()
    }

    fn required(&mut self, field: &str, what: &str) -> Result<String, StorageError> {
        self.text(field, what)?
            .ok_or_else(|| StorageError::Corrupt {
                key: what.to_owned(),
                source: serde::de::Error::custom("missing"),
            })
    }

    fn time(&mut self, field: &str, what: &str) -> Result<Option<OffsetDateTime>, StorageError> {
        self.take(field).map(|v| from_ts16(&v, what)).transpose()
    }

    fn required_time(&mut self, field: &str, what: &str) -> Result<OffsetDateTime, StorageError> {
        self.time(field, what)?
            .ok_or_else(|| StorageError::Corrupt {
                key: what.to_owned(),
                source: serde::de::Error::custom("missing"),
            })
    }
}

fn message_from_record(
    phone_number_id: &PhoneNumberId,
    id: String,
    mut r: Record,
) -> Result<StoredMessage, StorageError> {
    let contact = r.required("c", "message contact")?;
    let direction = r.required("d", "message direction")?;
    let payload = r.take("p").ok_or_else(|| StorageError::Corrupt {
        key: "message payload".to_owned(),
        source: serde::de::Error::custom("missing"),
    })?;
    let error = r.take("e");
    Ok(StoredMessage {
        conversation: ConversationKey::new(phone_number_id.clone(), contact),
        direction: parse_direction(&direction)?,
        kind: r.required("k", "message kind")?,
        text: r.text("t", "message text")?,
        payload: parse_json(&payload, "message payload")?,
        status: parse_status(r.required("s", "message status")?)?,
        timestamp: r.required_time("ts", "message time")?,
        status_at: r.time("sa", "message status time")?,
        error: error.map(|e| parse_json(&e, "message error")).transpose()?,
        id: MessageId::new(id),
    })
}

fn summary_from_record(
    phone_number_id: &PhoneNumberId,
    contact: String,
    mut r: Record,
) -> Result<ConversationSummary, StorageError> {
    let unread = r.required("u", "unread count")?;
    Ok(ConversationSummary {
        key: ConversationKey::new(phone_number_id.clone(), contact),
        last_message_at: r.required_time("at", "last message time")?,
        last_inbound_at: r.time("in", "last inbound time")?,
        last_text: r.text("t", "preview")?,
        // Never negative: it only grows by 1 and resets to 0.
        unread: unread.parse().unwrap_or(0),
    })
}

fn window_event_from_record(
    phone_number_id: &PhoneNumberId,
    id: String,
    mut r: Record,
) -> Result<WindowEvent, StorageError> {
    let contact = r.required("c", "window event contact")?;
    let kind = r.required("k", "window event kind")?;
    Ok(WindowEvent {
        conversation: ConversationKey::new(phone_number_id.clone(), contact),
        kind: WindowEventKind::from_name(&kind),
        id,
        at: r.required_time("at", "window event time")?,
    })
}

fn contact_from_record(
    phone_number_id: &PhoneNumberId,
    contact: String,
    mut r: Record,
) -> Result<Option<StoredContact>, StorageError> {
    if r.take("rm").is_some() {
        return Ok(None);
    }
    Ok(Some(StoredContact {
        key: ConversationKey::new(phone_number_id.clone(), contact),
        full_name: r.text("fn", "contact full name")?,
        first_name: r.text("fi", "contact first name")?,
        phone_number: r.text("ph", "contact phone number")?,
        user_id: r.text("u", "contact user id")?.map(UserId::new),
        parent_user_id: r.text("pu", "contact parent user id")?.map(UserId::new),
        username: r.text("un", "contact username")?,
        synced_at: r.required_time("at", "contact sync time")?,
    }))
}

/// A link member, `lp(previous) + current`, back to its two sides.
fn split_link(member: &[u8]) -> Result<(String, String), StorageError> {
    let corrupt = || StorageError::Corrupt {
        key: "identity link".to_owned(),
        source: serde::de::Error::custom("not a stored link"),
    };
    let colon = member.iter().position(|b| *b == b':').ok_or_else(corrupt)?;
    let len: usize = std::str::from_utf8(&member[..colon])
        .ok()
        .and_then(|n| n.parse().ok())
        .ok_or_else(corrupt)?;
    let rest = &member[colon + 1..];
    if rest.len() < len {
        return Err(corrupt());
    }
    let (previous, current) = rest.split_at(len);
    Ok((
        utf8("identity link", previous.to_vec())?,
        utf8("identity link", current.to_vec())?,
    ))
}

/// `(flag, value)` arguments of an optional string: `1` and the value, or
/// `0` and nothing.
fn optional(value: Option<&str>) -> (&'static str, &str) {
    match value {
        Some(v) => ("1", v),
        None => ("0", ""),
    }
}

impl<C> RedisConversationStore<C>
where
    C: ConnectionLike + Clone + Send + Sync + 'static,
{
    /// Store on `conn` with the default `wa:` key prefix (the key/value
    /// store's: their keys never meet, see the layout).
    pub fn new(conn: C) -> Self {
        Self {
            conn,
            prefix: Arc::from("wa:"),
            retention: Retention::Keep,
            erasure_mode: ErasureMode::Redact,
        }
    }

    /// Use `prefix` for every key instead of `wa:` (e.g. `tenant1:wa:`).
    /// Any string works, an empty one included; on Redis Cluster it must
    /// not contain `{}` (see [Operating Redis](Self#operating-redis-for-it)).
    #[must_use]
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Arc::from(prefix.into());
        self
    }

    /// This store applying `retention` in
    /// [`ConversationStore::apply_retention`] (the default keeps
    /// everything).
    #[must_use]
    pub fn with_retention(mut self, retention: Retention) -> Self {
        self.retention = retention;
        self
    }

    /// This store redacting or deleting an erased person's messages in
    /// other conversations (a group's) as `mode` says, in
    /// [`ConversationStore::erase_all`] (the default redacts:
    /// [`ErasureMode::Redact`]).
    #[must_use]
    pub fn with_erasure_mode(mut self, mode: ErasureMode) -> Self {
        self.erasure_mode = mode;
        self
    }

    /// The key prefix in use.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `{prefix}{conversations}`: the start of every key.
    fn base(&self) -> String {
        format!("{}{{conversations}}", self.prefix)
    }

    /// `B:<kind>:lp(pn)<rest>`.
    fn key(&self, kind: &str, pn: &PhoneNumberId, rest: &str) -> String {
        let pn = pn.as_str();
        format!("{}:{kind}:{}:{pn}{rest}", self.base(), pn.len())
    }

    /// Run `script` with the base and `args`.
    async fn run<T: redis::FromRedisValue>(
        &self,
        script: &Script,
        args: &[&[u8]],
    ) -> Result<T, StorageError> {
        let base = self.base();
        let mut invocation = script.prepare_invoke();
        invocation
            .key(format!("{base}:numbers"))
            .arg(base.as_bytes());
        for arg in args {
            invocation.arg(*arg);
        }
        let mut conn = self.conn.clone();
        invocation.invoke_async(&mut conn).await.map_err(backend)
    }

    async fn hgetall(&self, key: String) -> Result<Option<Record>, StorageError> {
        let mut conn = self.conn.clone();
        let flat: Vec<Vec<u8>> = redis::cmd("HGETALL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(backend)?;
        Ok((!flat.is_empty()).then(|| Record::new(flat)))
    }

    /// A page of `kind`'s sorted set (see [`PAGE`]): `(id, record)` rows.
    async fn page(
        &self,
        pn: &PhoneNumberId,
        kind: &str,
        contact: &str,
        bounds: (&[u8], &[u8]),
        limit: usize,
        newest_first: bool,
    ) -> Result<Vec<(String, Record)>, StorageError> {
        let limit = limit.to_string();
        let rows: Vec<Vec<Vec<u8>>> = self
            .run(
                &PAGE,
                &[
                    pn.as_str().as_bytes(),
                    kind.as_bytes(),
                    contact.as_bytes(),
                    bounds.0,
                    bounds.1,
                    limit.as_bytes(),
                    if newest_first { b"rev" } else { b"fwd" },
                ],
            )
            .await?;
        rows.into_iter()
            .filter_map(|mut row| {
                (!row.is_empty()).then(|| {
                    let id = utf8("record id", row.remove(0))?;
                    Ok((id, Record::new(row)))
                })
            })
            .collect()
    }

    /// The exclusive upper bound of a newest-first page: `+`, or `(` and
    /// the cursor's member.
    fn upper(before: Option<(OffsetDateTime, &str)>) -> Vec<u8> {
        match before {
            None => b"+".to_vec(),
            Some((at, id)) => format!("({}{id}", ts16(at)).into_bytes(),
        }
    }

    /// Append `messages` (live or synced) in scripts of at most
    /// [`SYNCED_PER_SCRIPT`]; whether each was stored.
    async fn append_messages(
        &self,
        messages: &[StoredMessage],
        live: bool,
    ) -> Result<Vec<bool>, StorageError> {
        let mut stored = Vec::with_capacity(messages.len());
        for chunk in messages.chunks(SYNCED_PER_SCRIPT) {
            let mut owned: Vec<Vec<u8>> = Vec::with_capacity(2 + chunk.len() * MESSAGE_ARGS);
            owned.push(if live { b"1".to_vec() } else { b"0".to_vec() });
            owned.push(chunk.len().to_string().into_bytes());
            for m in chunk {
                let (has_text, text) = optional(m.text.as_deref());
                owned.extend([
                    m.conversation.phone_number_id.as_str().as_bytes().to_vec(),
                    m.id.as_str().as_bytes().to_vec(),
                    m.conversation.contact.as_bytes().to_vec(),
                    direction_str(m.direction).as_bytes().to_vec(),
                    m.kind.as_bytes().to_vec(),
                    has_text.as_bytes().to_vec(),
                    text.as_bytes().to_vec(),
                    json_text(&m.payload, "message payload")?.into_bytes(),
                    status_str(m.status)?.into_bytes(),
                    ts16(m.timestamp).into_bytes(),
                    m.status_at.map(ts16).unwrap_or_default().into_bytes(),
                    m.error
                        .as_ref()
                        .map(|e| json_text(e, "message error"))
                        .transpose()?
                        .unwrap_or_default()
                        .into_bytes(),
                    m.sender().unwrap_or_default().as_bytes().to_vec(),
                ]);
            }
            let args: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
            let answers: Vec<i64> = self.run(&APPEND, &args).await?;
            stored.extend(answers.into_iter().map(|a| a == 1));
        }
        Ok(stored)
    }

    /// Move message `id` of `phone_number_id` (in `direction` only, when
    /// given) to `status` if that supersedes the stored one: read, decide in
    /// Rust, compare-and-swap.
    async fn set_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        direction: Option<Direction>,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<&serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let new = status_str(status)?;
        let error = error
            .map(|e| json_text(e, "message error"))
            .transpose()?
            .unwrap_or_default();
        let direction = direction.map(direction_str).unwrap_or_default();
        let at = ts16(at);
        let key = self.key("m", phone_number_id, id.as_str());
        for _ in 0..MAX_STATUS_ROUNDS {
            let mut conn = self.conn.clone();
            let (current, stored_direction): (Option<String>, Option<String>) = redis::cmd("HMGET")
                .arg(&key)
                .arg("s")
                .arg("d")
                .query_async(&mut conn)
                .await
                .map_err(backend)?;
            let Some(current) = current else {
                // Not stored for that number.
                return Ok(false);
            };
            if !direction.is_empty() && stored_direction.as_deref() != Some(direction) {
                return Ok(false);
            }
            if !status.supersedes(parse_status(current.clone())?) {
                return Ok(false);
            }
            let swapped: i64 = self
                .run(
                    &STATUS_SWAP,
                    &[
                        phone_number_id.as_str().as_bytes(),
                        id.as_str().as_bytes(),
                        current.as_bytes(),
                        new.as_bytes(),
                        at.as_bytes(),
                        error.as_bytes(),
                        direction.as_bytes(),
                    ],
                )
                .await?;
            if swapped == 1 {
                return Ok(true);
            }
            // Someone else moved the status between our read and our swap;
            // decide again against what they wrote.
        }
        Err(StorageError::Backend(anyhow::anyhow!(
            "a message status kept changing; gave up after {MAX_STATUS_ROUNDS} rounds"
        )))
    }

    /// The business numbers with records: what a purge of every number
    /// visits.
    async fn numbers(&self) -> Result<Vec<PhoneNumberId>, StorageError> {
        let mut conn = self.conn.clone();
        let numbers: Vec<String> = redis::cmd("SMEMBERS")
            .arg(format!("{}:numbers", self.base()))
            .query_async(&mut conn)
            .await
            .map_err(backend)?;
        Ok(numbers
            .into_iter()
            .map(PhoneNumberId::new_unchecked)
            .collect())
    }
}

#[async_trait]
impl<C> ConversationStore for RedisConversationStore<C>
where
    C: ConnectionLike + Clone + Send + Sync + 'static,
{
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        let stored = self
            .append_messages(std::slice::from_ref(&message), true)
            .await?;
        Ok(stored.first().copied().unwrap_or(false))
    }

    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>, StorageError> {
        self.append_messages(&messages, false).await
    }

    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool, StorageError> {
        let (has_text, text) = optional(text.as_deref());
        let payload = json_text(&payload, "message payload")?;
        let deleted = status_str(DeliveryStatus::Deleted)?;
        let filled: i64 = self
            .run(
                &FILL,
                &[
                    phone_number_id.as_str().as_bytes(),
                    id.as_str().as_bytes(),
                    kind.as_bytes(),
                    has_text.as_bytes(),
                    text.as_bytes(),
                    payload.as_bytes(),
                    StoredMessage::MEDIA_PLACEHOLDER.as_bytes(),
                    deleted.as_bytes(),
                ],
            )
            .await?;
        Ok(filled == 1)
    }

    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let deleted = status_str(DeliveryStatus::Deleted)?;
        let stamp = ts16(at);
        // History only: a tombstone is never part of the summary.
        let tombstone: i64 = self
            .run(
                &TOMBSTONE,
                &[
                    key.phone_number_id.as_str().as_bytes(),
                    id.as_str().as_bytes(),
                    key.contact.as_bytes(),
                    direction_str(direction).as_bytes(),
                    StoredMessage::REVOKED.as_bytes(),
                    stamp.as_bytes(),
                    deleted.as_bytes(),
                ],
            )
            .await?;
        if tombstone == 1 {
            return Ok(true);
        }
        self.set_status(
            &key.phone_number_id,
            id,
            Some(direction),
            DeliveryStatus::Deleted,
            at,
            None,
        )
        .await
    }

    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        self.set_status(phone_number_id, id, None, status, at, error.as_ref())
            .await
    }

    async fn messages(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let upper = Self::upper(before.as_ref().map(|(at, id)| (*at, id.as_str())));
        let rows = self
            .page(
                &key.phone_number_id,
                "h",
                &key.contact,
                (&upper, b"-"),
                limit,
                true,
            )
            .await?;
        rows.into_iter()
            .map(|(id, record)| message_from_record(&key.phone_number_id, id, record))
            .collect()
    }

    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let upper = Self::upper(before.as_ref().map(|(at, c)| (*at, c.as_str())));
        let rows = self
            .page(phone_number_id, "l", "", (&upper, b"-"), limit, true)
            .await?;
        rows.into_iter()
            .map(|(contact, record)| summary_from_record(phone_number_id, contact, record))
            .collect()
    }

    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError> {
        let _: i64 = self
            .run(
                &MARK_READ,
                &[
                    key.phone_number_id.as_str().as_bytes(),
                    key.contact.as_bytes(),
                ],
            )
            .await?;
        Ok(())
    }

    async fn last_inbound_at(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<OffsetDateTime>, StorageError> {
        let mut conn = self.conn.clone();
        let at: Option<Vec<u8>> = redis::cmd("HGET")
            .arg(self.key("s", &key.phone_number_id, &key.contact))
            .arg("in")
            .query_async(&mut conn)
            .await
            .map_err(backend)?;
        at.map(|at| from_ts16(&at, "last inbound time")).transpose()
    }

    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>, StorageError> {
        let Some(record) = self
            .hgetall(self.key("m", phone_number_id, id.as_str()))
            .await?
        else {
            return Ok(None);
        };
        message_from_record(phone_number_id, id.as_str().to_owned(), record).map(Some)
    }

    async fn record_window_event(&self, event: WindowEvent) -> Result<bool, StorageError> {
        let at = ts16(event.at);
        let recorded: i64 = self
            .run(
                &WINDOW_EVENT,
                &[
                    event.conversation.phone_number_id.as_str().as_bytes(),
                    event.id.as_bytes(),
                    event.conversation.contact.as_bytes(),
                    event.kind.as_str().as_bytes(),
                    at.as_bytes(),
                ],
            )
            .await?;
        Ok(recorded == 1)
    }

    async fn window_events(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<WindowEvent>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let upper = Self::upper(before.as_ref().map(|(at, id)| (*at, id.as_str())));
        let rows = self
            .page(
                &key.phone_number_id,
                "wi",
                &key.contact,
                (&upper, b"-"),
                limit,
                true,
            )
            .await?;
        rows.into_iter()
            .map(|(id, record)| window_event_from_record(&key.phone_number_id, id, record))
            .collect()
    }

    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool, StorageError> {
        let owner = owner_str(ownership.owner)?;
        let (has_role, role) = optional(ownership.role.as_deref());
        let (has_app, app) = optional(ownership.app_id.as_ref().map(AppId::as_str));
        let since = ts16(ownership.since);
        let stored: i64 = self
            .run(
                &SET_OWNER,
                &[
                    key.phone_number_id.as_str().as_bytes(),
                    key.contact.as_bytes(),
                    owner.as_bytes(),
                    has_role.as_bytes(),
                    role.as_bytes(),
                    has_app.as_bytes(),
                    app.as_bytes(),
                    since.as_bytes(),
                ],
            )
            .await?;
        Ok(stored == 1)
    }

    async fn thread_owner(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<ThreadOwnership>, StorageError> {
        let Some(mut r) = self
            .hgetall(self.key("o", &key.phone_number_id, &key.contact))
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(ThreadOwnership {
            owner: parse_owner(r.required("o", "thread owner")?)?,
            role: r.text("r", "thread owner role")?,
            app_id: r.text("a", "thread owner app")?.map(AppId::new_unchecked),
            since: r.required_time("s", "thread owner time")?,
        }))
    }

    async fn put_contact(&self, contact: StoredContact) -> Result<bool, StorageError> {
        let at = ts16(contact.synced_at);
        let fields = [
            optional(contact.full_name.as_deref()),
            optional(contact.first_name.as_deref()),
            optional(contact.phone_number.as_deref()),
            optional(contact.user_id.as_ref().map(UserId::as_str)),
            optional(contact.parent_user_id.as_ref().map(UserId::as_str)),
            optional(contact.username.as_deref()),
        ];
        let mut args: Vec<&[u8]> = vec![
            contact.key.phone_number_id.as_str().as_bytes(),
            contact.key.contact.as_bytes(),
            at.as_bytes(),
        ];
        for (flag, value) in &fields {
            args.push(flag.as_bytes());
            args.push(value.as_bytes());
        }
        let stored: i64 = self.run(&PUT_CONTACT, &args).await?;
        Ok(stored == 1)
    }

    async fn remove_contact(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let at = ts16(at);
        let removed: i64 = self
            .run(
                &REMOVE_CONTACT,
                &[
                    key.phone_number_id.as_str().as_bytes(),
                    key.contact.as_bytes(),
                    at.as_bytes(),
                ],
            )
            .await?;
        Ok(removed == 1)
    }

    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>, StorageError> {
        let Some(record) = self
            .hgetall(self.key("p", &key.phone_number_id, &key.contact))
            .await?
        else {
            return Ok(None);
        };
        contact_from_record(&key.phone_number_id, key.contact.clone(), record)
    }

    async fn contacts(
        &self,
        phone_number_id: &PhoneNumberId,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<StoredContact>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let lower = match &after {
            None => b"-".to_vec(),
            Some(contact) => format!("({contact}").into_bytes(),
        };
        let rows = self
            .page(phone_number_id, "pl", "", (&lower, b"+"), limit, false)
            .await?;
        let mut contacts = Vec::with_capacity(rows.len());
        for (contact, record) in rows {
            contacts.extend(contact_from_record(phone_number_id, contact, record)?);
        }
        Ok(contacts)
    }

    async fn link_identity(&self, link: IdentityLink) -> Result<bool, StorageError> {
        let at = ts16(link.at);
        let linked: i64 = self
            .run(
                &LINK,
                &[
                    link.phone_number_id.as_str().as_bytes(),
                    link.previous.as_bytes(),
                    link.current.as_bytes(),
                    at.as_bytes(),
                ],
            )
            .await?;
        Ok(linked == 1)
    }

    async fn identity_links(
        &self,
        key: &ConversationKey,
    ) -> Result<Vec<IdentityLink>, StorageError> {
        let flat: Vec<Vec<u8>> = self
            .run(
                &LINKS,
                &[
                    key.phone_number_id.as_str().as_bytes(),
                    key.contact.as_bytes(),
                ],
            )
            .await?;
        let mut links = Vec::with_capacity(flat.len() / 2);
        let mut it = flat.into_iter();
        while let (Some(member), Some(at)) = (it.next(), it.next()) {
            let (previous, current) = split_link(&member)?;
            links.push(IdentityLink::new(
                key.phone_number_id.clone(),
                previous,
                current,
                from_ts16(&at, "identity link time")?,
            ));
        }
        links.sort_by(|a, b| (a.at, &a.previous, &a.current).cmp(&(b.at, &b.previous, &b.current)));
        Ok(links)
    }

    async fn identities(&self, key: &ConversationKey) -> Result<BTreeSet<String>, StorageError> {
        let found: Vec<Vec<u8>> = self
            .run(
                &IDENTITIES,
                &[
                    key.phone_number_id.as_str().as_bytes(),
                    key.contact.as_bytes(),
                ],
            )
            .await?;
        found.into_iter().map(|id| utf8("identity", id)).collect()
    }

    async fn erase_all(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> Result<Erased, StorageError> {
        let ids: BTreeSet<&str> = contacts.iter().map(String::as_str).collect();
        if ids.is_empty() {
            return Ok(Erased::default());
        }
        let mode: &[u8] = if self.erasure_mode == ErasureMode::Delete {
            b"delete"
        } else {
            // `Redact`, and any mode added later: never keep the content.
            b"redact"
        };
        let count = ids.len().to_string();
        let mut args: Vec<&[u8]> = vec![
            phone_number_id.as_str().as_bytes(),
            mode,
            StoredMessage::ERASED.as_bytes(),
            StoredMessage::REVOKED.as_bytes(),
            count.as_bytes(),
        ];
        args.extend(ids.iter().map(|id| id.as_bytes()));
        let counts: Vec<u64> = self.run(&ERASE, &args).await?;
        let [
            messages,
            conversations,
            window_events,
            thread_owners,
            contacts,
            identity_links,
            group_messages,
        ] = <[u64; 7]>::try_from(counts).map_err(|_| {
            StorageError::Backend(anyhow::anyhow!(
                "the erasure script answered no seven counts"
            ))
        })?;
        Ok(Erased {
            messages,
            conversations,
            window_events,
            thread_owners,
            contacts,
            identity_links,
            group_messages,
        })
    }

    fn erasure_mode(&self) -> ErasureMode {
        self.erasure_mode
    }

    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged, StorageError> {
        let numbers = match phone_number_id {
            Some(number) => vec![number.clone()],
            None => self.numbers().await?,
        };
        let cutoff = ts16(cutoff);
        let mut purged = Purged::default();
        for number in numbers {
            let counts: Vec<u64> = self
                .run(&PURGE, &[number.as_str().as_bytes(), cutoff.as_bytes()])
                .await?;
            let [
                messages,
                conversations,
                window_events,
                thread_owners,
                contact_removals,
            ] = <[u64; 5]>::try_from(counts).map_err(|_| {
                StorageError::Backend(anyhow::anyhow!("the purge script answered no five counts"))
            })?;
            purged.messages += messages;
            purged.conversations += conversations;
            purged.window_events += window_events;
            purged.thread_owners += thread_owners;
            purged.contact_removals += contact_removals;
        }
        Ok(purged)
    }

    fn retention(&self) -> Retention {
        self.retention
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stored data: changing the layout strands what was written before
    /// (docs/architecture.md § "Stable identifiers", pending the owner's
    /// confirmation before the first release). `\x77` is `w`.
    #[test]
    fn the_conversation_layout_is_pinned() {
        // A store needs a connection only to run commands: the layout is
        // computed without one.
        let store = RedisConversationStore::new(NoConnection);
        assert_eq!(store.prefix(), "\x77a:");
        assert_eq!(store.base(), "\x77a:{conversations}");
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        assert_eq!(
            store.key("m", &pn, "wamid.1"),
            "\x77a:{conversations}:m:15:106540352242922wamid.1"
        );
        assert_eq!(
            store.with_prefix("t:").key("s", &pn, "US.1"),
            "t:{conversations}:s:15:106540352242922US.1"
        );
        assert_eq!(
            ts16(time::macros::datetime!(1970-01-01 0:00 UTC)),
            "8000000000000000"
        );
        // 2026-09-24T12:00:00Z is 1 790 251 200 unix seconds.
        assert_eq!(
            ts16(time::macros::datetime!(2026-09-24 12:00 UTC)),
            "80065c3956d5b000"
        );
        assert_eq!(
            ts16(time::macros::datetime!(1969-12-31 23:59:59.999999 UTC)),
            "7fffffffffffffff"
        );
    }

    /// A connection that is never used.
    #[derive(Clone)]
    struct NoConnection;

    impl ConnectionLike for NoConnection {
        fn req_packed_command<'a>(
            &'a mut self,
            _: &'a redis::Cmd,
        ) -> redis::RedisFuture<'a, redis::Value> {
            unreachable!("the layout needs no connection")
        }

        fn req_packed_commands<'a>(
            &'a mut self,
            _: &'a redis::Pipeline,
            _: usize,
            _: usize,
        ) -> redis::RedisFuture<'a, Vec<redis::Value>> {
            unreachable!("the layout needs no connection")
        }

        fn get_db(&self) -> i64 {
            0
        }
    }

    #[test]
    fn times_sort_as_their_bytes() {
        let times = [
            time::macros::datetime!(-9999-01-01 0:00 UTC),
            time::macros::datetime!(1969-12-31 23:59:59 UTC),
            time::macros::datetime!(1970-01-01 0:00 UTC),
            time::macros::datetime!(2026-09-24 12:00:00.000001 UTC),
            time::macros::datetime!(9999-12-31 23:59:59.999999 UTC),
        ];
        for pair in times.windows(2) {
            assert!(ts16(pair[0]) < ts16(pair[1]), "{pair:?}");
        }
        for t in times {
            assert_eq!(from_ts16(ts16(t).as_bytes(), "t").unwrap(), t);
        }
        assert!(from_ts16(b"zz", "t").is_err());
    }

    #[test]
    fn link_members_split_back() {
        assert_eq!(
            split_link(b"7:US.1:23US.2").unwrap(),
            ("US.1:23".to_owned(), "US.2".to_owned())
        );
        assert_eq!(split_link(b"0:x").unwrap(), (String::new(), "x".to_owned()));
        for bad in [&b"x"[..], b"9:short", b"a:b"] {
            assert!(split_link(bad).is_err(), "{bad:?}");
        }
    }
}
