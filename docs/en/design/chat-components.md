# Chat components

Data cards, form cards, steps next to a reply, approval cards, the approval inbox, the chat components themselves, and theming. These make an assistant that does work look like part of the product rather than a chat bot.

Code: server `server/crates/agentim-core/src/logic/cards.rs` (card and answer validation), `hub/requests.rs` (`inbox`, `card_answers`), `hub/assistants.rs` (`show_card`, `ask_form`, steps). Web `sdk/react/src/` (`logic.ts` headless hooks, `format.ts`, `cards.tsx`, `steps.tsx`, `inbox.tsx`, `components.tsx`, `styles.css`, `tokens.json`, `theme.css`). React Native `sdk/react-native-ui/src/components.tsx`. Agent SDKs `ctx.showCard`, `ctx.askForm`, `ctx.steps`.

## 1. Purpose

The design aims:
- **Look professional, not "AI"**: neutral panels, the product's own colours, light and dark.
- **Work with no setup**: `<Chat>` draws cards and steps, and `<Inbox>` lists what waits. Built-in assistants use them by default.
- **Easy to extend**: own card blocks, form fields, steps and inbox items, or headless hooks to draw everything.
- **Safe**: cards are declarative JSON from a fixed set of blocks, never scripts. The server checks every card and every answer.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| UI-01 | Cards | A message of kind `agentim.card`, at most 16 KB: `title`, 1–20 `blocks`, optional `to` (only these people answer) and `once` (one answer in all) |
| UI-02 | Data blocks | `text`; `facts` (label/value pairs); `table` (≤ 8 columns, ≤ 50 rows); `list` (≤ 20 items, optionally selectable) |
| UI-03 | Value formats | `format`: `text`, `number`, `currency:XXX`, `date`, `datetime`, `percent`, shown in the viewer's language, region and time zone. `tone` (`neutral`, `success`, `warning`, `danger`, `info`) colours badges |
| UI-04 | Buttons and forms | `buttons` (1–10 options); `form` (1–10 fields: `text`, `textarea`, `number`, `select`, `checkbox`, `date`, `time`, `email`, custom), with `required`, `placeholder`, `hint`, `default`, `pattern`, `min` and `max` |
| UI-05 | Answers | `respond {msg_id, block_id, action | values}`, checked like the components check them. One per person per card; `once` takes one in all; the sender cannot answer; `to` limits who may. An answer is a `card.response` event in the log, so webhooks see it |
| UI-06 | Answered forms stay answered | `card_answers {msg_id}` lists who answered which block. Values go only to the answerer and the sender, so a reload shows "what you sent" |
| UI-07 | Custom blocks and fields | Blocks of type `x-<name>` with `data` (≤ 8 KB) and a `fallback` (text, or `"drop"`); fields of type `x-<name>` with `config`. Clients without a renderer show the fallback |
| UI-08 | Steps | Agents report `progress` of kind `plan` and `step` (`{id, state, title, detail}`); plain `tool_call`/`tool_result` become steps too. Shown above the reply they belong to; open while something runs, waits or failed; folded into "3 steps · 8s" when done |
| UI-09 | Approval cards | A request with `context.operation` shows what, the values, the amount and the rule, with Approve and Decline, then the outcome |
| UI-10 | The inbox | `<Inbox>`: everything waiting for the signed-in person across conversations, with tabs and counts, search, list and detail (two pages on a phone). Operations, drafts (send, send edited, discard), hand-offs (open, hand back), questions. `<InboxBadge>` for navigation |
| UI-11 | Chat components | `<AgentIM>` (provider), `<Chat>`, the conversation list, the support window: grouping, dates, an unread line, edit and delete, images and files, the AI label, RTL |
| UI-12 | Theming | Design tokens (`tokens.json` → CSS variables `--aim-*` and React Native values), light and dark, contrast checked in tests. A live theme preview on the docs site generates the CSS |
| UI-13 | Extension points | `components.cardBlocks`, `components.formFields` (also to redraw built-in types), `components.Steps`, `useSteps`, `useInbox`, `useCardAnswers`, `<Inbox extraItems renderItem types onOpen>`, `strings` |
| UI-14 | Agent tools | Built-in assistants: `show_card`, `ask_form`, steps per tool call. Agent SDKs: `ctx.showCard`, `ctx.askForm` (waits for the answers), `ctx.steps` |
| UI-15 | Threads, reactions and pins | Reaction chips under a message (mine highlighted, a picker of common emoji) and "N replies" under a thread's root; replies only in a thread stay out of the message list; `Thread` shows a thread with its own composer ("also send to the conversation"); `PinnedMessages` shows the latest pin and how many. React and React Native (reactions and replies) ([messaging](messaging.md), MSG-19 to MSG-21) |

