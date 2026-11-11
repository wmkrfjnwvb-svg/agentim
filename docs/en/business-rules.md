# Business rule catalog

<!-- Generated from the `rule!` calls in the code by server/crates/agentim-core/tests/rules.rs. Do not edit: AGENTIM_WRITE_RULES=1 cargo test --test rules -->

Every business check is a numbered rule ([tracing and business rules](design/tracing.md)). A refused request carries the rule in its error (`error.rule` in the client protocol, `rule` in the open API, `structuredContent.error.rule` in MCP), and the metric `agentim_refusals_total{op, code, rule}` counts refusals by rule. Codes: `forbidden` not allowed or not a member, `not_found` unknown or gone, `conflict` the state does not allow it now, `bad_request` invalid input or not applicable.

Stage: ③ session layer (before the writer, `hub`); ⑦ writer (in the same transaction as the write, `logic`); file store. The text is the error message in the code (`{}` is the actual value).

450 rules.

## Conversations (`conv.*`, 20)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `conv.assignee_not_member` | `bad_request` | ③ session | the assignee must be a member of the task | `hub.rs` |
| `conv.assignee_only_task` | `bad_request` | ③ session | only a task has an assignee | `hub.rs` |
| `conv.create_rate` | `rate_limited` | ③ session | too many new conversations (agents.conversations_per_hour) | `hub.rs` |
| `conv.dissolve_not_owner` | `forbidden` | ⑦ writer | only the owner dissolves a conversation | `logic.rs` |
| `conv.dm_fixed_members` | `bad_request` | ⑦ writer | a direct conversation has fixed members; a direct conversation has fixed members; dissolve it instead | `logic.rs` |
| `conv.dm_no_owner` | `bad_request` | ⑦ writer | a direct conversation has no owner to transfer | `logic.rs` |
| `conv.dm_no_roles` | `bad_request` | ⑦ writer | a direct conversation has no roles | `logic.rs` |
| `conv.dm_no_title` | `bad_request` | ⑦ writer | a direct conversation has no title | `logic.rs` |
| `conv.dm_one_other` | `bad_request` | ③ session | a direct conversation has exactly one other member | `hub.rs` |
| `conv.kind` | `bad_request` | ③ session | kind must be dm, group or task | `hub.rs` |
| `conv.owner_is_person` | `bad_request` | ⑦ writer | only a person can own a conversation | `logic.rs` |
| `conv.system_via_open_api` | `forbidden` | ③ session | the system creates conversations through the open API | `hub.rs` |
| `conv.title` | `bad_request` | ③ session, ⑦ writer | title must be 1..255 bytes | `hub.rs` `logic.rs` |
| `conv.too_many_members` | `bad_request` | ③ session | at most {cap} members | `hub.rs` |
| `conv.transfer_not_owner` | `forbidden` | ⑦ writer | only the owner transfers ownership | `logic.rs` |
| `conv.transfer_self` | `conflict` | ⑦ writer | you are already the owner | `logic.rs` |
| `conv.unknown` | `not_found` | ③ session, ⑦ writer | conversation; no such conversation; not found | `hub.rs` `hub/compliance.rs` `logic.rs` `logic/routing.rs` |
| `conv.unknown_database` | `bad_request` | ③ session | unknown database {target} | `hub.rs` |
| `conv.update_not_admin` | `forbidden` | ⑦ writer | only the owner or an admin changes the conversation | `logic.rs` |
| `conv.update_nothing` | `bad_request` | ⑦ writer | nothing to change | `logic.rs` |

## Members (`member.*`, 19)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `member.add_history` | `bad_request` | ⑦ writer | history must be 0..{} | `logic.rs` |
| `member.add_not_admin` | `forbidden` | ⑦ writer | only the owner or an admin can add members | `logic.rs` |
| `member.admin_is_person` | `bad_request` | ⑦ writer | only people can be admins | `logic.rs` |
| `member.group_full` | `conflict` | ⑦ writer | the group is full ({cap} members) | `logic.rs` |
| `member.mute_mode` | `bad_request` | ⑦ writer | mode must be none, all or mentions | `logic.rs` |
| `member.mute_until` | `bad_request` | ⑦ writer | until must be in the future | `logic.rs` |
| `member.owner_leaves` | `conflict` | ⑦ writer | the owner must transfer ownership or dissolve the conversation | `logic.rs` |
| `member.remove_not_allowed` | `forbidden` | ⑦ writer | you cannot remove this member | `logic.rs` |
| `member.remove_owner` | `conflict` | ⑦ writer | transfer ownership before removing the owner | `logic.rs` |
| `member.required` | `forbidden` | ③ session, ⑦ writer | not a member; not a member of this conversation; you are not a member of this conversation | `hub.rs` `hub/compliance.rs` `logic.rs` |
| `member.role_not_owner` | `forbidden` | ⑦ writer | only the owner changes roles | `logic.rs` |
| `member.role_use_transfer` | `conflict` | ⑦ writer | use transfer_owner to change the owner | `logic.rs` |
| `member.role_value` | `bad_request` | ⑦ writer | role must be admin or member | `logic.rs` |
| `member.side_no_access` | `forbidden` | ③ session | no access | `hub.rs` |
| `member.silence_reason` | `bad_request` | ③ session | reason must be 1..500 bytes | `hub/developer.rs` |
| `member.silence_seconds` | `bad_request` | ③ session | seconds is 1..{MAX_SILENCE_S} | `hub/developer.rs` |
| `member.silence_system` | `bad_request` | ③ session | the system user is not silenced | `hub/developer.rs` |
| `member.silenced` | `forbidden` | ⑦ writer | silenced in this conversation until {u} (ms since 1970) | `logic.rs` |
| `member.unknown` | `not_found` | ⑦ writer | no such member; not a member of this conversation | `logic.rs` `logic/tasks.rs` |

## Tenants (`tenant.*`, 12)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `tenant.cluster` | `bad_request` | ③ session | no database cluster named {name:?} in [[tenancy.clusters]] | `hub/silos.rs` |
| `tenant.connections` | `rate_limited` | ③ session | this project allows {max} connections at once | `hub/tenants.rs` |
| `tenant.day` | `bad_request` | ③ session | day is YYYY-MM-DD | `hub/tenants.rs` |
| `tenant.disabled` | `conflict` | ③ session | this server has one tenant; set [tenancy] enabled = true | `admin_api.rs` |
| `tenant.exists` | `conflict` | ③ session, ⑦ writer | a tenant with this id exists; the tenant exists | `hub/tenants.rs` `logic.rs` |
| `tenant.id` | `bad_request` | ③ session | a tenant id is 1..{} of a-z 0-9 -, not starting or ending with -, and not `default` | `hub/tenants.rs` |
| `tenant.limits` | `bad_request` | ③ session | limits is a JSON object | `hub/tenants.rs` |
| `tenant.mau` | `rate_limited` | ③ session | this project has reached its {max} monthly active users; new people can sign in next month or after an upgrade | `hub/tenants.rs` |
| `tenant.name` | `bad_request` | ③ session | a tenant name is 1..255 bytes | `hub/tenants.rs` |
| `tenant.nothing` | `bad_request` | ③ session | nothing to change | `hub/tenants.rs` |
| `tenant.rate` | `rate_limited` | ③ session | this project is sending too many requests | `hub/tenants.rs` |
| `tenant.unknown` | `not_found` | ③ session, ⑦ writer | no such tenant | `admin_api.rs` `hub/silos.rs` `hub/tenants.rs` `logic.rs` |

