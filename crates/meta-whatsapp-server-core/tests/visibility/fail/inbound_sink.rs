// The webhook pipeline's sink, which routes an event and records it in
// the inbox and the outbox, is private to the core: an event reaches the
// stores only through `inbound::Pipeline::deliver`, after its body's
// signature checked out (roadmap S5e).
use meta_whatsapp_server_core::inbound::ServiceSink;

fn main() {
    let _ = std::mem::size_of::<ServiceSink>();
}
