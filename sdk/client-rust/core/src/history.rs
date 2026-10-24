//! Older messages (docs/12 §2.8): the cache first, then the server page by
//! page (`history`, docs/08 §2.8). The cache records which seq ranges it
//! holds completely ("segments"); sync grows the newest one, paging back
//! grows it downwards, jumping to a message makes an island.

use serde_json::{Value, json};

use crate::*;

/// Adds `[lo, hi]` to the ranges, merging what touches.
pub(crate) fn add_segment(segs: &mut Vec<(i64, i64)>, lo: i64, hi: i64) {
    segs.push((lo, hi.max(lo - 1)));
    segs.sort();
    let mut out: Vec<(i64, i64)> = vec![];
    for (l, h) in segs.drain(..) {
        match out.last_mut() {
            Some(last) if l <= last.1 + 1 => last.1 = last.1.max(h),
            _ => out.push((l, h)),
        }
    }
    *segs = out;
}

impl Core {
    /// The oldest message seq loaded in memory, and the start of the range
    /// it belongs to.
    fn bottom(&self, conv: &str) -> (i64, i64) {
        let Some(c) = self.convs.get(conv) else {
            return (1, 1);
        };
        let oldest = c.msgs.keys().next().copied().unwrap_or(c.last_seq + 1);
        let lo = c
            .segments
            .iter()
            .find(|(l, h)| *l <= oldest && oldest <= *h + 1)
            .map(|(l, _)| *l)
            .unwrap_or(oldest);
        (oldest, lo)
    }

    pub(crate) fn has_older(&self, conv: &str) -> bool {
        let Some(c) = self.convs.get(conv) else {
            return false;
        };
        let (oldest, lo) = self.bottom(conv);
        oldest > lo || (c.older_end.is_none() && lo > 1)
    }