## Tenant webhooks (`webhook.*`, 7)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `webhook.events` | `bad_request` | ③ session | events: like msg.created, msg.* or * (1..32 of them) | `hub/tenant_webhooks.rs` |
| `webhook.limit` | `conflict` | ⑦ writer | a tenant has at most {max} webhooks | `logic.rs` |
| `webhook.name` | `bad_request` | ③ session | name: 1..64 of A-Z a-z 0-9 _ - | `hub/tenant_webhooks.rs` |
| `webhook.retries` | `bad_request` | ③ session | max_attempts 1..20, timeout_ms 100..30000, backoff_ms at most 300000 | `hub/tenant_webhooks.rs` |
| `webhook.secret` | `bad_request` | ③ session | secret: at least 16 bytes (whsec_ + base64, or text) | `hub/tenant_webhooks.rs` |
| `webhook.unknown` | `not_found` | ⑦ writer | no such webhook | `logic.rs` |
| `webhook.url` | `bad_request` | ③ session | url: {why} | `hub/tenant_webhooks.rs` |

## Blocks (`block.*`, 5)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `block.blocked` | `blocked` | ③ session, ⑦ writer | you cannot message this user | `hub.rs` `logic.rs` |
| `block.refuses_conversations` | `blocked` | ③ session | {} does not accept conversations from you | `hub.rs` |
| `block.refuses_invitations` | `blocked` | ⑦ writer | this user does not accept invitations from you | `logic.rs` |
| `block.self` | `bad_request` | ③ session | you cannot block yourself | `hub.rs` |
| `block.unblock_first` | `blocked` | ③ session | unblock this user first | `hub.rs` |

## Friends (`friend.*`, 12)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `friend.already` | `conflict` | ⑦ writer | you are friends already | `logic.rs` |
| `friend.blocked` | `forbidden` | ⑦ writer | one of them blocked the other; one of you blocked the other | `logic.rs` |
| `friend.message` | `bad_request` | ③ session | a request's message is at most {FRIEND_MESSAGE_MAX} characters | `hub/friends.rs` |
| `friend.no_request` | `not_found` | ⑦ writer | no request from this user | `logic.rs` |
| `friend.nobody` | `forbidden` | ⑦ writer | this user does not take friend requests | `logic.rs` |
| `friend.not_friends` | `not_found` | ⑦ writer | you are not friends | `logic.rs` |
| `friend.pending` | `conflict` | ⑦ writer | your request is waiting for an answer | `logic.rs` |
| `friend.people_only` | `forbidden` | ③ session | friends are for people; agents are added as friends | `hub.rs` `hub/friends.rs` |
| `friend.policy` | `bad_request` | ③ session | friend_policy is anyone, confirm or nobody | `hub/friends.rs` |
| `friend.requests_limit` | `rate_limited` | ⑦ writer | at most {} requests waiting at once | `logic.rs` |
| `friend.required` | `forbidden` | ③ session | the app lets you talk only with friends: add them first | `hub/developer.rs` |
| `friend.self` | `bad_request` | ③ session | you cannot be your own friend | `hub/friends.rs` |

## Messages (`msg.*`, 28)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `msg.app_window` | `forbidden` | ⑦ writer | the app does not allow this: messages are {what} within {m} minutes | `logic.rs` |
| `msg.card_needs_payload` | `bad_request` | ⑦ writer | a card needs a payload | `logic.rs` |
| `msg.causation_other_conv` | `bad_request` | ⑦ writer | causation message is in another conversation | `logic.rs` |
| `msg.causation_unknown` | `bad_request` | ⑦ writer | unknown causation message | `logic.rs` |
| `msg.edit_file` | `bad_request` | ⑦ writer | the file of a message cannot be changed; send a new one | `logic.rs` |
| `msg.edit_not_allowed` | `forbidden` | ⑦ writer | cannot edit this message | `logic.rs` |
| `msg.edit_state` | `conflict` | ⑦ writer | message is {} | `logic.rs` |
| `msg.edit_text_payload` | `bad_request` | ⑦ writer | text messages have no payload | `logic.rs` |
| `msg.edit_version` | `conflict` | ⑦ writer | the message changed since (now version {}) | `logic.rs` |
| `msg.envelope_not_directed` | `bad_request` | ⑦ writer | an envelope is for messages in the conversation, not to_user | `logic.rs` |
| `msg.feedback_agents_only` | `bad_request` | ⑦ writer | feedback is for agents' messages | `logic/ai.rs` |
| `msg.feedback_own` | `bad_request` | ⑦ writer | one does not rate one's own message | `logic/ai.rs` |
| `msg.feedback_rating` | `bad_request` | ⑦ writer | rating is up, down or null | `logic/ai.rs` |
| `msg.feedback_reason` | `bad_request` | ⑦ writer | a reason (1..500 bytes) goes with a rating | `logic/ai.rs` |
| `msg.files_not_directed` | `bad_request` | ⑦ writer | files go to the conversation, not to_user | `logic.rs` |
| `msg.json_object` | `bad_request` | ⑦ writer | {what} must be a JSON object | `logic.rs` |
| `msg.json_size` | `bad_request` | ⑦ writer | {what} is larger than {max} bytes | `logic.rs` |
| `msg.kind_namespace` | `bad_request` | ⑦ writer | custom kinds are namespaced like com.example.order (a-z, 0-9, _ and -, at most 64 bytes) | `logic.rs` |
| `msg.kind_reserved` | `bad_request` | ⑦ writer | the agentim. namespace is reserved | `logic.rs` |
| `msg.on_behalf_not_owner` | `forbidden` | ⑦ writer | on_behalf_of must be the agent's owner | `logic.rs` |
| `msg.payload_needs_kind` | `bad_request` | ⑦ writer | payload needs a custom kind | `logic.rs` |
| `msg.policy` | `policy_blocked` | ⑦ writer | — | `logic.rs` |
| `msg.recall_done` | `conflict` | ⑦ writer | message is already recalled | `logic.rs` |
| `msg.recall_not_allowed` | `forbidden` | ⑦ writer | cannot recall this message | `logic.rs` |
| `msg.sources` | `bad_request` | ⑦ writer | — | `logic/routing.rs` |
| `msg.text_nul` | `bad_request` | ⑦ writer | text must not contain NUL | `logic.rs` |
| `msg.text_size` | `bad_request` | ⑦ writer | text must be 1..64KB | `logic.rs` |
| `msg.unknown` | `not_found` | ③ session | message; no such message | `hub.rs` `hub/compliance.rs` `hub/msgtrace.rs` `hub/requests.rs` |

