// The customer's page: the shop's chat. Refunds up to $30 show a card here
// to confirm (ApprovalCards, part of Chat); larger ones wait for a manager.
import { Chat } from '@agentimhq/react';
import { mount } from '../lib/session.js';

mount('maya', (s) => (
  <>
    <header className="ex-top">
      <strong>Acme</strong>
      <span className="ex-muted">Signed in as {s.name}</span>
    </header>
    <main className="ex-layout ex-single">
      <p className="ex-muted">
        Your orders: <b>1001</b> wireless headphones ($25) and <b>1002</b> a Bluetooth speaker ($120). Ask where one is, change its address, ask
        for a refund, or ask for a person.
      </p>
      <div className="ex-chat">
        <Chat convId={s.conv} />
      </div>
    </main>
  </>
));
