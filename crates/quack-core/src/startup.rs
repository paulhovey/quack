//! Async startup. Each subsystem reports through `Event::StartupProgress`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use quack_config::provider::ModelEntry;
use quack_config::settings::{EditFormat, MemoryWrites};
use quack_config::{
    Item, ItemState, LayerKind, Loaded, ProjectStatus, ProviderFile, glob_match, load_providers,
};
use quack_instructions::commands::CustomCommand;
use quack_instructions::skills;
use quack_profiles::{InternalTool, Profile, PromptVars, ToolSurface, load_profiles, select};
use quack_protocol::{CommandInfo, Event, LoadState, ProfileInfo, SkillInfo, Subsystem};
use quack_providers::{DiscoveredModel, Provider};
use quack_shell::ShellKind;
use quack_store::memory::Scope;
use quack_tools::{Permissions, ReadTracker, Registry, ToolCtx};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::env;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything a turn needs, resolved once per session.
#[derive(Debug)]
pub(crate) struct Setup {
    pub provider: Provider,
    pub model: ModelEntry,
    /// `<provider>/<model>` as shown to the user.
    pub model_ref: String,
    pub profile: Profile,
    /// Rendered once; byte-stable for the session.
    pub system: String,
    /// The profile's `context.md`, rendered once: put at the start of the first user
    /// message of each request (`turn::with_session_context`). Empty without one.
    pub session_context: String,
    pub surface: ToolSurface,
    pub registry: Arc<Registry>,
    /// Template for per-call contexts (cancel and output are replaced per call).
    pub tool_ctx: ToolCtx,
    pub permissions: Permissions,
    /// Planned context budget in tokens, when the window is known.
    pub budget: Option<u32>,
    /// Byte estimate corrected by this model's reported usage.
    pub estimator: std::sync::Mutex<quack_providers::tokens::Estimator>,
    pub context: quack_config::settings::ContextSettings,
    /// Directories whose AGENTS.md is in the system prompt (or that have none).
    pub instruction_dirs: Vec<std::path::PathBuf>,
    /// MCP servers; their tools join each request as they become ready.
    pub mcp: Arc<crate::mcp::Mcp>,
    /// The values the system prompt was rendered with, to preview other profiles.
    pub vars: PromptVars,
    /// Custom slash commands: `/<name> args` turns into the command's prompt.
    pub commands: Arc<Vec<CustomCommand>>,
    /// Sub-agents the `task` tool can start (`agents.d`), by id.
    pub agents: Arc<Vec<(String, quack_config::AgentFile)>>,
    /// Offer the MCP servers' tools; off for sub-agents that do not list `mcp`.
    pub mcp_tools: bool,
    /// Set once the session has warned that the window is too small to work in after a
    /// compaction, so the warning is given once.
    pub cramped: std::sync::atomic::AtomicBool,
}

/// Files read from the workspace and config folders that shape the prompt.
pub(crate) struct Files {
    pub instructions: quack_instructions::agents_md::Loaded,
    pub skills: skills::Found,
    pub commands: Vec<CustomCommand>,
    pub mcp: Arc<crate::mcp::Mcp>,
    pub lsp: Arc<quack_lsp::LspManager>,
}

