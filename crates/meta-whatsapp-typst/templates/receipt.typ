// Order confirmation / receipt, A5. Input: `meta_whatsapp_typst::ReceiptInput` as JSON in
// `sys.inputs.data`. Amounts and dates arrive formatted; this template never
// computes either. Fonts: only those bundled by typst-assets.
#let data = json(bytes(sys.inputs.data))

#set document(title: data.merchant_name + " order " + data.order_number)
#set page(paper: "a5", margin: 1.5cm)
#set text(font: "Libertinus Serif", size: 10pt)

#align(center)[
  #text(size: 16pt, weight: "bold", data.merchant_name) \
  Order confirmation
]

#v(1em)
#grid(
  columns: (1fr, 1fr),
  [Order *#data.order_number* \ #data.order_date],
  align(right)[Customer \ *#data.customer_name*],
)

#v(1em)
#table(
  columns: (1fr, auto, auto),
  align: (left, right, right),
  stroke: none,
  table.header([*Item*], [*Qty × price*], [*Amount (#data.currency)*]),
  table.hline(),
  ..data.items.map(item => (item.description, [#item.quantity × #item.unit_price], item.amount)).flatten(),
  table.hline(),
)

#align(right, table(
  columns: (auto, auto),
  align: (left, right),
  stroke: none,
  [Subtotal], data.subtotal,
  ..data.adjustments.map(line => (line.label, line.amount)).flatten(),
  table.hline(),
  [*Total paid*], [*#data.total #data.currency*],
))

Paid with #data.payment_method

#if data.delivery_address.len() > 0 [
  #v(0.5em)
  *Delivery address* \
  #data.delivery_address.join(linebreak())
]

#if data.estimated_delivery != none [
  #v(0.5em)
  Estimated delivery: #data.estimated_delivery
]

#v(1fr)
#align(center)[
  Thank you for your order!
  #if data.support_contact != none [ \ Questions? #data.support_contact ]
]
