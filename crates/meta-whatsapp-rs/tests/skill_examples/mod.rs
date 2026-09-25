//! Every `skills/<name>/examples/*.rs`, compiled against this commit's API
//! with its tests run. The consumer skills quote these files; `tests/skills.rs`
//! checks that the list below names every file on disk.

// Reference code: not every function is called from a test.
#![allow(dead_code)]

#[path = "../../../../skills/meta-whatsapp-rs-cms-inbox/examples/inbox.rs"]
mod wa_rs_cms_inbox_inbox;
#[path = "../../../../skills/meta-whatsapp-rs-commerce/examples/commerce.rs"]
mod wa_rs_commerce_commerce;
#[path = "../../../../skills/meta-whatsapp-rs-documents/examples/documents.rs"]
mod wa_rs_documents_documents;
#[path = "../../../../skills/meta-whatsapp-rs-embedded-signup/examples/onboarding.rs"]
mod wa_rs_embedded_signup_onboarding;
#[path = "../../../../skills/meta-whatsapp-rs-embedded-signup/examples/solution_partner.rs"]
mod wa_rs_embedded_signup_solution_partner;
#[path = "../../../../skills/meta-whatsapp-rs-errors/examples/handle.rs"]
mod wa_rs_errors_handle;
#[path = "../../../../skills/meta-whatsapp-rs-flows/examples/flows.rs"]
mod wa_rs_flows_flows;
#[path = "../../../../skills/meta-whatsapp-rs-groups-and-calling/examples/groups_calls.rs"]
mod wa_rs_groups_and_calling_groups_calls;
#[path = "../../../../skills/meta-whatsapp-rs-interactive-messages/examples/interactive.rs"]
mod wa_rs_interactive_messages_interactive;
#[path = "../../../../skills/meta-whatsapp-rs-live-updates/examples/sinks.rs"]
mod wa_rs_live_updates_sinks;
#[path = "../../../../skills/meta-whatsapp-rs-marketing/examples/marketing.rs"]
mod wa_rs_marketing_marketing;
#[path = "../../../../skills/meta-whatsapp-rs-media/examples/media.rs"]
mod wa_rs_media_media;
#[path = "../../../../skills/meta-whatsapp-rs-otp-login/examples/otp.rs"]
mod wa_rs_otp_login_otp;
#[path = "../../../../skills/meta-whatsapp-rs-phone-numbers/examples/numbers.rs"]
mod wa_rs_phone_numbers_numbers;
#[path = "../../../../skills/meta-whatsapp-rs-production/examples/production.rs"]
mod wa_rs_production_production;
#[path = "../../../../skills/meta-whatsapp-rs-send-messages/examples/send.rs"]
mod wa_rs_send_messages_send;
#[path = "../../../../skills/meta-whatsapp-rs-send-templates/examples/send.rs"]
mod wa_rs_send_templates_send;
#[path = "../../../../skills/meta-whatsapp-rs-setup/examples/client.rs"]
mod wa_rs_setup_client;
#[path = "../../../../skills/meta-whatsapp-rs-storage/examples/stores.rs"]
mod wa_rs_storage_stores;
#[path = "../../../../skills/meta-whatsapp-rs-templates/examples/manage.rs"]
mod wa_rs_templates_manage;
#[path = "../../../../skills/meta-whatsapp-rs-testing/examples/integration.rs"]
mod wa_rs_testing_integration;
#[path = "../../../../skills/meta-whatsapp-rs-token-vault/examples/vault.rs"]
mod wa_rs_token_vault_vault;
#[path = "../../../../skills/meta-whatsapp-rs-webhook-endpoint/examples/endpoint.rs"]
mod wa_rs_webhook_endpoint_endpoint;
#[path = "../../../../skills/meta-whatsapp-rs-webhook-events/examples/events.rs"]
mod wa_rs_webhook_events_events;
