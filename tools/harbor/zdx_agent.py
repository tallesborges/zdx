import os
import shlex
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent, ExecInput


class ZdxAgent(BaseInstalledAgent):
    def __init__(self, zdx_repo: str | None = None, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._zdx_repo = zdx_repo or "."

    @staticmethod
    def name() -> str:
        return "zdx"

    @property
    def _install_agent_template_path(self) -> Path:
        return Path(__file__).with_name("install-zdx.sh.j2")

    @property
    def _template_variables(self) -> dict:
        return {
            "zdx_repo": self._zdx_repo,
        }

    def create_run_agent_commands(self, instruction: str) -> list[ExecInput]:
        env = {}
        for key in (
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "GEMINI_API_KEY",
        ):
            value = os.environ.get(key)
            if value:
                env[key] = value

        root = os.environ.get("ZDX_ROOT", "/app")
        model = os.environ.get("ZDX_MODEL")
        thinking = os.environ.get("ZDX_THINKING")
        tools = os.environ.get("ZDX_TOOLS")

        args = ["/root/.cargo/bin/zdx", "--no-thread", "--root", root, "exec", "-p", instruction]
        if model:
            args.extend(["--model", model])
        if thinking:
            args.extend(["--thinking", thinking])
        if tools:
            args.extend(["--tools", tools])

        command = " ".join(shlex.quote(arg) for arg in args)
        command = f"{command} | tee /logs/agent/zdx.txt"

        return [ExecInput(command=command, cwd=root, env=env)]

    def populate_context_post_run(self, context) -> None:
        # No-op: zdx does not emit a Harbor trajectory yet.
        return