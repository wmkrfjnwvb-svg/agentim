// The AgentIM demo (docs/12 §3.4): the browser SDK and the React
// components, plus what only the demo shows: agent controls, approvals,
// whispers and the developer view.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { AgentIM, AIBadge, Composer, ConnectionBanner, ConversationList, HandoffNotice, MessageList, TypingIndicator, defaultComponents, type MessageProps } from '@agentimhq/react';
import { AgentIMProvider, useConnection, useConversations } from '@agentimhq/react/hooks';
import { remembered, signIn, signOut, type Session } from './session';
import { useTimeline } from './timeline';
import { Details } from './details';
import { Back, Code, Info, Logo, LogOut } from './icons';

const PEOPLE = [
  { name: 'Alice', about: 'Owns the AI agents Aria, RefundBot and PingA' },
  { name: 'Bob', about: 'A customer, and owner of PongB' },
  { name: 'Carol', about: 'A teammate in the product group' },
];

export function Root() {
  const [session, setSession] = useState<Session | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const go = useCallback(async (name: string) => {
    setBusy(name);
    setError(null);
    try {
      setSession(await signIn(name));
    } catch (e: any) {
      setError(e?.message ?? String(e));
    } finally {
      setBusy(null);
    }
  }, []);
  useEffect(() => {
    const name = remembered();
    if (name) void go(name);
  }, [go]);
  if (!session) return <SignIn onPick={go} busy={busy} error={error} />;
  return (
    <Workspace
      key={session.me.id}
      session={session}
      onSignOut={async () => {
        await signOut(session);
        setSession(null);
      }}
    />
  );
}

function SignIn({ onPick, busy, error }: { onPick(name: string): void; busy: string | null; error: string | null }) {
  const Avatar = defaultComponents.Avatar;
  return (
    <main className="signin">
      <div className="signin-card">
        <div className="brand">
          <Logo />
          <span>AgentIM</span>
        </div>
        <h1>Try the demo</h1>
        <p className="lede">Chat with people and AI agents in one place. Sign in as a demo person, then open a second window as someone else to see both sides.</p>
        <div className="people" role="list">
          {PEOPLE.map((p) => (
            <button key={p.name} type="button" role="listitem" className="person" onClick={() => onPick(p.name)} disabled={!!busy} aria-busy={busy === p.name || undefined}>
              <Avatar id={p.name} name={p.name} size={40} />
              <span className="person-text">
                <span className="person-name">Continue as {p.name}</span>
                <span className="person-about">{p.about}</span>
              </span>
              <span className="person-go" aria-hidden="true">
                {busy === p.name ? <span className="spinner" /> : '›'}
              </span>
            </button>
          ))}
        </div>
        {error ? (
          <p className="signin-error" role="alert">
            {error}
          </p>
        ) : null}
        <p className="fine">Demo mode: anyone can sign in as anyone, so don’t share anything real here.</p>
      </div>
    </main>
  );
}

/** The client for the hooks; the Shell adds the components' settings. */
function Workspace({ session, onSignOut }: { session: Session; onSignOut(): void }) {
  return (
    <AgentIMProvider client={session.client}>
      <Shell session={session} onSignOut={onSignOut} />
    </AgentIMProvider>
  );
}

/** Shows the run data (seq, version, hop) under each message: replaces the Message part. */
function DevMessage(p: MessageProps) {
  const m = p.message;
  return (
    <>
      <defaultComponents.Message {...p} />
      <div className="dev-line">
        seq {m.created_seq ?? '–'} · v{m.version ?? 0}
        {m.hop !== undefined ? ` · hop ${m.hop}` : ''}
        {m.trust ? ` · trust ${m.trust}` : ''}
        {m.state_reason ? ` · ${m.state_reason}` : ''}
      </div>
    </>
  );
}

