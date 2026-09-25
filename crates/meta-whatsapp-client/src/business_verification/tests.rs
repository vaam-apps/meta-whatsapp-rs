//! Partner-led business verification tests. Requests, responses and
//! example values are the ones of
//! `solution-providers/partner-led-business-verification`.

use futures::StreamExt;
use http::Method;
use meta_whatsapp_core::ErrorKind;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::secret::AccessToken;
use meta_whatsapp_core::testing::{RecordedBody, RecordedRequest, ScriptedTransport};
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::RetryPolicy;

/// The page's example values.
const PARTNER: &str = "506914307656634";
const CUSTOMER: &str = "2729063490586005";
const SYSTEM_TOKEN: &str = "SYSTEM_TOKEN";
const BUSINESS_TOKEN: &str = "BUSINESS_TOKEN";

const PDF: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0";

fn client(t: &ScriptedTransport, token: &str) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token(token)
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// The partner's system user token, as the page requires for submitting
/// and listing.
fn system(t: &ScriptedTransport) -> BusinessVerification {
    client(t, SYSTEM_TOKEN).business_verification()
}

fn partner() -> BusinessId {
    BusinessId::new(PARTNER)
}

fn customer() -> BusinessId {
    BusinessId::new(CUSTOMER)
}

fn documents() -> Vec<VerificationDocument> {
    vec![
        VerificationDocument::from_file_name("wind_and_wool_bank_statement_04302024.pdf", PDF)
            .unwrap(),
        VerificationDocument::from_file_name("registration.png", PNG).unwrap(),
        VerificationDocument::from_file_name("utility_bill.JPG", JPEG).unwrap(),
    ]
}

/// A multipart part: name, file name, content type, data.
type Part = (String, Option<String>, Option<String>, Vec<u8>);

fn parts(req: &RecordedRequest) -> Vec<Part> {
    let RecordedBody::Multipart(parts) = &req.body else {
        panic!("expected multipart, got {:?}", req.body);
    };
    parts
        .iter()
        .map(|(n, f, c, d)| (n.clone(), f.clone(), c.clone(), d.to_vec()))
        .collect()
}

fn part(name: &str, file: Option<&str>, ctype: Option<&str>, data: &[u8]) -> Part {
    (
        name.to_owned(),
        file.map(str::to_owned),
        ctype.map(str::to_owned),
        data.to_vec(),
    )
}

fn validation_field(err: &meta_whatsapp_core::Error) -> &str {
    match err {
        meta_whatsapp_core::Error::Validation(v) => v.field.as_str(),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// ─── Submit ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_sends_the_documented_form_with_the_system_token() {
    let t = ScriptedTransport::new();
    // "Submitting a business for verification", response.
    t.push_json(
        200,
        json!({
          "success": true,
          "message": "Your request has been received and will be reviewed shortly.",
          "verification_attempts": 1
        }),
    );
    let receipt = system(&t)
        .submit(&partner(), &customer(), &documents())
        .await
        .unwrap();
    assert_eq!(
        receipt,
        SubmissionReceipt {
            message: Some("Your request has been received and will be reviewed shortly.".into()),
            verification_attempts: Some(1),
        }
    );
    assert_eq!(receipt.attempts_left(), Some(2));

    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/506914307656634/self_certify_whatsapp_business"
    );
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some(SYSTEM_TOKEN));
    // `-F end_business_id=…` then one `-F business_documents[]=@…` per
    // document, in order.
    assert_eq!(
        parts(&req),
        [
            part("end_business_id", None, None, CUSTOMER.as_bytes()),
            part(
                "business_documents[]",
                Some("wind_and_wool_bank_statement_04302024.pdf"),
                Some("application/pdf"),
                PDF,
            ),
            part(
                "business_documents[]",
                Some("registration.png"),
                Some("image/png"),
                PNG,
            ),
            part(
                "business_documents[]",
                Some("utility_bill.JPG"),
                Some("image/jpeg"),
                JPEG,
            ),
        ]
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn submit_with_one_document_and_the_documented_token_switch() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true, "verification_attempts": 3}));
    // The module docs' pattern: one client, the system token for this call.
    let base = client(&t, "SOME_OTHER_TOKEN");
    let system = base.with_token(AccessToken::new(SYSTEM_TOKEN));
    let receipt = system
        .business_verification()
        .submit(&partner(), &customer(), &documents()[..1])
        .await
        .unwrap();
    assert_eq!(receipt.message, None);
    assert_eq!(receipt.attempts_left(), Some(0));
    let req = t.last_request().unwrap();
    assert_eq!(req.bearer(), Some(SYSTEM_TOKEN));
    assert_eq!(parts(&req).len(), 2);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn success_false_is_an_error() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": false}));
    let err = system(&t)
        .submit(&partner(), &customer(), &documents())
        .await
        .unwrap_err();
    assert!(
        matches!(err, meta_whatsapp_core::Error::Http { status: 200, .. }),
        "{err:?}"
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn submit_maps_graph_errors() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException", "code": 100}}),
    );
    let err = system(&t)
        .submit(&partner(), &customer(), &documents())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert_eq!(err.graph().map(|g| g.code), Some(100));

    t.push_json(
        403,
        json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException", "code": 200}}),
    );
    let err = system(&t)
        .submit(&partner(), &customer(), &documents())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Permission);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn submit_is_never_replayed() {
    for fail in [
        (|t: &ScriptedTransport| {
            t.push_error(|| TransportError::Timeout);
        }) as fn(&ScriptedTransport),
        |t| {
            t.push_json(503, json!({"error": {"message": "down", "code": 2}}));
        },
    ] {
        let t = ScriptedTransport::new();
        fail(&t);
        t.push_json(200, json!({"success": true, "verification_attempts": 2}));
        let c = Client::builder()
            .transport(t.clone())
            .access_token(SYSTEM_TOKEN)
            .retry(RetryPolicy::default())
            .build()
            .unwrap();
        assert!(
            c.business_verification()
                .submit(&partner(), &customer(), &documents())
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 1, "a replay could spend a submission");
    }
}

