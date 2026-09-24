#let data = json(bytes(sys.inputs.data))

#set page(margin: 1cm)
#set text(font: "Noto Sans")

= Order Confirmation

*Order #:* #data.order_id \
*Date:* #data.order_date

== Order Summary

#table(
  columns: (2fr, 1fr, 1fr, 1fr),
  [*Item*], [*Qty*], [*Unit Price*], [*Total*],
  ..data.items.map(item => (item.name, item.quantity, item.unit_price, item.total)).flatten()
)

== Totals

#table(
  columns: (3fr, 1fr),
  [Subtotal], data.subtotal,
  [Shipping], data.shipping,
  [Tax], data.tax,
  [*Total*], [*#data.total #data.currency*],
)

== Delivery Details

*Method:* #data.payment_method \
*Address:* #data.delivery_address
#if "delivery_eta" in data and data.delivery_eta != none [
  \ *Estimated Delivery:* #data.delivery_eta
]

Thank you for your order!
