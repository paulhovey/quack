"""Benchmark adapter registering quack with Harbor and Pier harnesses."""

import json
import os
import shlex
import tempfile
from pathlib import Path, PurePosixPath

BIN = "/usr/local/bin/quack"
DEFAULTS = "/usr/local/share/quack/defaults"
# Both frameworks copy the container's /logs/agent to the trial's agent dir before
# populate_context_post_run, even after a timeout, so the log and the session go there.
AGENT_LOGS = PurePosixPath("/logs/agent")
PROMPT = "/tmp/quack_instruction.txt"


def quack_model(name: str | None) -> str:
    m = name or "kimi-k3"
    if m.startswith("openai/"):
        m = m[len("openai/"):]
    if "/" not in m:
        m = f"charm-hyper/{m}"
    return m


def api_key() -> str:
    key = os.environ.get("CHARM_API_KEY") or os.environ.get("OPENAI_API_KEY") or ""
    if not key and os.path.exists("/etc/environment"):
        try:
            with open("/etc/environment") as ef:
                for line in ef:
                    if line.startswith("CHARM_API_KEY="):
                        key = line.split("=", 1)[1].strip().strip('"').strip("'")
                        break
        except OSError:
            pass
    return key


async def install_quack(environment) -> None:
    """Upload the host's binary and defaults bundle, then check the binary runs.

    The binary only ever arrives by upload: Pier inlines install_spec into the agent
    image's Dockerfile, where no binary exists yet. Raising here fails the trial as an
    install error instead of letting it run and score 0 with `quack: command not found`.
    """
    if not os.path.isfile(BIN):
        raise RuntimeError(f"{BIN} is missing on the host · run evals/adapters/install-quack.sh")
    await environment.exec(f"mkdir -p /usr/local/bin {DEFAULTS}", user="root")
    await environment.upload_file(BIN, BIN)
    await environment.upload_dir(DEFAULTS, DEFAULTS)
    await environment.exec(f"chmod +x {BIN}", user="root")
    res = await environment.exec(f"{BIN} --version")
    if res.return_code != 0:
        raise RuntimeError(
            f"quack does not run in the task container (exit {res.return_code}): "
            f"{(res.stdout or '').strip()} {(res.stderr or '').strip()} "
            "· build a static binary with scripts/install.sh --target x86_64-unknown-linux-musl"
        )


async def ensure_quack(environment) -> None:
    res = await environment.exec(f"{BIN} --version")
    if res.return_code != 0:
        await install_quack(environment)


async def upload_prompt(environment, instruction: str) -> None:
    with tempfile.NamedTemporaryFile("w", delete=False) as f:
        f.write(instruction)
        tmp_local = f.name
    try:
        await environment.upload_file(tmp_local, PROMPT)
    finally:
        os.unlink(tmp_local)


def quack_env(extra: dict[str, str] | None = None) -> dict[str, str]:
    env = dict(extra or {})
    key = api_key()
    if key:
        env["CHARM_API_KEY"] = key
    env["QUACK_DEFAULTS_DIR"] = DEFAULTS
    env["QUACK_DATA_DIR"] = (AGENT_LOGS / "quack-data").as_posix()
    return env


def quack_command(model: str) -> str:
    # pipefail keeps quack's exit code through tee; the code is logged, not raised, so
    # the verifier still scores whatever the agent left behind.
    log = (AGENT_LOGS / "quack.log").as_posix()
    return (
        f"set -o pipefail; mkdir -p {AGENT_LOGS}; "
        f"cat {PROMPT} | {BIN} --yolo -m {shlex.quote(model)} exec - 2>&1 | tee {log}; "
        f'echo "quack exit code: $?" >> {log}'
    )


def session_usage(logs_dir: Path) -> dict | None:
    """Sum quack's session logs: one `usage` record per model call, `agent_usage` per
    sub-agent call, each with `cost` when the model is priced. Records are flushed as
    they are written, so a run killed at the timeout still counts up to that point."""
    sessions = Path(logs_dir) / "quack-data" / "sessions"
    logs = [p for p in sessions.glob("*/*/events.jsonl")]
    if not logs:
        return None
    prompt = cached = completion = calls = agent_calls = compactions = peak = 0
    cost, unpriced = 0.0, 0
    for path in logs:
        for line in path.read_text(errors="replace").splitlines():
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            kind = rec.get("t")
            if kind == "compaction":
                compactions += 1
            if kind not in ("usage", "agent_usage"):
                continue
            u = rec.get("usage") or {}
            p = int(u.get("prompt_tokens") or 0)
            prompt += p
            cached += int(u.get("cached_prompt_tokens") or 0)
            completion += int(u.get("completion_tokens") or 0)
            if kind == "usage":
                calls += 1
                peak = max(peak, p)
            else:
                agent_calls += 1
            if isinstance(rec.get("cost"), (int, float)):
                cost += rec["cost"]
            else:
                unpriced += 1
    return {
        "n_input_tokens": prompt,
        "n_cache_tokens": cached,
        "n_output_tokens": completion,
        # A partial sum would understate the cost, so leave it out.
        "cost_usd": cost if unpriced == 0 and calls + agent_calls else None,
        "peak_context_tokens": peak,
        "summarization_count": compactions,
        "n_agent_steps": calls,
        "metadata": {
            "model_calls": calls,
            "sub_agent_calls": agent_calls,
            "unpriced_calls": unpriced,
            "sessions": len(logs),
        },
    }