## Threads (`thread.*`, 7)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `thread.nested` | `bad_request` | ⑦ writer | threads do not nest: reply in the root's thread instead | `logic/social.rs` |
| `thread.root_other_conv` | `bad_request` | ⑦ writer | the thread's root is in another conversation | `logic/social.rs` |
| `thread.root_recalled` | `conflict` | ⑦ writer | the thread's root was recalled; the thread's root was recalled; reject the draft | `logic.rs` `logic/social.rs` |
| `thread.root_system` | `bad_request` | ⑦ writer | a system message cannot start a thread | `logic/social.rs` |
| `thread.root_unknown` | `not_found` | ⑦ writer | no such message to reply to in a thread | `logic/social.rs` |
| `thread.shown_without_thread` | `bad_request` | ⑦ writer | also_in_conv goes with thread_id | `logic/social.rs` |
| `thread.unknown` | `not_found` | ③ session, ⑦ writer | no such thread | `hub/social.rs` `logic/social.rs` |

## Reactions (`reaction.*`, 6)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `reaction.key_invalid` | `bad_request` | ⑦ writer | a reaction is one emoji or one of the app's :names: | `logic/social.rs` |
| `reaction.not_reactable` | `bad_request` | ⑦ writer | system messages take no reactions | `logic/social.rs` |
| `reaction.recalled` | `conflict` | ⑦ writer | the message was recalled | `logic/social.rs` |
| `reaction.too_many_keys` | `conflict` | ⑦ writer | a message takes at most {} different reactions | `logic/social.rs` |
| `reaction.too_many_per_user` | `conflict` | ⑦ writer | at most {} reactions per person on a message | `logic/social.rs` |
| `reaction.unknown_message` | `not_found` | ③ session, ⑦ writer | no such message | `hub/social.rs` `logic/social.rs` |

## Pinned messages (`pin.*`, 9)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `pin.agents` | `forbidden` | ⑦ writer | agents do not pin messages in this app (messages.agents_pin) | `logic/social.rs` |
| `pin.forbidden` | `forbidden` | ⑦ writer | only the owner and admins pin messages here | `logic/social.rs` |
| `pin.limit` | `conflict` | ⑦ writer | a conversation holds at most {max} pinned messages; unpin one first | `logic/social.rs` |
| `pin.not_pinnable` | `bad_request` | ⑦ writer | system messages cannot be pinned | `logic/social.rs` |
| `pin.policy` | `bad_request` | ⑦ writer | pin_policy is members or admins | `logic/social.rs` |
| `pin.policy_dm` | `bad_request` | ⑦ writer | in a direct conversation both people pin | `logic/social.rs` |
| `pin.recalled` | `conflict` | ⑦ writer | the message was recalled | `logic/social.rs` |
| `pin.unknown_message` | `not_found` | ⑦ writer | no such message | `logic/social.rs` |
| `pin.unpin_forbidden` | `forbidden` | ⑦ writer | only who pinned it, the owner and admins unpin a message | `logic/social.rs` |

## Threads, reactions and pins: reading (`social.*`, 2)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `social.limit` | `bad_request` | ③ session | limit must be 1..{max} | `hub/social.rs` |
| `social.summary_size` | `bad_request` | ③ session | 1..{SUMMARY_MAX} message ids | `hub/social.rs` |

## Streaming replies (`stream.*`, 5)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `stream.agents_only` | `forbidden` | ⑦ writer | only agents stream | `logic.rs` |
| `stream.cancel_not_allowed` | `forbidden` | ⑦ writer | cannot interrupt this stream | `logic.rs` |
| `stream.concurrent` | `rate_limited` | ⑦ writer | too many concurrent streams in this conversation | `logic.rs` |
| `stream.not_yours` | `forbidden` | ⑦ writer | not your stream | `logic.rs` |
| `stream.unknown` | `not_found` | ③ session, ⑦ writer | no active stream | `hub.rs` `logic.rs` |

## Drafts and approval (`draft.*`, 13)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `draft.agent_silenced` | `conflict` | ⑦ writer | the agent is silenced in this conversation; its drafts wait | `logic.rs` |
| `draft.agent_stopped` | `conflict` | ⑦ writer | the agent is stopped; resume it before publishing its drafts | `logic.rs` |
| `draft.agent_taken_over` | `conflict` | ⑦ writer | the agent is taken over; release it before publishing its drafts | `logic.rs` |
| `draft.decided` | `conflict` | ⑦ writer | draft already decided | `logic.rs` |
| `draft.expired` | `conflict` | ⑦ writer | the draft has expired | `logic.rs` |
| `draft.no_approver` | `conflict` | ⑦ writer | nobody here can approve this agent's messages: none of its approvers is a member | `logic.rs` |
| `draft.not_approver` | `forbidden` | ⑦ writer | you don't approve this agent's messages here | `logic.rs` |
| `draft.not_reviewer` | `forbidden` | ⑦ writer | only the draft's reviewer escalates it | `logic.rs` |
| `draft.note` | `bad_request` | ⑦ writer | a note is 1..500 bytes | `logic.rs` |
| `draft.reviewer_edit` | `bad_request` | ⑦ writer | a reviewer approves the draft as it is | `logic.rs` |
| `draft.reviewer_reject` | `forbidden` | ⑦ writer | a reviewer approves or escalates; people reject | `logic.rs` |
| `draft.stage` | `conflict` | ⑦ writer | the draft is already with the people | `logic.rs` |
| `draft.unknown` | `not_found` | ③ session, ⑦ writer | draft | `hub.rs` `logic.rs` |

## Approval rules (`approval.*`, 8)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `approval.approvers` | `bad_request` | ⑦ writer | up to 20 approvers; {} is not a person in this conversation | `logic.rs` |
| `approval.managers_only` | `forbidden` | ⑦ writer | only the conversation's owner and admins require approval | `logic.rs` |
| `approval.mode` | `bad_request` | ⑦ writer | mode is off, all or rules | `logic.rs` |
| `approval.reviewer` | `bad_request` | ⑦ writer | an agent doesn't review itself; the reviewer is an agent; the reviewer must be in this conversation | `logic.rs` |
| `approval.reviewer_cycle` | `conflict` | ⑦ writer | these two agents would review each other | `logic.rs` |
| `approval.rule` | `bad_request` | ⑦ writer | mode rules needs rules or topics; rules are files, links, mentions, cards, first_message | `logic.rs` |
| `approval.topics` | `bad_request` | ⑦ writer | up to 20 topics of 1..64 bytes | `logic.rs` |
| `approval.ttl` | `bad_request` | ⑦ writer | review_secs is 60..86400; ttl_secs is 60..604800 | `logic.rs` |

