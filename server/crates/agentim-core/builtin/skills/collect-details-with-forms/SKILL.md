---
name: collect-details-with-forms
description: How to ask for several details at once with a form (a reason, a date, an email) instead of a back-and-forth. Use when you need two or more answers, or one answer that must have a fixed format.
---
# Collect details with a form

Use the `ask_form` tool when you need two or more pieces of information, or one that must be valid (a date, an email, one of a few choices). The form is checked before it is sent, only the person you are answering can fill it in, and it can be sent once. Their answers start your next reply.

## Design the form
- Ask only for what you need now. Two to four fields is typical; ten is the maximum.
- Prefer a `select` with the real options over free text ("Arrived late", "Damaged", "Not as described", "Changed my mind").
- Mark only what you truly need as `required`; make the rest optional with a placeholder "Optional".
- Use the right type:
  - `date` with `min`/`max` for days (no past dates for a pickup);
  - `time` for times;
  - `email` for an email address;
  - `number` with `min`/`max` for quantities;
  - `textarea` for "Anything else?";
  - `checkbox` for a yes/no consent.
- Give each field a short label and, when useful, a `hint` ("We collect it from your address").
- Give the form a clear `title` ("Refund order 1002"), a one-line `intro`, and a `submit` label that says what happens ("Request refund").

## After sending the form
End your reply with one sentence: "Please fill in the form above, and I'll take it from there." Do not ask the same questions in text.

## When the answers come
Your next reply starts from them. Check they make sense, then act (for example, request the refund) or ask one follow-up question if something is missing.

## Do not
- Never ask for passwords, full card numbers, or one-time codes in a form.
- Do not send a second form for the same thing; refer to the one already sent.

See `references/fields.md` for the field types.
