---
name: actions-with-approval
description: How to use tools that change things (refunds, cancellations, address changes, bookings) when some calls wait for a person to approve. Use before calling any tool that writes.
---
# Actions that may need approval

Some tools only read (look up an order). Others change things (refund, cancel, change an address). A call that changes something may wait for a person to approve it: the customer, a manager, or the agent's owner, depending on the rules. You do not decide who approves; the server does.

## Before you call a write tool
1. Make sure you have the right object: look it up first (order, booking, account) and check it matches what the customer said.
2. Make sure the customer wants it. If anything is unclear (which order, how much, which address), ask first. For a refund, know the reason; use a form if several details are needed.
3. Use the exact values from the lookup, not from memory: the order id, the amount, the currency.

## When the call is waiting
The tool answers that it is **not done yet** and who must approve it.
- Tell the customer plainly what happens now, for example:
  - "Please confirm the refund in the card below; I'll make it once you approve."
  - "Refunds this large need a manager to approve them. I've asked our team; you'll see the answer here."
- Never say it is done, never give a refund id or a date, and never call the tool again to "try again". The approval card is the only path.

## After the decision
- **Approved**: the server runs the call with the values that were approved and gives you the result. Report the result in one or two sentences, with the reference (refund id, new address) and what the customer should expect next.
- **Declined**: say it was not done, give the reason if there is one, and offer the next step (a person, another option).
- **Expired or cancelled**: say it was not done and offer to try again.

## Never
- Never change the values between asking and running (another amount, another order).
- Never split one action into smaller calls to get under an approval limit.
- Never call a write tool the customer did not ask for.

See `references/examples.md` for full exchanges.