/// What startup resolved, for the engine's use.
#[derive(Debug, Default)]
pub(crate) struct StartupOutcome {
    pub setup: Option<Setup>,
    /// Why there is no setup, for the error shown when a turn is submitted.
    pub problem: Option<String>,
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

fn warn(tx: &broadcast::Sender<Event>, message: impl Into<String>) {
    let _ = tx.send(Event::Warning {
        message: message.into(),
    });
}

/// Startup, and a reload: `keep` is the `(provider, model)` in use, tried before
/// `[model].default` so a reload does not change the model.
pub(crate) async fn run(
    config: Arc<Loaded>,
    tx: broadcast::Sender<Event>,
    mcp: Arc<crate::mcp::Mcp>,
    lsp: Arc<quack_lsp::LspManager>,
    keep: Option<(String, String)>,
) -> StartupOutcome {
    let reporter = |subsystem| Reporter {
        tx: tx.clone(),
        subsystem,
    };
    // MCP and LSP report their own rows.
    for s in Subsystem::ALL
        .into_iter()
        .filter(|s| !matches!(s, Subsystem::Mcp | Subsystem::Lsp))
    {
        reporter(s).send(LoadState::Pending);
    }
    for warning in &config.warnings {
        warn(&tx, warning.clone());
    }
    if let Some(w) = crate::sandbox::warning(&config) {
        warn(&tx, w);
    }
    reporter(Subsystem::Project).send(project_state(&config));
    if matches!(
        config.project,
        ProjectStatus::Untrusted | ProjectStatus::Changed
    ) {
        let _ = tx.send(Event::ProjectTrust {
            path: config.dirs.project.display().to_string(),
            summary: config.project_summary().describe(),
            changed: config.project == ProjectStatus::Changed,
        });
    }
    reporter(Subsystem::Tokenizer).send(LoadState::Ready {
        detail: "estimate, corrected by server usage".into(),
    });
    let store = env::memory_store(&config);
    let notes = store.list(Scope::Global).len() + store.list(Scope::Project).len();
    reporter(Subsystem::Memory).send(LoadState::Ready {
        detail: format!(
            "{notes} note{} · {}",
            if notes == 1 { "" } else { "s" },
            store.project.display()
        ),
    });

    let mut placeholders = JoinSet::new();
    let compat = reporter(Subsystem::Compat);
    placeholders.spawn(async move {
        compat.send(LoadState::Running {
            detail: "looking for other harnesses' files".into(),
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        compat.send(LoadState::Disabled {
            reason: "not implemented yet (M2)".into(),
        });
    });

    // Instructions (AGENTS.md) are cheap; read them while providers load.
    let instr = reporter(Subsystem::Instructions);
    instr.send(LoadState::Running {
        detail: "reading AGENTS.md".into(),
    });
    let cfg_for_instr = config.clone();
    let instructions = tokio::task::spawn_blocking(move || {
        (
            env::instructions(&cfg_for_instr),
            env::unmerged(&cfg_for_instr),
        )
    });
    let cfg_for_skills = config.clone();
    let skills = tokio::task::spawn_blocking(move || {
        (env::skills(&cfg_for_skills), env::commands(&cfg_for_skills))
    });

    // Config + providers.
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
    report_config(&cfg, &config, &providers, elapsed, &tx);

    let (instructions, unmerged) = instructions.await.unwrap_or_default();
    if !unmerged.is_empty() {
        let _ = tx.send(Event::Notice {
            message: format!(
                "{} {} written for other coding agents; quack reads {} where there is no \
                 AGENTS.md. /init merges {} into AGENTS.md, so every agent shares one file.",
                unmerged.join(", "),
                if unmerged.len() == 1 { "is" } else { "are" },
                if unmerged.len() == 1 { "it" } else { "them" },
                if unmerged.len() == 1 { "it" } else { "them" },
            ),
        });
    }
    instr.send(if instructions.sources.is_empty() {
        LoadState::Disabled {
            reason: "no AGENTS.md found".into(),
        }
    } else {
        LoadState::Ready {
            detail: format!(
                "{} · {} bytes",
                instructions.sources.join(", "),
                instructions.text.len()
            ),
        }
    });

    let (skills, (commands, bad_commands)) = skills.await.unwrap_or_default();
    report_skills(&tx, &skills);
    report_commands(&tx, &commands, &bad_commands);

    let kept = match keep {
        Some((pid, mid)) => match named(&config, &providers, &pid, &mid, &prov, &tx).await {
            Ok((p, m)) => Some((p, m, false)),
            Err(e) => {
                warn(&tx, format!("cannot keep {pid}/{mid}: {e}"));
                None
            }
        },
        None => None,
    };
    let resolved = match kept {
        Some(r) => Ok(r),
        None => resolve_model(&config, &providers, &prov, &tx).await,
    };
    let (provider, model, auto_picked) = match resolved {
        Ok(r) => r,
        Err((state, problem)) => {
            prov.send(state);
            reporter(Subsystem::Profile).send(LoadState::Disabled {
                reason: "no model selected".into(),
            });
            while placeholders.join_next().await.is_some() {}
            return StartupOutcome {
                setup: None,
                problem: Some(problem),
            };
        }
    };
    let model_ref = format!("{}/{}", provider.id, model.display_name());
    let ctx_label = model.context_window.map_or_else(
        || "context unknown".into(),
        |c| format!("{}k ctx", c / 1024),
    );
    prov.send(LoadState::Ready {
        detail: format!(
            "{model_ref} ({ctx_label}){}",
            if auto_picked {
                " · auto-selected; set [model].default to choose"
            } else {
                ""
            }
        ),
    });

    let setup = build_setup(
        &config,
        provider,
        model,
        model_ref,
        Files {
            instructions,
            skills,
            commands,
            mcp,
            lsp,
        },
        &reporter(Subsystem::Profile),
        &tx,
    )
    .await;
    while placeholders.join_next().await.is_some() {}
    StartupOutcome {
        problem: setup
            .is_none()
            .then(|| "the model profile failed to load".to_owned()),
        setup,
    }
}

/// Switch to another provider/model mid-session (`/models`): resolve it like startup does
/// and rebuild the profile, prompt and tool surface for it.
pub(crate) async fn switch(
    config: Arc<Loaded>,
    tx: broadcast::Sender<Event>,
    mcp: Arc<crate::mcp::Mcp>,
    lsp: Arc<quack_lsp::LspManager>,
    pid: String,
    mid: String,
) -> Result<Setup, String> {
    let reporter = |subsystem| Reporter {
        tx: tx.clone(),
        subsystem,
    };
    let cfg = config.clone();
    let providers = tokio::task::spawn_blocking(move || load_providers(&cfg))
        .await
        .unwrap_or_default();
    let item = providers
        .iter()
        .find(|p| p.id == pid)
        .ok_or_else(|| format!("provider `{pid}` not found"))?;
    let file = match &item.state {
        ItemState::Loaded(f) => f.clone(),
        ItemState::Disabled(why) => return Err(format!("provider `{pid}` is disabled: {why}")),
        ItemState::Failed(d) => return Err(d.one_line()),
    };
    let prov = reporter(Subsystem::Provider);
    let (provider, model) = try_provider(
        &config,
        &pid,
        file,
        Some(&mid),
        DISCOVERY_TIMEOUT,
        &prov,
        &tx,
    )
    .await?;
    let model_ref = format!("{pid}/{}", model.display_name());
    let ctx = model.context_window.map_or_else(
        || "context unknown".into(),
        |c| format!("{}k ctx", c / 1024),
    );
    prov.send(LoadState::Ready {
        detail: format!("{model_ref} ({ctx})"),
    });
    let cfg = config.clone();
    let (instructions, skills, (commands, _)) = tokio::task::spawn_blocking(move || {
        (
            env::instructions(&cfg),
            env::skills(&cfg),
            env::commands(&cfg),
        )
    })
    .await
    .unwrap_or_default();
    build_setup(
        &config,
        provider,
        model,
        model_ref,
        Files {
            instructions,
            skills,
            commands,
            mcp,
            lsp,
        },
        &reporter(Subsystem::Profile),
        &tx,
    )
    .await
    .ok_or_else(|| "the model profile failed to load".to_owned())
}

fn report_config(
    cfg: &Reporter,
    config: &Loaded,
    providers: &[Item<ProviderFile>],
    elapsed: u128,
    tx: &broadcast::Sender<Event>,
) {
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
        .filter(|l| l.status == quack_config::LayerStatus::Loaded)
        .count();
    let active = providers.iter().filter(|p| p.loaded().is_some()).count();
    for f in &failed {
        warn(tx, f.clone());
    }
    cfg.send(if failed.is_empty() {
        LoadState::Ready {
            detail: format!("{layers} config files · {active} providers · {elapsed}ms"),
        }
    } else {
        LoadState::Failed {
            error: format!(
                "{} file(s) failed: {} — run `quack config check`",
                failed.len(),
                failed[0]
            ),
        }
    });
}

type Resolved = (Provider, ModelEntry, bool);
type Unresolved = (LoadState, String);

/// Shorter discovery timeout while probing candidates for auto-selection.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Pick the provider and model, build the client, and discover models when needed.
///
/// With `[model].default` set, that provider is used. Without it, providers are tried in
/// order (the user's own files before the defaults bundle) and the first one whose key is
/// available and that answers is used.
async fn resolve_model(
    config: &Loaded,
    providers: &[Item<ProviderFile>],
    prov: &Reporter,
    tx: &broadcast::Sender<Event>,
) -> Result<Resolved, Unresolved> {
    let failed = |msg: String| (LoadState::Failed { error: msg.clone() }, msg);
    // The model last switched to comes first; if it is gone, say so and carry on.
    if let Some(last) = config.remembered_model()
        && let Some((pid, mid)) = last.split_once('/')
    {
        match named(config, providers, pid, mid, prov, tx).await {
            Ok((p, m)) => return Ok((p, m, false)),
            Err(e) => {
                let _ = tx.send(Event::Warning {
                    message: format!(
                        "last used model {last} is not available ({e}); using the default"
                    ),
                });
            }
        }
    }
    let wanted = config.settings.model.default.as_str();
    if !wanted.is_empty() {
        let Some((pid, mid)) = wanted.split_once('/') else {
            return Err(failed(format!(
                "[model].default `{wanted}` must be <provider>/<model>"
            )));
        };
        return named(config, providers, pid, mid, prov, tx)
            .await
            .map(|(p, m)| (p, m, false))
            .map_err(failed);
    }

    let mut candidates: Vec<(&Item<ProviderFile>, &ProviderFile)> = providers
        .iter()
        .filter_map(|i| i.loaded().map(|f| (i, f)))
        .collect();
    if candidates.is_empty() {
        let msg =
            "no providers configured · /setup (or `quack setup`) finds servers on this machine"
                .to_owned();
        return Err((
            LoadState::NeedsAttention {
                detail: msg.clone(),
            },
            msg,
        ));
    }
    // Higher layers first (project, user, system, defaults), then by id.
    candidates.sort_by(|a, b| b.0.layer.cmp(&a.0.layer).then_with(|| a.0.id.cmp(&b.0.id)));
    let mut reasons = Vec::new();
    for (item, file) in candidates {
        // The bundle's hosted services are offers: one is used once the user picks a
        // model, never on its own, since it may cost money.
        let offer = item.layer == LayerKind::Defaults && !quack_config::is_loopback(&file.base_url);
        if let Some(missing) = missing_env_key(file) {
            if !offer {
                reasons.push(format!("{}: {missing} not set", item.id));
            }
            continue;
        }
        if offer {
            reasons.push(format!("{}: key set; pick a model in /models", item.id));
            continue;
        }
        match try_provider(
            config,
            &item.id,
            file.clone(),
            None,
            PROBE_TIMEOUT,
            prov,
            tx,
        )
        .await
        {
            Ok((p, m)) => return Ok((p, m, true)),
            Err(e) => reasons.push(e),
        }
    }
    reasons.push("/setup finds servers on this machine and sets up hosted services".into());
    let msg = format!("no usable provider · {}", reasons.join(" · "));
    Err((
        LoadState::NeedsAttention {
            detail: msg.clone(),
        },
        msg,
    ))
}

/// A provider and model chosen by name: `[model].default`, or the one kept by a reload.
async fn named(
    config: &Loaded,
    providers: &[Item<ProviderFile>],
    pid: &str,
    mid: &str,
    prov: &Reporter,
    tx: &broadcast::Sender<Event>,
) -> Result<(Provider, ModelEntry), String> {
    let Some(item) = providers.iter().find(|p| p.id == pid) else {
        return Err(format!("provider `{pid}` not found in any providers.d/"));
    };
    let file = match &item.state {
        ItemState::Loaded(p) => p.clone(),
        ItemState::Disabled(why) => return Err(format!("provider `{pid}` is disabled: {why}")),
        ItemState::Failed(d) => return Err(d.one_line()),
    };
    try_provider(config, pid, file, Some(mid), DISCOVERY_TIMEOUT, prov, tx).await
}

/// The environment variable named by `api_key = { env = ... }`, when it is unset.
fn missing_env_key(file: &ProviderFile) -> Option<String> {
    match &file.api_key {
        Some(quack_config::Secret::Env { env, .. }) if std::env::var_os(env).is_none() => {
            Some(env.clone())
        }
        _ => None,
    }
}

async fn try_provider(
    config: &Loaded,
    pid: &str,
    file: ProviderFile,
    wanted_model: Option<&str>,
    timeout: Duration,
    prov: &Reporter,
    tx: &broadcast::Sender<Event>,
) -> Result<(Provider, ModelEntry), String> {
    prov.send(LoadState::Running {
        detail: format!("connecting to {pid}"),
    });
    let network = config.settings.network.clone();
    let id = pid.to_owned();
    let provider = tokio::task::spawn_blocking(move || {
        Provider::new(&id, file, &network, &|k| std::env::var_os(k))
    })
    .await
    .map_err(|e| format!("{pid}: {e}"))?
    .map_err(|e| format!("{pid}: {}", e.message))?;

    // Discover when no models are listed (the short provider form) or when asked to.
    let mut discovered: Vec<DiscoveredModel> = Vec::new();
    let mut discovery_error = None;
    // Also when the model to use is not listed, or listed without a context window or a
    // price: the models endpoint usually reports both, and the budget and costs depend on
    // them. A missing price alone is not worth a warning when the listing fails.
    let target = match wanted_model {
        Some(m) => provider.file.model(m),
        None => provider.file.models.first(),
    };
    let needed = provider.file.models.is_empty()
        || provider.file.discovery.enabled
        || target.is_none_or(|m| m.context_window.is_none());
    let priceless = target.is_some_and(|m| m.cost.is_none());
    if needed || priceless {
        prov.send(LoadState::Running {
            detail: format!("listing models at {pid}"),
        });
        match tokio::time::timeout(timeout, provider.list_models()).await {
            Ok(Ok(list)) => {
                let include = &provider.file.discovery.include;
                discovered = list
                    .into_iter()
                    .filter(|m| include.is_empty() || include.iter().any(|g| glob_match(g, &m.id)))
                    .collect();
            }
            Ok(Err(e)) => discovery_error = Some(e.message),
            Err(_) => discovery_error = Some("listing models timed out".into()),
        }
    }

    let mid = match wanted_model {
        Some(m) => m.to_owned(),
        None => provider
            .file
            .models
            .first()
            .map(|m| m.id.clone())
            .or_else(|| discovered.first().map(|m| m.id.clone()))
            .ok_or_else(|| match &discovery_error {
                Some(e) => format!("{pid}: {e}"),
                None => format!("{pid}: no models listed or discovered · add models = [\"…\"]"),
            })?,
    };
    if let Some(e) = discovery_error.as_ref().filter(|_| needed) {
        warn(tx, format!("provider `{pid}`: could not list models: {e}"));
    }
    let mut model = provider
        .file
        .model(&mid)
        .cloned()
        .unwrap_or_else(|| ModelEntry::new(mid.clone()));
    if let Some(d) = discovered.iter().find(|d| d.id == model.id) {
        model.context_window = model.context_window.or(d.context_window);
        if model.cost.is_none() {
            model.cost = d.cost.clone();
        }
    } else if !discovered.is_empty() && provider.file.model(&mid).is_none() {
        warn(
            tx,
            format!("model `{mid}` is not in {pid}'s model list; trying it anyway"),
        );
    }
    if let Some(cap) = config.settings.context.window {
        model.context_window = Some(model.context_window.map_or(cap, |w| w.min(cap)));
    }
    Ok((provider, model))
}

async fn build_setup(
    config: &Arc<Loaded>,
    provider: Provider,
    model: ModelEntry,
    model_ref: String,
    Files {
        instructions,
        skills: found,
        commands,
        mcp,
        lsp,
    }: Files,
    rep: &Reporter,
    tx: &broadcast::Sender<Event>,
) -> Option<Setup> {
    rep.send(LoadState::Running {
        detail: "loading profiles.d".into(),
    });
    let loaded = Arc::clone(config);
    let ((profiles, warnings), (agents, agent_warnings)) =
        tokio::task::spawn_blocking(move || (load_profiles(&loaded), env::agents(&loaded)))
            .await
            .unwrap_or_default();
    for w in warnings.into_iter().chain(agent_warnings) {
        warn(tx, w);
    }
    let mut names = vec![model.id.as_str()];
    names.extend(model.alias.as_deref());
    let requested = match config.settings.model.profile.as_str() {
        "" => model.profile.as_str(),
        forced => forced,
    };
    let profile = match select(&profiles, requested, &names) {
        Ok(p) => p,
        Err(e) => {
            rep.send(LoadState::Failed { error: e });
            return None;
        }
    };

    let root = config.dirs.workspace_root.clone();
    let shell = ShellKind::detect(
        &config.settings.tools.shell,
        std::env::var("SHELL").ok().as_deref(),
    );
    let timeout = Duration::from_secs(config.settings.tools.default_timeout_secs.max(1));
    let mut tool_ctx = ToolCtx {
        root: root.clone(),
        shell: shell.clone(),
        default_timeout: timeout,
        max_timeout: timeout.max(Duration::from_secs(600)),
        cancel: CancellationToken::new(),
        on_output: None,
        ask: None,
        reads: Arc::new(ReadTracker::default()),
        memory: Some(Arc::new(env::memory_store(config))),
        skills: Arc::new(found.skills),
        lsp: Some(lsp),
        sandbox: crate::sandbox::state(config),
        plan: Default::default(),
        agents: Arc::new(
            agents
                .iter()
                .map(|(id, a)| quack_tools::AgentInfo {
                    id: id.clone(),
                    description: a.description.trim().to_owned(),
                    read_only: a.read_only,
                })
                .collect(),
        ),
        spawn: None,
        edit_format: EditFormat::StrReplace,
        vision: false,
        // About 3.5 bytes per token.
        outline_above: config.settings.tools.read_outline_tokens as usize * 7 / 2,
        grammars: Arc::new(quack_tools::grammar::Grammars::from_layers(
            config.item_layers().iter().map(|(_, d)| d.as_path()),
        )),
        observations: Arc::new(quack_tools::recall::Observations::new(
            config.settings.context.recall,
        )),
    };
    let registry = Arc::new(Registry::builtin());
    let vars = PromptVars {
        os: std::env::consts::OS.into(),
        shell: shell.label().into(),
        cwd: config.dirs.cwd.display().to_string(),
        workspace_root: root.display().to_string(),
        date: env::today(),
        model: model.id.clone(),
        context_window: model.context_window,
        instructions: instructions.text,
        folder_listing: env::folder_listing(&root),
        git_branch: env::git_branch(&root),
        skills: skills::index(
            &tool_ctx.skills,
            config.settings.skills.index_max_tokens as usize * 7 / 2,
        ),
        memory_index: tool_ctx.memory.as_ref().map_or_else(String::new, |m| {
            // About 3.5 bytes per token.
            m.index_text(config.settings.memory.index_max_tokens as usize * 7 / 2)
        }),
        ..PromptVars::default()
    };
    // `shape` reads `vision` to decide whether `view_image` is offered.
    tool_ctx.vision = model.vision;
    let shaped = shape(
        &profile,
        config.settings.tools.edit_format,
        &registry,
        &tool_ctx,
        &vars,
    );
    for w in shaped.warnings {
        warn(tx, w);
    }
    let (system, session_context) = match (shaped.system, shaped.context) {
        (Ok(s), Ok(c)) => (s, c),
        (Err(e), _) | (_, Err(e)) => {
            rep.send(LoadState::Failed { error: e });
            return None;
        }
    };
    let surface = shaped.surface;
    tool_ctx.edit_format = shaped.edit_format;
    let mut settings = config.settings.permissions.clone();
    if let Some(cap) = config.settings.policy.max_permission_mode {
        settings.mode = settings.mode.min(cap);
    }
    let (mut permissions, perm_warnings) = Permissions::new(&settings, &root);
    permissions.memory_writes_ask = config.settings.memory.writes == MemoryWrites::Ask;
    permissions.powershell = matches!(shell, ShellKind::Pwsh(_) | ShellKind::PowerShell(_));
    for w in perm_warnings {
        warn(tx, format!("[permissions]: {w}"));
    }

    rep.send(LoadState::Ready {
        detail: match &profile.dir {
            Some(_) => format!(
                "{} · {} tools · {} mode",
                profile.id,
                surface.tools.len(),
                settings.mode.label()
            ),
            None => format!(
                "empty (no profiles installed) · {} tools · {} mode",
                surface.tools.len(),
                settings.mode.label()
            ),
        },
    });
    let budget = model.context_window.map(|c| {
        let ratio = config.settings.context.budget_ratio.clamp(0.1, 1.0);
        (f64::from(c) * ratio) as u32
    });
    Some(Setup {
        provider,
        model,
        model_ref,
        profile,
        system,
        session_context,
        surface,
        registry,
        tool_ctx,
        permissions,
        budget,
        estimator: std::sync::Mutex::default(),
        context: config.settings.context.clone(),
        instruction_dirs: instructions.dirs,
        mcp,
        vars,
        commands: Arc::new(commands),
        agents: Arc::new(agents),
        mcp_tools: true,
        cramped: Default::default(),
    })
}

/// `/profiles`: every installed profile, previewed for the session's model.
pub(crate) fn profiles(config: &Loaded, setup: &Setup) -> Event {
    let (installed, _) = load_profiles(config);
    let mut names = vec![setup.model.id.as_str()];
    names.extend(setup.model.alias.as_deref());
    let auto = select(&installed, &setup.model.profile, &names)
        .map(|p| p.id)
        .unwrap_or_default();
    let profiles = installed
        .iter()
        .map(|p| {
            let shaped = shape(
                p,
                config.settings.tools.edit_format,
                &setup.registry,
                &setup.tool_ctx,
                &setup.vars,
            );
            ProfileInfo {
                id: p.id.clone(),
                description: p.file.description.clone(),
                source: p.file.source.clone(),
                matches: p.id == auto,
                edit_format: serde_json::to_value(shaped.edit_format)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                tools: shaped
                    .surface
                    .tools
                    .iter()
                    .map(|t| t.name.clone())
                    .collect(),
                system: match (shaped.system, shaped.context) {
                    (Ok(s), Ok(c)) if c.is_empty() => s,
                    (Ok(s), Ok(c)) => {
                        format!("{s}\n\n── At the start of the first user message ──\n\n{c}")
                    }
                    (Err(e), _) | (_, Err(e)) => format!("(does not render: {e})"),
                },
            }
        })
        .collect();
    Event::Profiles {
        active: setup.profile.id.clone(),
        forced: !config.settings.model.profile.is_empty(),
        profiles,
    }
}

/// What a profile makes of a session: its edit format, tools and system prompt.
pub(crate) struct Shaped {
    pub edit_format: EditFormat,
    pub surface: ToolSurface,
    pub warnings: Vec<String>,
    pub system: Result<String, String>,
    /// For the first user message (`context.md`).
    pub context: Result<String, String>,
}

/// Apply `profile` to the session's tools and prompt variables. Used at startup and to
/// preview other profiles in `/profiles`.
pub(crate) fn shape(
    profile: &Profile,
    setting: EditFormat,
    registry: &Registry,
    base: &ToolCtx,
    vars: &PromptVars,
) -> Shaped {
    let mut ctx = base.clone();
    ctx.edit_format = edit_format(setting, profile);
    let internal: Vec<InternalTool> = registry
        .iter()
        .filter(|t| t.available(&ctx))
        .map(|t| InternalTool {
            name: t.name().to_owned(),
            description: t.description(&ctx),
            schema: t.schema(),
        })
        .collect();
    let known: Vec<&str> = registry.iter().map(|t| t.name()).collect();
    let (surface, warnings) = ToolSurface::build(profile, &internal, &known);
    let mut vars = vars.clone();
    vars.write_tool = surface.name_of("write").unwrap_or("write").to_owned();
    vars.edit_tool = surface
        .name_of(match ctx.edit_format {
            EditFormat::Hashline => "edit_hashline",
            EditFormat::ApplyPatch => "apply_patch",
            EditFormat::SearchReplaceBlocks => "edit_blocks",
            EditFormat::WholeFile => "write",
            EditFormat::StrReplace | EditFormat::Auto => "edit",
        })
        .unwrap_or("write")
        .to_owned();
    Shaped {
        edit_format: ctx.edit_format,
        system: profile.render_system(&vars),
        context: profile.render_context(&vars),
        surface,
        warnings,
    }
}

/// `[tools] edit_format`, else the profile's, else `str_replace`.
fn edit_format(setting: EditFormat, profile: &Profile) -> EditFormat {
    match setting {
        EditFormat::Auto => profile
            .file
            .defaults
            .edit_format
            .filter(|f| *f != EditFormat::Auto)
            .unwrap_or(EditFormat::StrReplace),
        chosen => chosen,
    }
}

fn report_commands(tx: &broadcast::Sender<Event>, commands: &[CustomCommand], failed: &[String]) {
    for f in failed {
        warn(tx, format!("command skipped: {f}"));
    }
    let _ = tx.send(Event::Commands {
        commands: commands
            .iter()
            .map(|c| CommandInfo {
                name: c.name.clone(),
                description: c.description.clone(),
                args: c.args.clone(),
                source: c.source.clone(),
                path: c.path.display().to_string(),
            })
            .collect(),
    });
}

fn report_skills(tx: &broadcast::Sender<Event>, found: &skills::Found) {
    let rep = Reporter {
        tx: tx.clone(),
        subsystem: Subsystem::Skills,
    };
    for f in &found.failed {
        warn(tx, format!("skill skipped: {f}"));
    }
    rep.send(match (found.skills.len(), found.failed.len()) {
        (0, 0) => LoadState::Disabled {
            reason: "no skills found".into(),
        },
        (0, n) => LoadState::Failed {
            error: format!("{n} invalid skill(s); see the warnings"),
        },
        (n, 0) => LoadState::Ready {
            detail: format!("{n} skill{}", if n == 1 { "" } else { "s" }),
        },
        (n, bad) => LoadState::NeedsAttention {
            detail: format!("{n} loaded, {bad} invalid (see the warnings)"),
        },
    });
    let _ = tx.send(Event::Skills {
        skills: found
            .skills
            .iter()
            .map(|s| SkillInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                source: s.source.clone(),
                dir: s.dir.display().to_string(),
            })
            .collect(),
    });
}

fn project_state(config: &Loaded) -> LoadState {
    let trust = |what: &str| LoadState::NeedsAttention {
        detail: format!(
            "{what} ({}); skipped · /trust to review",
            config.project_summary().describe()
        ),
    };
    match config.project {
        ProjectStatus::None => LoadState::Disabled {
            reason: "no .quack/ in this workspace".into(),
        },
        ProjectStatus::Trusted => LoadState::Ready {
            detail: "trusted".into(),
        },
        ProjectStatus::SettingsOnly => LoadState::Ready {
            detail: "trusted (settings only)".into(),
        },
        ProjectStatus::Untrusted => trust("project config found"),
        ProjectStatus::Changed => trust("project config changed since you trusted it"),
        ProjectStatus::Ignored => LoadState::Disabled {
            reason: "ignored (/trust to change)".into(),
        },
        ProjectStatus::DisabledByPolicy => LoadState::Disabled {
            reason: "disabled by policy".into(),
        },
    }
}
