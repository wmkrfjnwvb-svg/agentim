//! Threads, reactions and pinned messages, write side
//! (docs/en/design/messaging.md, MSG-19 to MSG-21). Each change is an event
//! in the conversation's log, written with the rows it changes, so pushes,
//! sync, replay and the verifier treat them like every other event.

use serde_json::{Value, json};

use super::{Ctx, is_manager, not_silenced};
use crate::model::*;
use crate::registry::Out;

/// People shown with a thread's summary and a reaction key.
pub const SUMMARY_PEOPLE: usize = 5;

/// Mutation checks of the race tests (docs/en/testing.md): set to `Error`,
/// each removes one guard, and the race test that covers it must fail.
pub const MUT_REACTION_LIMIT: &str = "mutation.reaction_limit";
pub const MUT_PIN_LIMIT: &str = "mutation.pin_limit";
pub const MUT_RECALL_KEEPS: &str = "mutation.recall_keeps_reactions";
pub const MUT_THREAD_COUNT: &str = "mutation.thread_count";

/// Is `s` one emoji: a single emoji character, keycap, flag, tag sequence
/// or ZWJ sequence of those, with variation selectors and skin tones
/// (Unicode Technical Standard #51, ED-15 to ED-17)?
pub fn is_emoji(s: &str) -> bool {
    fn pictographic(c: u32) -> bool {
        matches!(c,
            0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
            | 0x2194..=0x2199 | 0x21A9..=0x21AA | 0x231A..=0x231B | 0x2328 | 0x23CF
            | 0x23E9..=0x23F3 | 0x23F8..=0x23FA | 0x24C2 | 0x25AA..=0x25AB | 0x25B6
            | 0x25C0 | 0x25FB..=0x25FE | 0x2600..=0x27BF | 0x2934..=0x2935
            | 0x2B05..=0x2B07 | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55 | 0x3030 | 0x303D
            | 0x3297 | 0x3299 | 0x1F000..=0x1F1E5 | 0x1F200..=0x1F3FA | 0x1F400..=0x1FAFF)
    }
    let regional = |c: u32| (0x1F1E6..=0x1F1FF).contains(&c);
    let modifier = |c: u32| (0x1F3FB..=0x1F3FF).contains(&c);
    let cs: Vec<u32> = s.chars().map(|c| c as u32).collect();
    if cs.is_empty() || s.len() > 64 {
        return false;
    }
    // A flag is exactly two regional indicators.
    if cs.len() == 2 && regional(cs[0]) && regional(cs[1]) {
        return true;
    }
    // A keycap: 0-9, # or *, an optional FE0F, then U+20E3.
    if let [k, rest @ ..] = cs.as_slice()
        && (matches!(*k, 0x30..=0x39 | 0x23 | 0x2A))
    {
        return matches!(rest, [0x20E3] | [0xFE0F, 0x20E3]);
    }
    // Elements joined by ZWJ.
    let mut i = 0;
    loop {
        let Some(&c) = cs.get(i) else { return false };
        if !(pictographic(c) || modifier(c)) {
            return false;
        }
        i += 1;
        if cs.get(i) == Some(&0xFE0F) {
            i += 1;
        }
        if cs.get(i).is_some_and(|c| modifier(*c)) {
            i += 1;
        }
        // A tag sequence (subdivision flags): tags, then the cancel tag.
        if c == 0x1F3F4 && cs.get(i).is_some_and(|c| (0xE0020..=0xE007E).contains(c)) {
            while cs.get(i).is_some_and(|c| (0xE0020..=0xE007E).contains(c)) {
                i += 1;
            }
            if cs.get(i) != Some(&0xE007F) {
                return false;
            }
            i += 1;
        }
        match cs.get(i) {
            None => return true,
            Some(0x200D) => i += 1,
            Some(_) => return false,
        }
    }
}

impl Ctx<'_, '_> {
    /// Whether a guard stands (its mutation point is not set).
    async fn guard(&mut self, mutation: &str) -> bool {
        self.sh.failpoints.hit(mutation).await.is_ok()
    }

    // ------------------------------------------------------------- threads