#[tokio::test]
async fn submit_validates_before_sending() {
    let t = ScriptedTransport::new();
    let api = system(&t);
    let docs = documents();

    let err = api
        .submit(&BusinessId::new(" "), &customer(), &docs)
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "business_id");
    let err = api
        .submit(&partner(), &BusinessId::new(""), &docs)
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "end_business_id");
    let err = api.submit(&partner(), &customer(), &[]).await.unwrap_err();
    assert_eq!(validation_field(&err), "business_documents");
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);

    let mut four = docs.clone();
    four.push(docs[0].clone());
    let err = api
        .submit(&partner(), &customer(), &four)
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "business_documents");
    assert!(err.to_string().contains("at most 3"), "{err}");

    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn ids_stay_one_segment() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"data": []}));
    t.push_json(200, json!({"id": "1/x", "verification_status": "verified"}));
    let api = system(&t);
    api.submit(&BusinessId::new("1/x"), &customer(), &documents())
        .await
        .unwrap();
    api.submissions(
        &BusinessId::new("1/self_certify_whatsapp_business"),
        &ListVerificationSubmissions::new(),
    )
    .await
    .unwrap();
    api.status(&BusinessId::new("1/x")).await.unwrap();
    let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
    assert_eq!(
        paths,
        [
            "/v25.0/1%2Fx/self_certify_whatsapp_business",
            "/v25.0/1%2Fself_certify_whatsapp_business/self_certified_whatsapp_business_submissions",
            "/v25.0/1%2Fx",
        ]
    );
    assert_eq!(t.remaining(), 0);
}

// ─── Documents ───────────────────────────────────────────────────────────

