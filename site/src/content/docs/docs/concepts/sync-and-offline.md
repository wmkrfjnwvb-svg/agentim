---
title: "Sync and offline"
description: "How every device gets every event exactly once and in order, what the client SDKs keep offline, and how much history a new device or member sees."
sidebar:
  order: 3
---

## Push is a hint, sync is the truth

The server pushes each event to the devices that are online. Pushes can be lost: a phone sleeps, a network drops, a slow client's queue fills up. Correctness never depends on them.

- **The server keeps each device's position** in each conversation: the last `seq` the device acknowledged. A device does not say where to start. It asks to sync, and the server continues from what it acknowledged.
- **A push is only delivered if it continues exactly** from what this connection already received. Otherwise the device gets `resync` and pulls the rest with its next `sync`. So an event is never both pushed and synced, and never arrives out of order.
- **Acknowledging only moves forward.** A device cannot acknowledge something it was not sent, so a forged acknowledgement can only make it miss messages, never receive more.
- **Flow control:** at most 200 events per conversation per batch, and at most 1,000 unacknowledged per conversation, until the device acknowledges.

The client SDKs do all of this. Your app sees three observable things: the connection, the conversation list, and each conversation's messages.

## What the client SDK keeps

| | |
|---|---|
| **Offline queue** | Sends, edits, recalls, read marks, feedback and card answers are stored before they are sent. They go out once and in order when the connection allows, also after the app restarts. A message is `sending` until the server has it and `failed` only when the server refuses it by its rules (not a member, blocked, …). Network trouble, overload and rate limits are retried, never turned into failures. |
| **Local cache** | Conversations and messages, encrypted on disk (SQLite on devices, IndexedDB in browsers). The conversation list and recent messages show instantly on start, also offline. |
| **Shown at once** | What the app shows is the server's copy with the queue laid over it. An edit shows its new text immediately and is marked `pending` until confirmed; a refused operation simply disappears. |
| **Reconnection** | Exponential backoff with jitter. After two minutes offline it retries about once a minute (`suspended`). It reconnects at once when the network changes or the app returns to the foreground, and waits as long as the server asks after `rate_limited` or `goaway`. |

## History

### New devices

A new device, a reinstall or a cleared cache starts small. It gets the conversation list first, then the most recent 30 messages of each conversation from the last 90 days. Scrolling up loads older pages from the server (50 at a time) and keeps them in the cache. `jumpTo(seq)` and "jump to first unread" load a page around a message.

`hasOlder` becomes false when there is nothing earlier to load, with the reason:

| `olderEnd` | Meaning |
|---|---|
| `start` | The beginning of the conversation |
| `visible_from` | The beginning of what this member may see (they joined later) |
| `retention` | Older events were deleted by the retention period |
| `disabled` | The server does not allow paging back |

### History for new members

People added to a group see the most recent 30 messages from the last 30 days by default. Whoever adds them can choose a different number (`history`, 0 for none, up to 200), and everyone in the conversation sees how much was shared. Members added when the conversation is created see it from the start. Agents see nothing before they joined unless they are given history ([agents](/docs/concepts/agents/)).

The visible start is fixed when someone joins. Changing the server's settings later does not change what existing members can see.

### Server settings

```toml
[sync]
batch_size = 200       # events per conversation per batch
max_unacked = 1000     # unacknowledged events per conversation
max_gap = 10000        # a device further behind starts over like a new one
device_ttl_days = 30   # a device that acknowledged nothing for this long is forgotten
window_days = 90       # how far back a new device syncs (at most 90)
max_messages = 30      # messages per conversation for a new device
member_messages = 30   # history for a new member
member_days = 30
share_max = 200        # the most history someone adding a member may share

[retention]
days = 180             # 0 keeps everything
```

Every option is listed in the [configuration reference](/docs/operations/configuration/).

## Read state, unread counts and muting

- `read` marks a conversation read up to a `seq` on all of the user's devices. Sending a message marks everything before it read.
- The server counts unread messages and mentions per conversation. Edits, recalls of old messages and system events don't count.
- With `read_silences_others` (on by default), a message already read on one device doesn't raise the unread count or notify on the others.
- Muting (`none`, `all` or `mentions`, optionally until a time) is stored on the server and applies to all devices. A muted conversation still syncs and counts unread messages. It just doesn't notify.

## Retention

Events older than `retention.days` are deleted from the start of each log, never from the middle. A device that was away longer than that starts at the oldest event still kept and shows "Earlier messages are no longer available".