    /// Checks the root of a reply in a thread and returns where the message
    /// goes (MSG-19).
    pub(super) async fn thread_target(
        &mut self,
        conv_id: &str,
        thread_id: Option<String>,
        also_in_conv: bool,
    ) -> ApiResult<Option<ThreadRef>> {
        let Some(id) = thread_id else {
            if also_in_conv {
                return Err(rule!(
                    "thread.shown_without_thread",
                    "bad_request",
                    "also_in_conv goes with thread_id",
                ));
            }
            return Ok(None);
        };
        // A member who joined after the root sees its later replies and may
        // answer there (the root itself stays hidden from them).
        let root = self.db.message(&id).await?;
        let Some(root) = root else {
            return Err(rule!(
                "thread.root_unknown",
                "not_found",
                "no such message to reply to in a thread"
            ));
        };
        if root.conv_id != conv_id {
            return Err(rule!(
                "thread.root_other_conv",
                "bad_request",
                "the thread's root is in another conversation",
            ));
        }
        if root.thread_id.is_some() {
            return Err(rule!(
                "thread.nested",
                "bad_request",
                "threads do not nest: reply in the root's thread instead",
            ));
        }
        if root.sender_type == UserKind::System {
            return Err(rule!(
                "thread.root_system",
                "bad_request",
                "a system message cannot start a thread",
            ));
        }
        if root.state == "recalled" {
            return Err(rule!(
                "thread.root_recalled",
                "conflict",
                "the thread's root was recalled"
            ));
        }
        Ok(Some(ThreadRef {
            id,
            shown: also_in_conv,
        }))
    }

    /// Bookkeeping of a committed reply, before it is pushed: the thread's
    /// summary, and who follows it (the sender, the root's sender and the
    /// people it mentions). Writing counts as reading the thread.
    pub(super) async fn thread_replied(&mut self, msg: &MsgView) -> ApiResult<()> {
        let Some(root_id) = &msg.thread_id else {
            return Ok(());
        };
        let conv = &msg.conv_id;
        if self.guard(MUT_THREAD_COUNT).await {
            self.db
                .thread_replied(
                    conv,
                    root_id,
                    msg.created_seq,
                    msg.created_at,
                    &msg.sender_id,
                )
                .await?;
        }
        if let Some(root) = self.db.message(root_id).await? {
            self.db
                .thread_join(conv, root_id, &root.sender_id, None, None)
                .await?;
        }
        for u in &msg.mentions {
            self.db.thread_join(conv, root_id, u, None, None).await?;
        }
        self.db
            .thread_join(
                conv,
                root_id,
                &msg.sender_id,
                Some(true),
                Some(msg.created_seq),
            )
            .await?;
        Ok(())
    }

    /// The thread a member addresses: the root must be a message they see
    /// that has a thread or can start one.
    async fn thread_root(
        &mut self,
        conv_id: &str,
        user_id: &str,
        thread_id: &str,
    ) -> ApiResult<MsgView> {
        self.require_member(conv_id, user_id).await?;
        let root = self.db.message(thread_id).await?;
        match root {
            Some(r) if r.conv_id == conv_id && r.thread_id.is_none() => Ok(r),
            _ => Err(rule!("thread.unknown", "not_found", "no such thread")),
        }
    }

    pub(super) async fn read_thread(
        &mut self,
        me: &User,
        sid: u64,
        conv_id: &str,
        thread_id: &str,
        seq: i64,
    ) -> ApiResult<Value> {
        self.thread_root(conv_id, &me.id, thread_id).await?;
        // A position past the latest reply would hide replies that come later.
        let last = self
            .db
            .thread(conv_id, thread_id)
            .await?
            .map_or(0, |t| t.last_seq);
        let seq = seq.clamp(0, last);
        self.db
            .thread_join(conv_id, thread_id, &me.id, None, Some(seq))
            .await?;
        let read = self
            .db
            .thread_member(conv_id, thread_id, &me.id)
            .await?
            .map_or(seq, |m| m.read_seq);
        let frame =
            json!({"type": "thread_read", "conv_id": conv_id, "thread_id": thread_id, "seq": read});
        for s in self.sh.reg.others(&me.id, sid) {
            self.out.push(Out::ToSession {
                sid: s,
                frame: frame.clone(),
            });
        }
        Ok(json!({"seq": read}))
    }

