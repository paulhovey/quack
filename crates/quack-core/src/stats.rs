//! Session usage for `/stats`: tokens and cost per request, timings, tool calls.
//!
//! Most of it is read off the events a turn emits (`TurnCtx::emit`); requests and approval
//! waits are recorded where they happen. Token totals are rebuilt from the session log on
//! resume; timings are not logged, so they start at the resume.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use quack_protocol::{Event, RequestStat, StatsReport, ToolStat, Usage};
use quack_store::session::Record;

#[derive(Debug)]
pub(crate) struct Stats {
    opened: Instant,
    resumed: bool,
    requests: u32,
    unreported: u32,
    prompt: u64,
    completion: u64,
    cached: u64,
    cache_reported: u32,
    cost: f64,
    priced: u32,
    ttft_ms: Vec<u32>,
    tps: Vec<f32>,
    model: Duration,
    approval: Duration,
    tools: HashMap<String, Tally>,
    /// Calls begun and not yet ended: name and start.
    running: HashMap<String, (String, Instant)>,
    compactions: u32,
    pruned: u32,
    history: VecDeque<RequestStat>,
    /// The newest history entry is still waiting for its `Speed`.
    timing: bool,
}

#[derive(Debug, Default)]
struct Tally {
    calls: u32,
    failed: u32,
    time: Duration,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            opened: Instant::now(),
            resumed: false,
            requests: 0,
            unreported: 0,
            prompt: 0,
            completion: 0,
            cached: 0,
            cache_reported: 0,
            cost: 0.0,
            priced: 0,
            ttft_ms: Vec::new(),
            tps: Vec::new(),
            model: Duration::ZERO,
            approval: Duration::ZERO,
            tools: HashMap::new(),
            running: HashMap::new(),
            compactions: 0,
            pruned: 0,
            history: VecDeque::new(),
            timing: false,
        }
    }
}

impl Stats {
    /// A resumed session: token totals from its `usage` records.
    pub(crate) fn from_log(records: &[Record]) -> Self {
        let mut s = Self {
            resumed: true,
            ..Self::default()
        };
        for r in records {
            match r.kind.as_str() {
                "usage" => {
                    if let Some(u) = r
                        .data
                        .get("usage")
                        .and_then(|v| serde_json::from_value::<Usage>(v.clone()).ok())
                    {
                        let cost = r.data.get("cost").and_then(serde_json::Value::as_f64);
                        s.tokens(&u, cost);
                        s.price(cost);
                    }
                }
                "compaction" => s.compactions += 1,
                _ => {}
            }
        }
        s
    }

    fn tokens(&mut self, u: &Usage, cost: Option<f64>) {
        self.requests += 1;
        if self.history.len() == StatsReport::HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(RequestStat {
            prompt: u.prompt_tokens,
            cached: u.cached_prompt_tokens.unwrap_or(0),
            completion: u.completion_tokens,
            cost,
            ttft_ms: None,
            tokens_per_sec: None,
        });
        self.prompt += u64::from(u.prompt_tokens);
        self.completion += u64::from(u.completion_tokens);
        if let Some(c) = u.cached_prompt_tokens {
            self.cached += u64::from(c);
            self.cache_reported += 1;
        }
    }

    /// One model call, with its price when the model it went to has one.
    pub(crate) fn request(&mut self, usage: Option<&Usage>, cost: Option<f64>, took: Duration) {
        self.model += took;
        self.timing = usage.is_some();
        let Some(u) = usage else {
            self.requests += 1;
            self.unreported += 1;
            return;
        };
        self.tokens(u, cost);
        self.price(cost);
    }

    fn price(&mut self, cost: Option<f64>) {
        if let Some(c) = cost {
            self.cost += c;
            self.priced += 1;
        }
    }

    pub(crate) fn waited(&mut self, d: Duration) {
        self.approval += d;
    }

    pub(crate) fn observe(&mut self, e: &Event) {
        match e {
            Event::Speed {
                ttft_ms,
                tokens_per_sec,
            } => {
                self.ttft_ms.push(*ttft_ms);
                self.tps.extend(*tokens_per_sec);
                // Speed follows its request's usage, so it belongs to the newest entry.
                if let Some(last) = self.history.back_mut().filter(|_| self.timing) {
                    last.ttft_ms = Some(*ttft_ms);
                    last.tokens_per_sec = *tokens_per_sec;
                }
                self.timing = false;
            }
            Event::ToolCallBegin { call } => {
                self.running
                    .insert(call.call_id.0.clone(), (call.name.clone(), Instant::now()));
            }
            Event::ToolCallEnd { call_id, result } => {
                if let Some((name, start)) = self.running.remove(&call_id.0) {
                    let t = self.tools.entry(name).or_default();
                    t.calls += 1;
                    t.failed += u32::from(!result.ok);
                    t.time += start.elapsed();
                }
            }
            Event::Compacted { .. } => self.compactions += 1,
            Event::ContextPruned { report } => {
                self.pruned += u32::try_from(report.items.len()).unwrap_or(u32::MAX);
            }
            _ => {}
        }
    }