## 3. Design

### 3.1 Cards on the server

- `logic/cards.rs` checks the card when it is sent (`check_block`, `check_field`) and each answer (`valid_value`):
  - field types, lengths and option sets;
  - dates as `YYYY-MM-DD` and times as `HH:MM` within `min` and `max`;
  - emails;
  - a `pattern` compiled with a size limit, matched against the whole answer;
  - a `default` must itself be a valid answer.
- `to` takes 1–20 user ids or names, resolved when the card is sent. Anyone else who answers gets `forbidden` (`card.not_for_you`).
- `card_answers` reads the `card.response` events after the card's seq. It returns `{block_id, action?, values?, by, at}` and leaves out other people's values. Errors: `card.unknown`, `msg.unknown`.
- A recalled card refuses answers.

### 3.2 Forms from assistants

- `ask_form {title, intro?, submit?, fields}` sends a card with `to: [asker]`, `once: true` and `client_msg_id = ai-form-{reply}-{call}`.
- The reply then ends, telling the person to fill in the form.
- The answer wakes the assistant once (a claim on `ai-f-{card}-{by}`). The next reply starts from "Maya filled in the form …" and the values.
- The form's step is marked done by reading the card's `client_msg_id` from the `msg.created` event.

### 3.3 Steps

- Data convention:
  - `kind: "plan"`, `data: {steps: [{id, title}]}`;
  - `kind: "step"`, `data: {id, state, title?, detail?}`, where state is `running`, `waiting`, `done`, `failed` or `skipped`.
  - Other fields in `data` are kept for custom drawing.
  - With `msg_id`, the steps sit above that reply; without it, at the end of the conversation.
- The components fold progress into steps in a `StepStore` per client, so every view of a conversation shares them.
- Built-in assistants title a step with the tool's summary, never its route or arguments. A step that waits for approval is `waiting` until the decision.

### 3.4 Formatting

`formatValue(value, format, locale, timeZone)` uses `Intl`:
- currency in the viewer's convention (`$1,200.00` in the US, `1.200,00 $` in Germany);
- `date` as a calendar day, with no time zone shift;
- `datetime` in the viewer's time zone;
- `percent` from a fraction.

`formatAgo` shows relative times in the inbox.

### 3.5 The inbox

- Data comes from the server's `inbox`, refreshed on `request.*`, `draft.*` and hand-off events.
- `useInbox()` exposes `items`, `counts`, `decide`, `answer`, `approveDraft`, `discardDraft` and `handBack`.
- An item someone else decided shows the outcome instead of actions.
- The detail of an operation shows:
  - the amount, large;
  - the values;
  - "why it needs approval" (the rule, for example `amount > 30`);
  - the message that asked for it.

### 3.6 Look and theming

- **Cards** are neutral panels (`data-kind="card"`), not chat bubbles, with tone badges.
- **Tokens** are one source (`sdk/react/tokens.json`) for both web and React Native. `node scripts/tokens.mjs` regenerates `theme.css` and the React Native values.
- **Dark mode** follows `prefers-color-scheme`, or `data-theme` when it is set.
- **Contrast**: the tests check text/background pairs, including `success-soft` and the other tone backgrounds, against WCAG AA.
- **Accent and radius**: a product sets them with a few variables: `--aim-accent`, `--aim-accent-text`, `--aim-accent-soft`, `--aim-focus`, `--aim-radius-card`, `--aim-radius-control`, `--aim-radius-bubble`.

## Screenshots

Taken by the tests (`sdk/react/test/screens.mjs`, `examples/test/e2e.mjs`) from the real components.