#[test]
fn document_types_are_the_four_the_page_lists() {
    for (name, ty) in [
        ("a.pdf", DocumentType::Pdf),
        ("a.PDF", DocumentType::Pdf),
        ("a.jpeg", DocumentType::Jpeg),
        ("a.jpg", DocumentType::Jpeg),
        ("scan.2024.Jpg", DocumentType::Jpeg),
        ("a.png", DocumentType::Png),
    ] {
        assert_eq!(DocumentType::from_file_name(name), Ok(ty), "{name}");
    }
    // The page's example path ends in .txt: not a supported type.
    for name in [
        "NP7sEWs3x/wind_and_wool_bank_statement_04302024.txt",
        "a.gif",
        "a.webp",
        "a.docx",
        "pdf",
        "a.",
        "",
    ] {
        let err = DocumentType::from_file_name(name).unwrap_err();
        assert_eq!(err.field, "business_documents", "{name}");
    }

    for (mime, ty) in [
        ("application/pdf", DocumentType::Pdf),
        ("image/jpeg", DocumentType::Jpeg),
        ("image/jpg", DocumentType::Jpeg),
        ("IMAGE/PNG", DocumentType::Png),
        ("application/pdf; charset=binary", DocumentType::Pdf),
    ] {
        assert_eq!(DocumentType::from_mime_type(mime), Ok(ty), "{mime}");
        assert_eq!(
            DocumentType::from_mime_type(ty.mime_type()),
            Ok(ty),
            "round trip"
        );
    }
    for mime in ["text/plain", "image/gif", "application/octet-stream", ""] {
        assert!(DocumentType::from_mime_type(mime).is_err(), "{mime}");
    }
}

#[test]
fn documents_are_checked_when_built() {
    let ok = VerificationDocument::new("a.pdf", DocumentType::Pdf, PDF).unwrap();
    assert_eq!(ok.file_name(), "a.pdf");
    assert_eq!(ok.document_type(), DocumentType::Pdf);
    assert_eq!(&ok.data()[..], PDF);

    let refused = |name: &str, ty: DocumentType, data: Vec<u8>, why: &str| {
        let err = VerificationDocument::new(name, ty, data).unwrap_err();
        assert_eq!(err.field, "business_documents");
        assert!(err.to_string().contains(why), "{err} (expected {why:?})");
    };
    refused("", DocumentType::Pdf, PDF.to_vec(), "file name required");
    refused("  ", DocumentType::Pdf, PDF.to_vec(), "file name required");
    refused(
        "a\r\nContent-Type: text/html.pdf",
        DocumentType::Pdf,
        PDF.to_vec(),
        "control characters",
    );
    refused("a.pdf", DocumentType::Pdf, Vec::new(), "empty");
    // The content must be the declared type.
    refused(
        "a.pdf",
        DocumentType::Pdf,
        PNG.to_vec(),
        "declared file type",
    );
    refused(
        "a.png",
        DocumentType::Png,
        JPEG.to_vec(),
        "declared file type",
    );
    refused(
        "a.jpg",
        DocumentType::Jpeg,
        PDF.to_vec(),
        "declared file type",
    );
    let err = VerificationDocument::from_file_name("a.png", PDF).unwrap_err();
    assert!(err.to_string().contains("declared file type"), "{err}");
    let err = VerificationDocument::from_file_name("statement.txt", PDF).unwrap_err();
    assert!(err.to_string().contains("PDF, JPEG, JPG and PNG"), "{err}");
}

#[test]
fn documents_are_at_most_5_mb() {
    let mut at_limit = PDF.to_vec();
    at_limit.resize(MAX_DOCUMENT_BYTES, b' ');
    assert_eq!(MAX_DOCUMENT_BYTES, 5 * 1024 * 1024);
    VerificationDocument::new("a.pdf", DocumentType::Pdf, at_limit.clone()).unwrap();
    at_limit.push(b' ');
    let err = VerificationDocument::new("a.pdf", DocumentType::Pdf, at_limit).unwrap_err();
    assert!(err.to_string().contains("5 MB"), "{err}");
}

#[test]
fn signatures() {
    assert!(DocumentType::Pdf.matches(b"%PDF-"));
    // The PDF header may follow some bytes, within the first 1024.
    let mut late = vec![b' '; 1019];
    late.extend_from_slice(b"%PDF-1.4");
    assert!(DocumentType::Pdf.matches(&late));
    let mut too_late = vec![b' '; 1020];
    too_late.extend_from_slice(b"%PDF-1.4");
    assert!(!DocumentType::Pdf.matches(&too_late));
    assert!(!DocumentType::Pdf.matches(b"%PDF"));
    assert!(DocumentType::Jpeg.matches(&[0xFF, 0xD8, 0xFF]));
    assert!(!DocumentType::Jpeg.matches(&[0xFF, 0xD8]));
    assert!(DocumentType::Png.matches(b"\x89PNG\r\n\x1a\n"));
    assert!(!DocumentType::Png.matches(b"\x89PNG\r\n"));
}

