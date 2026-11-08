// An agent's steps next to its reply (docs/en/design/chat-components.md): the `progress` side
// events of a conversation folded into steps. While something runs or
// waits the list is open; once all is done it folds into one line
// ("3 steps · 8s") that opens on a click. `useSteps` gives the steps for
// your own drawing; `components.Steps` replaces the list.

import { useEffect, useState } from 'react';
import { useSteps } from './logic.js';
import type { Strings } from './strings.js';
import type { Step } from './format.js';
import { Check } from './icons.js';

/** What a replacement `Steps` gets. */
export interface StepsProps {
  steps: Step[];
  strings: Strings;
}

const active = (s: Step) => s.state === 'running' || s.state === 'waiting' || s.state === 'pending';

function StateMark({ state }: { state: Step['state'] }) {
  if (state === 'done') return <Check />;
  if (state === 'failed')
    return (
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
    );
  if (state === 'skipped')
    return (
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="M4 8h8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
    );
  return <span className="aim-step-dot" aria-hidden="true" />;
}

/** The default list. */
export function DefaultSteps({ steps, strings }: StepsProps) {
  const running = steps.some(active);
  const failedAny = steps.some((s) => s.state === 'failed');
  // Open while working, or when something failed (so the reason shows).
  const [open, setOpen] = useState(running || failedAny);
  useEffect(() => {
    if (running || failedAny) setOpen(true);
  }, [running, failedAny]);
  if (!steps.length) return null;
  const first = Math.min(...steps.map((s) => s.at));
  const lastAt = Math.max(...steps.map((s) => s.at));
  const secs = Math.max(0, Math.round((lastAt - first) / 1000));
  const failed = failedAny;
  return (
    <div className="aim-steps" data-open={open || undefined} data-running={running || undefined} data-failed={failed || undefined}>
      {!running ? (
        <button type="button" className="aim-steps-toggle" aria-expanded={open} onClick={() => setOpen((x) => !x)}>
          <span className="aim-steps-summary">{strings.stepsSummary(steps.length, secs)}</span>
          <svg className="aim-steps-chevron" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path d="M5 6l3 3 3-3" stroke="currentColor" strokeWidth="1.6" fill="none" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
      ) : null}
      {open ? (
        <ol className="aim-steps-list" aria-label={strings.stepsTitle} aria-live="polite">
          {steps.map((s) => (
            <li key={s.id} className="aim-step" data-state={s.state}>
              <span className="aim-step-mark">
                <StateMark state={s.state} />
              </span>
              <span className="aim-step-body">
                <span className="aim-step-title" dir="auto">
                  {s.title}
                </span>
                {s.detail || s.state === 'waiting' || s.state === 'failed' ? (
                  <span className="aim-step-detail" dir="auto">
                    {s.detail ?? strings.stepStates[s.state]}
                  </span>
                ) : null}
              </span>
              <span className="aim-sr">{strings.stepStates[s.state]}</span>
            </li>
          ))}
        </ol>
      ) : null}
    </div>
  );
}