| | Light | Dark or phone |
|---|---|---|
| Data cards, a form, steps (UI-01–UI-08) | <img src="../../images/components/cards.png" width="320" alt="An order table, a refund form and steps"> | <img src="../../images/components/cards-dark.png" width="320" alt="The same in dark mode"> |
| The approval inbox (UI-10) | <img src="../../images/components/inbox.png" width="320" alt="The inbox with a refund to approve"> | <img src="../../images/components/inbox-dark.png" width="320" alt="The inbox in dark mode"> |
| The inbox on a phone: list and detail | <img src="../../images/components/inbox-phone.png" width="160" alt="The inbox list on a phone"> | <img src="../../images/components/inbox-phone-detail.png" width="160" alt="An inbox item on a phone"> |
| Approval card in the chat (UI-09) | <img src="../../images/examples/aftersales-customer.png" width="320" alt="A refund the customer confirms"> | <img src="../../images/examples/aftersales-desk.png" width="320" alt="The manager's inbox next to the chat"> |
| Chat (UI-11) | <img src="../../images/components/web-chat-light.png" width="320" alt="The web chat"> | <img src="../../images/components/web-chat-dark.png" width="320" alt="The web chat in dark mode"> |
| Right-to-left, support window | <img src="../../images/components/web-chat-rtl.png" width="320" alt="The chat in a right-to-left language"> | <img src="../../images/components/web-support.png" width="320" alt="The support window"> |
| Files, images and the viewer | <img src="../../images/components/web-files.png" width="320" alt="Image and file messages"> | <img src="../../images/components/web-viewer-phone.png" width="160" alt="The image viewer on a phone"> |
| Offline and sending states | <img src="../../images/components/web-offline.png" width="200" alt="Messages waiting to be sent while offline"> | <img src="../../images/components/web-phone.png" width="160" alt="The chat on a phone"> |
| React Native | <img src="../../images/components/rn-light.png" width="320" alt="React Native list and chat"> | <img src="../../images/components/rn-dark.png" width="320" alt="React Native in dark mode"> |
| Theme preview (UI-12) | <img src="../../images/components/theming.png" width="200" alt="Accent, radius, light and dark, and the CSS"> | <img src="../../images/components/theming-dark.png" width="200" alt="The theme preview in dark mode"> |

## 4. Interfaces

- Protocol: `send` with `kind: "agentim.card"`, `respond`, `card_answers`, `progress`, `inbox`, `decide`, `approve`, `reject`, `handback`.
- Web: `@agentimhq/react` exports `AgentIM`, `Chat`, `Inbox`, `InboxBadge`, `ConversationSteps`, `Card`, the hooks, `formatValue`.
- Headless: `@agentimhq/react/headless`.
- React Native: `@agentimhq/react-native-ui`, with the same components.
- The guide for developers is on the docs site: "Chat components" and "Theme the components".

## 5. Rules and errors

Rule prefix `card.*` ([catalog](../business-rules.md)):
- `bad_request` (`card.input`): an invalid card or answer;
- `forbidden` (`card.not_for_you`): someone outside `to`;
- `conflict`: already answered, or a `once` card answered by someone;
- `bad_request`: a recalled card.

A refused answer writes nothing.

## 6. Tests

| Feature | Tests |
|---|---|
| UI-01–UI-07 (server) | `server/crates/agentim-core/tests/ai.rs`: `data_and_form_cards`, `cards_take_responses`, `race_once_cards_take_one_response`, `card_refusals_leave_no_trace` (three databases) |
| UI-08, UI-14 (server) | `assistant_tools.rs`: `cards_forms_and_steps`; `ai.rs`: `progress_goes_on_the_side`; `mcp.rs`: `agents_use_cards_progress_sources_and_handoff` |
| UI-02–UI-07 (web) | `sdk/react/test/chat-components.test.tsx`: "Data cards" (facts, tables, lists, tones, formats, picking a row, another locale), "Form cards" (checks like the server, read-only for others, answered after a reload), "Your own blocks and fields" |
| UI-08 (web) | `chat-components.test.tsx`: "Steps" (open while working, folded once done, per reply; drawn your way or with `useSteps`); `format.test.ts`: `foldStep` |
| UI-09, UI-10 (web) | `chat-components.test.tsx`: "Inbox" (counts and filters; approve, decline with a reason, edit a draft, hand back, answer; decided by someone else; empty; own items and details) |
| UI-03 | `sdk/react/test/format.test.ts`: `formatValue` in several locales and time zones |
| UI-11, UI-12 | `sdk/react/test/components.test.tsx` (components); `sdk/react/test/format.test.ts` and `scripts/tokens.mjs` (contrast of every token pair); `sdk/react/test/e2e.mjs` (against a real server); `sdk/react/test/screens.mjs` (screenshots in light, dark, phone, RTL compared with baselines in CI) |
| React Native | `sdk/react-native-ui/test/chat-components.test.tsx`: data cards, form cards, own blocks and fields, steps, inbox and badge; `components.test.tsx` |
| UI-14 (SDKs) | `sdk/agent-node/e2e.mjs`, `sdk/agent-python/e2e.py`: `showCard`, `askForm` with answers, `steps` |
| UI-15 | `sdk/react/test/social.test.tsx`: all 7 tests; `sdk/react-native-ui/test/components.test.tsx`: `keeps thread-only replies out, shows the reply count and toggles reactions` |
| End to end | `examples/test/e2e.mjs`: the after-sales example (order card, refund form, steps, approvals in the chat and in the inbox, hand-off) and the advisor example (drafts in the inbox) |

## 7. Limits and next steps

- Not built: document previews in the chat, a full support desk component (the after-sales example has a small one), and MCP Apps.