def fill_context(context, logs_dir: Path) -> None:
    usage = session_usage(logs_dir)
    if usage is None:
        return
    fields = getattr(type(context), "model_fields", {})
    for name, value in usage.items():
        if name in fields:
            setattr(context, name, value)


# --- Harbor Adapter ---
try:
    from harbor.agents.installed.base import BaseInstalledAgent as HarborBaseInstalledAgent
    from harbor.environments.base import BaseEnvironment as HarborBaseEnvironment
    from harbor.models.agent.context import AgentContext as HarborAgentContext
    from harbor.agents.model_connection import ModelConnectionSpec as HarborModelConnectionSpec

    class QuackHarborAgent(HarborBaseInstalledAgent):
        MODEL_CONNECTION = HarborModelConnectionSpec(passthrough=True)

        @staticmethod
        def name() -> str:
            return "quack"

        def version(self) -> str | None:
            return "0.1.0"

        async def install(self, environment: HarborBaseEnvironment) -> None:
            await install_quack(environment)

        def populate_context_post_run(self, context: HarborAgentContext) -> None:
            fill_context(context, self.logs_dir)

        async def run(
            self,
            instruction: str,
            environment: HarborBaseEnvironment,
            context: HarborAgentContext,
        ) -> None:
            await ensure_quack(environment)
            await upload_prompt(environment, instruction)
            env = quack_env()
            await environment.exec(quack_command(quack_model(self.model_name)), env=env)
except ImportError:
    pass

# --- Pier Adapter ---
try:
    from pier.agents.installed.base import BaseInstalledAgent as PierBaseInstalledAgent
    from pier.environments.base import BaseEnvironment as PierBaseEnvironment
    from pier.models.agent.context import AgentContext as PierAgentContext
    from pier.models.agent.install import AgentInstallSpec, InstallStep
    from pier.models.agent.network import NetworkAllowlist

    class QuackPierAgent(PierBaseInstalledAgent):
        @staticmethod
        def name() -> str:
            return "quack"

        def version(self) -> str | None:
            return "0.1.0"

        def network_allowlist(self) -> NetworkAllowlist:
            return NetworkAllowlist(domains=["hyper.charm.land"])

        def install_spec(self) -> AgentInstallSpec:
            # Baked into the agent image, where the binary isn't there yet; setup()
            # uploads and checks it.
            return AgentInstallSpec(
                agent_name=self.name(),
                version=self.version(),
                steps=[InstallStep(user="root", run="true")],
            )

        def populate_context_post_run(self, context: PierAgentContext) -> None:
            fill_context(context, self.logs_dir)

        async def setup(self, environment: PierBaseEnvironment) -> None:
            await install_quack(environment)

        async def install(self, environment: PierBaseEnvironment) -> None:
            await install_quack(environment)

        async def run(
            self,
            instruction: str,
            environment: PierBaseEnvironment,
            context: PierAgentContext,
        ) -> None:
            await ensure_quack(environment)
            await upload_prompt(environment, instruction)

            env = quack_env()
            env = environment.agent_process_env(env) or env

            # Upload CA certificate if present in the runtime environment
            for ca_cand in [
                "/usr/local/share/ca-certificates/runta-egress.crt",
                "/etc/ssl/certs/ca-certificates.crt",
            ]:
                if os.path.exists(ca_cand):
                    try:
                        await environment.upload_file(ca_cand, "/tmp/runta-ca.crt")
                        env["SSL_CERT_FILE"] = "/tmp/runta-ca.crt"
                        env["REQUESTS_CA_BUNDLE"] = "/tmp/runta-ca.crt"
                        break
                    except Exception:
                        pass

            await environment.exec(quack_command(quack_model(self.model_name)), env=env)
except ImportError:
    pass


def register():
    """Register quack in Harbor and Pier agent factories and enums."""
    try:
        from harbor.models.agent.name import AgentName as HAgentName
        from harbor.agents.factory import AgentFactory as HAgentFactory
        if "quack" not in HAgentName.values():
            m = str.__new__(HAgentName, "quack")
            m._name_ = "QUACK"
            m._value_ = "quack"
            HAgentName._member_map_["QUACK"] = m
            HAgentName._value2member_map_["quack"] = m
            if hasattr(HAgentName, "_member_names_"):
                HAgentName._member_names_.append("QUACK")
            type.__setattr__(HAgentName, "QUACK", m)
        m = HAgentName("quack")
        HAgentFactory._AGENT_MAP[m] = "quack_adapter:QuackHarborAgent"
    except Exception:
        pass

    try:
        from pier.models.agent.name import AgentName as PAgentName
        from pier.agents.factory import AgentFactory as PAgentFactory
        if "quack" not in PAgentName.values():
            m = str.__new__(PAgentName, "quack")
            m._name_ = "QUACK"
            m._value_ = "quack"
            PAgentName._member_map_["QUACK"] = m
            PAgentName._value2member_map_["quack"] = m
            if hasattr(PAgentName, "_member_names_"):
                PAgentName._member_names_.append("QUACK")
            type.__setattr__(PAgentName, "QUACK", m)
        m = PAgentName("quack")
        if "QuackPierAgent" in globals():
            PAgentFactory._AGENT_MAP[m] = QuackPierAgent
    except Exception:
        pass


register()