#[test]
fn document_debug_never_shows_the_content() {
    let doc = VerificationDocument::new("statement.pdf", DocumentType::Pdf, PDF).unwrap();
    let debug = format!("{doc:?}");
    assert_eq!(
        debug,
        format!(
            "VerificationDocument {{ file_name: \"statement.pdf\", document_type: Pdf, len: {} }}",
            PDF.len()
        )
    );
}

// ─── Submissions ─────────────────────────────────────────────────────────

/// "Getting submission status", response: a pending or approved
/// submission, then a rejected one. The page gives no example values for
/// these placeholders (timestamps, vertical, submission ids, cursors):
/// the ones here are ours; the business id is the page's.
fn submissions_page() -> serde_json::Value {
    json!({
      "data": [
        {
          "verification_status": "APPROVED",
          "submitted_time": "2024-11-04T20:39:21+0000",
          "update_time": "2024-11-04T20:44:02+0000",
          "client_business_id": "2729063490586005",
          "submitted_info": {
            "business_vertical": "RETAIL"
          },
          "id": "1066221828388475"
        },
        {
          "verification_status": "FAILED",
          "rejection_reasons": [
            "LEGAL_NAME_NOT_FOUND_IN_DOCUMENTS",
            "WEBSITE NOT MATCHING"
          ],
          "submitted_time": "2024-11-03T10:00:00+0000",
          "update_time": "2024-11-03T10:05:00+0000",
          "client_business_id": "2729063490586005",
          "submitted_info": {},
          "id": "1066221828388476"
        }
      ],
      "paging": {
        "cursors": {
          "before": "QVFIUjBEFORE",
          "after": "QVFIUjAFTER"
        },
        "next": "https://graph.facebook.com/v25.0/506914307656634/self_certified_whatsapp_business_submissions?after=QVFIUjAFTER"
      }
    })
}

#[tokio::test]
async fn submissions_for_one_customer_with_the_system_token() {
    let t = ScriptedTransport::new();
    t.push_json(200, submissions_page());
    let page = system(&t)
        .submissions(
            &partner(),
            &ListVerificationSubmissions::new().end_business_id(CUSTOMER),
        )
        .await
        .unwrap();

    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        "/v25.0/506914307656634/self_certified_whatsapp_business_submissions"
    );
    assert_eq!(req.url.query(), Some("end_business_id=2729063490586005"));
    assert_eq!(req.bearer(), Some(SYSTEM_TOKEN));
    assert_eq!(t.remaining(), 0);

    assert_eq!(page.next_cursor(), Some("QVFIUjAFTER"));
    let [approved, rejected] = &page.data[..] else {
        panic!("two submissions expected: {:?}", page.data);
    };
    assert_eq!(
        approved,
        &VerificationSubmission {
            id: VerificationSubmissionId::new("1066221828388475"),
            verification_status: Some(SubmissionStatus::Approved),
            rejection_reasons: vec![],
            submitted_time: Some("2024-11-04T20:39:21+0000".into()),
            update_time: Some("2024-11-04T20:44:02+0000".into()),
            client_business_id: Some(customer()),
            submitted_info: Some(SubmittedInfo {
                business_vertical: Some("RETAIL".into()),
            }),
        }
    );
    assert_eq!(rejected.verification_status, Some(SubmissionStatus::Failed));
    assert_eq!(rejected.submitted_info, Some(SubmittedInfo::default()));
    assert_eq!(
        rejected.reasons(),
        [
            RejectionReason::LegalNameNotFoundInDocuments,
            RejectionReason::WebsiteNotMatching
        ]
    );
}

