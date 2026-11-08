---
title: "Chat components"
description: "Data cards, form cards, steps next to a reply and the approval inbox: what they look like, how an agent uses them, and how to extend them."
sidebar:
  order: 2
---

Four components make an assistant that does things look like part of your product rather than a chat bot:

| | What the person sees | Who sends it |
|---|---|---|
| **Data cards** | An order as label and value pairs, records in a table, a short list | `show_card` (built-in agents), `ctx.showCard` (agent SDKs), or any card message |
| **Form cards** | A few fields at once (a reason, a date, an email), checked before sending, then shown as what was sent | `ask_form`, `ctx.askForm`, or a card with a `form` block |
| **Steps** | "Look up order ✓ → Refund $120 · waiting for Sam" next to the reply, folded into one line when done | Built-in agents for each tool call; `ctx.steps` in the agent SDKs |
| **Approval inbox** | Everything waiting for this person across conversations: operations to approve, drafts, hand-offs, questions | `<Inbox>`, from the server's `inbox` |

They work with no setup: `<Chat>` draws cards and steps, `<Inbox>` lists what waits. Built-in agents use cards, forms and steps unless you turn them off (console › Built-in agents, or the assistant's `settings`: `cards`, `forms`, `steps`, `handoff`). The look is yours: see [Theme the components](/docs/guides/theming/).

## Cards

A card is a message of kind `agentim.card` whose payload is JSON from a fixed set of blocks (never scripts), at most 16 KB:

```json
{
  "title": "Order 1002",
  "to": ["u_maya"],
  "once": true,
  "blocks": [
    { "type": "facts", "items": [
      { "label": "Total", "value": 120, "format": "currency:USD" },
      { "label": "Status", "value": "Delayed", "tone": "warning" }
    ] },
    { "type": "form", "id": "refund", "submit": "Request refund", "fields": [
      { "id": "reason", "label": "Reason", "type": "select", "options": ["Arrived late", "Damaged"], "required": true },
      { "id": "pickup", "label": "Pickup date", "type": "date", "min": "2026-11-08", "hint": "We collect it from your address" }
    ] }
  ]
}
```

| Card field | Meaning |
|---|---|
| `title` | Up to 100 bytes |
| `blocks` | 1 to 20 blocks |
| `to` | Only these people (ids or names, 1 to 20) may answer; others see it read-only. Anyone else answering gets `forbidden` (`card.not_for_you`) |
| `once` | The card takes one answer in all |

### Blocks

| Block | Fields | Limits |
|---|---|---|
| `text` | `text` | 1 byte to 4 KB |
| `facts` | `items: [{label, value, format?, tone?}]` | 1 to 20 items; a value is text (≤ 500 bytes), a number, true/false or null |
| `table` | `columns: [{label, align?, format?}]`, `rows: [[value, …]]`, `caption?` | 1 to 8 columns, 1 to 50 rows, one value per column, 8 KB of text in all |
| `list` | `items: [{id, title, subtitle?, meta?, badge?: {label, tone?}}]`, `select?`, `id` | 1 to 20 items; with `select: true` (and an `id`) a person picks a row: the answer's `action` is the row's id |
| `buttons` | `id`, `options: [{id, label, style?}]` | 1 to 10 options; `style` is `primary` or `danger` |
| `form` | `id`, `submit`, `fields` | 1 to 10 fields |
| `x-…` | `data?`, `fallback`, `id?` | Your own block, see below |

**`format`** (how a value is shown, in the viewer's language and region): `text`, `number`, `currency:XXX` (any ISO 4217 code), `date` (`YYYY-MM-DD`, a calendar day), `datetime` (ISO 8601, in the viewer's time zone), `percent` (`0.4` is 40%). The same card shows `$1,200.00` in the US and `1.200,00 $` in Germany.

**`tone`** (the colour of a badge): `neutral`, `success`, `warning`, `danger`, `info`.

### Form fields

| `type` | Extra fields | The answer |
|---|---|---|
| `text` | `max` (≤ 1000), `pattern` (a regular expression the whole answer matches), `error` (shown when it doesn't) | text |
| `textarea` | `max` (≤ 4000) | text |
| `number` | `min`, `max` | a number |
| `select` | `options` (1 to 10) | one of the options |
| `checkbox` | | true or false |
| `date` | `min`, `max` (`YYYY-MM-DD`) | `YYYY-MM-DD` |
| `time` | `min`, `max` (`HH:MM`) | `HH:MM` |
| `email` | | an email address |
| `x-…` | `config` (≤ 1 KB) | any JSON (≤ 1 KB) |

Every field takes `id`, `label`, `required`, `placeholder`, `hint` and `default` (which must itself be a valid answer). The components check answers the way the server does before sending; the server checks again and answers `bad_request` (`card.input`).

### Answers

An answer goes back to the card's sender as the conversation event `card.response` (`msg_id`, `block_id`, `action` or `values`, `by`). Because it is in the conversation log, a [webhook](/docs/reference/events/) subscribed to `card.*` gets it too: your backend can handle a form without any agent.

A form that was answered is shown as what was sent, also after a reload: the components read `card_answers` (`{op: "card_answers", msg_id}`), which lists who answered which block and when; the values only to the person who answered and the card's sender.

## Steps

Agents report progress on the side (`progress`); the components fold it into steps. The data follows a small convention:

- `kind: "plan"`, `data: {steps: [{id, title}]}` lists the steps to come.
- `kind: "step"`, `data: {id, state, title?, detail?}` sets one; `state` is `running`, `waiting`, `done`, `failed` or `skipped`. Anything else in `data` is kept for your own drawing.
- `tool_call` and `tool_result` without ids become a step each.

With `msg_id` (one of the agent's own messages) the steps show above that reply; without, at the end of the conversation. A list is open while something runs, waits or failed, and folds into "3 steps · 8s" once done. Built-in agents report each tool call as a step titled with the tool's summary, never its arguments; a call waiting for approval is `waiting` until it is decided.

## The approval inbox

`<Inbox>` lists what waits for the signed-in person across conversations, newest first: operations an agent wants to run (with the operation card: what, the values, the amount and the rule that requires approval, and the message that asked for it), agents' drafts (send, send edited, discard), conversations handed to them (open, hand back) and questions (answer or choose). Tabs count each kind; a search filters. On a narrow screen the list and the details are two pages. `<InboxBadge>` is the count for your navigation.

The server's `inbox` (`{op: "inbox", limit?}`, 1 to 500, default 100, with `more`) gives each item its `conv_title`, `agent_name` and, for operations, `asker_name`. A request that names its deciders is shown only to them, not to the agent's owner.

## Built-in agents

| Tool | What it does |
|---|---|
| `show_card` | Shows `text`, `facts`, `table` or `list` blocks as a card in reply |
| `ask_form` | Sends a form only the person being answered can fill in, once. Their answers start the next reply, with "Maya filled in the form …" and each value |
| `handoff_to_person` | Hands the conversation to the agent's owner after the reply |

## Agent SDKs

```js
agent.onMessage(async (msg, ctx) => {
  const s = await ctx.steps(['Look up order', 'Refund']);
  await s.start(0);
  const order = await shop.order(msg.content);
  await s.done(0);
  await ctx.showCard(`Order ${order.id}`, [{ type: 'facts', items: [{ label: 'Total', value: order.total, format: 'currency:USD' }] }]);
  const v = await ctx.askForm('Refund', [{ id: 'reason', label: 'Reason', type: 'select', options: ['Late', 'Damaged'], required: true }]);
  await s.wait(1, 'Waiting for approval');
  // …
});
```

Python has the same: `ctx.steps`, `ctx.show_card`, `ctx.ask_form` (it waits for the answers and returns them; `timeout` in seconds).

## Extending

| Hook | How |
|---|---|
| Your own card block | A block of type `x-<name>` (e.g. `x-acme.chart`) with `data` (≤ 8 KB) and `fallback` (text, or `"drop"`). Register a component in `components.cardBlocks['x-acme.chart']`; clients without it show the fallback. With an `id` it can be answered (`action` or `values`, ≤ 1 KB) |
| Your own form field | A field of type `x-<name>` with `config`; register `components.formFields['x-<name>']`. You can also register a built-in type to draw it your way, e.g. a native date picker for `date` in React Native |
| Steps your way | `components.Steps`, or `useSteps(convId, msgId)` for your own drawing |
| The inbox in your own to-do list | `useInbox()` (items, counts, `decide`, `answer`, `approveDraft`, `discardDraft`, `handBack`), or `<Inbox extraItems={…} renderItem={…} types={…} onOpen={…}>` |
| Approvals from any agent | Your own agent's `request_human` with `context.operation` (`name`, `description`, `fields`, `amount`) shows the same operation card |
| Texts and look | `strings` and the theme variables |