## Agents (`agent.*`, 41)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `agent.already_stopped` | `conflict` | ③ session, ⑦ writer | the agent is already stopped | `hub/developer.rs` `logic/lifecycle.rs` |
| `agent.contact_not_allowed` | `forbidden` | ③ session | an agent reaches only its owner, the owner's agents and those it already shares a conversation with; ask your owner to introduce {} | `hub/agents.rs` |
| `agent.daily_budget` | `budget_exceeded` | ⑦ writer | daily agent budget exhausted | `logic.rs` |
| `agent.deactivated` | `conflict` | ③ session | the agent is deactivated | `hub/agents.rs` |
| `agent.destroy_confirm` | `bad_request` | ③ session | confirm with the agent's name: destroying cannot be undone | `hub/lifecycle.rs` |
| `agent.feedback_limit` | `bad_request` | ③ session | limit must be 1..100 | `hub/lifecycle.rs` |
| `agent.guard_not_tripped` | `conflict` | ⑦ writer | the agent's guard is not tripped | `logic.rs` |
| `agent.handback_not_allowed` | `forbidden` | ⑦ writer | only the person it was handed to or its owner hands it back | `logic/ai.rs` |
| `agent.handoff_agents_only` | `forbidden` | ⑦ writer | only agents hand off | `logic/ai.rs` |
| `agent.handoff_owner_absent` | `bad_request` | ⑦ writer | your owner is not in this conversation; name a person (to) | `logic/ai.rs` |
| `agent.handoff_target` | `bad_request` | ⑦ writer | hand off to a person in this conversation | `logic/ai.rs` |
| `agent.handoff_to_person` | `bad_request` | ⑦ writer | hand off to a person, not an agent | `logic/ai.rs` |
| `agent.import_input` | `bad_request` | ③ session | — | `hub/export.rs` |
| `agent.import_state_keys` | `rate_limited` | ③ session | {} keys; at most {max} per agent (agents.state_keys) | `hub/export.rs` |
| `agent.import_unverified` | `forbidden` | ③ session | the package's source cannot be verified (unsigned, or signed by another deployment); import it with allow_unverified to accept it anyway | `hub/export.rs` |
| `agent.name` | `bad_request` | ③ session | agent names are 1..64 of A-Z a-z 0-9 _ - . @ : and do not start with _ | `hub/agents.rs` |
| `agent.not_agent` | `bad_request` | ⑦ writer | not an agent | `logic.rs` |
| `agent.not_handed_off` | `conflict` | ⑦ writer | the agent is not handed off | `logic/ai.rs` |
| `agent.not_owner` | `forbidden` | ③ session, ⑦ writer | not your agent; you do not own this agent | `hub.rs` `hub/agents.rs` `logic.rs` |
| `agent.not_stopped` | `conflict` | ③ session | the agent is not stopped | `hub/developer.rs` |
| `agent.note_size` | `bad_request` | ⑦ writer | a note must be 1 byte..4KB | `logic/ai.rs` |
| `agent.ownerless_adds_nobody` | `forbidden` | ③ session | an agent without an owner adds nobody | `hub.rs` |
| `agent.ownerless_starts_nothing` | `forbidden` | ③ session | an agent without an owner starts nothing | `hub.rs` |
| `agent.people_own` | `forbidden` | ③ session | only people own agents | `hub/agents.rs` |
| `agent.progress_agents_only` | `forbidden` | ⑦ writer | progress is for agents | `logic/ai.rs` |
| `agent.progress_data` | `bad_request` | ⑦ writer | data is at most 8KB | `logic/ai.rs` |
| `agent.progress_kind` | `bad_request` | ⑦ writer | kind is one of {} | `logic/ai.rs` |
| `agent.progress_msg` | `bad_request` | ⑦ writer | msg_id must be your own message in this conversation | `logic/ai.rs` |
| `agent.progress_rate` | `rate_limited` | ⑦ writer | at most {rate} progress reports a second (agents.progress_per_second) | `logic/ai.rs` |
| `agent.progress_text` | `bad_request` | ⑦ writer | text must be 1 byte..4KB | `logic/ai.rs` |
| `agent.root_budget` | `rate_limited` | ⑦ writer | root budget: {limit} agent messages since the last human message; wait for a person | `logic/routing.rs` |
| `agent.stop_reason` | `bad_request` | ③ session | reason must be 1..500 bytes | `hub/developer.rs` `hub/lifecycle.rs` |
| `agent.stopped` | `conflict` | ③ session, ⑦ writer | your owner stopped you; wait until you are resumed | `hub.rs` `logic/lifecycle.rs` |
| `agent.taken_over` | `conflict` | ⑦ writer | already taken over by {by} | `logic.rs` `logic/ai.rs` |
| `agent.team_needs_owner` | `bad_request` | ③ session | only agents with an owner are team agents | `hub.rs` |
| `agent.transfer_same_owner` | `conflict` | ③ session | the agent already belongs to them | `hub/lifecycle.rs` |
| `agent.transfer_team_only` | `bad_request` | ③ session | only team agents change owner; a person's assistant stays theirs | `hub/lifecycle.rs` |
| `agent.transfer_unit` | `bad_request` | ③ session | the new owner must be in the agent's unit | `hub/lifecycle.rs` |
| `agent.trigger_all` | `conflict` | ⑦ writer | all is for direct conversations or at most {MAX_AGENTS_FOR_ALL} agents; use addressed or topics | `logic/routing.rs` |
| `agent.unknown` | `not_found` | ③ session, ⑦ writer | agent; no such agent; the agent is gone | `hub.rs` `hub/agents.rs` `hub/assistants.rs` `hub/developer.rs` `hub/knowledge.rs` `hub/lifecycle.rs` `hub/roles.rs` `logic.rs` `logic/ai.rs` `logic/lifecycle.rs` |
| `agent.unknown_key` | `not_found` | ③ session | no such key | `hub/agents.rs` |

## Agent roles (`role.*`, 11)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `role.catalog` | `not_found` | ③ session | no such built-in role | `hub/library.rs` |
| `role.description` | `bad_request` | ③ session | — | `hub/roles.rs` |
| `role.exists` | `conflict` | ⑦ writer | the library already has a role named {}; install it with replace to update it | `logic.rs` |
| `role.header` | `bad_request` | ③ session | — | `hub/roles.rs` |
| `role.in_use` | `conflict` | ⑦ writer | {} agent(s) have this role ({}); give them another role first | `logic.rs` |
| `role.name` | `bad_request` | ③ session | — | `hub/roles.rs` |
| `role.name_mismatch` | `bad_request` | ③ session | — | `hub/roles.rs` |
| `role.people_assign` | `forbidden` | ③ session | only an agent's owner gives it a role | `hub/roles.rs` |
| `role.people_list` | `forbidden` | ③ session | agents read their own role with get_config | `hub.rs` |
| `role.size` | `bad_request` | ③ session | — | `hub/roles.rs` |
| `role.unknown` | `not_found` | ③ session, ⑦ writer | no such role | `hub/roles.rs` `logic.rs` |

## Agent configuration (`config.*`, 1)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `config.agents_only` | `forbidden` | ③ session | only agents have a configuration | `hub/roles.rs` |

## Model services (`model_service.*`, 16)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `model_service.builtin` | `conflict` | ③ session | a built-in service has this name | `hub/ai.rs` |
| `model_service.builtin_fixed` | `bad_request` | ③ session | built-in services are set in the deployment's configuration | `hub/ai.rs` |
| `model_service.daily_tokens` | `bad_request` | ③ session | daily_tokens is at most 10^12 | `hub/ai.rs` |
| `model_service.in_use` | `conflict` | ⑦ writer | used by {}; give them another service first | `logic.rs` |
| `model_service.key` | `bad_request` | ③ session | api_key is 1..512 characters without spaces; api_key is required for a new service | `hub/ai.rs` |
| `model_service.key_refused` | `bad_request` | ③ session | the provider refused the key: {m} | `hub/ai.rs` |
| `model_service.key_rejected` | `conflict` | ③ session | the provider no longer accepts the key: {m} | `hub/ai.rs` |
| `model_service.limit` | `rate_limited` | ⑦ writer | an app has at most {} model services | `logic.rs` |
| `model_service.name` | `bad_request` | ③ session | names are 1..64 of a-z 0-9 - (not starting or ending with -) | `hub/ai.rs` |
| `model_service.not_ready` | `conflict` | ③ session | the built-in service has no key ({} is not set) | `hub/ai.rs` |
| `model_service.provider` | `bad_request` | ③ session | provider is anthropic, openai, gemini, deepseek or compatible | `hub/ai.rs` |
| `model_service.provider_down` | `conflict` | ③ session | — | `hub/ai.rs` |
| `model_service.refused` | `bad_request` | ③ session | the provider refused the request: {m} | `hub/ai.rs` |
| `model_service.unknown` | `not_found` | ③ session, ⑦ writer | no such model service | `hub/ai.rs` `logic.rs` |
| `model_service.unreachable` | `bad_request` | ③ session | the provider could not be reached to check the key: {m} | `hub/ai.rs` |
| `model_service.url` | `bad_request` | ③ session | a compatible service needs base_url; base_url is too long; base_url: {e} | `hub/ai.rs` |

