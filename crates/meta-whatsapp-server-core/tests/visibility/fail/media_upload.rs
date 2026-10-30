// Only `Upload::new` makes an upload, checked against Meta's table of
// supported types and sizes, its file name made inert.
use meta_whatsapp_server_core::media::Upload;

fn unchecked(mime_type: String, filename: String) -> Upload {
    Upload {
        mime_type,
        filename,
        data: Vec::new().into(),
    }
}

fn main() {}
