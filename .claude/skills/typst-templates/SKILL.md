---
name: typst-templates
description: "Authoring and rendering Typst templates in meta-whatsapp-typst — invoices, receipts, vouchers rendered to PDF/PNG from JSON inputs for WhatsApp document/image messages and template headers. Use when adding or changing a .typ template, its input schema, or the renderer."
metadata:
  internal: true
---

# Typst templates

- Templates live in `crates/meta-whatsapp-typst/templates/*.typ` and are embedded in
  the binary; inputs arrive as JSON through `sys.inputs` (read with
  `json(bytes(sys.inputs.data))` or the helper the renderer documents).
- Each template has a typed Rust input struct (`Serialize`) and a sample
  input under `crates/meta-whatsapp-typst/tests/fixtures/`. Tests render every
  template with its sample and assert: PDF starts with `%PDF`, PNG decodes,
  deterministic output for identical input.
- Fonts are bundled; never depend on system fonts (output must be identical
  in CI and the devcontainer).
- No network: templates may not import `@preview` packages at render time.
- Size budget: WhatsApp documents ≤ 100 MB, images ≤ 5 MB; keep PNG DPI
  modest (150–200) for header images.
- Never render OTPs or secrets into media. Authentication codes go only
  through authentication templates.
- Preview locally with the `typst` CLI in the devcontainer:
  `typst compile --input data='{...}' crates/meta-whatsapp-typst/templates/invoice.typ /tmp/out.pdf`.
