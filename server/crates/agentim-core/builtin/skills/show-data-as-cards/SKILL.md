---
name: show-data-as-cards
description: How to show records (an order, a booking, an account, a list of items) as a data card instead of a paragraph. Use whenever a reply contains several facts about one thing or a small table.
---
# Show data as cards

Use the `show_card` tool when you would otherwise write three or more facts in a sentence. A card is easier to read and shows numbers and dates in the customer's own format.

## Which block
- **facts**: one record as label and value pairs (an order: item, total, status, address). Up to 20 items.
- **table**: several records with the same fields (recent orders, invoices). Up to 8 columns and 50 rows.
- **list**: a few choices or items with a title and subtitle (products, available slots). Up to 20.
- **text**: a short note inside the card, rarely needed.

## Values
- Put numbers as numbers and give a `format`, so they show the viewer's way:
  - `currency:USD` (or any ISO code) for money: `120` not `"$120"`;
  - `date` for a calendar day (`2026-11-08`), `datetime` for a moment (ISO 8601);
  - `percent` for a fraction (`0.4` is 40%).
- Give a status a `tone`: `success` (delivered, paid), `warning` (delayed, pending), `danger` (failed, cancelled), `info` (refunded, in progress), `neutral`.
- Use short labels in the customer's language: "Total", "Status", "Ships to".

## After the card
Write one short sentence, not a repeat of the card: "Here are the details. Anything else I can do for this order?"

## Do not
- Do not put secrets or internal ids the customer does not need into a card.
- Do not make up values to fill a field; leave the field out.

See `references/blocks.md` for the exact shapes.
