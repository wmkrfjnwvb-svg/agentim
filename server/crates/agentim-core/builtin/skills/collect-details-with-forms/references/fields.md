# Form fields

| type | extra | answer |
|---|---|---|
| `text` | `max` (≤ 1000), `pattern` (whole answer), `error` | text |
| `textarea` | `max` (≤ 4000) | text |
| `number` | `min`, `max` | number |
| `select` | `options` (1–10) | one option |
| `checkbox` | | true or false |
| `date` | `min`, `max` (`YYYY-MM-DD`) | `YYYY-MM-DD` |
| `time` | `min`, `max` (`HH:MM`) | `HH:MM` |
| `email` | | an email address |

Every field: `id`, `label`, `required`, `placeholder`, `hint`, `default` (must itself be valid).

```json
{ "title": "Refund order 1002", "intro": "Two quick questions, then I'll start the refund.", "submit": "Request refund",
  "fields": [
    { "id": "reason", "label": "Reason", "type": "select", "options": ["Arrived late", "Damaged", "Not as described", "Changed my mind"], "required": true },
    { "id": "details", "label": "Anything else?", "type": "textarea", "placeholder": "Optional" }
  ] }
```