    pub(super) async fn follow_thread(
        &mut self,
        me: &User,
        conv_id: &str,
        thread_id: &str,
        follow: bool,
    ) -> ApiResult<Value> {
        self.thread_root(conv_id, &me.id, thread_id).await?;
        let before = self.db.thread_member(conv_id, thread_id, &me.id).await?;
        if before.as_ref().map(|m| m.following) == Some(follow) || (before.is_none() && !follow) {
            return Ok(json!({"following": follow, "changed": false}));
        }
        self.db
            .thread_join(conv_id, thread_id, &me.id, Some(follow), None)
            .await?;
        let frame = json!({"type": "thread_follow", "conv_id": conv_id, "thread_id": thread_id, "following": follow});
        self.out.push(Out::ToUser {
            user_id: me.id.clone(),
            frame,
        });
        Ok(json!({"following": follow, "changed": true}))
    }

    // ---------------------------------------------------- shared checks

    /// The message a reaction or pin addresses, as `member` sees it.
    async fn target_message(
        &mut self,
        conv_id: &str,
        member: &Member,
        msg_id: &str,
        what: &'static str,
    ) -> ApiResult<MsgView> {
        let msg = self.db.message(msg_id).await?;
        match msg {
            Some(m) if m.conv_id == conv_id && m.created_seq >= member.visible_from => Ok(m),
            _ => Err(match what {
                "reaction" => rule!("reaction.unknown_message", "not_found", "no such message"),
                _ => rule!("pin.unknown_message", "not_found", "no such message"),
            }),
        }
    }

    // ----------------------------------------------------------- reactions

    /// Is `key` a reaction key: one emoji, or one of the app's short names
    /// written `:name:` (MSG-20)?
    fn check_reaction_key(&self, key: &str) -> ApiResult<()> {
        let named = key
            .strip_prefix(':')
            .and_then(|k| k.strip_suffix(':'))
            .is_some_and(|n| self.sh.cfg.messages.reaction_names.iter().any(|x| x == n));
        if named || is_emoji(key) {
            return Ok(());
        }
        Err(rule!(
            "reaction.key_invalid",
            "bad_request",
            "a reaction is one emoji or one of the app's :names:",
        ))
    }