function Shell({ session, onSignOut }: { session: Session; onSignOut(): void }) {
  const { client, me } = session;
  const convs = useConversations();
  const conn = useConnection();
  const [open, setOpen] = useState<string | undefined>();
  // Details beside the chat on a wide screen; on a narrow one it covers the chat, so it starts closed.
  const [details, setDetails] = useState(() => matchMedia('(min-width: 1181px)').matches);
  const [dev, setDev] = useState(false);
  const [drafts, setDrafts] = useState<any[]>([]);
  const [toast, setToast] = useState<string | null>(null);

  const names = useMemo(() => {
    const m = new Map<string, string>([[me.id, me.name]]);
    for (const c of convs) for (const x of c.members ?? []) m.set(x.user_id, x.name);
    return m;
  }, [convs, me]);
  const name = useCallback((id: string | undefined) => (id ? (names.get(id) ?? id.slice(0, 8)) : ''), [names]);

  const refreshDrafts = useCallback(() => {
    client
      .call('list_drafts')
      .then((r: any) => setDrafts(r.drafts ?? []))
      .catch(() => {});
  }, [client]);
  useEffect(() => {
    if (conn.state === 'connected') refreshDrafts();
  }, [conn.state, refreshDrafts]);
  const onSide = useCallback(
    (ev: any) => {
      if (String(ev.type).startsWith('draft')) refreshDrafts();
    },
    [refreshDrafts],
  );
  const timeline = useTimeline(client, me.id, name, onSide);

  // Open the first conversation on a wide screen.
  useEffect(() => {
    if (!open && convs.length && matchMedia('(min-width: 900px)').matches) setOpen(convs[0].conv_id);
  }, [open, convs]);

  const say = useCallback((t: string) => {
    setToast(t);
    window.setTimeout(() => setToast((x) => (x === t ? null : x)), 5000);
  }, []);
  // Requests from the panels: errors are shown, not thrown.
  const act = useCallback(
    async (op: string, args: Record<string, unknown>) => {
      try {
        return await client.call(op, args);
      } catch (e: any) {
        say(e?.message ?? e?.code ?? String(e));
        return undefined;
      }
    },
    [client, say],
  );

  const conv = convs.find((c) => c.conv_id === open);
  const members: any[] = conv?.members ?? [];
  const agents = members.filter((m) => m.kind === 'agent').length;

  return (
    <AgentIM client={client} me={me.id} components={dev ? { Message: DevMessage } : undefined}>
      <div className="shell" data-pane={open ? 'chat' : 'list'} data-details={(details && !!conv) || undefined}>
        <aside className="sidebar" aria-label="Conversations">
          <header className="sidebar-head">
            <Logo />
            <span className="product">AgentIM</span>
            <span className="demo-pill">Demo</span>
          </header>
          <h2 className="sidebar-title">Messages</h2>
          <ConversationList selected={open} onSelect={setOpen} />
          <footer className="me">
            <defaultComponents.Avatar id={me.id} name={me.name} size={32} />
            <span className="me-text">
              <span className="me-name">{me.name}</span>
              <span className="me-status" data-state={conn.state}>
                <span className="dot" aria-hidden="true" />
                {conn.state === 'connected' ? 'Online' : conn.state === 'connecting' ? 'Connecting…' : 'Offline'}
              </span>
            </span>
            <button type="button" className="icon-button" onClick={() => setDev((d) => !d)} aria-pressed={dev} title="Developer view: show seq, version and hop">
              <Code />
              <span className="sr">Developer view</span>
            </button>
            <button type="button" className="icon-button" onClick={onSignOut} title="Sign out">
              <LogOut />
              <span className="sr">Sign out</span>
            </button>
          </footer>
        </aside>

        <main className="chat">
          {conv ? (
            <>
              <header className="chat-head">
                <button type="button" className="icon-button back" onClick={() => setOpen(undefined)} title="All conversations">
                  <Back />
                  <span className="sr">All conversations</span>
                </button>
                <defaultComponents.Avatar id={conv.conv_id} name={conv.title} size={36} />
                <div className="chat-head-text">
                  <h1 className="chat-title">{conv.title}</h1>
                  <div className="chat-sub">
                    {members.length} {members.length === 1 ? 'member' : 'members'}
                    {agents ? (
                      <>
                        {' · '}
                        <AIBadge /> {agents === 1 ? '1 agent' : `${agents} agents`}
                      </>
                    ) : null}
                  </div>
                </div>
                <button type="button" className="icon-button" onClick={() => setDetails((d) => !d)} aria-pressed={details} title="Details">
                  <Info />
                  <span className="sr">Details</span>
                </button>
              </header>
              <ConnectionBanner />
              <HandoffNotice convId={conv.conv_id} />
              <MessageList convId={conv.conv_id} notices={timeline.notices(conv.conv_id)} />
              <TypingIndicator convId={conv.conv_id} />
              <Composer convId={conv.conv_id} />
            </>
          ) : (
            <div className="chat-empty">
              <Logo />
              <h1>Pick a conversation</h1>
              <p>Your conversations with people and AI agents show up on the left.</p>
            </div>
          )}
        </main>

        {conv && details ? (
          <aside className="details" aria-label="Details">
            <Details conv={conv} me={me.id} name={name} drafts={drafts} convs={convs} act={act} onDrafts={refreshDrafts} onClose={() => setDetails(false)} />
          </aside>
        ) : null}

        {toast ? (
          <div className="toast" role="alert">
            {toast}
          </div>
        ) : null}
      </div>
    </AgentIM>
  );
}
