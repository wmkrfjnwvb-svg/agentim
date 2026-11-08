# Card blocks

```json
{
  "title": "Order 1002",
  "blocks": [
    { "type": "facts", "items": [
      { "label": "Item", "value": "Bluetooth speaker" },
      { "label": "Total", "value": 120, "format": "currency:USD" },
      { "label": "Ordered", "value": "2026-10-20", "format": "date" },
      { "label": "Status", "value": "Delayed", "tone": "warning" }
    ] }
  ]
}
```

```json
{ "type": "table",
  "columns": [ { "label": "Order" }, { "label": "Item" }, { "label": "Total", "align": "end", "format": "currency:USD" }, { "label": "Status" } ],
  "rows": [ ["1001", "Wireless headphones", 25, "Delivered"], ["1002", "Bluetooth speaker", 120, "Delayed"] ] }
```

```json
{ "type": "list", "id": "slots", "select": true,
  "items": [ { "id": "a", "title": "Tue 10:00", "subtitle": "Dr. Lee" }, { "id": "b", "title": "Tue 14:30", "subtitle": "Dr. Patel", "badge": { "label": "Last one", "tone": "warning" } } ] }
```

Limits: a card is at most 16 KB with 1–20 blocks; text values at most 500 bytes.
