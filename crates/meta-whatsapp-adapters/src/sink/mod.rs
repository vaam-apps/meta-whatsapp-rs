//! [`EventSink`](meta_whatsapp_core::sink::EventSink) adapters and event streams
//! (feature `sinks`). All of them are generic over the event type `E`, so
//! they work for `meta_whatsapp_webhooks::WebhookEvent` and for your own events alike.
//!
//! | Sink | Delivers to | When it fails |
//! | --- | --- | --- |
//! | [`ChannelSink`] | a worker, through a bounded tokio `mpsc` channel | receiver dropped ([`Closed`](meta_whatsapp_core::error::SinkError::Closed)); full in [`ChannelMode::TryOrFail`] ([`Full`](meta_whatsapp_core::error::SinkError::Full)) |
//! | [`BroadcastSink`] | every live [`BroadcastSubscription`] (SSE, WebSocket) | never: no subscriber is not an error |
//! | [`FanoutSink`] | several sinks | tries all, returns the first error |
//! | [`FilterSink`] | an inner sink, for events matching a predicate | when the inner sink fails |
//! | [`FnSink`] | an async closure | when the closure fails |
//! | [`TracingSink`] | `tracing` events | never |
//!
//! The webhook handler answers Meta non-`200` when its sink fails, and Meta
//! then redelivers the whole batch — so keep sinks fast (queue slow work
//! behind a [`ChannelSink`]) and idempotent.
//!
//! ```
//! # async fn demo() {
//! use meta_whatsapp_adapters::sink::{BroadcastSink, FanoutSink, TracingSink, channel};
//! use meta_whatsapp_core::sink::EventSink;
//!
//! #[derive(Debug, Clone)]
//! struct Event(u32);
//!
//! let (worker, mut jobs) = channel::<Event>(1024);
//! let live = BroadcastSink::<Event>::new(256);
//! let mut inbox = live.subscribe(); // hand this to an SSE endpoint
//! let sink = FanoutSink::new().with(worker).with(live).with(TracingSink::new());
//!
//! sink.deliver(Event(1)).await.unwrap();
//! assert_eq!(jobs.recv().await.unwrap().0, 1);
//! # use futures::StreamExt;
//! assert_eq!(inbox.next().await.unwrap().unwrap().0, 1);
//! # }
//! ```

mod broadcast;
mod channel;
mod fanout;
mod filter;
mod func;
mod trace;

pub use broadcast::{BroadcastSink, BroadcastSubscription, Lagged};
pub use channel::{ChannelMode, ChannelSink, channel};
pub use fanout::FanoutSink;
pub use filter::FilterSink;
pub use func::FnSink;
pub use trace::TracingSink;
