//! `cutoffharness setup`: a short question-and-answer wizard that adds a provider and
//! picks the default model (docs/plan/14-packaging-and-distribution.md#first-run-experience).
//!
//! Every file it writes is a minimal provider file in the user layer. Input is read line by
//! line, so the wizard can also be scripted through stdin.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use cutoffharness_config::{ItemState, LayerKind, Loaded, Secret, load_providers};
use cutoffharness_core::setup;
use cutoffharness_exec::exit;

/// How many models to list before asking the user to type an id.
const MAX_LISTED: usize = 30;

pub(crate) struct Io<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    /// Reads a secret without echoing it.
    pub secret: &'a dyn Fn(&str) -> std::io::Result<String>,
}

impl Io<'_> {
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }

    /// One trimmed line; `None` at end of input.
    fn ask(&mut self, prompt: &str) -> Option<String> {
        let _ = write!(self.out, "{prompt}");
        let _ = self.out.flush();
        let mut line = String::new();
        match self.input.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim().to_owned()),
        }
    }
}

pub(crate) async fn run(config: Arc<Loaded>, io: &mut Io<'_>) -> i32 {
    io.say("Checking the providers you have…");
    let providers = setup::providers(config.clone()).await;
    if providers.is_empty() {
        io.say("  none configured yet\n");
    } else {
        for p in &providers {
            let mark = if p.available { "✓" } else { "✗" };
            io.say(&format!("  {mark} {:<14} {}", p.id, p.note));
        }
        io.say("");
    }
    loop {
        io.say("What would you like to do?");
        io.say("  1  look for model servers on this machine (Ollama, llama.cpp, vLLM, …)");
        io.say("  2  use a hosted service with an API key");
        io.say("  3  add an OpenAI-compatible server by its URL");
        io.say("  q  quit");
        let Some(choice) = io.ask("> ") else {
            return exit::OK;
        };
        let added = match choice.as_str() {
            "1" => local(&config, io).await,
            "2" => hosted(&config, io),
            "3" => by_url(&config, io),
            "q" | "Q" | "" => return exit::OK,
            _ => {
                io.say("Type 1, 2, 3 or q.\n");
                continue;
            }
        };
        match added {
            Ok(Some(id)) => return pick_model(&config, io, &id).await,
            Ok(None) => io.say(""),
            Err(e) => io.say(&format!("{e}\n")),
        }
    }
}

/// Detect local servers and add the chosen one. Returns the provider id.
async fn local(config: &Arc<Loaded>, io: &mut Io<'_>) -> Result<Option<String>, String> {
    io.say("Looking on 127.0.0.1 (ports 11434, 8080, 8000, 1234, 30000)…");
    let found = setup::detect(config.clone()).await;
    if found.is_empty() {
        io.say("No server answered. Start one (for example `ollama serve`, or");
        io.say("`llama-server --jinja -m <model.gguf>`) and try again, or use option 3");
        io.say("if it listens on another port.");
        return Ok(None);
    }
    for (i, s) in found.iter().enumerate() {
        let note = s
            .configured
            .as_ref()
            .map_or_else(String::new, |id| format!("  (already set up as `{id}`)"));
        let models = match s.models.len() {
            1 => "1 model".to_owned(),
            n => format!("{n} models"),
        };
        io.say(&format!(
            "  {}  {:<10} {:<28} {models}{note}",
            i + 1,
            s.name,
            s.base_url,
        ));
    }
    let Some(server) = choose(io, &found, "Which one? [1] ")? else {
        return Ok(None);
    };
    if let Some(id) = &server.configured {
        return Ok(Some(id.clone()));
    }
    let comment = format!("{} on this machine, found by setup.", server.name);
    let added = setup::add_provider(
        config,
        &server.id,
        &server.base_url,
        server.api.as_deref(),
        &comment,
    )?;
    if let Some(path) = &added.path {
        io.say(&format!("Wrote {}", path.display()));
    }
    Ok(Some(added.id))
}