    pub(super) async fn react(
        &mut self,
        me: &User,
        conv_id: &str,
        msg_id: &str,
        key: &str,
    ) -> ApiResult<Value> {
        let member = self.require_member(conv_id, &me.id).await?;
        not_silenced(&member)?;
        self.check_reaction_key(key)?;
        let msg = self
            .target_message(conv_id, &member, msg_id, "reaction")
            .await?;
        if msg.sender_type == UserKind::System {
            return Err(rule!(
                "reaction.not_reactable",
                "bad_request",
                "system messages take no reactions"
            ));
        }
        if msg.state == "recalled" {
            return Err(rule!(
                "reaction.recalled",
                "conflict",
                "the message was recalled"
            ));
        }
        self.check_dm_block(conv_id, &me.id).await?;
        let mine = self.db.my_reactions(msg_id, &me.id).await?;
        if mine.iter().any(|k| k == key) {
            return Ok(json!({"changed": false}));
        }
        let single = self.sh.cfg.messages.reaction_mode == "single";
        if !single && mine.len() >= self.sh.cfg.messages.reactions_per_user {
            return Err(rule!(
                "reaction.too_many_per_user",
                "conflict",
                format!(
                    "at most {} reactions per person on a message",
                    self.sh.cfg.messages.reactions_per_user
                ),
            ));
        }
        let keys = self.db.reaction_keys(msg_id).await?;
        // In single mode the person's old key goes first, which can free
        // its place when nobody else has it.
        let mut distinct = keys.len();
        if single {
            for k in &mine {
                if self.only_user_of(msg_id, k, &me.id).await? {
                    distinct -= 1;
                }
            }
        }
        if !keys.iter().any(|k| k == key)
            && distinct >= self.sh.cfg.messages.reaction_keys
            && self.guard(MUT_REACTION_LIMIT).await
        {
            return Err(rule!(
                "reaction.too_many_keys",
                "conflict",
                format!(
                    "a message takes at most {} different reactions",
                    self.sh.cfg.messages.reaction_keys
                ),
            ));
        }
        if single {
            for old in &mine {
                self.unreact_one(me, &msg, old).await?;
            }
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "reaction.added",
                Some(&me.id),
                None,
                json!({"msg_id": msg_id, "msg_seq": msg.created_seq, "msg_sender": msg.sender_id, "key": key, "user_id": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.db
            .add_reaction(conv_id, msg_id, &me.id, key, seq)
            .await?;
        self.emit(ev).await?;
        self.reaction_feedback(me, &msg, Some(key)).await?;
        Ok(json!({"changed": true, "seq": seq}))
    }

    /// Whether `user` is the only one with `key` on a message.
    async fn only_user_of(&mut self, msg_id: &str, key: &str, user: &str) -> ApiResult<bool> {
        let users = self.db.reaction_users(msg_id, key, 0, 2).await?;
        Ok(users.iter().all(|(u, _, _)| u == user))
    }

    async fn unreact_one(&mut self, me: &User, msg: &MsgView, key: &str) -> ApiResult<i64> {
        self.db.remove_reaction(&msg.msg_id, &me.id, key).await?;
        let ev = self
            .db
            .append_event(
                &msg.conv_id,
                "reaction.removed",
                Some(&me.id),
                None,
                json!({"msg_id": msg.msg_id, "msg_seq": msg.created_seq, "msg_sender": msg.sender_id, "key": key, "user_id": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(seq)
    }

    pub(super) async fn unreact(
        &mut self,
        me: &User,
        conv_id: &str,
        msg_id: &str,
        key: &str,
    ) -> ApiResult<Value> {
        let member = self.require_member(conv_id, &me.id).await?;
        let msg = self
            .target_message(conv_id, &member, msg_id, "reaction")
            .await?;
        let mine = self.db.my_reactions(msg_id, &me.id).await?;
        if !mine.iter().any(|k| k == key) {
            return Ok(json!({"changed": false}));
        }
        let seq = self.unreact_one(me, &msg, key).await?;
        self.reaction_feedback(me, &msg, None).await?;
        Ok(json!({"changed": true, "seq": seq}))
    }

    /// With `messages.reactions_as_feedback`, a 👍 or 👎 on an agent's message
    /// is also feedback on the agent; removing it takes the feedback back.
    async fn reaction_feedback(
        &mut self,
        me: &User,
        msg: &MsgView,
        added: Option<&str>,
    ) -> ApiResult<()> {
        if !self.sh.cfg.messages.reactions_as_feedback
            || msg.sender_type != UserKind::Agent
            || msg.sender_id == me.id
            || me.kind != UserKind::Human
        {
            return Ok(());
        }
        let rating = |k: &str| match k {
            "👍" | "👍🏻" | "👍🏼" | "👍🏽" | "👍🏾" | "👍🏿" => Some("up"),
            "👎" | "👎🏻" | "👎🏼" | "👎🏽" | "👎🏾" | "👎🏿" => Some("down"),
            _ => None,
        };
        let mine = self.db.my_reactions(&msg.msg_id, &me.id).await?;
        // The latest thumb the person still has decides.
        let now = mine.iter().rev().find_map(|k| rating(k));
        if added.is_some_and(|k| rating(k).is_none()) {
            return Ok(());
        }
        let current = self.db.feedback(&msg.msg_id, &me.id).await?;
        if current.as_ref().map(|(r, _)| r.as_str()) == now {
            return Ok(());
        }
        self.feedback(me, &msg.msg_id, now.map(String::from), None)
            .await?;
        Ok(())
    }

    // ---------------------------------------------------------------- pins

    /// May `me` pin in this conversation (MSG-21)?
    async fn may_pin(&mut self, me: &User, conv_id: &str, member: &Member) -> ApiResult<()> {
        if me.kind == UserKind::Agent && !self.sh.cfg.messages.agents_pin {
            return Err(rule!(
                "pin.agents",
                "forbidden",
                "agents do not pin messages in this app (messages.agents_pin)",
            ));
        }
        not_silenced(member)?;
        if !self.is_dm(conv_id).await?
            && self.db.pin_policy(conv_id).await? == "admins"
            && !is_manager(&member.role)
        {
            return Err(rule!(
                "pin.forbidden",
                "forbidden",
                "only the owner and admins pin messages here",
            ));
        }
        Ok(())
    }

    pub(super) async fn pin(&mut self, me: &User, conv_id: &str, msg_id: &str) -> ApiResult<Value> {
        let member = self.require_member(conv_id, &me.id).await?;
        self.may_pin(me, conv_id, &member).await?;
        let msg = self.target_message(conv_id, &member, msg_id, "pin").await?;
        if msg.sender_type == UserKind::System {
            return Err(rule!(
                "pin.not_pinnable",
                "bad_request",
                "system messages cannot be pinned"
            ));
        }
        if msg.state == "recalled" {
            return Err(rule!(
                "pin.recalled",
                "conflict",
                "the message was recalled"
            ));
        }
        self.check_dm_block(conv_id, &me.id).await?;
        if self.db.pin_of(conv_id, msg_id).await?.is_some() {
            return Ok(json!({"changed": false}));
        }
        let max = self.sh.cfg.messages.pins_per_conv;
        if self.db.pin_count(conv_id).await? >= max as i64 && self.guard(MUT_PIN_LIMIT).await {
            return Err(rule!(
                "pin.limit",
                "conflict",
                format!("a conversation holds at most {max} pinned messages; unpin one first"),
            ));
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "msg.pinned",
                Some(&me.id),
                None,
                json!({"msg_id": msg_id, "msg_seq": msg.created_seq, "msg_sender": msg.sender_id, "by": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.db.add_pin(conv_id, msg_id, &me.id, seq).await?;
        self.db
            .audit(
                Some(&me.id),
                "msg.pinned",
                Some(conv_id),
                Some(msg_id),
                json!({}),
            )
            .await?;
        self.emit(ev).await?;
        Ok(json!({"changed": true, "seq": seq}))
    }

    pub(super) async fn unpin(
        &mut self,
        me: &User,
        conv_id: &str,
        msg_id: &str,
    ) -> ApiResult<Value> {
        let member = self.require_member(conv_id, &me.id).await?;
        if me.kind == UserKind::Agent && !self.sh.cfg.messages.agents_pin {
            return Err(rule!(
                "pin.agents",
                "forbidden",
                "agents do not pin messages in this app (messages.agents_pin)",
            ));
        }
        not_silenced(&member)?;
        let msg = self.target_message(conv_id, &member, msg_id, "pin").await?;
        let Some((by, _, _)) = self.db.pin_of(conv_id, msg_id).await? else {
            return Ok(json!({"changed": false}));
        };
        if by != me.id && !is_manager(&member.role) && !self.is_dm(conv_id).await? {
            return Err(rule!(
                "pin.unpin_forbidden",
                "forbidden",
                "only who pinned it, the owner and admins unpin a message",
            ));
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "msg.unpinned",
                Some(&me.id),
                None,
                json!({"msg_id": msg_id, "msg_seq": msg.created_seq, "msg_sender": msg.sender_id, "by": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.db.remove_pin(conv_id, msg_id).await?;
        self.db
            .audit(
                Some(&me.id),
                "msg.unpinned",
                Some(conv_id),
                Some(msg_id),
                json!({}),
            )
            .await?;
        self.emit(ev).await?;
        Ok(json!({"changed": true, "seq": seq}))
    }

    pub(super) async fn set_pin_policy(&mut self, conv_id: &str, policy: &str) -> ApiResult<()> {
        if !matches!(policy, "members" | "admins") {
            return Err(rule!(
                "pin.policy",
                "bad_request",
                "pin_policy is members or admins"
            ));
        }
        if self.is_dm(conv_id).await? {
            return Err(rule!(
                "pin.policy_dm",
                "bad_request",
                "in a direct conversation both people pin",
            ));
        }
        self.db.set_pin_policy(conv_id, policy).await
    }

    // --------------------------------------------------------------- recall

    /// What goes with a recalled message: its reactions and pin (the recall
    /// event says so), and its place in a thread's summary.
    pub(super) async fn social_recalled(&mut self, msg: &MsgView) -> ApiResult<()> {
        if self.guard(MUT_RECALL_KEEPS).await {
            self.db.clear_reactions(&msg.msg_id).await?;
            self.db.remove_pin(&msg.conv_id, &msg.msg_id).await?;
        }
        if let Some(root) = &msg.thread_id {
            self.db.thread_recount(&msg.conv_id, root).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_emoji;

    #[test]
    fn emoji() {
        for ok in [
            "👍",
            "👍🏽",
            "❤️",
            "❤",
            "🇯🇵",
            "1️⃣",
            "#⃣",
            "👩‍💻",
            "👨‍👩‍👧‍👦",
            "🏳️‍🌈",
            "🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
            "🫠",
            "©️",
        ] {
            assert!(is_emoji(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "ok",
            "👍👍",
            "👍 ",
            " 👍",
            "🇯",
            "🇯🇵🇯🇵",
            "1",
            "11️⃣",
            "👩‍",
            "‍👩",
            ":x:",
            "\u{FE0F}",
            "🏴\u{E0067}",
            "👍\u{0301}",
        ] {
            assert!(!is_emoji(bad), "{bad:?}");
        }
    }
}