    pub(crate) fn report(&self, model: &str, turns: u32) -> StatsReport {
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let mut tools: Vec<ToolStat> = self
            .tools
            .iter()
            .map(|(name, t)| ToolStat {
                name: name.clone(),
                calls: t.calls,
                failed: t.failed,
                ms: ms(t.time),
            })
            .collect();
        tools.sort_by(|a, b| b.calls.cmp(&a.calls).then(b.ms.cmp(&a.ms)));
        let tool_time: Duration = self.tools.values().map(|t| t.time).sum();
        let mut ttft = self.ttft_ms.clone();
        ttft.sort_unstable();
        let mut tps = self.tps.clone();
        tps.sort_by(f32::total_cmp);
        StatsReport {
            model: model.to_owned(),
            turns,
            requests: self.requests,
            unreported: self.unreported,
            prompt_tokens: self.prompt,
            completion_tokens: self.completion,
            cached_tokens: self.cached,
            cache_reported: self.cache_reported,
            cost: (self.priced > 0).then_some(self.cost),
            unpriced: self.requests.saturating_sub(self.unreported + self.priced),
            ttft_ms: ttft.last().map(|max| (ttft[ttft.len() / 2], *max)),
            tokens_per_sec: tps.get(tps.len() / 2).copied(),
            open_ms: ms(self.opened.elapsed()),
            model_ms: ms(self.model),
            // Approval waits happen inside tool calls; count them once, as the user's.
            tools_ms: ms(tool_time.saturating_sub(self.approval)),
            approval_ms: ms(self.approval),
            tools,
            compactions: self.compactions,
            pruned: self.pruned,
            resumed: self.resumed,
            history: self.history.iter().cloned().collect(),
            sampling: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_config::provider::Cost;
    use quack_protocol::{CallId, ToolCallInfo, ToolResultSummary};

    fn usage(prompt: u32, completion: u32, cached: Option<u32>) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_prompt_tokens: cached,
        }
    }

    #[test]
    fn counts_priced_unpriced_and_unreported_requests() {
        let mut s = Stats::default();
        s.request(
            Some(&usage(1_000_000, 100_000, Some(400_000))),
            Some(1.1),
            Duration::from_secs(2),
        );
        s.request(Some(&usage(10, 5, None)), None, Duration::ZERO);
        s.request(None, None, Duration::ZERO);
        let r = s.report("m/x", 1);
        assert_eq!(r.requests, 3);
        assert_eq!(r.unreported, 1);
        assert_eq!(r.unpriced, 1);
        assert_eq!(r.prompt_tokens, 1_000_010);
        assert_eq!(r.cache_reported, 1);
        assert!((r.cost.expect("priced") - 1.1).abs() < 1e-9, "{:?}", r.cost);
        assert_eq!(r.model_ms, 2000);
    }

    #[test]
    fn prices_cached_tokens_apart() {
        let c = Cost {
            input: 1.0,
            output: 4.0,
            cached_input: Some(0.25),
        };
        // 600k fresh + 400k cached + 100k out = 0.6 + 0.1 + 0.4
        assert!((c.of(1_000_000, 400_000, 100_000) - 1.1).abs() < 1e-9);
    }

    #[test]
    fn unset_cached_price_is_the_input_price() {
        let c = Cost {
            input: 2.0,
            output: 0.0,
            cached_input: None,
        };
        assert!((c.of(1_000_000, 500_000, 0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn tools_and_timings_come_from_events() {
        let mut s = Stats::default();
        let call = |id: &str, name: &str| Event::ToolCallBegin {
            call: ToolCallInfo {
                call_id: CallId(id.into()),
                name: name.into(),
                summary: String::new(),
            },
        };
        let end = |id: &str, ok| Event::ToolCallEnd {
            call_id: CallId(id.into()),
            result: ToolResultSummary {
                ok,
                summary: String::new(),
                detail: None,
            },
        };
        for e in [
            call("1", "grep"),
            end("1", true),
            call("2", "shell"),
            end("2", false),
            call("3", "grep"),
            end("3", true),
            Event::Speed {
                ttft_ms: 300,
                tokens_per_sec: Some(40.0),
            },
            Event::Speed {
                ttft_ms: 900,
                tokens_per_sec: None,
            },
            Event::Speed {
                ttft_ms: 500,
                tokens_per_sec: Some(60.0),
            },
        ] {
            s.observe(&e);
        }
        let r = s.report("m/x", 2);
        assert_eq!(r.tools[0].name, "grep");
        assert_eq!((r.tools[0].calls, r.tools[0].failed), (2, 0));
        assert_eq!((r.tools[1].calls, r.tools[1].failed), (1, 1));
        assert_eq!(r.ttft_ms, Some((500, 900)));
        assert_eq!(r.tokens_per_sec, Some(60.0));
        assert_eq!(r.cost, None);
    }

    #[test]
    fn history_keeps_each_request_with_its_speed() {
        let mut s = Stats::default();
        let speed = |ttft_ms| Event::Speed {
            ttft_ms,
            tokens_per_sec: Some(50.0),
        };
        s.request(Some(&usage(100, 10, Some(60))), Some(0.5), Duration::ZERO);
        s.observe(&speed(300));
        // No usage: nothing to chart, and its speed must not land on the request before.
        s.request(None, None, Duration::ZERO);
        s.observe(&speed(900));
        s.request(Some(&usage(200, 20, None)), None, Duration::ZERO);
        let h = s.report("m/x", 1).history;
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].prompt, h[0].cached, h[0].completion), (100, 60, 10));
        assert_eq!((h[0].ttft_ms, h[0].cost), (Some(300), Some(0.5)));
        assert_eq!((h[1].prompt, h[1].ttft_ms), (200, None));

        for _ in 0..StatsReport::HISTORY + 5 {
            s.request(Some(&usage(1, 1, None)), None, Duration::ZERO);
        }
        let h = s.report("m/x", 1).history;
        assert_eq!(h.len(), StatsReport::HISTORY, "only the newest are kept");
    }
}
