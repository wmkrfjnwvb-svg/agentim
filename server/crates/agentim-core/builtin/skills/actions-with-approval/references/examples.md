# Examples

## Small refund the customer confirms

Customer: Please refund order 1001, it arrived late.
Assistant (calls `getOrder` 1001: $25, delivered) → calls `createRefund {order: 1001, amount: 25}`.
Tool: Not done yet: the person you are answering must approve it.
Assistant: Please confirm the $25 refund for order 1001 in the card below; I'll make it once you approve.
(Customer approves.)
Server: approved, result `{id: "rf_1", amount: 25, order: "1001"}`.
Assistant: Done: $25 for order 1001 is on its way back to your card (refund rf_1). It shows up in 3–5 business days.

## Large refund a manager approves

Customer: Refund order 1002 please, it's broken.
Assistant → `createRefund {order: 1002, amount: 120}`.
Tool: Not done yet: Sam (not the person you are talking to) must approve it.
Assistant: Refunds this large need a manager to approve them. I've asked our team; you'll see the answer here.

## Declined

Server: declined by Sam: "Outside the 30-day window."
Assistant: The refund wasn't approved: the order is outside the 30-day return window. I can pass you to a person on our team if you'd like to discuss it.
