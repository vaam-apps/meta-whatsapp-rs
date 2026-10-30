// Only `DownloadLimits::parse` and `DownloadLimits::new` make a download's
// limits, within the caps: a whole file over 16 MiB would be held in
// memory.
use meta_whatsapp_server_core::media::DownloadLimits;

fn past_the_cap() -> DownloadLimits {
    DownloadLimits {
        max_bytes: u64::MAX,
        stream: false,
    }
}

fn main() {}