/// Configured providers that take a key from an environment variable.
fn hosted(config: &Loaded, io: &mut Io<'_>) -> Result<Option<String>, String> {
    let items = load_providers(config);
    let services: Vec<(String, String, PathBuf, LayerKind)> = items
        .iter()
        .filter_map(|i| match &i.state {
            ItemState::Loaded(f) => match &f.api_key {
                Some(Secret::Env { env, .. }) => {
                    Some((i.id.clone(), env.clone(), i.path.clone(), i.layer))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    if services.is_empty() {
        io.say("No hosted services are configured. The defaults bundle ships files for");
        io.say("several (`cutoffharness config paths` shows where it is looked for); or use");
        io.say("option 3 with the service's URL.");
        return Ok(None);
    }
    for (i, (id, env, _, _)) in services.iter().enumerate() {
        let state = if std::env::var_os(env).is_some() {
            format!("{env} is set")
        } else {
            format!("needs {env}")
        };
        io.say(&format!("  {}  {id:<14} {state}", i + 1));
    }
    let Some((id, env, path, layer)) = choose(io, &services, "Which one? [1] ")? else {
        return Ok(None);
    };
    if std::env::var_os(env).is_some() {
        return Ok(Some(id.clone()));
    }
    io.say(&format!("Where should the {id} API key come from?"));
    io.say("  1  the OS credential store (stored now, recommended)");
    io.say(&format!("  2  the {env} environment variable (you set it)"));
    match io.ask("> [1] ").as_deref() {
        Some("2") => {
            io.say(&format!("Set {env} in your shell profile, for example"));
            io.say(&format!("  export {env}=…            (bash, zsh)"));
            io.say(&format!("  $env:{env} = \"…\"         (PowerShell)"));
            io.say("then start cutoffharness, or run setup again to pick a model.");
            Ok(None)
        }
        None => Ok(None),
        Some(_) => {
            store_key(io, id)?;
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let text = with_keyring_key(&text, id);
            let target = setup::providers_dir(config)?.join(format!("{id}.toml"));
            if *layer == LayerKind::User {
                std::fs::write(&target, text)
                    .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
            } else {
                setup::write_new(&target, &text)?;
            }
            io.say(&format!("Wrote {}", target.display()));
            Ok(Some(id.clone()))
        }
    }
}

/// Any OpenAI-compatible endpoint.
fn by_url(config: &Loaded, io: &mut Io<'_>) -> Result<Option<String>, String> {
    let Some(url) = io.ask("Base URL (for example http://gpu-box:8000/v1): ") else {
        return Ok(None);
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("The URL must start with http:// or https://".into());
    }
    let suggested = id_from_url(&url);
    let id = io
        .ask(&format!("Name for this provider [{suggested}]: "))
        .filter(|s| !s.is_empty())
        .unwrap_or(suggested);
    setup::check_id(&id)?;
    let key = (io.secret)("API key, if it needs one (input hidden; Enter for none): ")
        .map_err(|e| format!("cannot read the key: {e}"))?;
    let mut text = setup::provider_text("Added by setup.", &url, None);
    if !key.is_empty() {
        cutoffharness_config::keyring_set(&id, &key).map_err(|e| e.to_string())?;
        text.push_str(&format!("api_key = {{ keyring = \"{id}\" }}\n"));
    }
    let target = setup::providers_dir(config)?.join(format!("{id}.toml"));
    if target.exists() {
        return Err(format!(
            "{} already exists; pick another name",
            target.display()
        ));
    }
    setup::write_new(&target, &text)?;
    io.say(&format!("Wrote {}", target.display()));
    Ok(Some(id))
}

fn store_key(io: &mut Io<'_>, id: &str) -> Result<(), String> {
    let key = (io.secret)(&format!("{id} API key (input hidden): "))
        .map_err(|e| format!("cannot read the key: {e}"))?;
    if key.is_empty() {
        return Err("Nothing entered; nothing stored.".into());
    }
    cutoffharness_config::keyring_set(id, &key).map_err(|e| e.to_string())
}

/// List the provider's models and save the chosen one as `model.default`.
async fn pick_model(config: &Arc<Loaded>, io: &mut Io<'_>, id: &str) -> i32 {
    io.say(&format!("Asking {id} for its models…"));
    let Some(p) = setup::provider_models(config.clone(), id).await else {
        io.say(&format!(
            "`{id}` did not load; `cutoffharness config check` says why."
        ));
        return exit::CONFIG_ERROR;
    };
    if !p.available {
        io.say(&format!("{id} is not usable yet: {}", p.note));
        return exit::CONFIG_ERROR;
    }
    for (i, m) in p.models.iter().take(MAX_LISTED).enumerate() {
        let ctx = m
            .context_window
            .map_or_else(String::new, |c| format!("  {}k context", c / 1024));
        io.say(&format!("  {:>2}  {}{ctx}", i + 1, m.id));
    }
    if p.models.len() > MAX_LISTED {
        io.say(&format!(
            "  … {} more; type a model id to pick one not listed",
            p.models.len() - MAX_LISTED
        ));
    }
    let model = match io.ask("Default model [1]: ") {
        None => return exit::OK,
        Some(a) if a.is_empty() => p.models[0].id.clone(),
        Some(a) => match a.parse::<usize>() {
            Ok(n) if (1..=p.models.len()).contains(&n) => p.models[n - 1].id.clone(),
            _ => a,
        },
    };
    let reference = format!("{id}/{model}");
    let quoted = serde_json::Value::from(reference.as_str()).to_string();
    match config.save_setting(LayerKind::User, "model.default", &quoted) {
        Ok((path, note)) => {
            io.say(&format!(
                "Saved model.default = {quoted} in {}",
                path.display()
            ));
            if let Some(note) = note {
                io.say(&format!("Note: {note}"));
            }
            io.say("Done. Run `cutoffharness` to start.");
            exit::OK
        }
        Err(e) => {
            io.say(&format!("cannot save the default model: {e}"));
            exit::CONFIG_ERROR
        }
    }
}

/// A numbered choice; Enter picks the first.
fn choose<'a, T>(io: &mut Io<'_>, items: &'a [T], prompt: &str) -> Result<Option<&'a T>, String> {
    let Some(answer) = io.ask(prompt) else {
        return Ok(None);
    };
    if answer.is_empty() {
        return Ok(items.first());
    }
    match answer.parse::<usize>() {
        Ok(n) if (1..=items.len()).contains(&n) => Ok(items.get(n - 1)),
        _ => Err(format!("`{answer}` is not one of the numbers above.")),
    }
}

/// Point a provider file's `api_key` at the credential store, keeping everything else.
fn with_keyring_key(text: &str, id: &str) -> String {
    let line = format!("api_key = {{ keyring = \"{id}\" }}");
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for l in text.lines() {
        if l.trim_start().starts_with("api_key") {
            out.push(line.clone());
            replaced = true;
        } else {
            out.push(l.to_owned());
        }
    }
    if !replaced {
        out.push(line);
    }
    out.join("\n") + "\n"
}

/// `https://api.example.com/v1` → `example`; `http://10.0.0.5:8000/v1` → `server-8000`.
fn id_from_url(url: &str) -> String {
    let host_port = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or_default();
    let (host, port) = host_port.rsplit_once(':').unwrap_or((host_port, ""));
    let is_ip = host.chars().all(|c| c.is_ascii_digit() || c == '.');
    let name = if is_ip || host == "localhost" {
        String::new()
    } else {
        let labels: Vec<&str> = host
            .split('.')
            .filter(|l| !matches!(*l, "api" | "www" | "llm" | "inference"))
            .collect();
        let pick = if labels.len() >= 2 {
            labels[labels.len() - 2]
        } else {
            labels.first().copied().unwrap_or_default()
        };
        pick.to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    };
    match (name.is_empty(), port.is_empty()) {
        (false, _) => name,
        (true, false) => format!("server-{port}"),
        (true, true) => "server".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated(root: &std::path::Path) -> Loaded {
        let vars: std::collections::HashMap<String, std::ffi::OsString> = [
            ("CUTOFFHARNESS_CONFIG_DIR", "user"),
            ("CUTOFFHARNESS_SYSTEM_DIR", "system"),
            ("CUTOFFHARNESS_DATA_DIR", "data"),
            ("CUTOFFHARNESS_CACHE_DIR", "cache"),
        ]
        .into_iter()
        .map(|(k, d)| (k.to_owned(), root.join(d).into_os_string()))
        .chain([("CUTOFFHARNESS_DEFAULTS_DIR".to_owned(), "none".into())])
        .collect();
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).expect("mkdir");
        cutoffharness_config::load_with(
            &move |k| vars.get(k).cloned(),
            &cutoffharness_config::LoadOptions::new(ws),
        )
        .expect("load")
    }

    #[tokio::test]
    async fn wizard_adds_a_server_by_url_and_saves_the_default() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": [{"id": "small"}, {"id": "coder"}]})),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = Arc::new(isolated(tmp.path()));
        let url = format!("{}/v1", server.uri());
        // Option 3, the URL, the suggested name, then the second model.
        let script = format!("3\n{url}\n\n2\n");
        let mut input = std::io::Cursor::new(script.into_bytes());
        let mut out = Vec::new();
        let mut io = Io {
            input: &mut input,
            out: &mut out,
            secret: &|_| Ok(String::new()),
        };
        let code = run(config, &mut io).await;
        let out = String::from_utf8_lossy(&out);
        assert_eq!(code, exit::OK, "{out}");
        let id = id_from_url(&url);
        let file = std::fs::read_to_string(tmp.path().join(format!("user/providers.d/{id}.toml")))
            .expect("provider file");
        assert_eq!(file, format!("# Added by setup.\nbase_url = \"{url}\"\n"));
        let cfg = std::fs::read_to_string(tmp.path().join("user/config.toml")).expect("config");
        assert!(cfg.contains(&format!("default = \"{id}/coder\"")), "{cfg}");
        assert!(out.contains("none configured yet"), "{out}");
    }

    #[test]
    fn ids_from_urls() {
        assert_eq!(id_from_url("https://api.example.com/v1"), "example");
        assert_eq!(id_from_url("http://gpu-box:8000/v1"), "gpu-box");
        assert_eq!(id_from_url("http://10.0.0.5:8000/v1"), "server-8000");
        assert_eq!(id_from_url("https://inference.corp.example.org"), "example");
    }

    #[test]
    fn keyring_key_replaces_the_env_key() {
        let text =
            "# Novita.\nbase_url = \"https://x/v1\"\napi_key = { env = \"NOVITA_API_KEY\" }\n";
        assert_eq!(
            with_keyring_key(text, "novita"),
            "# Novita.\nbase_url = \"https://x/v1\"\napi_key = { keyring = \"novita\" }\n"
        );
    }
}