## Built-in assistants (`assistant.*`, 8)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `assistant.catalog` | `not_found` | ③ session | no such built-in agent | `hub/assistants.rs` |
| `assistant.exists` | `conflict` | ③ session | {id} is taken | `hub/assistants.rs` |
| `assistant.model` | `bad_request` | ③ session | {service} has no model {model} | `hub/assistants.rs` |
| `assistant.model_unpriced` | `bad_request` | ③ session | this model has no known price, so the built-in key can't be used for it | `hub/assistants.rs` |
| `assistant.owner` | `bad_request` | ③ session | owner is a person of this app | `hub/assistants.rs` |
| `assistant.settings` | `bad_request` | ③ session | — | `hub/assistants.rs` |
| `assistant.unknown` | `not_found` | ⑦ writer | this agent is not run by the server | `logic.rs` |
| `assistant.usage_limit` | `bad_request` | ③ session | limit is 1..10000 | `hub/assistants.rs` |

## Agent knowledge files (`knowledge.*`, 9)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `knowledge.empty` | `bad_request` | ③ session | the file has no text | `hub/knowledge.rs` |
| `knowledge.encoding` | `bad_request` | ③ session | text files must be UTF-8 | `hub/knowledge.rs` |
| `knowledge.limit` | `rate_limited` | ⑦ writer | an agent has at most {MAX_FILES} knowledge files | `logic.rs` |
| `knowledge.name` | `bad_request` | ③ session | file names are 1..200 letters, digits, spaces and . _ - ( ) | `hub/knowledge.rs` |
| `knowledge.size` | `bad_request` | ③ session | a file is at most 5 MB | `hub/knowledge.rs` |
| `knowledge.total` | `rate_limited` | ⑦ writer | an agent's knowledge files hold at most 2 million characters of text | `logic.rs` |
| `knowledge.type` | `bad_request` | ③ session | knowledge files are .md, .txt, .csv, .json or .pdf | `hub/knowledge.rs` |
| `knowledge.unknown` | `not_found` | ⑦ writer | no such knowledge file | `logic.rs` |
| `knowledge.unreadable` | `bad_request` | ③ session | the PDF could not be read | `hub/knowledge.rs` |

## Skills (`skill.*`, 14)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `skill.catalog` | `not_found` | ③ session | no such built-in skill | `hub/library.rs` |
| `skill.description` | `bad_request` | ③ session | — | `hub/skills.rs` |
| `skill.exists` | `conflict` | ⑦ writer | the library already has a skill named {}; install it with replace to update it | `logic.rs` |
| `skill.file_encoding` | `bad_request` | ③ session | skill files are UTF-8 text | `hub/skills.rs` |
| `skill.file_size` | `bad_request` | ③ session | a skill's file is at most 512 KB | `hub/skills.rs` |
| `skill.file_unknown` | `not_found` | ③ session, ⑦ writer | no such file in this skill | `hub/skills.rs` `logic.rs` |
| `skill.files` | `rate_limited` | ⑦ writer | a skill has at most {max} files | `logic.rs` |
| `skill.header` | `bad_request` | ③ session | — | `hub/skills.rs` |
| `skill.in_use` | `conflict` | ⑦ writer | the role(s) {} use this skill; remove it from them first | `logic.rs` |
| `skill.name` | `bad_request` | ③ session | — | `hub/skills.rs` |
| `skill.name_mismatch` | `bad_request` | ③ session | — | `hub/skills.rs` |
| `skill.path` | `bad_request` | ③ session | paths are segments of a-z A-Z 0-9 . _ - separated by /, up to 200 characters | `hub/skills.rs` |
| `skill.size` | `bad_request` | ③ session | — | `hub/skills.rs` |
| `skill.unknown` | `not_found` | ③ session, ⑦ writer | no such skill | `hub/skills.rs` `logic.rs` |

## Connections: MCP servers and external APIs (`connection.*`, 18)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `connection.approval_amount` | `bad_request` | ③ session | amount_field and over go together; amount_field is 1..64 characters and over a number of at least 0 | `hub/connections.rs` |
| `connection.approval_tool` | `bad_request` | ③ session | {} is not an operation of this connection | `hub/connections.rs` |
| `connection.approvals` | `bad_request` | ③ session | at most 20 approval rules | `hub/connections.rs` |
| `connection.approvers` | `bad_request` | ③ session | 1..20 approvers; approvers are for operations that ask (not confirm: false); approvers are people | `hub/connections.rs` |
| `connection.down` | `conflict` | ③ session | — | `hub/connections.rs` |
| `connection.header` | `bad_request` | ③ session | a secret needs auth_header (e.g. Authorization); auth_header is a header name (letters, digits, -) | `hub/connections.rs` |
| `connection.in_use` | `conflict` | ⑦ writer | the role(s) {} use this connection; remove it from them first | `logic.rs` |
| `connection.invalid` | `bad_request` | ③ session | — | `hub/connections.rs` |
| `connection.kind` | `bad_request` | ③ session | kind is mcp or api | `hub/connections.rs` |
| `connection.limit` | `rate_limited` | ⑦ writer | an app has at most {max} connections | `logic.rs` |
| `connection.name` | `bad_request` | ③ session | names are 1..64 of a-z 0-9 - (not starting or ending with -) | `hub/connections.rs` |
| `connection.refused` | `bad_request` | ③ session | — | `hub/connections.rs` |
| `connection.secret` | `bad_request` | ③ session | secret is up to 4096 characters without control characters | `hub/connections.rs` |
| `connection.spec` | `bad_request` | ③ session | an api needs its OpenAPI document (spec); spec is for apis | `hub/connections.rs` |
| `connection.unknown` | `not_found` | ③ session, ⑦ writer | no such connection | `hub/connections.rs` `logic.rs` |
| `connection.unreachable` | `bad_request` | ③ session | — | `hub/connections.rs` |
| `connection.url` | `bad_request` | ③ session | url is too long; url: {e} | `hub/connections.rs` |
| `connection.writes` | `bad_request` | ③ session | writes is read_only, confirm or allow | `hub/connections.rs` |