    /// `load_older {conv_id}`: the page before the oldest loaded message.
    /// Answers `{added, has_older, older_end?}`; the messages come as
    /// changes of the conversation.
    pub(crate) fn cmd_load_older(&mut self, id: u64, cmd: &Value) {
        let Some(conv) = cmd["conv_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
        else {
            return self.reply(
                id,
                Err(ClientError::new("bad_request", "conv_id is required")),
            );
        };
        self.conv_mut(&conv);
        if self.convs[&conv].loading_older {
            return self.reply(id, Err(ClientError::new("conflict", "already loading")));
        }
        let (oldest, lo) = self.bottom(&conv);
        if oldest > lo {
            // The cache has more of this range.
            self.next_req += 1;
            let req = self.next_req;
            self.page_reqs.insert(req, (conv.clone(), Some(id)));
            self.conv_mut(&conv).loading_older = true;
            self.out.push(Effect::LoadPage {
                req,
                conv_id: conv,
                before_seq: oldest,
                limit: PAGE,
            });
            return;
        }
        self.older_from_server(&conv, lo, Some(id));
    }

    fn older_from_server(&mut self, conv: &str, lo: i64, cmd: Option<u64>) {
        let end = self.convs.get(conv).and_then(|c| c.older_end.clone());
        if end.is_some() || lo <= 1 {
            let end = end.unwrap_or_else(|| "start".into());
            if let Some(id) = cmd {
                self.reply(
                    id,
                    Ok(json!({"added": 0, "has_older": false, "older_end": end})),
                );
            }
            return;
        }
        if self.phase != Phase::Live {
            // Offline: only what the cache has.
            if let Some(id) = cmd {
                self.reply(
                    id,
                    Ok(json!({"added": 0, "has_older": true, "offline": true})),
                );
            }
            return;
        }
        self.conv_mut(conv).loading_older = true;
        let anchor = lo - 1;
        self.request(
            "history",
            json!({"conv_id": conv, "anchor": anchor, "before": PAGE, "after": 0}),
            Purpose::History {
                conv_id: conv.to_string(),
                anchor,
                jump: false,
                cmd,
            },
        );
    }

    pub(crate) fn on_page_loaded(&mut self, req: u64, msgs: Vec<Value>) {
        // The request names the conversation (not the answer).
        let Some((conv, cmd)) = self.page_reqs.remove(&req) else {
            return;
        };
        let conv = conv.as_str();
        self.conv_mut(conv).loading_older = false;
        let n = msgs.len();
        for m in msgs {
            self.put_msg(conv, m, false);
        }
        if n == 0 {
            // The cache has nothing more of this range: the server.
            let (_, lo) = self.bottom(conv);
            // The range is complete down to `lo`, so nothing below the
            // loaded messages in it is missing: treat it as reached.
            let oldest = self.convs[conv].msgs.keys().next().copied();
            if let Some(o) = oldest
                && o > lo
            {
                // Messages are events too: the range simply has no more.
                let c = self.conv_mut(conv);
                if let Some(s) = c.segments.iter_mut().find(|(l, _)| *l == lo) {
                    s.0 = o;
                }
                c.dirty = true;
            }
            let (_, lo) = self.bottom(conv);
            return self.older_from_server(conv, lo, cmd);
        }
        if let Some(id) = cmd {
            let has = self.has_older(conv);
            self.reply(id, Ok(json!({"added": n, "has_older": has})));
        }
    }

    /// `jump_to {conv_id, seq}`: a page around a message, kept as an island.
    pub(crate) fn cmd_jump_to(&mut self, id: u64, cmd: &Value) {
        let (Some(conv), Some(seq)) = (
            cmd["conv_id"].as_str().map(String::from),
            cmd["seq"].as_i64(),
        ) else {
            return self.reply(
                id,
                Err(ClientError::new(
                    "bad_request",
                    "conv_id and seq are required",
                )),
            );
        };
        if seq < 0 {
            return self.reply(id, Err(ClientError::new("bad_request", "seq >= 0")));
        }
        self.conv_mut(&conv);
        let known = self.convs[&conv]
            .segments
            .iter()
            .any(|(l, h)| *l <= seq && seq <= *h);
        if known && self.convs[&conv].msgs.range(..=seq).next().is_some() {
            return self.reply(id, Ok(json!({"added": 0, "cached": true})));
        }
        if self.phase != Phase::Live {
            return self.reply(id, Err(ClientError::new("disconnected", "not connected")));
        }
        self.request(
            "history",
            json!({"conv_id": conv, "anchor": seq, "before": PAGE / 2, "after": PAGE / 2}),
            Purpose::History {
                conv_id: conv,
                anchor: seq,
                jump: true,
                cmd: Some(id),
            },
        );
    }

    pub(crate) fn on_history(
        &mut self,
        conv: &str,
        anchor: i64,
        jump: bool,
        cmd: Option<u64>,
        f: &Value,
        err: Option<ClientError>,
    ) {
        self.conv_mut(conv).loading_older = false;
        if let Some(e) = err {
            if let Some(id) = cmd {
                self.reply(id, Err(e));
            }
            return;
        }
        let msgs: Vec<Value> = f["messages"].as_array().cloned().unwrap_or_default();
        let seqs: Vec<i64> = msgs
            .iter()
            .filter_map(|m| m["created_seq"].as_i64())
            .collect();
        let has_before = f["has_before"] == true;
        let has_after = f["has_after"] == true;
        let n = msgs.len();
        for m in msgs {
            self.put_msg(conv, m, true);
        }
        // What is now known completely.
        let lo = if has_before {
            seqs.first().copied().unwrap_or(anchor + 1)
        } else {
            1
        };
        let hi = if jump && has_after {
            seqs.last().copied().unwrap_or(anchor)
        } else if jump {
            self.convs[conv].last_seq.max(anchor)
        } else {
            anchor
        };
        let c = self.conv_mut(conv);
        add_segment(&mut c.segments, lo, hi);
        if !has_before {
            c.older_end = Some(f["before_end"].as_str().unwrap_or("start").to_string());
        }
        c.dirty = true;
        let end = c.older_end.clone();
        if let Some(id) = cmd {
            let has = self.has_older(conv);
            self.reply(
                id,
                Ok(json!({"added": n, "has_older": has, "older_end": end})),
            );
        }
    }

    /// Writes a whole conversation again (after the store recovered).
    pub(crate) fn store_conv_all(&mut self, id: &str) {
        let Some(c) = self.convs.get_mut(id) else {
            return;
        };
        c.dirty = true;
        let msgs: Vec<Value> = c.msgs.values().cloned().collect();
        self.batch.put_messages.extend(msgs);
    }
}

#[cfg(test)]
mod tests {
    use super::add_segment;

    #[test]
    fn segments_merge_when_they_touch() {
        let mut s = vec![(10, 20)];
        add_segment(&mut s, 21, 30);
        assert_eq!(s, vec![(10, 30)]);
        add_segment(&mut s, 1, 5);
        assert_eq!(s, vec![(1, 5), (10, 30)]);
        add_segment(&mut s, 6, 9);
        assert_eq!(s, vec![(1, 30)]);
        add_segment(&mut s, 40, 39);
        assert_eq!(s, vec![(1, 30), (40, 39)]);
        add_segment(&mut s, 31, 39);
        assert_eq!(s, vec![(1, 39)]);
    }
}
