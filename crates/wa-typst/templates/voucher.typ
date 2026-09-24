#let data = json(bytes(sys.inputs.data))

#set page(
  paper: "custom",
  width: 800px,
  height: 400px,
  margin: 0pt,
  fill: rgb(data.brand_color)
)
#set text(font: "Noto Sans", fill: white)

#align(center + horizon)[
  #text(size: 48pt, weight: "bold")[#data.title]

  #text(size: 36pt, weight: "bold")[#data.discount_text]

  #v(20pt)

  #text(size: 20pt)[Code: #text(weight: "bold")[#data.code]]

  #text(size: 14pt)[Expires: #data.expiry_date]

  #if "description" in data and data.description != none [
    #v(10pt)
    #text(size: 14pt)[#data.description]
  ]
]
