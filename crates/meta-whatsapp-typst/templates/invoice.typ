// Invoice, A4. Input: `meta_whatsapp_typst::InvoiceInput` as JSON in `sys.inputs.data`.
// Amounts and dates arrive formatted; this template never computes either.
// Fonts: only those bundled by typst-assets (see the crate docs).
#let data = json(bytes(sys.inputs.data))

#set document(title: "Invoice " + data.invoice_number, author: data.seller.name)
#set page(paper: "a4", margin: 2cm)
#set text(font: "Libertinus Serif", size: 10pt)

// Name, address lines, then whichever optional contact fields are present.
#let party(p) = {
  strong(p.name)
  for line in p.address {
    linebreak()
    line
  }
  if p.tax_id != none {
    linebreak()
    [Tax ID: #p.tax_id]
  }
  if p.email != none {
    linebreak()
    p.email
  }
  if p.phone != none {
    linebreak()
    p.phone
  }
}

#grid(
  columns: (1fr, auto),
  party(data.seller),
  align(right)[
    #text(size: 20pt, weight: "bold")[Invoice] \
    No. #data.invoice_number \
    Issued #data.issue_date
    #if data.due_date != none [ \ Due #data.due_date ]
  ],
)

#v(1.5em)
#text(weight: "bold")[Bill to] \
#party(data.buyer)

#v(1.5em)
#table(
  columns: (1fr, auto, auto, auto),
  align: (left, right, right, right),
  stroke: none,
  table.header(
    [*Description*], [*Qty*], [*Unit price (#data.currency)*], [*Amount (#data.currency)*],
  ),
  table.hline(),
  ..data.items.map(item => (item.description, str(item.quantity), item.unit_price, item.amount)).flatten(),
  table.hline(),
)

#align(right, table(
  columns: (auto, auto),
  align: (left, right),
  stroke: none,
  [Subtotal], data.subtotal,
  ..data.adjustments.map(line => (line.label, line.amount)).flatten(),
  table.hline(),
  [*Total due*], [*#data.total #data.currency*],
))

#if data.payment_terms != none [
  #v(1.5em)
  *Payment terms* \
  #data.payment_terms
]

#if data.notes != none [
  #v(1em)
  #text(size: 9pt, data.notes)
]