## Asking people to decide (`request.*`, 14)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `request.agents_only` | `forbidden` | ⑦ writer | only agents ask for a decision | `logic/requests.rs` |
| `request.cancel_not_asker` | `forbidden` | ⑦ writer | only the agent that asked cancels | `logic/requests.rs` |
| `request.decided` | `conflict` | ⑦ writer | the request was already decided | `logic/requests.rs` |
| `request.expired` | `conflict` | ⑦ writer | the request has expired | `logic/requests.rs` |
| `request.id_other_conv` | `conflict` | ⑦ writer | client_req_id already used in another conversation | `logic/requests.rs` |
| `request.inbox_limit` | `bad_request` | ③ session | limit is 1..500 | `hub/requests.rs` |
| `request.input` | `bad_request` | ⑦ writer | — | `logic/requests.rs` |
| `request.not_decider` | `forbidden` | ⑦ writer | this request is not yours to decide | `logic/requests.rs` |
| `request.not_yours` | `forbidden` | ③ session | not your request | `hub/requests.rs` |
| `request.ownerless` | `forbidden` | ⑦ writer | an agent without an owner asks nobody | `logic/requests.rs` |
| `request.state` | `conflict` | ⑦ writer | the request is {} | `logic/requests.rs` |
| `request.too_many` | `rate_limited` | ⑦ writer | too many requests waiting for a decision | `logic/requests.rs` |
| `request.unknown` | `not_found` | ③ session | request | `hub.rs` `hub/requests.rs` |
| `request.wait_secs` | `bad_request` | ③ session | wait_secs must be 0..25 | `hub/requests.rs` |

## Tasks, claims and turns (`task.*`, 12)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `task.claimed_already` | `conflict` | ⑦ writer | — | `logic/tasks.rs` |
| `task.claimed_by_other` | `forbidden` | ⑦ writer | claimed by someone else | `logic/tasks.rs` |
| `task.end_not_allowed` | `forbidden` | ⑦ writer | only the creator, a manager or the assignee ends a task | `logic/tasks.rs` |
| `task.floor_free` | `conflict` | ⑦ writer | nobody holds the floor | `logic/tasks.rs` |
| `task.floor_not_manager` | `forbidden` | ⑦ writer | only a manager gives the floor | `logic/tasks.rs` |
| `task.floor_release_not_allowed` | `forbidden` | ⑦ writer | only the holder or a manager releases the floor | `logic/tasks.rs` |
| `task.floor_wait` | `conflict` | ⑦ writer | waiting for the floor (coordinated); answer only what is addressed to you | `logic/tasks.rs` |
| `task.input` | `bad_request` | ⑦ writer | — | `logic/tasks.rs` |
| `task.not_claimed` | `not_found` | ⑦ writer | nobody claimed it | `logic/tasks.rs` |
| `task.reassign_not_allowed` | `forbidden` | ⑦ writer | only the creator or a manager reassigns a task | `logic/tasks.rs` |
| `task.reopen_not_allowed` | `forbidden` | ⑦ writer | only the creator or a manager reopens a task | `logic/tasks.rs` |
| `task.state` | `conflict` | ⑦ writer | the task is already {current}; the task is {current}; the task is {st}; the task is {s} | `logic.rs` `logic/tasks.rs` |

## Cards (`card.*`, 5)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `card.input` | `bad_request` | ⑦ writer | — | `logic/cards.rs` |
| `card.not_for_you` | `forbidden` | ⑦ writer | this card is for someone else | `logic/cards.rs` |
| `card.responded` | `conflict` | ⑦ writer | you already responded to this card | `logic/cards.rs` |
| `card.single_response` | `conflict` | ⑦ writer | this card took its one response | `logic/cards.rs` |
| `card.unknown` | `not_found` | ③ session | no such card | `hub/requests.rs` |

## Files (`file.*`, 32)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `file.daily_bytes` | `rate_limited` | ⑦ writer | the daily upload allowance is used up (files.daily_bytes_per_user) | `logic/files.rs` |
| `file.empty` | `bad_request` | ⑦ writer | the file is empty | `logic/files.rs` |
| `file.ingest_size` | `bad_request` | ③ session | send files over {INGEST_MAX} bytes in parts (upload_start) | `hub/files.rs` |
| `file.max_bytes` | `bad_request` | ⑦ writer | files are at most {} bytes (files.max_bytes) | `logic/files.rs` |
| `file.not_image` | `bad_request` | ⑦ writer | the file is not an image; send it as kind file | `logic/files.rs` |
| `file.not_visible` | `forbidden` | ⑦ writer | you cannot see the message with this file | `logic/files.rs` |
| `file.not_yours` | `forbidden` | ⑦ writer | another member's file | `logic/files.rs` |
| `file.nothing_uploaded` | `bad_request` | file store | nothing was uploaded | `filestore.rs` |
| `file.open_uploads` | `rate_limited` | ⑦ writer | at most {} uploads open at once | `logic/files.rs` |
| `file.other_conv` | `forbidden` | ⑦ writer | the file belongs to another conversation | `logic/files.rs` |
| `file.part_missing` | `bad_request` | file store | part {n} is missing or has the wrong size | `filestore.rs` |
| `file.part_range` | `bad_request` | ⑦ writer | parts are 1..={total} | `logic/files.rs` |
| `file.parts_etags` | `bad_request` | file store | give the ETag of each of the {total} parts | `filestore.rs` |
| `file.parts_unjoinable` | `bad_request` | file store | the parts could not be joined (missing or wrong ETags) | `filestore.rs` |
| `file.payload_caption` | `bad_request` | ⑦ writer | payload.caption is text of at most 4096 bytes | `logic/files.rs` |
| `file.payload_fields` | `bad_request` | ⑦ writer | payload takes file_id and caption | `logic/files.rs` |
| `file.payload_file_id` | `bad_request` | ⑦ writer | payload.file_id is required | `logic/files.rs` |
| `file.payload_needs_file` | `bad_request` | ⑦ writer | a {kind} message needs payload {{file_id}} | `logic/files.rs` |
| `file.size_mismatch` | `bad_request` | file store | the file has {s} bytes, not the {size} declared | `filestore.rs` |
| `file.storage_not_shared` | `conflict` | ⑦ writer | files are off in this cluster: set files.storage = "s3" or a shared folder (files.shared) | `logic/files.rs` |
| `file.thumb_dimensions` | `bad_request` | ③ session | a thumbnail is at most {} pixels on its longest side | `hub/files.rs` |
| `file.thumb_for_images` | `bad_request` | ③ session | a thumbnail is for images | `hub/files.rs` |
| `file.thumb_format` | `bad_request` | ③ session | a thumbnail is a JPEG or PNG | `hub/files.rs` |
| `file.thumb_max_bytes` | `bad_request` | ⑦ writer | a thumbnail is at most {} bytes | `logic/files.rs` |
| `file.thumb_size_mismatch` | `bad_request` | ③ session | the thumbnail was not uploaded with its declared size | `hub/files.rs` |
| `file.unknown` | `not_found` | ⑦ writer | no such file; no such file (expired or never uploaded) | `logic/files.rs` |
| `file.unknown_upload` | `not_found` | ⑦ writer | no such upload | `logic/files.rs` |
| `file.upload_complete` | `conflict` | ⑦ writer | the upload is complete | `logic/files.rs` |
| `file.upload_incomplete` | `not_found` | ⑦ writer | the upload is not complete | `logic/files.rs` |
| `file.upload_not_yours` | `forbidden` | ⑦ writer | another member's upload | `logic/files.rs` |
| `file.used` | `conflict` | ⑦ writer | another message already carries this file | `logic/files.rs` |
| `file.variant` | `bad_request` | ⑦ writer | variant is thumb or original | `logic/files.rs` |

