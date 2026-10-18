use std::io::Read;
use std::path::PathBuf;

use cutoffharness_config::{ItemState, LayerStatus, Loaded, load_providers, schema};
use cutoffharness_exec::exit;

pub(crate) fn config_check(loaded: &Loaded) -> i32 {
    let mut errors = 0;
    for d in &loaded.diagnostics {
        eprintln!("{:?}", miette::Report::new(d.clone()));
        errors += 1;
    }
    let providers = load_providers(loaded);
    for item in providers {
        match item.state {
            ItemState::Loaded(_) => {
                println!("ok       provider {:<20} {}", item.id, item.path.display())
            }
            ItemState::Disabled(why) => println!("disabled provider {:<20} {why}", item.id),
            ItemState::Failed(d) => {
                eprintln!("{:?}", miette::Report::new(d));
                errors += 1;
            }
        }
    }
    let profiles = cutoffharness_config::load_dir_items::<cutoffharness_config::profile::ProfileFile>(
        &loaded.item_layers(),
        "profiles.d",
        "profile.toml",
    );
    for item in profiles {
        match item.state {
            ItemState::Loaded(p) => {
                let prompt = item.path.with_file_name(&p.prompt.system);
                if prompt.is_file() {
                    println!("ok       profile  {:<20} {}", item.id, item.path.display());
                } else {
                    eprintln!(
                        "error: profile `{}`: prompt file {} is missing",
                        item.id,
                        prompt.display()
                    );
                    errors += 1;
                }
            }
            ItemState::Disabled(why) => println!("disabled profile  {:<20} {why}", item.id),
            ItemState::Failed(d) => {
                eprintln!("{:?}", miette::Report::new(d));
                errors += 1;
            }
        }
    }
    for w in &loaded.warnings {
        eprintln!("warning: {w}");
    }
    if errors > 0 {
        eprintln!("{errors} error(s)");
        exit::CONFIG_ERROR
    } else {
        println!("configuration ok");
        exit::OK
    }
}

pub(crate) fn config_show(loaded: &Loaded, origin: bool) -> i32 {
    print!("{}", loaded.render(origin));
    exit::OK
}

pub(crate) fn config_schema(loaded: &Loaded, dir: Option<PathBuf>) -> i32 {
    let dir = dir.unwrap_or_else(|| loaded.dirs.user_config.join("schemas"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return exit::CONFIG_ERROR;
    }
    for (name, value) in schema::all() {
        let path = dir.join(name);
        let text = serde_json::to_string_pretty(&value).unwrap_or_default() + "\n";
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return exit::CONFIG_ERROR;
        }
        println!("wrote {}", path.display());
    }
    exit::OK
}

pub(crate) fn config_paths(loaded: &Loaded) -> i32 {
    let d = &loaded.dirs;
    let defaults = d
        .defaults
        .as_ref()
        .map_or_else(|| "(none)".into(), |p| p.display().to_string());
    println!("defaults   {defaults}");
    println!("system     {}", d.system.display());
    println!("user       {}", d.user_config.display());
    println!("data       {}", d.user_data.display());
    println!("cache      {}", d.user_cache.display());
    println!("workspace  {}", d.workspace_root.display());
    println!("project    {}", d.project.display());
    exit::OK
}

pub(crate) fn doctor(loaded: &Loaded) -> i32 {
    println!("cutoffharness {}\n", env!("CARGO_PKG_VERSION"));
    println!("config layers:");
    for layer in &loaded.layers {
        let dir = layer
            .dir
            .as_ref()
            .map_or_else(|| "-".into(), |p| p.display().to_string());
        let status = match &layer.status {
            LayerStatus::Loaded => "loaded".to_owned(),
            LayerStatus::Missing => "no config.toml".to_owned(),
            LayerStatus::Failed => "FAILED (run `config check`)".to_owned(),
            LayerStatus::Skipped(why) => format!("skipped: {why}"),
        };
        println!("  {:<9} {status:<28} {dir}", layer.kind.label());
    }
    println!("\nproject: {:?}", loaded.project);
    println!(
        "terminal: TERM={} NO_COLOR={}",
        std::env::var("TERM").unwrap_or_default(),
        std::env::var_os("NO_COLOR").is_some()
    );
    println!("\nProvider reachability, tokenizer, MCP/LSP and Windows checks land in M1.");
    if loaded.diagnostics.is_empty() {
        exit::OK
    } else {
        exit::CONFIG_ERROR
    }
}

pub(crate) fn read_prompt(arg: &str) -> std::io::Result<String> {
    if arg != "-" {
        return Ok(arg.to_owned());
    }
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s.trim_end().to_owned())
}

pub(crate) fn sessions(loaded: &Loaded) -> i32 {
    let (dir, list) = cutoffharness_core::list_sessions(loaded);
    if list.is_empty() {
        println!("no saved sessions for this project ({})", dir.display());
        return exit::OK;
    }
    let now = cutoffharness_store::time::unix_now();
    println!(
        "{:<14} {:>5} {:>5}  {:<28} title",
        "id", "age", "turns", "model"
    );
    for e in list.iter().take(50) {
        let id: String = e.id.chars().take(12).collect();
        let title: String = e.title.chars().take(60).collect();
        println!(
            "{id:<14} {:>5} {:>5}  {:<28} {title}",
            cutoffharness_store::time::age(e.updated, now),
            e.turns,
            e.model
        );
    }
    println!("\nresume with: cutoffharness --resume <id>   (or --continue for the latest)");
    exit::OK
}