#[tokio::test]
async fn submissions_for_every_customer_and_cursors() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"data": []}));
    t.push_json(200, json!({"data": []}));
    let api = system(&t);
    let page = api
        .submissions(&partner(), &ListVerificationSubmissions::new())
        .await
        .unwrap();
    assert!(page.data.is_empty());
    assert_eq!(t.requests()[0].url.query(), None);

    api.submissions(
        &partner(),
        &ListVerificationSubmissions::new()
            .end_business_id(CUSTOMER)
            .after("QVFIUjAFTER")
            .before("QVFIUjBEFORE"),
    )
    .await
    .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.query("end_business_id").as_deref(), Some(CUSTOMER));
    assert_eq!(req.query("after").as_deref(), Some("QVFIUjAFTER"));
    assert_eq!(req.query("before").as_deref(), Some("QVFIUjBEFORE"));
    assert_eq!(req.query("fields"), None);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn submissions_validate_ids() {
    let t = ScriptedTransport::new();
    let api = system(&t);
    let err = api
        .submissions(&BusinessId::new(""), &ListVerificationSubmissions::new())
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "business_id");
    let err = api
        .submissions(
            &partner(),
            &ListVerificationSubmissions::new().end_business_id(" "),
        )
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "end_business_id");
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn submissions_stream_follows_cursors_and_refuses_the_callers() {
    let t = ScriptedTransport::new();
    t.push_json(200, submissions_page());
    t.push_json(
        200,
        json!({"data": [{"id": "1066221828388477", "verification_status": "PENDING"}]}),
    );
    let api = system(&t);
    let items: Vec<VerificationSubmission> = api
        .submissions_stream(
            &partner(),
            &ListVerificationSubmissions::new().end_business_id(CUSTOMER),
        )
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[2].verification_status,
        Some(SubmissionStatus::Pending)
    );
    let reqs = t.requests();
    assert_eq!(reqs[1].query("after").as_deref(), Some("QVFIUjAFTER"));
    assert_eq!(reqs[1].query("end_business_id").as_deref(), Some(CUSTOMER));
    assert!(reqs.iter().all(|r| r.bearer() == Some(SYSTEM_TOKEN)));
    assert_eq!(t.remaining(), 0);

    for query in [
        ListVerificationSubmissions::new().after("x"),
        ListVerificationSubmissions::new().before("x"),
    ] {
        let items: Vec<_> = api.submissions_stream(&partner(), &query).collect().await;
        let [Err(err)] = &items[..] else {
            panic!("one error expected: {items:?}");
        };
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    }
    let items: Vec<_> = api
        .submissions_stream(&BusinessId::new(""), &ListVerificationSubmissions::new())
        .collect()
        .await;
    assert!(matches!(&items[..], [Err(_)]), "{items:?}");
    assert_eq!(t.requests().len(), 2, "nothing sent for a refused stream");
}

// ─── Status ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn status_reads_the_customer_business_with_its_business_token() {
    let t = ScriptedTransport::new();
    // "Get business verification status", response.
    t.push_json(
        200,
        json!({"verification_status": "verified", "id": "2729063490586005"}),
    );
    let business = client(&t, BUSINESS_TOKEN);
    let info = business
        .business_verification()
        .status(&customer())
        .await
        .unwrap();
    assert_eq!(
        info,
        BusinessVerificationInfo {
            id: customer(),
            verification_status: Some(BusinessVerificationStatus::Verified),
        }
    );
    assert!(info.is_verified());

    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/2729063490586005");
    assert_eq!(req.url.query(), Some("fields=verification_status"));
    assert_eq!(req.bearer(), Some(BUSINESS_TOKEN));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn status_validates_and_maps_errors() {
    let t = ScriptedTransport::new();
    let api = client(&t, BUSINESS_TOKEN).business_verification();
    let err = api.status(&BusinessId::new(" ")).await.unwrap_err();
    assert_eq!(validation_field(&err), "business_id");
    assert!(t.requests().is_empty());

    t.push_json(
        400,
        json!({"error": {"message": "(#100) Unsupported get request", "type": "GraphMethodException", "code": 100}}),
    );
    let err = api.status(&customer()).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn status_without_verification_status_is_not_verified() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": CUSTOMER}));
    let info = client(&t, BUSINESS_TOKEN)
        .business_verification()
        .status(&customer())
        .await
        .unwrap();
    assert_eq!(info.verification_status, None);
    assert!(!info.is_verified());
}

// ─── Enums ───────────────────────────────────────────────────────────────