## History, context, search, summaries (`history.*`, 20)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `history.after_or_before` | `bad_request` | ③ session | give after_seq or before_seq, not both | `hub/history.rs` |
| `history.agent_read_args` | `bad_request` | ③ session | agents read with after_seq, before_seq and limit | `hub.rs` |
| `history.agents_only` | `forbidden` | ③ session | reading history is for agents; people sync (docs/en/design/sync-and-clients.md) | `hub/history.rs` |
| `history.anchor` | `bad_request` | ③ session | anchor is a seq (>= 0), "newest" or "first_unread" | `hub/pages.rs` |
| `history.filter_size` | `bad_request` | ③ session | {name} must be 1..64 bytes | `hub/history.rs` |
| `history.limit` | `bad_request` | ③ session | limit must be 1..200 | `hub/history.rs` |
| `history.max_tokens` | `bad_request` | ③ session | max_tokens must be 256..200000 | `hub/history.rs` |
| `history.page_direction` | `bad_request` | ③ session | ask for messages before or after the anchor | `hub/pages.rs` |
| `history.page_size` | `bad_request` | ③ session | before and after are 0..{} | `hub/pages.rs` |
| `history.people_page_args` | `bad_request` | ③ session | people page with anchor, before and after (docs/en/design/sync-and-clients.md) | `hub.rs` |
| `history.rate` | `rate_limited` | ③ session | too many {kind} reads (at most {n} a minute) | `hub/history.rs` |
| `history.search_limit` | `bad_request` | ③ session | limit must be 1..50 | `hub/history.rs` |
| `history.search_query` | `bad_request` | ③ session | query must be 2..200 bytes | `hub/history.rs` |
| `history.sender_type` | `bad_request` | ③ session | sender_type must be human or agent | `hub/history.rs` |
| `history.seq_range` | `bad_request` | ③ session | after_seq must be >= 0 and before_seq >= 1 | `hub/history.rs` |
| `history.since_after_until` | `bad_request` | ③ session | since is after until | `hub/history.rs` |
| `history.summary_agents_only` | `forbidden` | ⑦ writer | summaries are for agents | `logic/history.rs` |
| `history.summary_not_visible` | `forbidden` | ⑦ writer | you can see this conversation from seq {} | `logic/history.rs` |
| `history.summary_range` | `bad_request` | ⑦ writer | need 1 <= from_seq <= to_seq <= {last} | `logic/history.rs` |
| `history.summary_text` | `bad_request` | ⑦ writer | text must be 1 byte..64KB | `logic/history.rs` |

## Checkpoints (`state.*`, 11)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `state.agents_only` | `forbidden` | ③ session | checkpoints are for agents | `hub/state.rs` |
| `state.archive_read_only` | `bad_request` | ③ session | archive/ is read-only | `hub/state.rs` |
| `state.key` | `bad_request` | ③ session | a key is 1..255 bytes without control characters | `hub/state.rs` |
| `state.key_limit` | `rate_limited` | ⑦ writer | at most {max_keys} keys per agent (agents.state_keys) | `logic/state.rs` |
| `state.list_args` | `bad_request` | ③ session | prefix and after are at most 255 bytes | `hub/state.rs` |
| `state.list_limit` | `bad_request` | ③ session | limit must be 1..500 | `hub/state.rs` |
| `state.unknown_key` | `not_found` | ⑦ writer | no key {key} | `logic/state.rs` |
| `state.unknown_version` | `not_found` | ③ session | — | `hub/state.rs` |
| `state.value_size` | `bad_request` | ③ session | a value is at most 1 MiB as JSON | `hub/state.rs` |
| `state.version_conflict` | `conflict` | ⑦ writer | {key} is at version {head}; read it and retry | `logic/state.rs` |
| `state.version_min` | `bad_request` | ③ session | version must be >= 0 (0 creates); version must be >= 1 | `hub/state.rs` |

## Sync (`sync.*`, 6)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `sync.ack_ahead` | `bad_request` | ③ session | {}: acknowledged more than this session was sent | `hub/sync.rs` |
| `sync.ack_count` | `bad_request` | ③ session | 1..1000 acks per call | `hub/sync.rs` |
| `sync.ack_negative` | `bad_request` | ③ session | positions are >= 0 | `hub/sync.rs` |
| `sync.heads_max` | `bad_request` | ③ session | at most {HEADS_MAX} conversations | `hub/sync.rs` |
| `sync.limit` | `bad_request` | ③ session | limit must be 1..{SYNC_LIMIT_MAX} | `hub/sync.rs` |
| `sync.no_pull_by_seq` | `bad_request` | ③ session | reading history by seq was replaced by sync and ack | `hub.rs` |

## Session layer (`session.*`, 2)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `session.people_only` | `forbidden` | ③ session | this is for people | `hub.rs` |
| `session.wrong_writer` | `bad_request` | ⑦ writer | {} is not handled by a conversation writer | `logic.rs` |

## Accounts, devices, tokens (`account.*`, 17)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `account.app_forbids` | `forbidden` | ③ session | the app does not allow this: {what} | `hub/developer.rs` |
| `account.deactivated` | `forbidden` | ③ session, ⑦ writer | the account is deactivated; the user is deactivated; this person is deactivated | `hub.rs` `hub/login.rs` `logic.rs` `logic/files.rs` |
| `account.delete_people_only` | `bad_request` | ③ session | only a person's account is deleted; agents are destroyed | `hub/compliance.rs` |
| `account.device_field` | `bad_request` | ③ session | device_name and platform are 1..64 bytes; {what} must be 1..64 bytes | `hub.rs` `hub/login.rs` |
| `account.device_id` | `bad_request` | ③ session | connect with device=<the id this app chose for the device, 1..64 of A-Z a-z 0-9 _ -> | `hub/login.rs` |
| `account.export_cancelled` | `bad_request` | ③ session | the export was cancelled | `hub/compliance.rs` |
| `account.logins_busy` | `rate_limited` | ③ session | too many logins at once; retry shortly | `hub.rs` |
| `account.name_taken` | `conflict` | ③ session | name taken | `hub.rs` |
| `account.permissions_people_only` | `bad_request` | ③ session | permissions are a person's | `hub/developer.rs` |
| `account.permissions_value` | `bad_request` | ③ session | — | `hub/developer.rs` |
| `account.rate_limited` | `rate_limited` | ③ session | too many {kind} requests; retry in {wait} ms | `hub.rs` |
| `account.reserved_name` | `bad_request` | ③ session | names starting with _ are reserved | `hub.rs` |
| `account.system_no_login` | `bad_request` | ③ session | the system user does not log in | `hub.rs` |
| `account.token_ttl` | `bad_request` | ③ session | ttl must be positive | `hub.rs` |
| `account.unknown_device` | `not_found` | ③ session | device | `hub.rs` |
| `account.unknown_unit` | `bad_request` | ③ session | unknown unit {unit} | `hub.rs` |
| `account.unknown_user` | `not_found` | ③ session, ⑦ writer | no such person; no such user; user; user {key}; user {k} | `hub.rs` `hub/compliance.rs` `hub/connections.rs` `hub/friends.rs` `hub/lifecycle.rs` `logic.rs` `logic/requests.rs` |

