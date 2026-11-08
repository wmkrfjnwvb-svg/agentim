---
name: handoff-to-a-person
description: When and how to hand a conversation over to a person on the team, and what to write in the hand-off note. Use when you cannot help, the customer asks for a person, or the situation needs judgement.
---
# Hand off to a person

## Hand off when
- The customer asks for a person, a manager, or "someone real". Do it at once; do not try to talk them out of it.
- The customer is upset, has asked the same thing twice, or the conversation is going in circles.
- The request needs something you cannot do or decide: an exception to a policy, a complaint, a legal or medical question, a security or account-ownership issue, or anything about safety.
- A tool failed twice, or the answer is not in your knowledge and guessing could cause harm.

## How
1. Tell the customer in one sentence that you are passing them to a person and that the person will answer here. Do not promise a time unless you were given one.
2. Hand off with a note (the `handoff_to_person` tool, or `handoff` in the SDKs). The hand-off happens after your reply, so write the sentence first.
3. After the hand-off, stay quiet until the conversation is handed back.

## The note
The person reads the note before the conversation. Make it enough to act on without scrolling:

```
Customer: <name> · Order 1002 (Bluetooth speaker, $120)
Wants: a refund; the speaker arrived damaged (photo in chat).
Done so far: looked up the order; refund over $30 needs a manager.
Why handed off: customer asked for a manager.
```

- Facts only. No opinions about the customer.
- Include ids (order, ticket, account) and what you already tried.
- Never include passwords, full card numbers or other secrets, even if the customer sent them.

## When it is handed back
Read what the person wrote, do not repeat it, and continue from where they left off.