#[test]
fn business_statuses_are_the_references_ten_and_open() {
    for (wire, status) in [
        ("expired", BusinessVerificationStatus::Expired),
        ("failed", BusinessVerificationStatus::Failed),
        ("ineligible", BusinessVerificationStatus::Ineligible),
        ("not_verified", BusinessVerificationStatus::NotVerified),
        ("pending", BusinessVerificationStatus::Pending),
        (
            "pending_need_more_info",
            BusinessVerificationStatus::PendingNeedMoreInfo,
        ),
        (
            "pending_submission",
            BusinessVerificationStatus::PendingSubmission,
        ),
        ("rejected", BusinessVerificationStatus::Rejected),
        ("revoked", BusinessVerificationStatus::Revoked),
        ("verified", BusinessVerificationStatus::Verified),
    ] {
        let parsed: BusinessVerificationStatus = serde_json::from_value(json!(wire)).unwrap();
        assert_eq!(parsed, status);
        assert_eq!(serde_json::to_value(&status).unwrap(), json!(wire));
        assert_eq!(status.to_string(), wire);
    }
    let other: BusinessVerificationStatus = serde_json::from_value(json!("VERIFIED")).unwrap();
    assert_eq!(other, BusinessVerificationStatus::Other("VERIFIED".into()));
    assert_eq!(serde_json::to_value(&other).unwrap(), json!("VERIFIED"));
    let info = BusinessVerificationInfo {
        id: customer(),
        verification_status: Some(other),
    };
    assert!(!info.is_verified());
}

#[test]
fn submission_statuses_are_open() {
    for (wire, status) in [
        ("APPROVED", SubmissionStatus::Approved),
        ("DISCARDED", SubmissionStatus::Discarded),
        ("FAILED", SubmissionStatus::Failed),
        ("PENDING", SubmissionStatus::Pending),
        ("REVOKED", SubmissionStatus::Revoked),
    ] {
        let parsed: SubmissionStatus = serde_json::from_value(json!(wire)).unwrap();
        assert_eq!(parsed, status);
        assert_eq!(serde_json::to_value(&status).unwrap(), json!(wire));
        assert_eq!(status.as_str(), wire);
    }
    let submission: VerificationSubmission = serde_json::from_value(json!({
        "id": "1",
        "verification_status": "IN_REVIEW",
        "a_field_meta_adds": {"x": 1}
    }))
    .unwrap();
    assert_eq!(
        submission.verification_status,
        Some(SubmissionStatus::Other("IN_REVIEW".into()))
    );
    assert_eq!(
        serde_json::to_value(&submission).unwrap(),
        json!({"id": "1", "verification_status": "IN_REVIEW"})
    );
}

#[test]
fn rejection_reasons_in_both_spellings() {
    for (spaces, reason) in [
        ("ADDRESS NOT MATCHING", RejectionReason::AddressNotMatching),
        (
            "BUSINESS NOT ELIGIBLE",
            RejectionReason::BusinessNotEligible,
        ),
        (
            "LEGAL NAME NOT MATCHING",
            RejectionReason::LegalNameNotMatching,
        ),
        (
            "LEGAL NAME NOT FOUND IN DOCUMENTS",
            RejectionReason::LegalNameNotFoundInDocuments,
        ),
        ("MALFORMED DOCUMENTS", RejectionReason::MalformedDocuments),
        ("NONE", RejectionReason::None),
        ("WEBSITE NOT MATCHING", RejectionReason::WebsiteNotMatching),
    ] {
        assert_eq!(RejectionReason::parse(spaces), reason, "{spaces}");
        let underscores = spaces.replace(' ', "_");
        assert_eq!(
            RejectionReason::parse(&underscores),
            reason,
            "{underscores}"
        );
    }
    assert_eq!(
        RejectionReason::parse("DOCUMENT EXPIRED"),
        RejectionReason::Other("DOCUMENT EXPIRED".into())
    );
    assert!(RejectionReason::BusinessNotEligible.is_ineligible());
    assert!(!RejectionReason::LegalNameNotMatching.is_ineligible());
    assert!(!RejectionReason::Other("BUSINESS NOT ELIGIBLE?".into()).is_ineligible());
}

#[test]
fn attempts_left_counts_down_from_three() {
    let receipt = |n: Option<u32>| SubmissionReceipt {
        message: None,
        verification_attempts: n,
    };
    assert_eq!(receipt(Some(1)).attempts_left(), Some(2));
    assert_eq!(receipt(Some(2)).attempts_left(), Some(1));
    assert_eq!(receipt(Some(3)).attempts_left(), Some(0));
    assert_eq!(receipt(Some(4)).attempts_left(), Some(0));
    assert_eq!(receipt(None).attempts_left(), None);
}
