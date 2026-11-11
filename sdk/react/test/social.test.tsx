// @vitest-environment jsdom
// Threads, reactions and pinned messages in the components (docs/en/design/messaging.md):
// thread-only replies stay out of the conversation, the root shows "N replies" and opens the
// thread, which answers in it; reaction chips and the picker; the pinned bar.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { Observable } from '@agentimhq/client-web';
import { AgentIM, Chat, PinnedMessages, Thread, onlyInThread } from '../src/index.js';

afterEach(cleanup);

let n = 0;
function msg(key: string, content: string, extra: any = {}) {
  return { key, msg_id: key, conv_id: 'c1', content, status: 'sent', state: 'committed', sender_id: 'u_bob', sender_name: 'Bob', sender_type: 'human', version: 1, created_seq: ++n, order: n, ...extra };
}

function fake(messages: any[], answers: Record<string, any> = {}) {
  const view = Object.assign(new Observable<any[]>(messages), { hasOlder: false, loadOlder: vi.fn() });
  const listeners = new Map<string, Set<(e: any) => void>>();
  const client: any = {
    connection: new Observable<any>({ state: 'connected' }),
    conversations: new Observable<any[]>([]),
    messages: vi.fn(() => view),
    send: vi.fn(async () => ({ client_msg_id: 'x', key: 'local:x' })),
    edit: vi.fn(),
    recall: vi.fn(),
    retry: vi.fn(),
    discard: vi.fn(),
    feedback: vi.fn(),
    respond: vi.fn(),
    read: vi.fn(async () => ({})),
    call: vi.fn(async (op: string) => answers[op] ?? {}),
    on: (t: string, f: (e: any) => void) => {
      let s = listeners.get(t);
      if (!s) listeners.set(t, (s = new Set()));
      s.add(f);
      return () => s!.delete(f);
    },
    emit: (t: string, e: any) => act(() => listeners.get(t)?.forEach((f) => f(e))),
  };
  return { client, view };
}

describe('threads', () => {
  it('keeps thread-only replies out of the conversation and opens the thread from its root', async () => {
    const root = msg('r', 'plan for friday?', { thread: { replies: 2, last_seq: 9 } });
    const { client } = fake(
      [root, msg('t1', 'in the thread', { thread_id: 'r' }), msg('t2', 'shown too', { thread_id: 'r', thread_shown: true })],
      { pull_thread: { root, replies: [msg('t1', 'in the thread', { thread_id: 'r' })] } },
    );
    render(
      <AgentIM client={client} me="u_me">
        <Chat convId="c1" />
      </AgentIM>,
    );
    expect(screen.queryByText('in the thread')).toBeNull();
    expect(screen.getByText('shown too')).toBeTruthy();
    await act(async () => fireEvent.click(screen.getByText('2 replies')));
    await waitFor(() => expect(screen.getByText('in the thread')).toBeTruthy());
    expect(client.call).toHaveBeenCalledWith('pull_thread', { conv_id: 'c1', thread_id: 'r', limit: 200 });
    // Opening it reads it.
    expect(client.call).toHaveBeenCalledWith('read_thread', expect.objectContaining({ thread_id: 'r' }));
    // Answering goes into the thread, optionally also to the conversation.
    fireEvent.change(screen.getByPlaceholderText('Reply…'), { target: { value: 'works for me' } });
    fireEvent.click(screen.getByLabelText('Also send to the conversation'));
    await act(async () => fireEvent.click(screen.getByText('Send')));
    expect(client.send).toHaveBeenCalledWith('c1', 'works for me', { threadId: 'r', alsoInConv: true });
    // A new reply in this thread reloads it; back to the conversation.
    const before = client.call.mock.calls.length;
    client.emit('event', { event: { conv_id: 'c1', type: 'msg.created', msg: { thread_id: 'r' } } });
    await waitFor(() => expect(client.call.mock.calls.length).toBeGreaterThan(before));
    fireEvent.click(screen.getByLabelText('Close'));
    expect(screen.getByText('plan for friday?')).toBeTruthy();
  });

  it('shows a recalled reply as deleted', async () => {
    const { client } = fake([], { pull_thread: { root: null, replies: [msg('t', '', { state: 'recalled' })] } });
    render(
      <AgentIM client={client} me="u_me">
        <Thread convId="c1" threadId="r" />
      </AgentIM>,
    );
    await waitFor(() => expect(screen.getByText('This message was deleted')).toBeTruthy());
    expect(screen.getByText('1 reply')).toBeTruthy();
  });

  it('tells thread-only replies apart', () => {
    expect(onlyInThread(msg('a', 'x', { thread_id: 'r' }) as any)).toBe(true);
    expect(onlyInThread(msg('a', 'x', { thread_id: 'r', thread_shown: true }) as any)).toBe(false);
    expect(onlyInThread(msg('a', 'x') as any)).toBe(false);
  });
});