## API keys (`apikey.*`, 5)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `apikey.bad_address` | `bad_request` | ③ session | bad address {bad} | `hub.rs` |
| `apikey.name` | `bad_request` | ③ session | name must be 1..64 bytes | `hub.rs` |
| `apikey.no_scope` | `bad_request` | ③ session | at least one scope | `hub.rs` |
| `apikey.ttl` | `bad_request` | ③ session | ttl must be positive | `hub.rs` |
| `apikey.unknown_scope` | `bad_request` | ③ session | unknown scope {s} | `hub.rs` |

## Signing keys (`signing.*`, 4)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `signing.name` | `bad_request` | ③ session | a key name is 1..64 bytes | `hub/login.rs` |
| `signing.none` | `conflict` | ③ session | no signing key: create one with `agentim signing-key create` | `hub/login.rs` |
| `signing.ttl` | `bad_request` | ③ session | ttl is 1..{} seconds | `hub/login.rs` |
| `signing.unknown` | `not_found` | ③ session | no such signing key in force | `hub/login.rs` |

## Reports and moderation (`report.*`, 15)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `report.action` | `bad_request` | ③ session | action must be none, remove_content, suspend_user or ban_user | `hub/compliance.rs` |
| `report.category` | `bad_request` | ③ session | category must be one of {} | `hub/compliance.rs` |
| `report.days` | `bad_request` | ③ session | days must be 1..3650 | `hub/compliance.rs` |
| `report.no_user` | `bad_request` | ③ session | this report names no user | `hub/compliance.rs` |
| `report.own_message` | `bad_request` | ③ session | you cannot report your own message | `hub/compliance.rs` |
| `report.people_only` | `forbidden` | ③ session | only people file reports | `hub.rs` |
| `report.rate` | `rate_limited` | ⑦ writer | too many reports; try again later | `logic/compliance.rs` |
| `report.reason_size` | `bad_request` | ③ session | reason must be 1..2000 characters | `hub/compliance.rs` |
| `report.remove_needs_message` | `bad_request` | ③ session | remove_content applies to a message | `hub/compliance.rs` |
| `report.resolved` | `conflict` | ⑦ writer | this report was already resolved | `logic/compliance.rs` |
| `report.self` | `bad_request` | ③ session | you cannot report yourself | `hub/compliance.rs` |
| `report.suspend_needs_days` | `bad_request` | ③ session | suspend_user needs days | `hub/compliance.rs` |
| `report.target_kind` | `bad_request` | ③ session | target_kind must be message, user or conversation | `hub/compliance.rs` |
| `report.text_size` | `bad_request` | ③ session | text must be at most 1000 characters | `hub/compliance.rs` |
| `report.unknown` | `not_found` | ③ session, ⑦ writer | no such report | `hub/compliance.rs` `logic/compliance.rs` `open_api.rs` |

## Console and admin (`admin.*`, 28)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `admin.client_defaults` | `bad_request` | ③ session | the client defaults are a JSON object | `hub/admin.rs` |
| `admin.client_windows` | `bad_request` | ③ session | edit_minutes and recall_minutes are 0..525600 | `hub/admin.rs` |
| `admin.cluster_input` | `bad_request` | ③ session | — | `hub/admin.rs` |
| `admin.disabled` | `forbidden` | ③ session | the admin is disabled | `hub/admin.rs` |
| `admin.input` | `bad_request` | ③ session | — | `hub/admin.rs` |
| `admin.key_name` | `bad_request` | ③ session | a key name is 1..64 bytes | `hub/admin.rs` |
| `admin.last_owner` | `conflict` | ⑦ writer | the last owner cannot be demoted or disabled | `logic.rs` |
| `admin.login_input` | `bad_request` | ③ session | too long | `hub/admin.rs` |
| `admin.login_rate` | `rate_limited` | ③ session | too many failed logins; retry in {}s | `hub/admin.rs` |
| `admin.master_key_invalid` | `bad_request` | ③ session, ⑦ writer | — | `hub.rs` `logic.rs` |
| `admin.name` | `bad_request` | ③ session | admin names are 1..64 of A-Z a-z 0-9 _ - . @ | `hub/admin.rs` |
| `admin.name_taken` | `conflict` | ⑦ writer | an admin with this name exists | `logic.rs` |
| `admin.not_clustered` | `bad_request` | ③ session | this server is not part of a cluster | `hub/admin.rs` |
| `admin.password` | `bad_request` | ③ session | — | `hub/admin.rs` |
| `admin.role` | `bad_request` | ③ session | role is owner, developer, operator or moderator | `hub/admin.rs` |
| `admin.setup_done` | `conflict` | ③ session | the console is already set up | `hub/admin.rs` |
| `admin.setup_input` | `bad_request` | ③ session | — | `hub/admin.rs` |
| `admin.setup_link` | `forbidden` | ③ session | unknown or used setup link | `hub/admin.rs` |
| `admin.totp_code_replayed` | `conflict` | ⑦ writer | this code was already used | `logic.rs` |
| `admin.totp_code_used` | `bad_request` | ③ session | this code was already used | `hub/admin.rs` |
| `admin.totp_off` | `conflict` | ③ session | two-factor login is off | `hub/admin.rs` |
| `admin.totp_on` | `conflict` | ③ session | two-factor login is already on; turn it off first | `hub/admin.rs` |
| `admin.totp_setup_first` | `conflict` | ③ session, ⑦ writer | run totp_setup first; set up two-factor login first | `hub/admin.rs` `logic.rs` |
| `admin.totp_wrong_code` | `bad_request` | ③ session | wrong code | `hub/admin.rs` |
| `admin.unknown` | `not_found` | ③ session, ⑦ writer | no such admin | `hub/admin.rs` `logic.rs` |
| `admin.unknown_key` | `not_found` | ③ session | no such key in force | `hub/admin.rs` |
| `admin.update_nothing` | `bad_request` | ③ session | nothing to change | `hub/admin.rs` |
| `admin.wrong_password` | `forbidden` | ③ session | the current password is wrong | `hub/admin.rs` |

## Dye rules (`dye.*`, 8)

| Rule | Code | Stage | Text | Code location |
|---|---|---|---|---|
| `dye.duplicate` | `conflict` | ③ session | this target is already dyed | `hub/dye.rs` |
| `dye.kind` | `bad_request` | ③ session | kind must be one of {} | `hub/dye.rs` |
| `dye.limit` | `rate_limited` | ⑦ writer | at most {} dye rules at once; remove one first | `logic.rs` |
| `dye.minutes` | `bad_request` | ③ session | minutes must be 1..{DYE_MAX_MINUTES} (24 hours at most) | `hub/dye.rs` |
| `dye.reason` | `bad_request` | ③ session | a reason of 1..500 bytes is required | `hub/dye.rs` |
| `dye.target` | `bad_request` | ③ session | target must be 1..128 bytes | `hub/dye.rs` |
| `dye.unknown` | `not_found` | ⑦ writer | no such dye rule (it may have expired) | `logic.rs` |
| `dye.unknown_target` | `not_found` | ③ session | no such agent; no such conversation; no such person | `hub/dye.rs` |

