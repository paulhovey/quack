//! Async startup. Each subsystem reports through `Event::StartupProgress`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cutoffharness_config::{ItemState, Loaded, ProjectStatus, ProviderFile, load_providers};
use cutoffharness_protocol::{Event, LoadState, Subsystem};
use tokio::sync::broadcast;
use tokio::task::JoinSet;

/// What startup resolved, for the engine's use.
#[derive(Debug, Default, Clone)]
pub(crate) struct StartupOutcome {
    /// `<provider>/<model>` when the configured model resolved.
    pub model: Option<String>,
}

struct Reporter {
    tx: broadcast::Sender<Event>,
    subsystem: Subsystem,
}

impl Reporter {
    fn send(&self, state: LoadState) {
        let _ = self.tx.send(Event::StartupProgress {
            subsystem: self.subsystem,
            state,
        });
    }
}

pub(crate) async fn run(config: Arc<Loaded>, tx: broadcast::Sender<Event>) -> StartupOutcome {
    let reporter = |subsystem| Reporter {
        tx: tx.clone(),
        subsystem,
    };
    for s in Subsystem::ALL {
        reporter(s).send(LoadState::Pending);
    }
    for warning in &config.warnings {
        let _ = tx.send(Event::Warning {
            message: warning.clone(),
        });
    }
    reporter(Subsystem::Project).send(project_state(config.project));
    reporter(Subsystem::Profile).send(LoadState::Ready {
        detail: "empty (compiled-in)".into(),
    });

    let mut placeholders = JoinSet::new();
    for (subsystem, work, milestone) in [
        (Subsystem::Tokenizer, "selecting tokenizer", "M2"),
        (
            Subsystem::Instructions,
            "scanning AGENTS.md and skills",
            "M2",
        ),
        (
            Subsystem::Compat,
            "looking for other harnesses' files",
            "M2",
        ),
        (Subsystem::Memory, "reading memory index", "M2"),
        (Subsystem::Mcp, "reading mcp.d", "M3"),
        (Subsystem::Lsp, "checking lsp.d binaries", "M4"),
    ] {
        let r = reporter(subsystem);
        placeholders.spawn(async move {
            r.send(LoadState::Running {
                detail: work.into(),
            });
            tokio::time::sleep(Duration::from_millis(60)).await;
            r.send(LoadState::Disabled {
                reason: format!("not implemented yet ({milestone})"),
            });
        });
    }

    // Config + providers are real.
    let cfg = reporter(Subsystem::Config);
    let prov = reporter(Subsystem::Provider);
    cfg.send(LoadState::Running {
        detail: "reading providers.d".into(),
    });
    prov.send(LoadState::Running {
        detail: "waiting for providers.d".into(),
    });
    let started = Instant::now();
    let loaded = config.clone();
    let providers = tokio::task::spawn_blocking(move || load_providers(&loaded))
        .await
        .unwrap_or_default();
    let elapsed = started.elapsed().as_millis();

    let failed: Vec<String> = providers
        .iter()
        .filter_map(|p| match &p.state {
            ItemState::Failed(d) => Some(d.one_line()),
            _ => None,
        })
        .chain(config.diagnostics.iter().map(|d| d.one_line()))
        .collect();
    let layers = config
        .layers
        .iter()
        .filter(|l| l.status == cutoffharness_config::LayerStatus::Loaded)
        .count();
    let active = providers.iter().filter(|p| p.loaded().is_some()).count();
    for f in &failed {
        let _ = tx.send(Event::Warning { message: f.clone() });
    }
    cfg.send(if failed.is_empty() {
        LoadState::Ready {
            detail: format!("{layers} config files · {active} providers · {elapsed}ms"),
        }
    } else {
        LoadState::Failed {
            error: format!(
                "{} file(s) failed: {} — run `cutoffharness config check`",
                failed.len(),
                failed[0]
            ),
        }
    });

    let (state, model) = resolve_model(&config.settings.model.default, &providers);
    prov.send(state);

    while placeholders.join_next().await.is_some() {}
    StartupOutcome { model }
}

fn project_state(status: ProjectStatus) -> LoadState {
    match status {
        ProjectStatus::None => LoadState::Disabled {
            reason: "no .cutoffharness/ in this workspace".into(),
        },
        ProjectStatus::Trusted => LoadState::Ready {
            detail: "trusted".into(),
        },
        ProjectStatus::SettingsOnly => LoadState::Ready {
            detail: "trusted (settings only)".into(),
        },
        ProjectStatus::Untrusted => LoadState::NeedsAttention {
            detail: "project config found but not trusted; skipped (trust prompt lands in M1)"
                .into(),
        },
        ProjectStatus::Ignored => LoadState::Disabled {
            reason: "ignored (trust.toml)".into(),
        },
        ProjectStatus::DisabledByPolicy => LoadState::Disabled {
            reason: "disabled by policy".into(),
        },
    }
}

fn resolve_model(
    wanted: &str,
    providers: &[cutoffharness_config::Item<ProviderFile>],
) -> (LoadState, Option<String>) {
    if wanted.is_empty() {
        let hint = if providers.iter().any(|p| p.loaded().is_some()) {
            "no model selected · set [model].default or use /models"
        } else {
            "no providers configured · add a file to providers.d/"
        };
        return (
            LoadState::NeedsAttention {
                detail: hint.into(),
            },
            None,
        );
    }
    let Some((pid, mid)) = wanted.split_once('/') else {
        return (
            LoadState::Failed {
                error: format!("[model].default `{wanted}` must be <provider>/<model>"),
            },
            None,
        );
    };
    let Some(item) = providers.iter().find(|p| p.id == pid) else {
        return (
            LoadState::Failed {
                error: format!("provider `{pid}` not found in any providers.d/"),
            },
            None,
        );
    };
    let provider = match &item.state {
        ItemState::Loaded(p) => p,
        ItemState::Disabled(why) => {
            return (
                LoadState::Failed {
                    error: format!("provider `{pid}` is disabled: {why}"),
                },
                None,
            );
        }
        ItemState::Failed(d) => {
            return (
                LoadState::Failed {
                    error: d.one_line(),
                },
                None,
            );
        }
    };
    let ctx = provider
        .model(mid)
        .and_then(|m| m.context_window)
        .map_or_else(
            || "context unknown".into(),
            |c| format!("{}k ctx", c / 1024),
        );
    let name = provider.model(mid).map_or(mid, |m| m.display_name());
    (
        LoadState::Ready {
            detail: format!(
                "{pid} · {name} ({ctx}) · {} adapter lands in M1",
                provider.api
            ),
        },
        Some(wanted.to_owned()),
    )
}
