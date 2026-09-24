#let data = json(bytes(sys.inputs.data))

#set page(margin: 1cm)
#set text(font: "Noto Sans")

= Invoice

*Invoice #:* #data.invoice_number \
*Date:* #data.invoice_date

== Bill To
#data.buyer_name
#if "buyer_contact" in data and data.buyer_contact != none [
  \ #data.buyer_contact
]

== From
#data.seller_name
#if "seller_contact" in data and data.seller_contact != none [
  \ #data.seller_contact
]

== Items

#table(
  columns: (2fr, 1fr, 1fr, 1fr),
  [*Description*], [*Qty*], [*Unit Price*], [*Total*],
  ..data.items.map(item => (item.description, item.quantity, item.unit_price, item.total)).flatten()
)

== Summary

#table(
  columns: (3fr, 1fr),
  [Subtotal], data.subtotal,
  [Tax], data.tax,
  [*Total*], [*#data.total #data.currency*],
)

#if "notes" in data and data.notes != none [
  == Notes
  #data.notes
]
