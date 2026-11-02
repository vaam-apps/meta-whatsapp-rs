-- meta-whatsapp-server, migration 6: the media ids the outbox recorded as
-- received (roadmap M2e, OPEN_QUESTIONS.md #43). Expand-only, like every
-- migration of the service (docs/design/server.md, section 2.4), and with
-- no table rewrite: the column is added without a default, and the
-- existing rows are not updated. sqlx records this file's checksum: never
-- edit it once released, a comment included.
--
-- `media_id` is the media id a `message_received` event's customer sent
-- (an image, a video, an audio, a document or a sticker, or an edit into
-- one), set at insert by this release from the event (the core's
-- `NewEvent::received_media`); NULL for every other row, for the rows
-- written before this migration, and for those a replica of the previous
-- release inserts during a rolling deploy. The media routes read it: a
-- media id recorded on the caller's own number and tenant is asked from
-- Meta without `phone_number_id`, one recorded on any other row is not
-- asked at all, and one with no row (NULL included) goes through Meta's
-- `phone_number_id` check, as every id did before. The rows go with the
-- outbox's retention and with their tenant, as the rest of the row.
--
-- The index is partial: only the rows that record a media id. Building it
-- reads the table once (every existing row is NULL, so it starts empty).

ALTER TABLE wa_server_events ADD COLUMN media_id TEXT COLLATE "C";

CREATE INDEX wa_server_events_media_idx ON wa_server_events (media_id)
    WHERE media_id IS NOT NULL;
