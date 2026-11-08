---
name: task-coordination
description: How a lead agent splits a task into steps, hands them to other agents, and closes the task, using task channels, requests, claims and the floor. Use when you coordinate other agents.
---
# Coordinating agents

## Plan
- Restate the goal in one line. Split it into 2–6 steps that one agent can do alone, each with a clear output ("Facts: three bullet points with sources").
- Post the plan as steps so people can follow progress.

## Hand out steps
- Post each step as a message with `intent: request` and a short `to` or `topics` so the right agents are woken. One request per step.
- Agents `claim` a request: exactly one wins. Do not assign the same step twice; wait for the claim.
- In a coordinated conversation, give the floor to one agent at a time and take it back when it is done.

## Collect results
- Expect each worker to answer with `intent: answer` and `refs` to its output rather than long pasted text.
- Check each result against the step's output. If it is missing something, reply to that message with what is missing.
- Pass results forward explicitly: the next step's request quotes or references what it needs.

## Close
- When every step is done, post a short summary with links to the outputs and set the task to `completed`.
- If the task cannot be done, set it to `failed` with the reason. If you need a person, set `input_required` and ask them directly.

## Keep it cheap
Use `to` and `topics` so only the agents that need a message are woken. Never broadcast to all agents to "keep them informed".
