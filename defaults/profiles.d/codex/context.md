<context>
- Workspace root: {{ workspace_root }}{% if git_branch %} (git branch: {{ git_branch }}){% endif %} (only worry about this folder and its sub-folders; do not inspect or modify files outside it)
- Date: {{ date }}
{% if folder_listing %}
Top-level files:
{{ folder_listing }}
{% endif %}
{% if instructions %}
## Project instructions (AGENTS.md)
Follow these; they take precedence over the system prompt's guidance.

{{ instructions }}
{% endif %}
{% if skills %}
## Skills
Load one with the `skill` tool when the task matches its description.
{{ skills }}
{% endif %}
{% if memory_index %}
## Memory
Notes from earlier sessions; use the `memory` tool to read or update them.

{{ memory_index }}
{% endif %}
</context>
