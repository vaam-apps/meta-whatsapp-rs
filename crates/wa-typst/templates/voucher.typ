// Coupon / gift voucher, 400 x 210 pt (about 1.91:1, a WhatsApp image header).
// Render it to PNG. Input: `wa_typst::VoucherInput` as JSON in
// `sys.inputs.data`. Fonts: only those bundled by typst-assets.
// `code` is a shareable promo code, never an OTP or a bearer secret.
#let data = json(bytes(sys.inputs.data))

#set document(title: data.merchant_name + " voucher")
#set page(width: 400pt, height: 210pt, margin: 16pt, fill: rgb(data.accent_color))
#set text(font: "Libertinus Serif", size: 11pt, fill: white)

#text(size: 12pt, weight: "bold", upper(data.merchant_name))

#v(1fr)
#align(center)[
  #text(size: 34pt, weight: "bold", data.headline) \
  #data.description
  #v(4pt)
  #box(
    stroke: (paint: white, thickness: 1pt, dash: "dashed"),
    inset: (x: 10pt, y: 6pt),
    radius: 4pt,
    text(font: "DejaVu Sans Mono", size: 16pt, weight: "bold", data.code),
  )
]
#v(1fr)

#grid(
  columns: (auto, 1fr),
  column-gutter: 12pt,
  if data.valid_until != none [Valid until #data.valid_until],
  align(right, if data.terms != none { text(size: 7pt, data.terms) }),
)