describe('reactions', () => {
  it('shows chips, toggles mine and adds one from the picker', async () => {
    const m = msg('m', 'lunch?', { reactions: [{ key: '👍', count: 2, me: true }, { key: '🎉', count: 1, me: false }] });
    const { client } = fake([m]);
    render(
      <AgentIM client={client} me="u_me">
        <Chat convId="c1" />
      </AgentIM>,
    );
    const mine = screen.getByLabelText('👍 2, including you');
    expect(mine.getAttribute('aria-pressed')).toBe('true');
    await act(async () => fireEvent.click(mine));
    expect(client.call).toHaveBeenCalledWith('unreact', { conv_id: 'c1', msg_id: 'm', key: '👍' });
    await act(async () => fireEvent.click(screen.getByLabelText('🎉 1')));
    expect(client.call).toHaveBeenCalledWith('react', { conv_id: 'c1', msg_id: 'm', key: '🎉' });
    fireEvent.click(screen.getByLabelText('Add reaction'));
    await act(async () => fireEvent.click(screen.getByRole('menuitem', { name: '👀' })));
    expect(client.call).toHaveBeenCalledWith('react', { conv_id: 'c1', msg_id: 'm', key: '👀' });
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('offers no reactions on a message that is not sent yet, nor on a recalled one', () => {
    const { client } = fake([msg('local:x', 'sending', { msg_id: undefined, status: 'sending', mine: true }), msg('gone', '', { state: 'recalled', reactions: [{ key: '👍', count: 1, me: false }] })]);
    render(
      <AgentIM client={client} me="u_me">
        <Chat convId="c1" />
      </AgentIM>,
    );
    expect(screen.queryByLabelText('Add reaction')).toBeNull();
    expect(screen.queryByLabelText('👍 1')).toBeNull();
  });
});

describe('pins', () => {
  it('shows the latest pin and the count, and follows pin events', async () => {
    const pins = [{ msg_id: 'p2', msg: { content: 'ship friday' } }, { msg_id: 'p1', msg: { content: 'rules' } }];
    const { client } = fake([], { list_pins: { pins, count: 2 } });
    const onOpen = vi.fn();
    render(
      <AgentIM client={client} me="u_me">
        <PinnedMessages convId="c1" onOpen={onOpen} />
      </AgentIM>,
    );
    await waitFor(() => expect(screen.getByText('ship friday')).toBeTruthy());
    fireEvent.click(screen.getByLabelText('2 pinned messages'));
    expect(onOpen).toHaveBeenCalledWith(pins);
    const before = client.call.mock.calls.length;
    client.emit('event', { event: { conv_id: 'c1', type: 'msg.unpinned', data: { msg_id: 'p2' } } });
    client.emit('event', { event: { conv_id: 'other', type: 'msg.pinned' } });
    await waitFor(() => expect(client.call.mock.calls.length).toBe(before + 1));
  });

  it('shows nothing without pins, or when the call fails', async () => {
    const { client } = fake([]);
    client.call = vi.fn(async () => {
      throw Object.assign(new Error('offline'), { code: 'unavailable' });
    });
    const { container } = render(
      <AgentIM client={client} me="u_me">
        <PinnedMessages convId="c1" />
      </AgentIM>,
    );
    await waitFor(() => expect(client.call).toHaveBeenCalled());
    expect(container.querySelector('.aim-pinned')).toBeNull();
  });
});
