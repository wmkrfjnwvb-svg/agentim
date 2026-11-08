---
title: "Permissions"
description: "What end users may do in your app, per app and per user, and how roles work inside a conversation."
sidebar:
  order: 10
---

## Your app decides what users may do

| Key | Default | Allows a user to |
|---|---|---|
| `create_groups` | `true` | Create groups |
| `create_direct` | `true` | Open direct conversations |
| `add_members` | `true` | Add people to conversations they may manage |
| `find_users` | `true` | Look people up (`find_user`) |
| `create_agents` | **`false`** | Create their own agents and manage their keys |
| `edit_minutes` | no limit | Edit their own messages for this long |
| `recall_minutes` | no limit | Recall their own messages for this long |

Set the defaults in the [console](/docs/operations/dashboard/) (App settings) or with `PUT /admin/v1/app/settings/clients`. The configuration file's `[clients.allow]` is only the first value. Changes apply on every node at once.

Override them for one user from your backend:

```http
PATCH /v1/users/maya
{"permissions": {"create_agents": true}}
```

`null` for a key, or for the whole object, goes back to the defaults. The client learns its permissions when it connects (`me.permissions`) and gets `user.updated` when they change, so the app can show or hide buttons.

- A refused request is `forbidden` ("the app does not allow this: …") and writes nothing.
- Your backend acting **as the user** (`"as": "maya"`) follows the same rules. Acting **as `system`** does not.
- Agents follow their owner's permissions.
- Time limits only apply to a person's own messages. Owners and admins recalling others' messages are not limited.
- With `create_agents` taken away, the user's agents keep running. Supervising them (stop, approve, take over) always stays possible: that is the owner's safety duty.

## Roles in a conversation

| | Owner | Admin | Member |
|---|---|---|---|
| Send, edit and recall own messages | ✓ | ✓ | ✓ |
| Add members | ✓ | ✓ | if the app allows it |
| Remove members | ✓ | ✓ (not the owner or admins) | agents they added |
| Recall others' messages | ✓ | ✓ | |
| Change roles, transfer ownership, dissolve | ✓ | | |
| Change a team agent's policy | ✓ | ✓ | |

## The open API

API keys have scopes ([open API](/docs/reference/open-api/)). Reading message history needs its own scope (`messages:read`) because it reveals private conversations, and every read is audited. Exporting and deleting user data needs `compliance`. Neither is granted by default.
